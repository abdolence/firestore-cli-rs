use crate::render::counted;
use crate::script::Input;
use firestore::errors::FirestoreError;
use firestore::gcloud_sdk::tonic::Code;
use firestore::FirestoreCollectionId;
use std::fmt::{Display, Formatter};
use std::num::{NonZeroUsize, ParseIntError};
use std::path::{Path, PathBuf};
use thiserror::Error;

pub type FsqResult<T> = Result<T, FsqError>;

/// Whether `error` is Firestore refusing to run a pipeline on this database, as a Standard edition
/// database does with `FAILED_PRECONDITION`.
pub fn is_pipeline_refusal(error: &FirestoreError) -> bool {
    matches!(
        error,
        FirestoreError::DatabaseError(refused)
            if refused.public.code == format!("{:?}", Code::FailedPrecondition)
    )
}

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
    InvalidTreeDepth(InvalidTreeDepth),
    #[error(transparent)]
    UnknownMetaCommand(UnknownMetaCommand),
    #[error(transparent)]
    InvalidMetaArgument(InvalidMetaArgument),
    #[error(transparent)]
    MissingMetaArgument(MissingMetaArgument),
    #[error(transparent)]
    UnclosedQuote(UnclosedQuote),
    #[error("{}", .0.to_string().trim().trim_start_matches("error: "))]
    MetaArguments(#[from] clap::Error),
    #[error(transparent)]
    InvalidParameter(InvalidParameter),
    #[error("no SELECT has run yet: give \\gen one")]
    NoSelectToGenerate,
    #[error("no rows have been shown yet, so there is no row to show")]
    NoRowsToShow,
    #[error(transparent)]
    RowOutOfRange(RowOutOfRange),
    #[error(transparent)]
    FileAccessFailed(FileAccessFailed),
    #[error("export needs a collection or --sql")]
    ExportWithoutSource,
    #[error("export --sql runs a SELECT, and this statement is not one")]
    ExportNeedsRows,
    #[error(transparent)]
    ImportLine(ImportLine),
    #[error(transparent)]
    ImportWrite(ImportWrite),
    #[error(transparent)]
    ImportBatchRefused(ImportBatchRefused),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
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
    ScriptInputFailed(ScriptInputFailed),
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
#[error("`{value}` is not a depth: {source}")]
pub struct InvalidTreeDepth {
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
#[error("row {row} is out of range: the last result has {}", counted(*.rows, "row", "rows"))]
pub struct RowOutOfRange {
    pub row: NonZeroUsize,
    pub rows: usize,
}

#[derive(Debug, Error)]
#[error("{command} needs {expected}")]
pub struct MissingMetaArgument {
    pub command: &'static str,
    pub expected: &'static str,
}

#[derive(Debug, Error)]
#[error("{command} was given a quote that is never closed: {arguments}")]
pub struct UnclosedQuote {
    pub command: &'static str,
    pub arguments: String,
}

#[derive(Debug, Error)]
#[error("`{argument}` is not a parameter: {problem}")]
pub struct InvalidParameter {
    pub argument: String,
    #[source]
    pub problem: ParameterProblem,
}

/// What is wrong with a `--param`.
#[derive(Debug, Error)]
pub enum ParameterProblem {
    #[error("write it as name=value")]
    MissingValue,
    #[error("the name is empty")]
    EmptyName,
    #[error("the value is not a JSON literal, and a string needs its quotes: {0}")]
    NotJson(#[source] serde_json::Error),
}

#[derive(Debug, Error)]
#[error("cannot {access} {}: {source}", path.display())]
pub struct FileAccessFailed {
    pub path: PathBuf,
    pub access: FileAccess,
    #[source]
    pub source: std::io::Error,
}

/// What fsq wanted a file for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileAccess {
    Read,
    Write,
}

impl Display for FileAccess {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Read => "read",
            Self::Write => "write",
        })
    }
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
    Firestore(#[from] FirestoreError),
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

/// A request that Firestore refused as a whole, so that none of the documents of its lines was
/// written.
#[derive(Debug, Error)]
#[error("lines {first_line} to {last_line}: the request was refused: {cause}")]
pub struct ImportBatchRefused {
    pub first_line: usize,
    pub last_line: usize,
    #[source]
    pub cause: FirestoreError,
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
    FieldMode(InvalidFieldMode),
    #[error(transparent)]
    UnsupportedSetting(UnsupportedSetting),
}

/// A Firebase index setting that the firestore crate has no model for.
#[derive(Debug, Error)]
#[error("the index of `{collection_group}` sets {setting}, which fsq cannot declare")]
pub struct UnsupportedSetting {
    pub collection_group: FirestoreCollectionId,
    pub setting: &'static str,
}

#[derive(Debug, Error)]
#[error("the field `{field_path}` must set exactly one of order, arrayConfig, searchConfig and vectorConfig")]
pub struct InvalidFieldMode {
    pub field_path: String,
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

/// An input of a script, which stops at the first failure, with the position and start of the
/// input that failed.
#[derive(Debug, Error)]
#[error("input {number} ({excerpt}) failed: {source}")]
pub struct ScriptInputFailed {
    pub number: usize,
    pub excerpt: String,
    #[source]
    pub source: Box<FsqError>,
}

impl FsqError {
    /// This error as the failure of input `number` of a script. An interrupt and a closed output
    /// pipe are not failures of the input, and the caller's exit code depends on seeing them as
    /// they are.
    pub fn in_input(self, number: usize, input: &Input) -> Self {
        if matches!(self, Self::Interrupted) || self.is_broken_pipe() {
            return self;
        }
        Self::ScriptInputFailed(ScriptInputFailed {
            number,
            excerpt: input.excerpt(),
            source: Box::new(self),
        })
    }

    pub fn file_access(path: &Path, access: FileAccess, source: std::io::Error) -> Self {
        Self::FileAccessFailed(FileAccessFailed {
            path: path.to_path_buf(),
            access,
            source,
        })
    }

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failure_is_named_by_its_input_and_an_interrupt_or_a_closed_pipe_is_not() {
        let input = Input::Statement("SELECT 1".to_string());
        let closed_pipe = FsqError::Io(std::io::ErrorKind::BrokenPipe.into());

        assert!(matches!(
            FsqError::NoProject.in_input(2, &input),
            FsqError::ScriptInputFailed(ScriptInputFailed { number: 2, ref source, .. })
                if matches!(**source, FsqError::NoProject)
        ));
        assert!(matches!(
            FsqError::Interrupted.in_input(2, &input),
            FsqError::Interrupted
        ));
        assert!(closed_pipe.in_input(2, &input).is_broken_pipe());
    }
}
