use crate::errors::{is_pipeline_refusal, FsqError, FsqResult};
use firestore::errors::FirestoreError;
use firestore::{
    FirestoreDatabaseEdition, FirestoreDb, FirestoreDbOptions, FirestoreLiteralsSource,
    FirestorePipeline, FirestorePipelineSource, FirestoreResult, FIREBASE_DEFAULT_DATABASE_ID,
};
use std::fmt::{Display, Formatter};
use tracing::debug;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectId(String);

impl From<&str> for ProjectId {
    fn from(value: &str) -> Self {
        Self(value.to_string())
    }
}

impl Display for ProjectId {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DatabaseId(String);

impl DatabaseId {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for DatabaseId {
    fn default() -> Self {
        Self(FIREBASE_DEFAULT_DATABASE_ID.to_string())
    }
}

impl From<&str> for DatabaseId {
    fn from(value: &str) -> Self {
        Self(value.to_string())
    }
}

impl Display for DatabaseId {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug)]
pub enum EditionStatus {
    Known(FirestoreDatabaseEdition),
    Unreadable(FirestoreError),
}

impl Display for EditionStatus {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Known(FirestoreDatabaseEdition::Standard) => f.write_str("standard"),
            Self::Known(FirestoreDatabaseEdition::Enterprise) => f.write_str("enterprise"),
            Self::Known(FirestoreDatabaseEdition::Unknown(raw)) => {
                write!(f, "edition unknown (raw {raw})")
            }
            // The crate may add editions that this version of fsq has no name for.
            Self::Known(_) => f.write_str("edition unknown"),
            Self::Unreadable(_) => f.write_str("edition unknown"),
        }
    }
}

/// What Firestore answered to a pipeline that reads nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PipelineProbe {
    Accepted,
    /// Firestore refused to run pipelines on this database.
    Refused,
    /// The probe failed for another reason, such as the network or a permission, so it says
    /// nothing about pipelines.
    Failed,
}

impl PipelineProbe {
    /// Runs a pipeline over no rows, which reads nothing and bills no read units.
    pub async fn run(db: &FirestoreDb) -> Self {
        let pipeline = FirestorePipeline::new(FirestorePipelineSource::Literals(
            FirestoreLiteralsSource::new(vec![]),
        ));
        let outcome = db
            .fluent()
            .pipeline()
            .select()
            .from_pipeline(pipeline)
            .query()
            .await
            .map(drop);
        Self::of(outcome)
    }

    fn of(outcome: FirestoreResult<()>) -> Self {
        match outcome {
            Ok(()) => Self::Accepted,
            Err(error) if is_pipeline_refusal(&error) => Self::Refused,
            Err(error) => {
                debug!(%error, "The pipeline probe failed.");
                Self::Failed
            }
        }
    }
}

pub struct Connection {
    pub db: FirestoreDb,
    pub project: ProjectId,
    pub database: DatabaseId,
    pub edition: EditionStatus,
    pub pipelines: PipelineProbe,
}

impl Connection {
    /// Connects to `database` of `project`, or of the project the firestore crate detects from the
    /// environment when none is given.
    pub async fn open(project: Option<ProjectId>, database: DatabaseId) -> FsqResult<Self> {
        let options = match project {
            Some(project) => FirestoreDbOptions::new(project.0),
            None => FirestoreDbOptions::for_default_project_id()
                .await
                .ok_or(FsqError::NoProject)?,
        }
        .with_database_id(database.as_str().to_string());
        let project = ProjectId(options.google_project_id.clone());
        let db = FirestoreDb::with_options(options).await?;
        let edition = match db.read_database_edition().await {
            Ok(edition) => EditionStatus::Known(edition),
            Err(error) => EditionStatus::Unreadable(error),
        };
        let pipelines = PipelineProbe::run(&db).await;
        Ok(Self {
            db,
            project,
            database,
            edition,
            pipelines,
        })
    }

    pub fn label(&self) -> String {
        format!("{}/{} ({})", self.project, self.database, self.edition)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use firestore::gcloud_sdk::tonic::{Code, Status};

    #[test]
    fn a_refused_pipeline_probe_is_the_edition_refusal_and_no_other_failure() {
        let answered = |code: Code| Err(FirestoreError::from(Status::new(code, "answer")));

        assert_eq!(PipelineProbe::of(Ok(())), PipelineProbe::Accepted);
        assert_eq!(
            PipelineProbe::of(answered(Code::FailedPrecondition)),
            PipelineProbe::Refused
        );
        assert_eq!(
            PipelineProbe::of(answered(Code::PermissionDenied)),
            PipelineProbe::Failed
        );
        assert_eq!(
            PipelineProbe::of(answered(Code::Unavailable)),
            PipelineProbe::Failed
        );
    }
}
