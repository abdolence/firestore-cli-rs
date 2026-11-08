use crate::connection::Connection;
use crate::errors::{
    FileAccess, FsqError, FsqResult, ImportLine, ImportLineCause, ImportWrite, InputsFailed,
    InvalidDocument,
};
use crate::render::{animated, counted, Output};
use clap::ValueEnum;
use firestore::errors::FirestoreError;
use firestore::gcloud_sdk::google::firestore::v1::{write::Operation, DocumentMask, Write};
use firestore::gcloud_sdk::tonic::Code;
use firestore::{
    FirestoreDocument, FirestoreFieldPathSegment, FirestoreJsonConverter,
    FirestoreSimpleBatchWriter, FirestoreTaggedJson, FirestoreWritePrecondition,
};
use indicatif::ProgressBar;
use std::convert::Infallible;
use std::fmt::{Display, Formatter};
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::str::FromStr;

/// How many writes one request carries: the most a Firestore commit takes.
const WRITES_PER_BATCH: usize = 500;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImportInput {
    Stdin,
    File(PathBuf),
}

impl FromStr for ImportInput {
    type Err = Infallible;

    fn from_str(value: &str) -> Result<Self, Infallible> {
        Ok(if value == "-" {
            Self::Stdin
        } else {
            Self::File(PathBuf::from(value))
        })
    }
}

/// What import does with a document that already exists. Without a policy, import replaces it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum OnConflict {
    /// Leave the document as it is and count it as skipped.
    Skip,
    /// Report the document as a failure.
    Fail,
}

#[derive(Debug, Clone, PartialEq, Eq, clap::Args)]
pub struct ImportArgs {
    #[arg(help = "File of JSON lines to import, or `-` for stdin")]
    pub input: ImportInput,

    #[arg(
        long,
        value_enum,
        help = "What to do with a document that already exists, instead of replacing it"
    )]
    pub on_conflict: Option<OnConflict>,

    #[arg(
        long,
        conflicts_with = "on_conflict",
        help = "Write only the fields a line lists and keep the other fields of an existing document"
    )]
    pub merge: bool,

    #[arg(
        long,
        help = "Go on after a line or a write fails, and report every failure"
    )]
    pub keep_going: bool,
}

impl ImportArgs {
    /// The write that stores `document` as this import was asked to: replacing the document,
    /// merging into it, or only creating it.
    fn write(&self, document: FirestoreDocument) -> FsqResult<Write> {
        let current_document = self
            .on_conflict
            .map(|_| FirestoreWritePrecondition::Exists(false).try_into())
            .transpose()?;
        let update_mask = self.merge.then(|| {
            let mut field_paths: Vec<_> = document
                .fields
                .keys()
                .map(|name| FirestoreFieldPathSegment::from(name.as_str()).to_string())
                .collect();
            field_paths.sort_unstable();
            DocumentMask { field_paths }
        });
        Ok(Write {
            update_mask,
            current_document,
            operation: Some(Operation::Update(document)),
            ..Write::default()
        })
    }

    /// Whether a write that failed with `code` is a document skipped for already existing, and
    /// not a failure.
    fn skips(&self, code: Code) -> bool {
        self.on_conflict == Some(OnConflict::Skip) && code == Code::AlreadyExists
    }
}

pub async fn run(connection: &Connection, args: &ImportArgs) -> FsqResult<()> {
    let writer = connection.db.create_simple_batch_writer().await?;
    let output = Output::detect();
    let (input, size): (Box<dyn BufRead>, Option<u64>) = match &args.input {
        ImportInput::Stdin => (Box::new(std::io::stdin().lock()), None),
        ImportInput::File(path) => {
            let access = |source| FsqError::file_access(path, FileAccess::Read, source);
            let file = File::open(path).map_err(access)?;
            let size = file.metadata().map_err(access)?.len();
            (Box::new(BufReader::new(file)), Some(size))
        }
    };
    let mut import = Import {
        args,
        reader: LineReader {
            converter: connection.db.json(),
        },
        writer: &writer,
        output,
        progress: progress(size),
        writes: Vec::new(),
        origins: Vec::new(),
        tally: Tally::default(),
    };
    let result = import.read(input).await;
    import.progress.finish_and_clear();
    let tally = import.tally;
    import.output.notice(&format!("{tally}."))?;
    result?;
    match tally.failed {
        0 => Ok(()),
        failed => Err(FsqError::InputsFailed(InputsFailed { failed })),
    }
}

/// A bar over the bytes of a file, and a spinner for stdin, whose size is not known.
fn progress(size: Option<u64>) -> ProgressBar {
    let (bar, template) = match size {
        Some(size) => (
            ProgressBar::new(size),
            "{bar:40.cyan/blue} {percent}% {msg} {elapsed}",
        ),
        None => (ProgressBar::new_spinner(), "{spinner} {msg} {elapsed}"),
    };
    let bar = animated(bar, template);
    bar.set_message(Tally::default().to_string());
    bar
}

#[derive(Debug, Default, Clone, Copy)]
struct Tally {
    written: usize,
    skipped: usize,
    failed: usize,
}

impl Display for Tally {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} written, {} skipped, {} failed",
            counted(self.written, "document", "documents"),
            self.skipped,
            self.failed
        )
    }
}

/// The line a queued write came from, to name in a failure.
struct Origin {
    line: usize,
    document: String,
}

struct Import<'a> {
    args: &'a ImportArgs,
    reader: LineReader<'a>,
    writer: &'a FirestoreSimpleBatchWriter,
    output: Output,
    progress: ProgressBar,
    writes: Vec<Write>,
    origins: Vec<Origin>,
    tally: Tally,
}

impl Import<'_> {
    async fn read<R: BufRead>(&mut self, mut input: R) -> FsqResult<()> {
        let mut text = Vec::new();
        let mut line = 0;
        loop {
            text.clear();
            let read = input.read_until(b'\n', &mut text)?;
            if read == 0 {
                break;
            }
            line += 1;
            self.progress.inc(read as u64);
            if text.trim_ascii().is_empty() {
                continue;
            }
            match self.reader.document(line, &text) {
                Ok(document) => {
                    self.queue(line, document)?;
                    if self.writes.len() == WRITES_PER_BATCH {
                        self.flush().await?;
                    }
                }
                Err(error) => self.refuse(FsqError::ImportLine(error)).await?,
            }
        }
        self.flush().await
    }

    fn queue(&mut self, line: usize, document: FirestoreDocument) -> FsqResult<()> {
        let name = self.reader.converter.relative_name(&document.name);
        self.origins.push(Origin {
            line,
            document: name.to_string(),
        });
        self.writes.push(self.args.write(document)?);
        Ok(())
    }

    /// Counts a line that is no document. Without `--keep-going` the import ends here, after it
    /// has written the documents of the lines before.
    async fn refuse(&mut self, error: FsqError) -> FsqResult<()> {
        self.tally.failed += 1;
        if self.args.keep_going {
            self.report(&error);
            return Ok(());
        }
        self.flush().await?;
        Err(error)
    }

    /// Sends the queued writes in one request. A write that failed does not stop the others of
    /// the request, so without `--keep-going` the first failure is returned once all are counted.
    async fn flush(&mut self) -> FsqResult<()> {
        if self.writes.is_empty() {
            return Ok(());
        }
        let mut batch = self.writer.new_batch();
        batch.writes = std::mem::take(&mut self.writes);
        let origins = std::mem::take(&mut self.origins);
        let response = batch.write().await?;
        let mut first_failure = None;
        for (origin, status) in origins.into_iter().zip(response.statuses) {
            let code = Code::from(status.code);
            if code == Code::Ok {
                self.tally.written += 1;
            } else if self.args.skips(code) {
                self.tally.skipped += 1;
            } else {
                self.tally.failed += 1;
                let failure = FsqError::ImportWrite(ImportWrite {
                    line: origin.line,
                    document: origin.document,
                    code,
                    message: status.message,
                });
                if self.args.keep_going {
                    self.report(&failure);
                } else {
                    first_failure.get_or_insert(failure);
                }
            }
        }
        self.progress.set_message(self.tally.to_string());
        first_failure.map_or(Ok(()), Err)
    }

    fn report(&self, error: &FsqError) {
        self.progress.suspend(|| self.output.report(error));
    }
}

/// Reads the lines of an import file as documents.
struct LineReader<'a> {
    converter: FirestoreJsonConverter<'a>,
}

impl LineReader<'_> {
    fn document(&self, line: usize, text: &[u8]) -> Result<FirestoreDocument, ImportLine> {
        let failed = |cause| ImportLine { line, cause };
        let tagged = serde_json::from_slice::<FirestoreTaggedJson>(text)
            .map_err(|error| failed(ImportLineCause::InvalidJson(error)))?;
        self.converter
            .document_from_tagged(tagged)
            .map_err(|error| failed(error.into()))
    }
}

impl From<FirestoreError> for ImportLineCause {
    fn from(error: FirestoreError) -> Self {
        match error {
            FirestoreError::DeserializeError(error) => Self::InvalidDocument(InvalidDocument {
                code: error.public.code,
                message: error.message,
            }),
            other => Self::Firestore(other),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::args::{CliArgs, CliCommand};
    use crate::export::ExportFormat;
    use clap::{error::ErrorKind, Parser};
    use firestore::errors::codes::JSON_UNKNOWN_WRAPPER;
    use firestore::gcloud_sdk::google::firestore::v1::value::ValueType;
    use firestore::gcloud_sdk::google::firestore::v1::{ArrayValue, MapValue, Value};
    use firestore::gcloud_sdk::google::r#type::LatLng;
    use firestore::gcloud_sdk::prost_types::Timestamp;
    use firestore::FirestoreJsonDocumentOptions;

    const DOCUMENTS_PATH: &str = "projects/demo/databases/(default)/documents";

    fn import_args(arguments: &[&str]) -> Result<ImportArgs, clap::Error> {
        let arguments = ["fsq", "import", "books.jsonl"]
            .into_iter()
            .chain(arguments.iter().copied());
        match CliArgs::try_parse_from(arguments)?.command {
            Some(CliCommand::Import(args)) => Ok(args),
            other => panic!("not an import: {other:?}"),
        }
    }

    fn reader() -> LineReader<'static> {
        LineReader {
            converter: FirestoreJsonConverter::new(DOCUMENTS_PATH),
        }
    }

    fn value(value_type: ValueType) -> Value {
        Value {
            value_type: Some(value_type),
        }
    }

    fn map(fields: impl IntoIterator<Item = (&'static str, Value)>) -> Value {
        value(ValueType::MapValue(MapValue {
            fields: fields
                .into_iter()
                .map(|(name, field)| (name.to_string(), field))
                .collect(),
        }))
    }

    fn dune() -> FirestoreDocument {
        FirestoreDocument {
            name: format!("{DOCUMENTS_PATH}/books/dune"),
            fields: [(
                "title".to_string(),
                value(ValueType::StringValue("Dune".to_string())),
            )]
            .into(),
            ..FirestoreDocument::default()
        }
    }

    /// One field of every Firestore value type, including the shapes the tagged encoding escapes.
    fn every_value_type() -> FirestoreDocument {
        let fields = [
            ("null", value(ValueType::NullValue(0))),
            ("boolean", value(ValueType::BooleanValue(true))),
            ("integer", value(ValueType::IntegerValue(-42))),
            ("big integer", value(ValueType::IntegerValue(i64::MAX))),
            ("double", value(ValueType::DoubleValue(1.5))),
            ("infinity", value(ValueType::DoubleValue(f64::INFINITY))),
            ("string", value(ValueType::StringValue("Dune".to_string()))),
            ("bytes", value(ValueType::BytesValue(vec![0, 1, 2, 255]))),
            (
                "timestamp",
                value(ValueType::TimestampValue(Timestamp {
                    seconds: 1_700_000_000,
                    nanos: 123_456_000,
                })),
            ),
            (
                "reference",
                value(ValueType::ReferenceValue(format!(
                    "{DOCUMENTS_PATH}/books/dune"
                ))),
            ),
            (
                "geo point",
                value(ValueType::GeoPointValue(LatLng {
                    latitude: 51.5,
                    longitude: -0.12,
                })),
            ),
            (
                "array",
                value(ValueType::ArrayValue(ArrayValue {
                    values: vec![value(ValueType::IntegerValue(1)), map([])],
                })),
            ),
            (
                "map",
                map([(
                    "address",
                    map([("city", value(ValueType::StringValue("London".to_string())))]),
                )]),
            ),
            (
                "wrapper lookalike",
                map([("$timestamp", value(ValueType::StringValue("x".to_string())))]),
            ),
            (
                "vector",
                map([
                    (
                        "__type__",
                        value(ValueType::StringValue("__vector__".to_string())),
                    ),
                    (
                        "value",
                        value(ValueType::ArrayValue(ArrayValue {
                            values: vec![
                                value(ValueType::DoubleValue(1.0)),
                                value(ValueType::DoubleValue(2.5)),
                            ],
                        })),
                    ),
                ]),
            ),
        ];
        FirestoreDocument {
            name: format!("{DOCUMENTS_PATH}/books/every-type"),
            fields: fields
                .into_iter()
                .map(|(name, field)| (name.to_string(), field))
                .collect(),
            ..FirestoreDocument::default()
        }
    }

    #[test]
    fn a_line_that_is_not_json_names_its_line_number() {
        let error = reader()
            .document(3, b"{\"__name__\": ")
            .expect_err("not JSON");

        assert_eq!(error.line, 3);
        assert!(matches!(error.cause, ImportLineCause::InvalidJson(_)));
    }

    #[test]
    fn a_refused_document_carries_the_code_of_the_crate() {
        let error = reader()
            .document(7, br#"{"__name__": "books/dune", "at": {"$when": "x"}}"#)
            .expect_err("an unknown wrapper");

        assert_eq!(error.line, 7);
        assert!(matches!(
            error.cause,
            ImportLineCause::InvalidDocument(InvalidDocument { ref code, .. }) if code == JSON_UNKNOWN_WRAPPER
        ));
    }

    #[test]
    fn an_exported_document_reads_back_unchanged() {
        let document = every_value_type();
        let format = ExportFormat {
            converter: FirestoreJsonConverter::new(DOCUMENTS_PATH),
            options: FirestoreJsonDocumentOptions::new(),
            plain: false,
        };
        let line = format.line(&document).expect("a line");

        let read = reader().document(1, line.as_bytes()).expect("a document");

        assert_eq!(read, document);
    }

    #[test]
    fn by_default_a_write_replaces_the_document() {
        let write = import_args(&[])
            .expect("arguments")
            .write(dune())
            .expect("a write");

        assert_eq!(write.update_mask, None);
        assert_eq!(write.current_document, None);
        assert!(matches!(write.operation, Some(Operation::Update(_))));
    }

    #[test]
    fn skip_and_fail_write_only_a_document_that_is_absent() {
        for flag in ["skip", "fail"] {
            let write = import_args(&["--on-conflict", flag])
                .expect("arguments")
                .write(dune())
                .expect("a write");

            assert_eq!(
                write.current_document.and_then(|condition| condition.condition_type),
                Some(firestore::gcloud_sdk::google::firestore::v1::precondition::ConditionType::Exists(false)),
                "--on-conflict {flag}"
            );
        }
    }

    #[test]
    fn merge_writes_the_listed_fields_only() {
        let mut document = dune();
        document.fields.insert(
            "page count".to_string(),
            value(ValueType::IntegerValue(412)),
        );

        let write = import_args(&["--merge"])
            .expect("arguments")
            .write(document)
            .expect("a write");

        assert_eq!(
            write.update_mask,
            Some(DocumentMask {
                field_paths: vec!["`page count`".to_string(), "title".to_string()]
            })
        );
    }

    #[test]
    fn skip_counts_a_document_that_exists_and_fail_does_not() {
        let skip = import_args(&["--on-conflict", "skip"]).expect("arguments");
        let fail = import_args(&["--on-conflict", "fail"]).expect("arguments");

        assert!(skip.skips(Code::AlreadyExists));
        assert!(!skip.skips(Code::PermissionDenied));
        assert!(!fail.skips(Code::AlreadyExists));
    }

    #[test]
    fn overwrite_is_not_a_conflict_policy() {
        let error = import_args(&["--on-conflict", "overwrite"]).expect_err("not a policy");

        assert_eq!(error.kind(), ErrorKind::InvalidValue);
    }

    #[test]
    fn merge_and_on_conflict_exclude_each_other() {
        let error = import_args(&["--merge", "--on-conflict", "skip"]).expect_err("conflict");

        assert_eq!(error.kind(), ErrorKind::ArgumentConflict);
    }
}
