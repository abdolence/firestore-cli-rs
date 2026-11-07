use firestore::errors::FirestoreError;
use std::path::PathBuf;
use thiserror::Error;

pub type FsqResult<T> = Result<T, FsqError>;

#[derive(Debug, Error)]
pub enum FsqError {
    #[error(transparent)]
    Firestore(#[from] FirestoreError),
    #[error(
        "no project: pass -p, set GOOGLE_CLOUD_PROJECT, or set one with `gcloud config set project`"
    )]
    NoProject,
    #[error("`{path}` is not a collection path: {reason}")]
    InvalidCollectionPath { path: String, reason: &'static str },
    #[error("`{value}` is not a sample size: {reason}")]
    InvalidSampleSize { value: String, reason: String },
    #[error("unknown command `{command}`, \\? lists the commands")]
    UnknownMetaCommand { command: String },
    #[error("{command} takes {expected}, not `{argument}`")]
    InvalidMetaArgument {
        command: &'static str,
        argument: String,
        expected: &'static str,
    },
    #[error("{command} needs {expected}")]
    MissingMetaArgument {
        command: &'static str,
        expected: &'static str,
    },
    #[error("cannot read {}: {source}", path.display())]
    ReadScript {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("the pager `{command}` failed: {source}")]
    Pager {
        command: String,
        #[source]
        source: std::io::Error,
    },
    #[error("interrupted")]
    Interrupted,
    #[error("{failed} of the inputs failed")]
    InputsFailed { failed: usize },
    #[error(transparent)]
    Readline(#[from] reedline::ReedlineError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

impl FsqError {
    /// The reader of piped output, such as `head`, stopped reading.
    pub fn is_broken_pipe(&self) -> bool {
        matches!(self, Self::Io(error) if error.kind() == std::io::ErrorKind::BrokenPipe)
    }
}
