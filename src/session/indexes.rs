use super::statement::INDEX_WAIT_TIMEOUT;
use super::{interruptible, Session};
use crate::errors::{FileAccess, FsqError, FsqResult, IndexesFileProblem, InvalidIndexesFile};
use crate::index_command::{IndexCommand, IndexCreateArgs, IndexDeleteArgs, IndexFileArgs};
use crate::index_file::IndexesFile;
use crate::meta::Switch;
use crate::render::{counted, Footer, Grid, IndexChanges};
use firestore::index_builder::FirestoreIndexesBuilder;
use firestore::{
    FirestoreCollectionId, FirestoreDb, FirestoreIndexDeleteTarget, FirestoreIndexListing,
    FirestoreIndexParams,
};
use std::collections::BTreeSet;
use std::path::Path;
use std::time::Instant;

impl Session {
    pub async fn run_index(&mut self, command: IndexCommand) -> FsqResult<()> {
        match command {
            IndexCommand::Plan(args) => self.index_plan(&args).await,
            IndexCommand::Sync(args) => self.index_sync(&args).await,
            IndexCommand::List(args) => self.index_list(args.group).await,
            IndexCommand::Export(args) => self.index_export(args.output.as_deref()).await,
            IndexCommand::Create(args) => self.index_create(args).await,
            IndexCommand::Delete(args) => self.index_delete(args).await,
        }
    }

    /// Lists the indexes and field overrides of `group`, or of every collection group.
    async fn list_live_indexes(
        &self,
        group: Option<&FirestoreCollectionId>,
    ) -> FsqResult<FirestoreIndexListing> {
        let list = self.connection.db.fluent().indexes().list();
        let list = match group {
            Some(group) => list.collection_group(group),
            None => list,
        };
        Ok(interruptible(list.get()).await??)
    }

    /// The fluent statement that declares `params` on its collection group.
    fn declared_indexes(
        &self,
        params: FirestoreIndexParams,
        prune: bool,
    ) -> FirestoreIndexesBuilder<'_, FirestoreDb> {
        let builder = self
            .connection
            .db
            .fluent()
            .indexes()
            .collection_group(&params.collection_group)
            .composite(|_| params.composite_indexes)
            .field_overrides(|_| params.field_overrides)
            .ttl(params.ttl_fields);
        if prune {
            builder.prune_undeclared()
        } else {
            builder
        }
    }

    /// The declarations of the file, one per collection group it names. A group the database has
    /// indexes or field overrides in and the file does not name declares nothing, so that the plan
    /// shows what it holds as undeclared and `--prune` removes it.
    async fn index_declarations(
        &self,
        args: &IndexFileArgs,
    ) -> FsqResult<Vec<FirestoreIndexParams>> {
        let text = std::fs::read_to_string(&args.file)
            .map_err(|source| FsqError::file_access(&args.file, FileAccess::Read, source))?;
        let invalid = |source: IndexesFileProblem| {
            FsqError::InvalidIndexesFile(InvalidIndexesFile {
                path: args.file.clone(),
                source,
            })
        };
        let mut declarations = text
            .parse::<IndexesFile>()
            .map_err(invalid)?
            .into_params()
            .map_err(invalid)?;
        let live = self.list_live_indexes(None).await?;
        let live_groups: BTreeSet<&FirestoreCollectionId> = live
            .indexes
            .iter()
            .map(|listed| &listed.collection_group)
            .chain(live.fields.iter().map(|field| &field.collection_group))
            .collect();
        for group in live_groups {
            if !declarations
                .iter()
                .any(|declared| &declared.collection_group == group)
            {
                declarations.push(FirestoreIndexParams::new(group.clone()));
            }
        }
        declarations.sort_by(|left, right| left.collection_group.cmp(&right.collection_group));
        Ok(declarations)
    }

    async fn index_plan(&mut self, args: &IndexFileArgs) -> FsqResult<()> {
        let started = Instant::now();
        let declarations = self.index_declarations(args).await?;
        let mut changes = IndexChanges::default();
        for params in declarations {
            let group = params.collection_group.clone();
            let plan = interruptible(self.declared_indexes(params, args.prune).plan()).await??;
            if let Some(reason) = &plan.skipped {
                self.output.notice(&format!("{group} skipped: {reason}"))?;
            }
            changes.add_plan(&group, &plan);
        }
        self.show_changes(&changes, started)
    }

    /// Syncs the groups one after another, each with a spinner while its builds run. A group that
    /// fails stops the run after the changes of the groups before it are shown, since those were
    /// written.
    async fn index_sync(&mut self, args: &IndexFileArgs) -> FsqResult<()> {
        let started = Instant::now();
        let declarations = self.index_declarations(args).await?;
        let waits = self.settings.wait == Switch::On;
        let mut changes = IndexChanges::default();
        let mut failure = None;
        for params in declarations {
            let group = params.collection_group.clone();
            // A pruning sync waits for the indexes it created before it deletes, with or without
            // the wait.
            let spinner = (waits || args.prune).then(|| self.wait_spinner(&group));
            let declared = self.declared_indexes(params, args.prune);
            let outcome = interruptible(if waits {
                declared.wait_until_ready(INDEX_WAIT_TIMEOUT).sync()
            } else {
                declared.sync()
            })
            .await;
            if let Some(spinner) = &spinner {
                spinner.finish_and_clear();
            }
            match outcome.and_then(|synced| synced.map_err(FsqError::from)) {
                Ok(report) => {
                    if let Some(reason) = &report.skipped {
                        self.output.notice(&format!("{group} skipped: {reason}"))?;
                    }
                    if let Some(withheld) = &report.withheld_deletes {
                        self.output.notice(&format!(
                            "{group}: {} deletes withheld, because {}",
                            withheld.indexes.len(),
                            withheld.reason
                        ))?;
                    }
                    changes.add_report(&group, &report);
                }
                Err(error) => {
                    failure = Some(error);
                    break;
                }
            }
        }
        if failure.is_none() || !changes.is_empty() {
            self.show_changes(&changes, started)?;
        }
        match failure {
            Some(FsqError::Interrupted) => {
                self.output.notice(
                    "Stopped waiting. The changes already sent go on in Firestore; `fsq index list` shows their state.",
                )?;
                Err(FsqError::Interrupted)
            }
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    fn show_changes(&self, changes: &IndexChanges, started: Instant) -> FsqResult<()> {
        let footer = Footer {
            counted: counted(changes.written(), "change", "changes"),
            elapsed: started.elapsed(),
            notes: Vec::new(),
        };
        self.print_grids(&[("", &Grid::from(changes))], &footer)
    }

    async fn index_list(&mut self, group: Option<FirestoreCollectionId>) -> FsqResult<()> {
        let started = Instant::now();
        let listing = self.list_live_indexes(group.as_ref()).await?;
        self.show_indexes(&listing, started)
    }

    /// Writes the live state as an indexes file, to `output` or to stdout.
    async fn index_export(&mut self, output: Option<&Path>) -> FsqResult<()> {
        let listing = self.list_live_indexes(None).await?;
        for line in IndexesFile::unexported(&listing) {
            self.output.notice(&format!("not exported: {line}"))?;
        }
        let file = IndexesFile::from(&listing);
        let text = format!("{file}\n");
        match output {
            Some(path) => {
                std::fs::write(path, &text)
                    .map_err(|source| FsqError::file_access(path, FileAccess::Write, source))?;
                self.output.notice(&format!(
                    "Wrote {} and {} to {}.",
                    counted(file.indexes.len(), "index", "indexes"),
                    counted(
                        file.field_overrides.len(),
                        "field override",
                        "field overrides"
                    ),
                    path.display()
                ))
            }
            None => self.output.print(&text),
        }
    }

    async fn index_create(&mut self, args: IndexCreateArgs) -> FsqResult<()> {
        let if_not_exists = args.if_not_exists;
        let definition = args.into_definition();
        let group = definition.collection_group.clone();
        let waits = self.settings.wait == Switch::On;
        let started = Instant::now();
        let spinner = waits.then(|| self.wait_spinner(&group));
        let mut create = self
            .connection
            .db
            .fluent()
            .indexes()
            .create()
            .collection_group(&group)
            .index(|_| definition.index);
        if waits {
            create = create.wait_until_ready(INDEX_WAIT_TIMEOUT);
        }
        if if_not_exists {
            create = create.if_not_exists();
        }
        let outcome = interruptible(create.execute()).await;
        if let Some(spinner) = &spinner {
            spinner.finish_and_clear();
        }
        match outcome {
            Err(FsqError::Interrupted) => {
                self.output.notice(&format!(
                    "Stopped waiting. The build goes on in Firestore; `fsq index list --group {group}` shows its state."
                ))?;
                Err(FsqError::Interrupted)
            }
            outcome => self.show_index_created(
                &outcome??,
                &format!("`fsq index list --group {group}`"),
                started,
            ),
        }
    }

    async fn index_delete(&mut self, args: IndexDeleteArgs) -> FsqResult<()> {
        let if_exists = args.if_exists;
        let target = FirestoreIndexDeleteTarget::try_from(args)?;
        let started = Instant::now();
        let delete = self.connection.db.fluent().indexes().delete();
        let delete = match target {
            FirestoreIndexDeleteTarget::Id(target) => delete.id(target.id),
            FirestoreIndexDeleteTarget::Definition(definition) => delete
                .collection_group(&definition.collection_group)
                .index(|_| definition.index),
        };
        let dropped = if if_exists {
            interruptible(delete.if_exists().execute()).await??
        } else {
            Some(interruptible(delete.execute()).await??)
        };
        self.show_index_dropped(dropped.as_ref(), started)
    }
}
