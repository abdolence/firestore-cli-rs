use crate::connection::Connection;
use crate::errors::{
    FileAccess, FsqError, FsqResult, ImportBatchRefused, ImportLine, ImportLineCause, ImportWrite,
    InputsFailed,
};
use crate::render::{animated, counted, Output};
use clap::ValueEnum;
use firestore::gcloud_sdk::google::firestore::v1::{
    write::Operation, DocumentMask, Precondition, Write,
};
use firestore::gcloud_sdk::google::rpc::Status;
use firestore::gcloud_sdk::prost::Message;
use firestore::gcloud_sdk::tonic::Code;
use firestore::{
    FirestoreDb, FirestoreDocument, FirestoreFieldPathSegment, FirestoreResult,
    FirestoreSimpleBatchWriter, FirestoreTaggedJson, FirestoreWritePrecondition,
};
use indicatif::ProgressBar;
use std::convert::Infallible;
use std::fmt::{Display, Formatter};
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::str::FromStr;

/// How many writes one request carries: the most a BatchWrite takes.
const WRITES_PER_BATCH: usize = 500;

/// The encoded size at which a request is sent, below the 10 MiB that BatchWrite refuses.
const BYTES_PER_BATCH: usize = 9 * 1024 * 1024;

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
    /// The precondition every write carries: the document must not exist yet, when the import
    /// has a conflict policy.
    fn current_document(&self) -> FsqResult<Option<Precondition>> {
        Ok(self
            .on_conflict
            .map(|_| FirestoreWritePrecondition::Exists(false).try_into())
            .transpose()?)
    }

    /// The write that stores `document` as this import was asked to: replacing the document,
    /// merging into it, or only creating it.
    fn write(&self, document: FirestoreDocument, current_document: Option<Precondition>) -> Write {
        let update_mask = self.merge.then(|| {
            let mut field_paths: Vec<_> = document
                .fields
                .keys()
                .map(|name| FirestoreFieldPathSegment::from(name.as_str()).to_string())
                .collect();
            field_paths.sort_unstable();
            DocumentMask { field_paths }
        });
        Write {
            update_mask,
            current_document,
            operation: Some(Operation::Update(document)),
            ..Write::default()
        }
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
        documents_path: connection.db.get_documents_path(),
        current_document: args.current_document()?,
        writer: &writer,
        output,
        progress: progress(size),
        batch: Batch::default(),
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

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct Tally {
    written: usize,
    skipped: usize,
    failed: usize,
}

impl Tally {
    /// Counts what Firestore answered for the writes of one request, and returns each failure.
    /// A write that failed does not stop the others of the request, and a refused request fails
    /// every write of it.
    fn settle(
        &mut self,
        args: &ImportArgs,
        origins: Vec<Origin>,
        outcome: FirestoreResult<Vec<Status>>,
    ) -> Vec<FsqError> {
        let statuses = match outcome {
            Ok(statuses) => statuses,
            Err(cause) => {
                self.failed += origins.len();
                let (Some(first), Some(last)) = (origins.first(), origins.last()) else {
                    return Vec::new();
                };
                return vec![FsqError::ImportBatchRefused(ImportBatchRefused {
                    first_line: first.line,
                    last_line: last.line,
                    cause,
                })];
            }
        };
        let mut failures = Vec::new();
        for (origin, status) in origins.into_iter().zip(statuses) {
            let code = Code::from(status.code);
            if code == Code::Ok {
                self.written += 1;
            } else if args.skips(code) {
                self.skipped += 1;
            } else {
                self.failed += 1;
                failures.push(FsqError::ImportWrite(ImportWrite {
                    line: origin.line,
                    document: origin.document,
                    code,
                    message: status.message,
                }));
            }
        }
        failures
    }
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

struct Origin {
    line: usize,
    document: String,
}

struct QueuedWrite {
    origin: Origin,
    write: Write,
    encoded_len: usize,
}

impl QueuedWrite {
    fn new(origin: Origin, write: Write) -> Self {
        let encoded_len = write.encoded_len();
        Self {
            origin,
            write,
            encoded_len,
        }
    }
}

#[derive(Default)]
struct Batch {
    queued: Vec<QueuedWrite>,
    encoded_len: usize,
}

impl Batch {
    /// Whether `queued` can join this request. BatchWrite refuses a request that writes one
    /// document twice, or that is over its size limit, and fails every write of it.
    fn accepts(&self, queued: &QueuedWrite) -> bool {
        self.queued.is_empty()
            || (self.queued.len() < WRITES_PER_BATCH
                && self.encoded_len + queued.encoded_len <= BYTES_PER_BATCH
                && self
                    .queued
                    .iter()
                    .all(|held| held.origin.document != queued.origin.document))
    }

    fn push(&mut self, queued: QueuedWrite) {
        self.encoded_len += queued.encoded_len;
        self.queued.push(queued);
    }
}

struct Import<'a> {
    args: &'a ImportArgs,
    documents_path: &'a str,
    current_document: Option<Precondition>,
    writer: &'a FirestoreSimpleBatchWriter,
    output: Output,
    progress: ProgressBar,
    batch: Batch,
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
            match read_document(self.documents_path, line, &text) {
                Ok(document) => {
                    let queued = self.queued(line, document);
                    if !self.batch.accepts(&queued) {
                        self.flush().await?;
                    }
                    self.batch.push(queued);
                }
                Err(error) => self.refuse(FsqError::ImportLine(error)).await?,
            }
        }
        self.flush().await
    }

    fn queued(&self, line: usize, document: FirestoreDocument) -> QueuedWrite {
        let name = FirestoreDb::relative_document_name(self.documents_path, &document.name);
        let origin = Origin {
            line,
            document: name.to_string(),
        };
        QueuedWrite::new(origin, self.args.write(document, self.current_document))
    }

    /// Counts a line that is no document. Without `--keep-going` the import ends here, after it
    /// has written the documents of the lines before.
    async fn refuse(&mut self, error: FsqError) -> FsqResult<()> {
        self.tally.failed += 1;
        if self.args.keep_going {
            self.report(&error);
            return Ok(());
        }
        if let Err(flush_error) = self.flush().await {
            self.report(&error);
            return Err(flush_error);
        }
        Err(error)
    }

    /// Sends the queued writes in one request. Without `--keep-going` the first failure is
    /// returned once all are counted.
    async fn flush(&mut self) -> FsqResult<()> {
        let batch = std::mem::take(&mut self.batch);
        if batch.queued.is_empty() {
            return Ok(());
        }
        let (origins, writes): (Vec<_>, Vec<_>) = batch
            .queued
            .into_iter()
            .map(|queued| (queued.origin, queued.write))
            .unzip();
        let mut request = self.writer.new_batch();
        request.writes = writes;
        let outcome = request.write().await.map(|response| response.statuses);
        let failures = self.tally.settle(self.args, origins, outcome);
        self.progress.set_message(self.tally.to_string());
        if self.args.keep_going {
            failures.iter().for_each(|failure| self.report(failure));
            return Ok(());
        }
        failures.into_iter().next().map_or(Ok(()), Err)
    }

    fn report(&self, error: &FsqError) {
        self.progress.suspend(|| self.output.report(error));
    }
}

fn read_document(
    documents_path: &str,
    line: usize,
    text: &[u8],
) -> Result<FirestoreDocument, ImportLine> {
    let failed = |cause| ImportLine { line, cause };
    let tagged = serde_json::from_slice::<FirestoreTaggedJson>(text)
        .map_err(|error| failed(ImportLineCause::InvalidJson(error)))?;
    FirestoreDb::deserialize_doc_from_tagged_json(documents_path, tagged)
        .map_err(|error| failed(error.into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::args::{CliArgs, CliCommand};
    use firestore::errors::codes::JSON_UNKNOWN_WRAPPER;
    use firestore::errors::{
        FirestoreError, FirestoreInvalidParametersError, FirestoreInvalidParametersPublicDetails,
    };
    use firestore::gcloud_sdk::google::firestore::v1::value::ValueType;
    use firestore::gcloud_sdk::google::firestore::v1::Value;

    const DOCUMENTS_PATH: &str = "projects/demo/databases/(default)/documents";

    fn import_args(arguments: &[&str]) -> Result<ImportArgs, clap::Error> {
        match CliArgs::parse_command(&[&["import", "books.jsonl"], arguments].concat())? {
            Some(CliCommand::Import(args)) => Ok(args),
            other => panic!("not an import: {other:?}"),
        }
    }

    fn write_of(arguments: &[&str], document: FirestoreDocument) -> Write {
        let args = import_args(arguments).expect("arguments");
        args.write(document, args.current_document().expect("a precondition"))
    }

    fn value(value_type: ValueType) -> Value {
        Value {
            value_type: Some(value_type),
        }
    }

    fn book(id: &str, title: String) -> FirestoreDocument {
        FirestoreDocument {
            name: format!("{DOCUMENTS_PATH}/books/{id}"),
            fields: [("title".to_string(), value(ValueType::StringValue(title)))].into(),
            ..FirestoreDocument::default()
        }
    }

    fn dune() -> FirestoreDocument {
        book("dune", "Dune".to_string())
    }

    fn queued(line: usize, document: FirestoreDocument) -> QueuedWrite {
        let origin = Origin {
            line,
            document: format!("books/{}", document.name.rsplit('/').next().unwrap_or("")),
        };
        QueuedWrite::new(origin, write_of(&[], document))
    }

    fn origins(lines: impl IntoIterator<Item = usize>) -> Vec<Origin> {
        lines
            .into_iter()
            .map(|line| Origin {
                line,
                document: format!("books/{line}"),
            })
            .collect()
    }

    fn status(code: Code) -> Status {
        Status {
            code: code as i32,
            message: "refused".to_string(),
            details: Vec::new(),
        }
    }

    #[test]
    fn a_line_that_is_not_json_names_its_line_number() {
        let error = read_document(DOCUMENTS_PATH, 3, b"{\"__name__\": ").expect_err("not JSON");

        assert_eq!(error.line, 3);
        assert!(matches!(error.cause, ImportLineCause::InvalidJson(_)));
    }

    #[test]
    fn a_refused_document_carries_the_code_of_the_crate() {
        let error = read_document(
            DOCUMENTS_PATH,
            7,
            br#"{"__name__": "books/dune", "at": {"$when": "x"}}"#,
        )
        .expect_err("an unknown wrapper");

        assert_eq!(error.line, 7);
        assert!(matches!(
            error.cause,
            ImportLineCause::Firestore(FirestoreError::DeserializeError(ref refused))
                if refused.public.code == JSON_UNKNOWN_WRAPPER
        ));
    }

    #[test]
    fn by_default_a_write_replaces_the_document() {
        let write = write_of(&[], dune());

        assert_eq!(write.update_mask, None);
        assert_eq!(write.current_document, None);
        assert!(matches!(write.operation, Some(Operation::Update(_))));
    }

    #[test]
    fn skip_and_fail_write_only_a_document_that_is_absent() {
        for flag in ["skip", "fail"] {
            let write = write_of(&["--on-conflict", flag], dune());

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

        let write = write_of(&["--merge"], document);

        assert_eq!(
            write.update_mask,
            Some(DocumentMask {
                field_paths: vec!["`page count`".to_string(), "title".to_string()]
            })
        );
    }

    #[test]
    fn a_batch_takes_a_document_once() {
        let mut batch = Batch::default();
        batch.push(queued(1, dune()));

        assert!(!batch.accepts(&queued(2, dune())));
        assert!(batch.accepts(&queued(2, book("emma", "Emma".to_string()))));
    }

    #[test]
    fn a_batch_stops_before_the_size_limit() {
        let large = |id: &str| queued(1, book(id, "x".repeat(5 * 1024 * 1024)));
        let mut batch = Batch::default();
        assert!(batch.accepts(&large("first")));
        batch.push(large("first"));

        assert!(!batch.accepts(&large("second")));
    }

    #[test]
    fn a_batch_holds_the_most_writes_of_a_request() {
        let mut batch = Batch::default();
        for line in 0..WRITES_PER_BATCH {
            let document = book(&line.to_string(), "x".to_string());
            assert!(batch.accepts(&queued(line, document.clone())));
            batch.push(queued(line, document));
        }

        assert!(!batch.accepts(&queued(WRITES_PER_BATCH, dune())));
    }

    #[test]
    fn a_request_counts_each_write_by_its_status() {
        let args = import_args(&["--on-conflict", "skip"]).expect("arguments");
        let mut tally = Tally::default();

        let failures = tally.settle(
            &args,
            origins([4, 5, 6, 7]),
            Ok(vec![
                status(Code::Ok),
                status(Code::AlreadyExists),
                status(Code::PermissionDenied),
                status(Code::Ok),
            ]),
        );

        assert_eq!(
            tally,
            Tally {
                written: 2,
                skipped: 1,
                failed: 1
            }
        );
        assert!(matches!(
            failures.as_slice(),
            [FsqError::ImportWrite(ImportWrite {
                line: 6,
                code: Code::PermissionDenied,
                ..
            })]
        ));
    }

    #[test]
    fn a_conflict_without_a_skip_policy_is_a_failure() {
        let args = import_args(&["--on-conflict", "fail"]).expect("arguments");
        let mut tally = Tally::default();

        tally.settle(&args, origins([1]), Ok(vec![status(Code::AlreadyExists)]));

        assert_eq!(tally.failed, 1);
        assert_eq!(tally.skipped, 0);
    }

    #[test]
    fn a_refused_request_fails_every_write_of_it() {
        let args = import_args(&[]).expect("arguments");
        let mut tally = Tally::default();

        let failures = tally.settle(
            &args,
            origins([10, 11, 12]),
            Err(FirestoreError::InvalidParametersError(
                FirestoreInvalidParametersError::new(FirestoreInvalidParametersPublicDetails::new(
                    "writes".to_string(),
                    "refused".to_string(),
                )),
            )),
        );

        assert_eq!(
            tally,
            Tally {
                written: 0,
                skipped: 0,
                failed: 3
            }
        );
        assert!(matches!(
            failures.as_slice(),
            [FsqError::ImportBatchRefused(ImportBatchRefused {
                first_line: 10,
                last_line: 12,
                ..
            })]
        ));
    }
}
