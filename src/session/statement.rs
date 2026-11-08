use super::{interruptible, Session};
use crate::errors::{FsqError, FsqResult};
use crate::meta::Switch;
use crate::render::{animated, counted, Cell, DurationText, Footer, Grid, OutputFormat};
use firestore::{
    FirestoreCollectionId, FirestoreExplainOptions, FirestoreIndexListing, FirestoreIndexState,
    FirestoreListedCompositeIndex, FirestoreSqlOptions, FirestoreSqlOutcome,
    FirestoreSqlParameters, FirestoreSqlStatement,
};
use indicatif::ProgressBar;
use std::time::{Duration, Instant};

/// How long `CREATE INDEX` and `ALTER COLLECTION` wait for their build before failing. A build
/// takes minutes even on an empty collection; Ctrl-C stops the wait sooner.
pub(super) const INDEX_WAIT_TIMEOUT: Duration = Duration::from_secs(60 * 60);

/// The collection group whose index build a statement starts, for `\wait` to wait on.
fn waited_collection_group(statement: &FirestoreSqlStatement) -> Option<&FirestoreCollectionId> {
    match statement {
        FirestoreSqlStatement::CreateIndex(create) => Some(&create.definition.collection_group),
        FirestoreSqlStatement::AlterCollection(update) => Some(&update.collection_group),
        _ => None,
    }
}

impl Session {
    pub(super) async fn run_statement(&mut self, sql: &str) -> FsqResult<()> {
        let db = self.connection.db.clone();
        let options = FirestoreSqlOptions::new().allow_index_statements();
        let statement =
            FirestoreSqlStatement::translate(&db, sql, &FirestoreSqlParameters::new(), &options)?;
        if matches!(statement, FirestoreSqlStatement::Query(_)) {
            self.last_select = Some(sql.to_string());
        }
        let mut builder = db.fluent().sql(sql).options(options);
        let waits_for =
            waited_collection_group(&statement).filter(|_| self.settings.wait == Switch::On);
        if waits_for.is_some() {
            builder = builder.wait_until_ready(INDEX_WAIT_TIMEOUT);
        }
        let analyzes = matches!(statement, FirestoreSqlStatement::Query(_))
            && self.settings.timing == Switch::On;
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
                self.explain_interruption(&statement, waits_for)?;
                Err(FsqError::Interrupted)
            }
            outcome => self.show_outcome(outcome??, &statement, started).await,
        }
    }

    async fn show_outcome(
        &mut self,
        outcome: FirestoreSqlOutcome<'_>,
        statement: &FirestoreSqlStatement,
        started: Instant,
    ) -> FsqResult<()> {
        match outcome {
            FirestoreSqlOutcome::Rows(rows) => self.show_rows(rows, statement, started).await,
            FirestoreSqlOutcome::Written(written) => {
                let text = match self.settings.format {
                    OutputFormat::Table => self.done_line(
                        &format!(
                            "{} modified",
                            counted(written.documents_modified, "document", "documents")
                        ),
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
                let parent = match statement {
                    FirestoreSqlStatement::ShowCollections(params) => params
                        .parent
                        .as_deref()
                        .map(|parent| self.connection.db.json().relative_name(parent)),
                    _ => None,
                };
                if parent.is_none() {
                    self.remember_collections(&collection_ids);
                }
                let grid = Grid::collections(&collection_ids, parent);
                let footer = Footer {
                    counted: counted(grid.len(), "collection", "collections"),
                    elapsed: started.elapsed(),
                    notes: Vec::new(),
                };
                self.print_grids(&[("", &grid)], &footer)
            }
            FirestoreSqlOutcome::Indexes(listing) => self.show_indexes(&listing, started),
            FirestoreSqlOutcome::IndexCreated(created) => self.show_index_created(
                &created,
                &format!("SHOW INDEXES FROM {}", created.collection_group),
                started,
            ),
            FirestoreSqlOutcome::IndexDropped(dropped) => {
                self.show_index_dropped(dropped.as_ref(), started)
            }
            FirestoreSqlOutcome::FieldUpdated(field) => {
                self.show_done("Field updated", &Grid::fields([&field]), started)
            }
        }
    }

    pub(super) fn show_indexes(
        &self,
        listing: &FirestoreIndexListing,
        started: Instant,
    ) -> FsqResult<()> {
        let footer = Footer {
            counted: counted(listing.indexes.len(), "index", "indexes"),
            elapsed: started.elapsed(),
            notes: Vec::new(),
        };
        let grids = [
            ("indexes", &Grid::indexes(&listing.indexes)),
            ("fields", &Grid::fields(&listing.fields)),
            ("unrecognised", &Grid::unrecognised(&listing.unrecognised)),
        ];
        self.print_grids(&grids, &footer)
    }

    pub(super) fn show_index_created(
        &self,
        created: &FirestoreListedCompositeIndex,
        listing_command: &str,
        started: Instant,
    ) -> FsqResult<()> {
        let message = match created.state {
            FirestoreIndexState::Creating => {
                format!("Index created and building; {listing_command} shows when it is READY")
            }
            _ => "Index created".to_string(),
        };
        self.show_done(&message, &Grid::indexes([created]), started)
    }

    pub(super) fn show_index_dropped(
        &self,
        dropped: Option<&FirestoreListedCompositeIndex>,
        started: Instant,
    ) -> FsqResult<()> {
        match dropped {
            Some(dropped) => self.show_done("Index dropped", &Grid::indexes([dropped]), started),
            None => {
                self.output.notice("no such index, nothing dropped")?;
                match self.settings.format {
                    OutputFormat::Json => self.output.print(&self.view().grid(&Grid::indexes([]))),
                    OutputFormat::Table | OutputFormat::Jsonl => Ok(()),
                }
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

    pub(super) fn wait_spinner(&self, group: &FirestoreCollectionId) -> ProgressBar {
        animated(
            ProgressBar::new_spinner().with_message(format!(
                "waiting for the build on {group}, Ctrl-C stops waiting"
            )),
            "{spinner} {msg} {elapsed}",
        )
    }

    fn explain_interruption(
        &self,
        statement: &FirestoreSqlStatement,
        waited_for: Option<&FirestoreCollectionId>,
    ) -> FsqResult<()> {
        if let Some(group) = waited_for {
            self.output.notice(&format!(
                "Stopped waiting. The build goes on in Firestore; SHOW INDEXES FROM {group} shows its state."
            ))?;
        } else if matches!(statement, FirestoreSqlStatement::Write(_)) {
            self.output
                .notice("A write that reached Firestore before Ctrl-C may still apply.")?;
        }
        Ok(())
    }
}
