use crate::errors::{FsqError, FsqResult};
use firestore::errors::FirestoreError;
use firestore::{
    FirestoreDatabaseEdition, FirestoreDb, FirestoreDbOptions, FIREBASE_DEFAULT_DATABASE_ID,
};
use std::convert::Infallible;
use std::fmt::{Display, Formatter};
use std::path::PathBuf;
use std::str::FromStr;

const EMULATOR_HOST_VAR: &str = "FIRESTORE_EMULATOR_HOST";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectId(String);

impl ProjectId {
    pub fn resolve(given: Option<ProjectId>) -> FsqResult<ProjectId> {
        given
            .or_else(|| env_var("GOOGLE_CLOUD_PROJECT").map(ProjectId))
            .or_else(gcloud_configured_project)
            .ok_or(FsqError::NoProject)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

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
    pub async fn open(project: ProjectId, database: DatabaseId) -> FsqResult<Self> {
        let db = FirestoreDb::with_options(
            FirestoreDbOptions::new(project.as_str().to_string())
                .with_database_id(database.as_str().to_string()),
        )
        .await?;
        let edition = if env_var(EMULATOR_HOST_VAR).is_some() {
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

fn env_var(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.is_empty())
}

fn gcloud_configured_project() -> Option<ProjectId> {
    if let Some(project) = env_var("CLOUDSDK_CORE_PROJECT") {
        return Some(ProjectId(project));
    }
    let config_dir = env_var("CLOUDSDK_CONFIG")
        .map(PathBuf::from)
        .or_else(gcloud_default_config_dir)?;
    let active = env_var("CLOUDSDK_ACTIVE_CONFIG_NAME")
        .or_else(|| {
            read_gcloud_file(config_dir.join("active_config")).map(|name| name.trim().to_string())
        })
        .unwrap_or_else(|| "default".to_string());
    let config = read_gcloud_file(
        config_dir
            .join("configurations")
            .join(format!("config_{active}")),
    )?;
    project_in_gcloud_config(&config)
}

fn gcloud_default_config_dir() -> Option<PathBuf> {
    if cfg!(windows) {
        dirs::config_dir().map(|dir| dir.join("gcloud"))
    } else {
        dirs::home_dir().map(|dir| dir.join(".config").join("gcloud"))
    }
}

fn read_gcloud_file(path: PathBuf) -> Option<String> {
    match std::fs::read_to_string(&path) {
        Ok(text) => Some(text),
        Err(error) => {
            tracing::debug!(path = %path.display(), %error, "no gcloud configuration read");
            None
        }
    }
}

fn project_in_gcloud_config(config: &str) -> Option<ProjectId> {
    let mut in_core = false;
    for line in config.lines().map(str::trim) {
        if let Some(section) = line.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
            in_core = section.trim() == "core";
        } else if let Some((key, value)) = line.split_once('=') {
            if in_core && key.trim() == "project" && !value.trim().is_empty() {
                return Some(ProjectId(value.trim().to_string()));
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gcloud_project_is_read_from_the_core_section_only() {
        let config = "[compute]\nproject = not-this-one\n\n[core]\naccount = someone@example.com\nproject = latestbit\n";

        assert_eq!(
            project_in_gcloud_config(config),
            Some(ProjectId("latestbit".to_string()))
        );
        assert_eq!(
            project_in_gcloud_config("[compute]\nproject = not-this-one\n"),
            None
        );
    }

    #[test]
    fn unknown_edition_displays_its_raw_value() {
        assert_eq!(
            EditionStatus::Known(FirestoreDatabaseEdition::Unknown(7)).to_string(),
            "edition unknown (raw 7)"
        );
    }
}
