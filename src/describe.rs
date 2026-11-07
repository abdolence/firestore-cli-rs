use crate::connection::{Connection, DatabaseEdition, EditionStatus};
use crate::errors::{FsqError, FsqResult};
use crate::render::{Cell, Grid, ValueKind};
use crate::script::SqlName;
use firestore::gcloud_sdk::google::firestore::v1::value::ValueType;
use firestore::gcloud_sdk::google::firestore::v1::Value;
use firestore::{
    FirestoreCollectionSource, FirestoreDocument, FirestorePipeline, FirestorePipelineSource,
    FirestorePipelineStage, FirestoreSampleSize, FirestoreSampleStage,
};
use futures::StreamExt;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt::{Display, Formatter};
use std::str::FromStr;

/// A collection by its path from the documents root: `books`, or `users/alice/orders` for a
/// subcollection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CollectionPath {
    parent: Option<String>,
    collection_id: String,
}

impl CollectionPath {
    pub fn parent(&self) -> Option<&str> {
        self.parent.as_deref()
    }

    pub fn collection_id(&self) -> &str {
        &self.collection_id
    }
}

impl FromStr for CollectionPath {
    type Err = FsqError;

    fn from_str(path: &str) -> FsqResult<Self> {
        let invalid = |reason| FsqError::InvalidCollectionPath {
            path: path.to_string(),
            reason,
        };
        if path.split('/').any(str::is_empty) {
            return Err(invalid("a segment is empty"));
        }
        if path.split('/').count().is_multiple_of(2) {
            return Err(invalid(
                "it names a document; a collection path has an odd number of segments",
            ));
        }
        let (parent, collection_id) = match path.rsplit_once('/') {
            Some((parent, collection_id)) => (Some(parent.to_string()), collection_id),
            None => (None, path),
        };
        Ok(Self {
            parent,
            collection_id: collection_id.to_string(),
        })
    }
}

impl Display for CollectionPath {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match &self.parent {
            Some(parent) => write!(f, "{parent}/{}", self.collection_id),
            None => f.write_str(&self.collection_id),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SampleSize(u32);

impl SampleSize {
    pub fn documents(self) -> u32 {
        self.0
    }
}

impl Default for SampleSize {
    fn default() -> Self {
        Self(500)
    }
}

impl FromStr for SampleSize {
    type Err = FsqError;

    fn from_str(value: &str) -> FsqResult<Self> {
        match value.parse::<u32>() {
            Ok(0) => Err(FsqError::InvalidSampleSize {
                value: value.to_string(),
                reason: "a sample reads at least one document".to_string(),
            }),
            Ok(size) => Ok(Self(size)),
            Err(error) => Err(FsqError::InvalidSampleSize {
                value: value.to_string(),
                reason: error.to_string(),
            }),
        }
    }
}

impl Display for SampleSize {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DescribeTarget {
    pub collection: CollectionPath,
    pub sample: SampleSize,
}

#[derive(Debug, Default)]
pub struct Description {
    documents: usize,
    /// The names of the top-level fields, as a document holds them.
    top_level_fields: BTreeSet<String>,
    fields: BTreeMap<String, FieldPathSummary>,
}

#[derive(Debug, Default)]
struct FieldPathSummary {
    kinds: BTreeSet<ValueKind>,
    documents_with_it: usize,
    example: Option<Value>,
}

impl Description {
    /// Reads a sample of the collection: a random one through a pipeline on an Enterprise
    /// edition database, and the first documents by ID through a listing elsewhere, since
    /// Standard runs no pipelines.
    pub async fn sample(connection: &Connection, target: &DescribeTarget) -> FsqResult<Self> {
        let db = &connection.db;
        let collection = &target.collection;
        let parent = collection
            .parent()
            .map(|parent| format!("{}/{parent}", db.get_documents_path()));
        let size = target.sample.documents();
        let mut documents = match connection.edition {
            EditionStatus::Known(DatabaseEdition::Enterprise) => {
                let pipeline = FirestorePipeline::new(FirestorePipelineSource::Collection(
                    FirestoreCollectionSource::new(collection.collection_id().to_string())
                        .opt_parent(parent),
                ))
                .with_stage(FirestorePipelineStage::Sample(
                    FirestoreSampleStage::new(FirestoreSampleSize::Documents(size)),
                ));
                db.fluent()
                    .pipeline()
                    .select()
                    .from_pipeline(pipeline)
                    .stream_query_with_errors()
                    .await?
            }
            _ => {
                let listing = db
                    .fluent()
                    .list()
                    .from(collection.collection_id())
                    .page_size(size as usize);
                let listing = match parent {
                    Some(parent) => listing.parent(parent),
                    None => listing,
                };
                listing
                    .stream_all_with_errors()
                    .await?
                    .take(size as usize)
                    .boxed()
            }
        };
        let mut description = Self::default();
        while let Some(document) = documents.next().await {
            description.add(&document?);
        }
        Ok(description)
    }

    pub fn documents(&self) -> usize {
        self.documents
    }

    pub fn top_level_fields(&self) -> &BTreeSet<String> {
        &self.top_level_fields
    }

    pub fn add(&mut self, document: &FirestoreDocument) {
        self.documents += 1;
        self.top_level_fields
            .extend(document.fields.keys().cloned());
        self.add_fields(None, &document.fields);
    }

    fn add_fields(&mut self, prefix: Option<&str>, fields: &HashMap<String, Value>) {
        for (name, value) in fields {
            let path = match prefix {
                Some(prefix) => format!("{prefix}.{}", SqlName(name)),
                None => SqlName(name).to_string(),
            };
            let summary = self.fields.entry(path.clone()).or_default();
            summary.kinds.insert(ValueKind::from(value));
            summary.documents_with_it += 1;
            let is_null = matches!(value.value_type, Some(ValueType::NullValue(_)) | None);
            if summary.example.is_none() || (!is_null && summary.example_is_null()) {
                summary.example = Some(value.clone());
            }
            if let Some(ValueType::MapValue(map)) = &value.value_type {
                self.add_fields(Some(&path), &map.fields);
            }
        }
    }

    /// One row per field path: its types, the share of sampled documents that have it, and an
    /// example value, which is not null when any sampled value was not.
    pub fn into_grid(self) -> Grid {
        let mut grid = Grid::new(["field", "types", "present", "example"]);
        for (path, summary) in self.fields {
            let types = summary
                .kinds
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", ");
            grid.push_row(vec![
                Some(Cell::Text(path)),
                Some(Cell::Text(types)),
                Some(Cell::Percent(
                    summary.documents_with_it as f64 * 100.0 / self.documents as f64,
                )),
                summary.example.map(Cell::Value),
            ]);
        }
        grid
    }
}

impl FieldPathSummary {
    fn example_is_null(&self) -> bool {
        matches!(
            self.example.as_ref().map(|value| &value.value_type),
            Some(Some(ValueType::NullValue(_)) | None)
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::fixtures::{document, draft, dune, string, value};

    fn cells(grid: &Grid, field: &str) -> Vec<Option<Cell>> {
        grid.rows()
            .iter()
            .find(|row| row[0] == Some(Cell::Text(field.to_string())))
            .cloned()
            .unwrap_or_else(|| panic!("no row for {field}"))
    }

    #[test]
    fn a_sample_reports_each_field_path_with_its_types_share_and_a_non_null_example() {
        let mut description = Description::default();
        for sampled in [
            document(
                "untitled",
                vec![
                    ("title", value(ValueType::NullValue(0))),
                    ("unit-price", value(ValueType::IntegerValue(12))),
                ],
            ),
            dune(),
            draft(),
        ] {
            description.add(&sampled);
        }

        let grid = description.into_grid();

        assert_eq!(grid.columns(), ["field", "types", "present", "example"]);
        assert_eq!(
            cells(&grid, "title"),
            vec![
                Some(Cell::Text("title".to_string())),
                Some(Cell::Text("null, string".to_string())),
                Some(Cell::Percent(100.0)),
                Some(Cell::Value(string("Dune"))),
            ]
        );
        assert_eq!(
            cells(&grid, "address.city")[2],
            Some(Cell::Percent(100.0 / 3.0))
        );
        assert_eq!(
            cells(&grid, "`unit-price`")[1],
            Some(Cell::Text("integer".to_string()))
        );
    }

    #[test]
    fn a_collection_path_names_a_root_collection_or_one_under_a_document() {
        let nested: CollectionPath = "users/alice/orders".parse().expect("a collection path");

        assert_eq!(nested.parent(), Some("users/alice"));
        assert_eq!(nested.collection_id(), "orders");
        assert_eq!(
            "books"
                .parse::<CollectionPath>()
                .expect("a collection path")
                .parent(),
            None
        );
        assert!(matches!(
            "users/alice".parse::<CollectionPath>(),
            Err(FsqError::InvalidCollectionPath { .. })
        ));
        assert!(matches!(
            "users//orders".parse::<CollectionPath>(),
            Err(FsqError::InvalidCollectionPath { .. })
        ));
    }
}
