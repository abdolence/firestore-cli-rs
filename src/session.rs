mod indexes;
mod rows;
mod statement;

use crate::codegen::{highlighted, CodegenOptions};
use crate::collection_tree::{CollectionTree, CollectionTreeTarget};
use crate::connection::{Connection, EditionStatus};
use crate::describe::{DescribeTarget, Description};
use crate::errors::{FsqError, FsqResult};
use crate::meta::{meta_help, MetaCommand, Switch};
use crate::render::{
    counted, ExpandedMode, Footer, FooterNote, Grid, Output, OutputFormat, Sample, Theme, View,
    NAME_COLUMN,
};
use crate::script::{Input, Script};
use firestore::{FirestoreCollectionId, FirestoreDatabaseEdition, FirestoreSqlOutcome};
use std::collections::BTreeSet;
use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::Instant;

pub struct Settings {
    pub format: OutputFormat,
    pub expanded: ExpandedMode,
    pub timing: Switch,
    pub wait: Switch,
}

/// Whether a person is at the keyboard, who can be asked for the next page of rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Interaction {
    Repl,
    Script,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flow {
    Continue,
    Quit,
}

/// The names the REPL completes: the collections listed so far and the fields seen in results.
#[derive(Debug, Default)]
pub struct Vocabulary {
    pub collections: BTreeSet<String>,
    pub fields: BTreeSet<String>,
}

pub type SharedVocabulary = Arc<Mutex<Vocabulary>>;

pub struct Session {
    pub connection: Connection,
    pub settings: Settings,
    pub vocabulary: SharedVocabulary,
    pub output: Output,
    pub theme: Theme,
    interaction: Interaction,
    /// The text of the last `SELECT` that ran, for `\gen` without one.
    last_select: Option<String>,
}

impl Session {
    pub fn new(connection: Connection, settings: Settings, interaction: Interaction) -> Self {
        let output = Output::detect();
        Self {
            connection,
            settings,
            vocabulary: SharedVocabulary::default(),
            theme: output.theme(),
            output,
            interaction,
            last_select: None,
        }
    }

    fn view(&self) -> View<'_> {
        View {
            theme: &self.theme,
            format: self.settings.format,
            expanded: self.settings.expanded,
            width: self.output.width(),
            documents_path: self.connection.db.get_documents_path(),
        }
    }

    /// Runs every statement and meta-command of `text` in order, and stops at the first error.
    pub async fn run_script(&mut self, text: &str) -> FsqResult<()> {
        for input in Script::parse(text).into_inputs() {
            if self.run_input(&input).await? == Flow::Quit {
                break;
            }
        }
        Ok(())
    }

    pub async fn run_input(&mut self, input: &Input) -> FsqResult<Flow> {
        match input {
            Input::Statement(sql) => {
                self.run_statement(sql).await?;
                Ok(Flow::Continue)
            }
            Input::Meta(line) => self.run_meta(line.parse()?).await,
        }
    }

    async fn run_meta(&mut self, command: MetaCommand) -> FsqResult<Flow> {
        match command {
            MetaCommand::Quit => return Ok(Flow::Quit),
            MetaCommand::Help => self.output.print(&meta_help())?,
            MetaCommand::ListCollections(None) => self.run_statement("SHOW COLLECTIONS").await?,
            MetaCommand::ListCollections(Some(target)) => self.list_collection_tree(target).await?,
            MetaCommand::Describe(target) => self.describe(&target).await?,
            MetaCommand::Expanded(mode) => {
                self.settings.expanded = mode.unwrap_or(match self.settings.expanded {
                    ExpandedMode::Off => ExpandedMode::On,
                    ExpandedMode::On | ExpandedMode::Auto => ExpandedMode::Off,
                });
                self.output
                    .notice(&format!("Expanded display is {}.", self.settings.expanded))?;
            }
            MetaCommand::Connect(database) => {
                self.connection =
                    Connection::open(Some(self.connection.project.clone()), database).await?;
                self.greet()?;
                if let Ok(mut vocabulary) = self.vocabulary.lock() {
                    *vocabulary = Vocabulary::default();
                }
                if self.interaction == Interaction::Repl {
                    self.load_collections().await;
                }
            }
            MetaCommand::Timing(switch) => {
                self.settings.timing = switch.unwrap_or(self.settings.timing.toggled());
                self.output
                    .notice(&format!("Timing is {}.", self.settings.timing))?;
            }
            MetaCommand::Generate(generate) => {
                self.codegen(generate.sql.as_deref(), &generate.options)
                    .await?
            }
            MetaCommand::Wait(switch) => {
                self.settings.wait = switch.unwrap_or(self.settings.wait.toggled());
                self.output.notice(&format!(
                    "Waiting for index builds is {}.",
                    self.settings.wait
                ))?;
            }
        }
        Ok(Flow::Continue)
    }

    pub async fn describe(&mut self, target: &DescribeTarget) -> FsqResult<()> {
        let started = Instant::now();
        let description = interruptible(Description::sample(&self.connection, target)).await??;
        if let Ok(mut vocabulary) = self.vocabulary.lock() {
            vocabulary
                .fields
                .extend(description.top_level_fields().iter().cloned());
        }
        let documents = description.documents();
        let grid = description.into_grid();
        let footer = Footer {
            counted: counted(grid.len(), "field path", "field paths"),
            elapsed: started.elapsed(),
            notes: vec![FooterNote::Sampled(Sample { documents })],
        };
        self.print_grids(&[("", &grid)], &footer)
    }

    /// Prints the fluent Rust code of `sql`, or of the last `SELECT` that ran when it is `None`.
    pub async fn codegen(&self, sql: Option<&str>, options: &CodegenOptions) -> FsqResult<()> {
        let sql = sql
            .or(self.last_select.as_deref())
            .ok_or(FsqError::NoSelectToGenerate)?;
        let snippet = match options.sample_rows() {
            Some(rows) => interruptible(options.sampled(&self.connection.db, sql, rows)).await??,
            None => options.offline(sql)?,
        };
        self.output.print(&highlighted(&snippet, &self.theme))
    }

    async fn list_collection_tree(&self, target: CollectionTreeTarget) -> FsqResult<()> {
        let started = Instant::now();
        let tree = interruptible(CollectionTree::walk(&self.connection.db, target)).await??;
        let sample = tree.sample();
        let grid = tree.into_grid();
        let footer = Footer {
            counted: counted(grid.len(), "collection", "collections"),
            elapsed: started.elapsed(),
            notes: sample
                .map(FooterNote::SampledPerCollection)
                .into_iter()
                .collect(),
        };
        self.print_grids(&[("", &grid)], &footer)
    }

    /// Prints the grids of a statement and its footer in one write.
    fn print_grids(&self, grids: &[(&str, &Grid)], footer: &Footer) -> FsqResult<()> {
        let view = self.view();
        self.output
            .print(&format!("{}{}", view.grids(grids), view.footer(footer)))
    }

    pub fn greet(&self) -> FsqResult<()> {
        self.output
            .notice(&format!("Connected to {}.", self.connection.label()))?;
        match &self.connection.edition {
            EditionStatus::Known(FirestoreDatabaseEdition::Standard) => self.output.notice(
                &self.theme.warning.apply_to(
                    "SQL needs an Enterprise edition database, and on this Standard edition database SELECT and the writes fail. \\l, \\d, SHOW COLLECTIONS, SHOW INDEXES and the index statements work on both editions.",
                ).to_string(),
            ),
            EditionStatus::Unreadable(error) => self
                .output
                .notice(&format!("The edition of the database could not be read: {error}")),
            EditionStatus::Known(FirestoreDatabaseEdition::Enterprise) | EditionStatus::Emulator => {
                Ok(())
            }
            EditionStatus::Known(_) => self.output.notice(
                &self.theme.warning.apply_to(
                    "SQL needs an Enterprise edition database, and the edition of this database is unknown to this version of fsq, so SELECT and the writes may fail. \\l, \\d, SHOW COLLECTIONS, SHOW INDEXES and the index statements may not work either.",
                ).to_string(),
            ),
        }
    }

    /// Lists the root collections for completion, quietly: completion works without them.
    pub async fn load_collections(&self) {
        match self
            .connection
            .db
            .fluent()
            .sql("SHOW COLLECTIONS")
            .run()
            .await
        {
            Ok(FirestoreSqlOutcome::Collections(collection_ids)) => {
                self.remember_collections(&collection_ids)
            }
            Ok(_) => {}
            Err(error) => tracing::debug!(%error, "no collections listed for completion"),
        }
    }

    fn remember_collections(&self, collection_ids: &[FirestoreCollectionId]) {
        if let Ok(mut vocabulary) = self.vocabulary.lock() {
            vocabulary
                .collections
                .extend(collection_ids.iter().map(ToString::to_string));
        }
    }

    pub fn report(&self, error: &FsqError) {
        self.output.report(error);
    }

    fn remember_fields(&self, grid: &Grid) {
        if let Ok(mut vocabulary) = self.vocabulary.lock() {
            vocabulary.fields.extend(
                grid.columns()
                    .iter()
                    .filter(|column| *column != NAME_COLUMN)
                    .cloned(),
            );
        }
    }
}

/// Runs `future` until it finishes or Ctrl-C stops it. Dropping a call stops the client's wait
/// for it, not Firestore's work on it.
async fn interruptible<F: Future>(future: F) -> FsqResult<F::Output> {
    tokio::select! {
        output = future => Ok(output),
        _ = tokio::signal::ctrl_c() => Err(FsqError::Interrupted),
    }
}
