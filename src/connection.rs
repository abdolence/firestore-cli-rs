use crate::errors::{FsqError, FsqResult};
use firestore::errors::FirestoreError;
use firestore::{
    FirestoreDatabaseEdition, FirestoreDb, FirestoreDbOptions, FIREBASE_DEFAULT_DATABASE_ID,
};
use std::convert::Infallible;
use std::fmt::{Display, Formatter};
use std::str::FromStr;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectId(String);

impl FromStr for ProjectId {
    type Err = Infallible;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Ok(Self(value.to_string()))
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

impl FromStr for DatabaseId {
    type Err = Infallible;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Ok(Self(value.to_string()))
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
    /// The emulator serves no Admin API to read the edition from.
    Emulator,
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
            Self::Emulator => f.write_str("emulator"),
            Self::Unreadable(_) => f.write_str("edition unknown"),
        }
    }
}

pub struct Connection {
    pub db: FirestoreDb,
    pub project: ProjectId,
    pub database: DatabaseId,
    pub edition: EditionStatus,
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
        let edition = if db.is_emulator() {
            EditionStatus::Emulator
        } else {
            match db.read_database_edition().await {
                Ok(edition) => EditionStatus::Known(edition),
                Err(error) => EditionStatus::Unreadable(error),
            }
        };
        Ok(Self {
            db,
            project,
            database,
            edition,
        })
    }

    pub fn label(&self) -> String {
        format!("{}/{} ({})", self.project, self.database, self.edition)
    }
}
