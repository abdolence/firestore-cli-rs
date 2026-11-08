use firestore::errors::FirestoreError;
use firestore::gcloud_sdk::tonic::Code;
use std::num::ParseIntError;
use std::path::PathBuf;
use thiserror::Error;

pub type FsqResult<T> = Result<T, FsqError>;

#[derive(Debug, Error)]
pub enum FsqError {
    #[error(transparent)]
    Firestore(#[from] FirestoreError),
    #[error("no project: pass -p or set GOOGLE_CLOUD_PROJECT")]
    NoProject,
    #[error(transparent)]
    InvalidCollectionPath(InvalidCollectionPath),
    #[error(transparent)]
    InvalidSampleSize(InvalidSampleSize),
    #[error(transparent)]
    UnknownMetaCommand(UnknownMetaCommand),
    #[error(transparent)]
    InvalidMetaArgument(InvalidMetaArgument),
    #[error(transparent)]
    MissingMetaArgument(MissingMetaArgument),
    #[error(transparent)]
    ReadScript(ReadScript),
    #[error(transparent)]
    OpenFile(OpenFile),
    #[error("export needs a collection or --sql")]
    ExportWithoutSource,
    #[error("export --sql runs a SELECT, and this statement is not one")]
    ExportNeedsRows,
    #[error(transparent)]
    ImportLine(ImportLine),
    #[error(transparent)]
    ImportWrite(ImportWrite),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    ReadIndexesFile(ReadIndexesFile),
    #[error(transparent)]
    WriteIndexesFile(WriteIndexesFile),
    #[error(transparent)]
    InvalidIndexesFile(InvalidIndexesFile),
    #[error(transparent)]
    InvalidIndexField(InvalidIndexField),
    #[error(transparent)]
    PagerFailed(PagerFailed),
    #[error("interrupted")]
    Interrupted,
    #[error(transparent)]
    InputsFailed(InputsFailed),
    #[error(transparent)]
    Readline(#[from] reedline::ReedlineError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Error)]
#[error("`{path}` is not a collection path: {reason}")]
pub struct InvalidCollectionPath {
    pub path: String,
    pub reason: &'static str,
}

#[derive(Debug, Error)]
#[error("`{value}` is not a sample size: {source}")]
pub struct InvalidSampleSize {
    pub value: String,
    #[source]
    pub source: ParseIntError,
}

#[derive(Debug, Error)]
#[error("unknown command `{command}`, \\? lists the commands")]
pub struct UnknownMetaCommand {
    pub command: String,
}

#[derive(Debug, Error)]
#[error("{command} takes {expected}, not `{argument}`")]
pub struct InvalidMetaArgument {
    pub command: &'static str,
    pub argument: String,
    pub expected: &'static str,
}

#[derive(Debug, Error)]
#[error("{command} needs {expected}")]
pub struct MissingMetaArgument {
    pub command: &'static str,
    pub expected: &'static str,
}

#[derive(Debug, Error)]
#[error("cannot read {}: {source}", path.display())]
pub struct ReadScript {
    pub path: PathBuf,
    #[source]
    pub source: std::io::Error,
}

#[derive(Debug, Error)]
#[error("cannot {purpose} {}: {source}", path.display())]
pub struct OpenFile {
    pub path: PathBuf,
    /// The verb that names what fsq wanted the file for: `read` or `write`.
    pub purpose: &'static str,
    #[source]
    pub source: std::io::Error,
}

#[derive(Debug, Error)]
#[error("cannot read {}: {source}", path.display())]
pub struct ReadIndexesFile {
    pub path: PathBuf,
    #[source]
    pub source: std::io::Error,
}

/// A line of an import file that is not a document.
#[derive(Debug, Error)]
#[error("line {line}: {cause}")]
pub struct ImportLine {
    pub line: usize,
    #[source]
    pub cause: ImportLineCause,
}

#[derive(Debug, Error)]
pub enum ImportLineCause {
    #[error("not JSON: {0}")]
    InvalidJson(#[source] serde_json::Error),
    #[error(transparent)]
    InvalidDocument(InvalidDocument),
    #[error(transparent)]
    Firestore(FirestoreError),
}

/// A document the tagged encoding refused.
#[derive(Debug, Error)]
#[error("{code}: {message}")]
pub struct InvalidDocument {
    /// One of the constants of `firestore::errors::codes`.
    pub code: String,
    pub message: String,
}

/// A document of an import file that Firestore refused to write.
#[derive(Debug, Error)]
#[error("line {line}: writing {document} failed with {code:?}: {message}")]
pub struct ImportWrite {
    pub line: usize,
    pub document: String,
    pub code: Code,
    pub message: String,
}

#[derive(Debug, Error)]
#[error("cannot write {}: {source}", path.display())]
pub struct WriteIndexesFile {
    pub path: PathBuf,
    #[source]
    pub source: std::io::Error,
}

#[derive(Debug, Error)]
#[error("{} is not an indexes file: {source}", path.display())]
pub struct InvalidIndexesFile {
    pub path: PathBuf,
    #[source]
    pub source: IndexesFileProblem,
}

/// What is wrong with the text of an indexes file.
#[derive(Debug, Error)]
pub enum IndexesFileProblem {
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    CollectionGroup(#[from] FirestoreError),
    #[error(transparent)]
    FieldMode(InvalidFieldMode),
    #[error(transparent)]
    UnreadKeys(UnreadKeys),
}

#[derive(Debug, Error)]
#[error("the field `{field_path}` must set exactly one of order, arrayConfig, vectorConfig and fsq.mode")]
pub struct InvalidFieldMode {
    pub field_path: String,
}

#[derive(Debug, Error)]
#[error("fsq does not read the keys {}", keys.join(", "))]
pub struct UnreadKeys {
    pub keys: Vec<String>,
}

#[derive(Debug, Error)]
#[error(
    "`{value}` is not an index field: write it as <field>:asc, <field>:desc or <field>:contains"
)]
pub struct InvalidIndexField {
    pub value: String,
}

#[derive(Debug, Error)]
#[error("the pager `{command}` failed: {source}")]
pub struct PagerFailed {
    pub command: String,
    #[source]
    pub source: std::io::Error,
}

#[derive(Debug, Error)]
#[error("{failed} of the inputs failed")]
pub struct InputsFailed {
    pub failed: usize,
}

impl FsqError {
    pub fn invalid_meta_argument(
        command: &'static str,
        argument: &str,
        expected: &'static str,
    ) -> Self {
        Self::InvalidMetaArgument(InvalidMetaArgument {
            command,
            argument: argument.to_string(),
            expected,
        })
    }

    pub fn missing_meta_argument(command: &'static str, expected: &'static str) -> Self {
        Self::MissingMetaArgument(MissingMetaArgument { command, expected })
    }

    /// The reader of piped output, such as `head`, stopped reading.
    pub fn is_broken_pipe(&self) -> bool {
        matches!(self, Self::Io(error) if error.kind() == std::io::ErrorKind::BrokenPipe)
    }
}
