mod indexes;
mod rows;
mod statement;

use crate::codegen::CodegenOptions;
use crate::collection_tree::{CollectionTree, CollectionTreeTarget};
use crate::connection::{Connection, EditionStatus, PipelineProbe};
use crate::describe::{DescribeTarget, Description};
use crate::errors::{FsqError, FsqResult};
use crate::meta::{meta_help, MetaCommand, Switch};
use crate::render::{
    counted, ExpandedMode, Footer, FooterNote, Grid, Output, OutputFormat, Sample, Theme, View,
    NAME_COLUMN,
};
use crate::script::{Input, Script};
use firestore::{FirestoreCollectionId, FirestoreSqlOutcome};
use std::collections::BTreeSet;
use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tokio::task::JoinHandle;

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

impl Vocabulary {
    fn remember_collections(&mut self, collection_ids: &[FirestoreCollectionId]) {
        self.collections
            .extend(collection_ids.iter().map(ToString::to_string));
    }
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
    /// The task that lists the root collections for completion while the prompt is already open.
    collections_loading: Option<JoinHandle<()>>,
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
            collections_loading: None,
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
                let connecting = self.output.spinner("connecting", self.settings.format);
                let connected =
                    Connection::open(Some(self.connection.project.clone()), database).await;
                drop(connecting);
                self.connection = connected?;
                self.stop_loading_collections().await;
                self.greet()?;
                if let Ok(mut vocabulary) = self.vocabulary.lock() {
                    *vocabulary = Vocabulary::default();
                }
                if self.interaction == Interaction::Repl {
                    self.start_loading_collections();
                }
            }
            MetaCommand::Timing(switch) => {
                self.settings.timing = self.switched("Timing", self.settings.timing, switch)?;
            }
            MetaCommand::Generate(generate) => {
                self.codegen(generate.sql.as_deref(), &generate.options)
                    .await?
            }
            MetaCommand::Wait(switch) => {
                self.settings.wait =
                    self.switched("Waiting for index builds", self.settings.wait, switch)?;
            }
        }
        Ok(Flow::Continue)
    }

    /// The switch `\timing` or `\wait` leaves `current` at: `requested`, or the other position
    /// when none was named. Tells the user which it is.
    fn switched(
        &self,
        name: &str,
        current: Switch,
        requested: Option<Switch>,
    ) -> FsqResult<Switch> {
        let switch = requested.unwrap_or(current.toggled());
        self.output.notice(&format!("{name} is {switch}."))?;
        Ok(switch)
    }

    pub async fn describe(&mut self, target: &DescribeTarget) -> FsqResult<()> {
        let started = Instant::now();
        let description = self
            .waiting(
                "sampling documents",
                Description::sample(&self.connection, target),
            )
            .await??;
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
            Some(rows) => {
                self.waiting(
                    "sampling documents",
                    options.sampled(&self.connection.db, sql, rows),
                )
                .await??
            }
            None => options.offline(sql)?,
        };
        self.output.print(&snippet)
    }

    async fn list_collection_tree(&self, target: CollectionTreeTarget) -> FsqResult<()> {
        let started = Instant::now();
        let tree = self
            .waiting(
                "listing collections",
                CollectionTree::walk(&self.connection.db, target),
            )
            .await??;
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
        if self.connection.pipelines == PipelineProbe::Refused {
            self.output.notice(
                &self
                    .theme
                    .warning
                    .apply_to("SQL may need an Enterprise edition database.")
                    .to_string(),
            )?;
        }
        match &self.connection.edition {
            EditionStatus::Unreadable(error) => self.output.notice(&format!(
                "The edition of the database could not be read: {error}"
            )),
            EditionStatus::Known(_) => Ok(()),
        }
    }

    /// Starts listing the root collections for completion in the background, quietly: Firestore
    /// can take seconds to list them, and completion works without them until they arrive.
    pub fn start_loading_collections(&mut self) {
        let db = self.connection.db.clone();
        let vocabulary = self.vocabulary.clone();
        self.collections_loading = Some(tokio::spawn(async move {
            match db.fluent().sql("SHOW COLLECTIONS").run().await {
                Ok(FirestoreSqlOutcome::Collections(collection_ids)) => {
                    if let Ok(mut vocabulary) = vocabulary.lock() {
                        vocabulary.remember_collections(&collection_ids);
                    }
                }
                Ok(_) => {}
                Err(error) => tracing::debug!(%error, "no collections listed for completion"),
            }
        }));
    }

    /// Ends the listing that [`Self::start_loading_collections`] started and waits until it has
    /// stopped touching the vocabulary, so that a listing of the previous database cannot land
    /// in the vocabulary of the next one.
    async fn stop_loading_collections(&mut self) {
        if let Some(loading) = self.collections_loading.take() {
            loading.abort();
            let _ = loading.await;
        }
    }

    fn remember_collections(&self, collection_ids: &[FirestoreCollectionId]) {
        if let Ok(mut vocabulary) = self.vocabulary.lock() {
            vocabulary.remember_collections(collection_ids);
        }
    }

    /// Waits for `future` as [`interruptible`] does, with a spinner reading `message` once the
    /// wait has lasted a moment.
    async fn waiting<F: Future>(&self, message: &str, future: F) -> FsqResult<F::Output> {
        let _spinner = self.output.spinner(message, self.settings.format);
        interruptible(future).await
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
