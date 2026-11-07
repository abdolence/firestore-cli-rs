use firestore::errors::FirestoreError;
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

/// A value that is none of the choices a setting offers.
#[derive(Debug, Error)]
#[error("`{0}` is not one of the choices")]
pub struct UnknownChoice(pub String);

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
