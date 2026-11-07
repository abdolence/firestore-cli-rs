use super::rows::{ResultShape, RowsOf};
use super::{interruptible, Session};
use crate::errors::{FsqError, FsqResult};
use crate::meta::Switch;
use crate::render::{relative_name, Cell, Counted, DurationText, Footer, Grid, OutputFormat};
use firestore::{
    FirestoreCollectionId, FirestoreExplainOptions, FirestoreIndexState, FirestoreSqlOptions,
    FirestoreSqlOutcome, FirestoreSqlParameters, FirestoreSqlStatement,
};
use indicatif::{ProgressBar, ProgressStyle};
use std::time::{Duration, Instant};

/// How long `CREATE INDEX` and `ALTER COLLECTION` wait for their build before failing. A build
/// takes minutes even on an empty collection; Ctrl-C stops the wait sooner.
const INDEX_WAIT_TIMEOUT: Duration = Duration::from_secs(60 * 60);

enum StatementKind {
    Query(ResultShape),
    Explain(ResultShape),
    Write,
    /// The path of the document `IN` names, `None` for the root.
    ShowCollections(Option<String>),
    ShowIndexes,
    CreateIndex(FirestoreCollectionId),
    DropIndex,
    AlterCollection(FirestoreCollectionId),
}

impl StatementKind {
    fn new(statement: &FirestoreSqlStatement, documents_path: &str) -> Self {
        match statement {
            FirestoreSqlStatement::Query(pipeline) => Self::Query(ResultShape::from(pipeline)),
            FirestoreSqlStatement::Explain(explain) => {
                Self::Explain(ResultShape::from(&explain.pipeline))
            }
            FirestoreSqlStatement::Write(_) => Self::Write,
            FirestoreSqlStatement::ShowCollections(params) => Self::ShowCollections(
                params
                    .parent
                    .as_deref()
                    .map(|parent| relative_name(documents_path, parent).to_string()),
            ),
            FirestoreSqlStatement::ShowIndexes(_) => Self::ShowIndexes,
            FirestoreSqlStatement::CreateIndex(create) => {
                Self::CreateIndex(create.definition.collection_group.clone())
            }
            FirestoreSqlStatement::DropIndex(_) => Self::DropIndex,
            FirestoreSqlStatement::AlterCollection(update) => {
                Self::AlterCollection(update.collection_group.clone())
            }
        }
    }

    /// The collection group whose build a statement starts, for `\wait` to wait on.
    fn builds(&self) -> Option<&FirestoreCollectionId> {
        match self {
            Self::CreateIndex(group) | Self::AlterCollection(group) => Some(group),
            _ => None,
        }
    }
}

impl Session {
    pub(super) async fn run_statement(&mut self, sql: &str) -> FsqResult<()> {
        let db = self.connection.db.clone();
        let options = FirestoreSqlOptions::new().allow_index_statements();
        let statement =
            FirestoreSqlStatement::translate(&db, sql, &FirestoreSqlParameters::new(), &options)?;
        let kind = StatementKind::new(&statement, db.get_documents_path());
        let mut builder = db.fluent().sql(sql).options(options);
        let waits_for = kind.builds().filter(|_| self.settings.wait == Switch::On);
        if waits_for.is_some() {
            builder = builder.wait_until_ready(INDEX_WAIT_TIMEOUT);
        }
        let analyzes =
            matches!(kind, StatementKind::Query(_)) && self.settings.timing == Switch::On;
        if analyzes {
            builder =
                builder.explain_with_options(FirestoreExplainOptions::new().with_analyze(true));
        }
        let started = Instant::now();
        let spinner = waits_for.map(|group| self.wait_spinner(group));
        let outcome = interruptible(builder.run()).await;
        if let Some(spinner) = &spinner {
            spinner.finish_and_clear();
        }
        match outcome {
            Err(FsqError::Interrupted) => {
                self.explain_interruption(&kind, waits_for)?;
                Err(FsqError::Interrupted)
            }
            outcome => self.show_outcome(outcome??, &kind, started).await,
        }
    }

    async fn show_outcome(
        &mut self,
        outcome: FirestoreSqlOutcome<'_>,
        kind: &StatementKind,
        started: Instant,
    ) -> FsqResult<()> {
        match outcome {
            FirestoreSqlOutcome::Rows(rows) => match kind {
                StatementKind::Explain(shape) => {
                    self.show_rows(rows, shape, RowsOf::Explain, started).await
                }
                StatementKind::Query(shape) => {
                    self.show_rows(rows, shape, RowsOf::Select, started).await
                }
                _ => {
                    self.show_rows(rows, &ResultShape::default(), RowsOf::Select, started)
                        .await
                }
            },
            FirestoreSqlOutcome::Written(written) => {
                let text = match self.settings.format {
                    OutputFormat::Table => self.done_line(
                        &match written.documents_modified {
                            1 => "1 document modified".to_string(),
                            count => format!("{count} documents modified"),
                        },
                        started,
                    ),
                    OutputFormat::Json | OutputFormat::Jsonl => {
                        let mut grid = Grid::new(["documents_modified"]);
                        grid.push_row(vec![Some(Cell::Count(written.documents_modified))]);
                        self.view().grid(&grid)
                    }
                };
                self.output.print(&text)
            }
            FirestoreSqlOutcome::Collections(collection_ids) => {
                let parent = match kind {
                    StatementKind::ShowCollections(parent) => parent.as_deref(),
                    _ => None,
                };
                if parent.is_none() {
                    self.remember_collections(&collection_ids);
                }
                let grid = Grid::collections(&collection_ids, parent);
                let footer = Footer {
                    count: grid.len(),
                    counted: Counted::Collections,
                    elapsed: started.elapsed(),
                    notes: Vec::new(),
                };
                let view = self.view();
                self.output
                    .print(&format!("{}{}", view.grid(&grid), view.footer(&footer)))
            }
            FirestoreSqlOutcome::Indexes(listing) => {
                let footer = Footer {
                    count: listing.indexes.len(),
                    counted: Counted::Indexes,
                    elapsed: started.elapsed(),
                    notes: Vec::new(),
                };
                let view = self.view();
                let grids = [
                    ("indexes", &Grid::indexes(&listing.indexes)),
                    ("fields", &Grid::fields(&listing.fields)),
                    ("unrecognised", &Grid::unrecognised(&listing.unrecognised)),
                ];
                self.output
                    .print(&format!("{}{}", view.grids(&grids), view.footer(&footer)))
            }
            FirestoreSqlOutcome::IndexCreated(created) => {
                let message = match created.state {
                    FirestoreIndexState::Creating => format!(
                        "Index created and building; SHOW INDEXES FROM {} shows when it is READY",
                        created.collection_group
                    ),
                    _ => "Index created".to_string(),
                };
                self.show_done(&message, &Grid::indexes([&created]), started)
            }
            FirestoreSqlOutcome::IndexDropped(Some(dropped)) => {
                self.show_done("Index dropped", &Grid::indexes([&dropped]), started)
            }
            FirestoreSqlOutcome::IndexDropped(None) => {
                self.output.notice("no such index, nothing dropped")?;
                match self.settings.format {
                    OutputFormat::Json => self.output.print(&self.view().grid(&Grid::indexes([]))),
                    OutputFormat::Table | OutputFormat::Jsonl => Ok(()),
                }
            }
            FirestoreSqlOutcome::FieldUpdated(field) => {
                self.show_done("Field updated", &Grid::fields([&field]), started)
            }
        }
    }

    fn show_done(&self, message: &str, changed: &Grid, started: Instant) -> FsqResult<()> {
        let view = self.view();
        let text = match self.settings.format {
            OutputFormat::Table => {
                format!("{}{}", self.done_line(message, started), view.grid(changed))
            }
            OutputFormat::Json | OutputFormat::Jsonl => view.grid(changed),
        };
        self.output.print(&text)
    }

    fn done_line(&self, message: &str, started: Instant) -> String {
        format!(
            "{} {}\n",
            self.theme.success.apply_to(message),
            self.theme
                .muted
                .apply_to(format!("· {}", DurationText(started.elapsed())))
        )
    }

    fn wait_spinner(&self, group: &FirestoreCollectionId) -> ProgressBar {
        let spinner = ProgressBar::new_spinner().with_message(format!(
            "waiting for the build on {group}, Ctrl-C stops waiting"
        ));
        spinner.set_style(
            ProgressStyle::with_template("{spinner} {msg} {elapsed}")
                .expect("the template is a literal with keys indicatif knows"),
        );
        spinner.enable_steady_tick(Duration::from_millis(120));
        spinner
    }

    fn explain_interruption(
        &self,
        kind: &StatementKind,
        waited_for: Option<&FirestoreCollectionId>,
    ) -> FsqResult<()> {
        if let Some(group) = waited_for {
            self.output.notice(&format!(
                "Stopped waiting. The build goes on in Firestore; SHOW INDEXES FROM {group} shows its state."
            ))?;
        } else if matches!(kind, StatementKind::Write) {
            self.output
                .notice("A write that reached Firestore before Ctrl-C may still apply.")?;
        }
        Ok(())
    }
}
