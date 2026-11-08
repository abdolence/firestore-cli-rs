use crate::connection::Connection;
use crate::describe::CollectionPath;
use crate::errors::{FileAccess, FsqError, FsqResult, InvalidCollectionPath};
use crate::render::{animated, counted, Output};
use clap::ArgGroup;
use firestore::{
    FirestoreCollectionId, FirestoreDb, FirestoreDocument, FirestoreJsonConverter,
    FirestoreJsonDocumentOptions, FirestoreResult, FirestoreSqlOptions, FirestoreSqlOutcome,
};
use futures::stream::BoxStream;
use futures::StreamExt;
use indicatif::ProgressBar;
use std::collections::VecDeque;
use std::fs::File;
use std::io::{BufWriter, IsTerminal, Write};
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq, clap::Args)]
#[command(group(ArgGroup::new("source").required(true).args(["collection", "sql"])))]
pub struct ExportArgs {
    #[arg(
        help = "Collection path, such as `books` or `users/alice/orders`; with --group, a collection ID"
    )]
    pub collection: Option<CollectionPath>,

    #[arg(
        long,
        requires = "collection",
        help = "Export every collection with this ID, at any depth"
    )]
    pub group: bool,

    #[arg(
        long,
        conflicts_with_all = ["sql", "group"],
        help = "Include the subcollections of every document, with their full relative paths as names"
    )]
    pub recursive: bool,

    #[arg(
        long,
        value_name = "SELECT",
        help = "Export the rows of a SELECT instead of a collection. Needs an Enterprise edition database"
    )]
    pub sql: Option<String>,

    #[arg(
        long,
        help = "Add __create_time__ and __update_time__ to every document"
    )]
    pub with_metadata: bool,

    #[arg(
        long,
        help = "Write the plain encoding, for reading; fsq import cannot read it back"
    )]
    pub plain: bool,

    #[arg(short, long, help = "Write to this file instead of stdout")]
    pub output: Option<PathBuf>,
}

pub async fn run(connection: &Connection, args: &ExportArgs) -> FsqResult<()> {
    let source = ExportSource::try_from(args)?;
    let to_terminal = args.output.is_none() && std::io::stdout().is_terminal();
    let export = Export {
        db: &connection.db,
        format: ExportFormat {
            converter: connection.db.json(),
            options: FirestoreJsonDocumentOptions::new().with_metadata(args.with_metadata),
            plain: args.plain,
        },
        recursive: args.recursive,
        progress: if to_terminal {
            ProgressBar::hidden()
        } else {
            animated(
                ProgressBar::new_spinner(),
                "{spinner} {pos} documents {elapsed}",
            )
        },
    };
    let exported = match &args.output {
        Some(path) => {
            let file = File::create(path)
                .map_err(|source| FsqError::file_access(path, FileAccess::Write, source))?;
            export.write(source, BufWriter::new(file)).await
        }
        None => {
            export
                .write(source, BufWriter::new(std::io::stdout()))
                .await
        }
    };
    export.progress.finish_and_clear();
    Output::detect().notice(&format!(
        "Exported {}.",
        counted(exported?, "document", "documents")
    ))
}

type DocumentStream<'b> = BoxStream<'b, FirestoreResult<FirestoreDocument>>;

struct Export<'a> {
    db: &'a FirestoreDb,
    format: ExportFormat<'a>,
    recursive: bool,
    progress: ProgressBar,
}

impl Export<'_> {
    async fn write<W: Write>(&self, source: ExportSource, mut writer: W) -> FsqResult<usize> {
        let mut subcollections = VecDeque::new();
        let first = match source {
            ExportSource::Sql(sql) => Some(self.rows(&sql).await?),
            ExportSource::Group(group) => Some(self.group(&group).await?),
            ExportSource::Collection(path) => {
                subcollections.push_back(path);
                None
            }
        };
        let mut exported = 0;
        if let Some(documents) = first {
            exported += self
                .write_documents(documents, &mut writer, &mut subcollections)
                .await?;
        }
        while let Some(path) = subcollections.pop_front() {
            let documents = self.collection(&path).await?;
            exported += self
                .write_documents(documents, &mut writer, &mut subcollections)
                .await?;
        }
        writer.flush()?;
        Ok(exported)
    }

    async fn write_documents<W: Write>(
        &self,
        mut documents: DocumentStream<'_>,
        writer: &mut W,
        subcollections: &mut VecDeque<CollectionPath>,
    ) -> FsqResult<usize> {
        let mut written = 0;
        while let Some(document) = documents.next().await {
            let document = document?;
            // A missing document, listed under --recursive for its subcollections, has no times.
            let exists = document.create_time.is_some();
            if exists {
                self.format.write_line(writer, &document)?;
                self.progress.inc(1);
                written += 1;
            }
            if self.recursive {
                self.queue_subcollections(&document, subcollections).await?;
            }
        }
        Ok(written)
    }

    async fn queue_subcollections(
        &self,
        document: &FirestoreDocument,
        subcollections: &mut VecDeque<CollectionPath>,
    ) -> FsqResult<()> {
        let parent = self.format.converter.relative_name(&document.name);
        let mut collection_ids = self
            .db
            .fluent()
            .list()
            .collections()
            .parent(&document.name)
            .stream_all_with_errors()
            .await?;
        while let Some(collection_id) = collection_ids.next().await {
            subcollections.push_back(format!("{parent}/{}", collection_id?).parse()?);
        }
        Ok(())
    }

    async fn collection(&self, path: &CollectionPath) -> FsqResult<DocumentStream<'_>> {
        let listing = self.db.fluent().list().from(path.collection_id());
        let listing = match path.parent() {
            Some(parent) => listing.parent(format!("{}/{parent}", self.db.get_documents_path())),
            None => listing,
        };
        // ListDocuments skips a document that does not exist, and with it every subcollection
        // below it.
        let listing = if self.recursive {
            listing.show_missing()
        } else {
            listing
        };
        Ok(listing.stream_all_with_errors().await?)
    }

    async fn group(&self, group: &FirestoreCollectionId) -> FsqResult<DocumentStream<'_>> {
        Ok(self
            .db
            .fluent()
            .select()
            .from(group)
            .all_descendants()
            .stream_query_with_errors()
            .await?)
    }

    async fn rows(&self, sql: &str) -> FsqResult<DocumentStream<'_>> {
        let outcome = self
            .db
            .fluent()
            .sql(sql)
            .options(FirestoreSqlOptions::read_only())
            .run()
            .await?;
        match outcome {
            FirestoreSqlOutcome::Rows(rows) => Ok(rows
                .filter_map(|row| async { row.map(|row| row.document).transpose() })
                .boxed()),
            _ => Err(FsqError::ExportNeedsRows),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ExportSource {
    Collection(CollectionPath),
    Group(FirestoreCollectionId),
    Sql(String),
}

impl TryFrom<&ExportArgs> for ExportSource {
    type Error = FsqError;

    fn try_from(args: &ExportArgs) -> FsqResult<Self> {
        let Some(path) = &args.collection else {
            return args
                .sql
                .clone()
                .map(Self::Sql)
                .ok_or(FsqError::ExportWithoutSource);
        };
        if !args.group {
            return Ok(Self::Collection(path.clone()));
        }
        match path.parent() {
            None => Ok(Self::Group(path.collection_id().clone())),
            Some(parent) => Err(FsqError::InvalidCollectionPath(InvalidCollectionPath {
                path: format!("{parent}/{}", path.collection_id()),
                reason: "a collection group is named by a collection ID, without a parent path",
            })),
        }
    }
}

struct ExportFormat<'a> {
    converter: FirestoreJsonConverter<'a>,
    options: FirestoreJsonDocumentOptions,
    plain: bool,
}

impl ExportFormat<'_> {
    fn write_line<W: Write>(&self, writer: &mut W, document: &FirestoreDocument) -> FsqResult<()> {
        let json = if self.plain {
            self.converter
                .plain_document(document, &self.options)
                .into_inner()
        } else {
            self.converter
                .tagged_document(document, &self.options)?
                .into_inner()
        };
        writeln!(writer, "{json}")?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::args::{CliArgs, CliCommand};
    use clap::Parser;

    fn export_args(arguments: &[&str]) -> Result<ExportArgs, clap::Error> {
        let arguments = ["fsq", "export"]
            .into_iter()
            .chain(arguments.iter().copied());
        match CliArgs::try_parse_from(arguments)?.command {
            Some(CliCommand::Export(args)) => Ok(args),
            other => panic!("not an export: {other:?}"),
        }
    }

    #[test]
    fn a_group_is_named_by_a_collection_id_without_a_parent() {
        let args = export_args(&["users/alice/orders", "--group"]).expect("arguments parse");

        let error = ExportSource::try_from(&args).expect_err("a path names no group");

        assert!(matches!(error, FsqError::InvalidCollectionPath(_)));
    }

    #[test]
    fn a_group_with_a_collection_id_is_a_group_source() {
        let args = export_args(&["orders", "--group"]).expect("arguments parse");

        let source = ExportSource::try_from(&args).expect("a collection ID names a group");

        assert_eq!(
            source,
            ExportSource::Group("orders".parse().expect("a collection ID"))
        );
    }
}
