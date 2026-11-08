//! The `firestore.indexes.json` format of the Firebase CLI, read into the declarations the
//! firestore crate plans and syncs, and written from a listing of a live database.
//!
//! The Firebase format has no place for what only the Enterprise edition offers, nor for a TTL
//! field that keeps its automatic single-field indexes. Those go under `fsq` keys, which the
//! Firebase CLI ignores: `fsq` on the file (`ttl`), on an index (`unique`, `density`, `search`)
//! and on an index field (`mode`, for text and geo search fields). Keys that neither the Firebase
//! format nor `fsq` defines are refused rather than ignored, since an ignored `ttl` or `density`
//! of the Firebase format would be dropped from the declaration without a word.

use crate::errors::{IndexesFileProblem, InvalidFieldMode, UnreadKeys};
use firestore::{
    FirestoreCollectionId, FirestoreCompositeIndex, FirestoreFieldOverride,
    FirestoreFieldOverrideIndex, FirestoreFieldOverrideOutcome, FirestoreFieldOverrideTarget,
    FirestoreFieldTtlOutcome, FirestoreGeoIndexConfig, FirestoreIndexDensity, FirestoreIndexField,
    FirestoreIndexFieldMode, FirestoreIndexListing, FirestoreIndexParams, FirestoreIndexQueryScope,
    FirestoreListedCompositeIndex, FirestoreQueryDirection, FirestoreSearchIndexOptions,
    FirestoreVectorIndexConfig,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt::{Display, Formatter};
use std::str::FromStr;

/// The `fieldPath` that names every field of a collection group, as in the Firebase format.
const ALL_FIELDS_PATH: &str = "*";

#[derive(Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexesFile {
    #[serde(default)]
    pub indexes: Vec<FileIndex>,
    #[serde(default)]
    pub field_overrides: Vec<FileFieldOverride>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fsq: Option<FileExtras>,
}

/// The `fsq` key of the file.
#[derive(Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileExtras {
    /// The fields whose timestamp value enables document TTL, one per collection group.
    #[serde(default)]
    pub ttl: Vec<FileTtlField>,
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileTtlField {
    pub collection_group: String,
    pub field_path: String,
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileIndex {
    pub collection_group: String,
    pub query_scope: FileQueryScope,
    pub fields: Vec<FileIndexField>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fsq: Option<FileIndexExtras>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum FileQueryScope {
    Collection,
    CollectionGroup,
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileIndexField {
    pub field_path: String,
    #[serde(flatten)]
    pub mode: FileMode,
}

/// How a field participates in an index. Exactly one of the four keys is set.
#[derive(Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileMode {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order: Option<FileOrder>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub array_config: Option<FileArrayConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vector_config: Option<FileVectorConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fsq: Option<FileFieldExtras>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum FileOrder {
    Ascending,
    Descending,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum FileArrayConfig {
    Contains,
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
pub struct FileVectorConfig {
    pub dimension: u32,
    #[serde(default)]
    pub flat: FlatVectorIndex,
}

/// The `flat` key of a vector config, the one kind of vector index Firestore has.
#[derive(Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct FlatVectorIndex {}

/// The `fsq` key of an index field: the modes the Firebase format has no key for.
#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum FileFieldExtras {
    Text,
    Geo(FileGeoOptions),
}

#[derive(Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileGeoOptions {
    #[serde(default)]
    pub geo_json_indexing_disabled: bool,
}

/// The `fsq` key of an index.
#[derive(Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileIndexExtras {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unique: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub density: Option<FileDensity>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub search: Option<FileSearchOptions>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum FileDensity {
    SparseAll,
    SparseAny,
    Dense,
}

#[derive(Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileSearchOptions {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text_language: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text_language_override_field_path: Option<String>,
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileFieldOverride {
    pub collection_group: String,
    pub field_path: String,
    pub indexes: Vec<FileOverrideIndex>,
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileOverrideIndex {
    pub query_scope: FileQueryScope,
    #[serde(flatten)]
    pub mode: FileMode,
}

impl FromStr for IndexesFile {
    type Err = IndexesFileProblem;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let input: serde_json::Value = serde_json::from_str(text)?;
        let file: Self = serde_json::from_value(input.clone())?;
        let mut unread = Vec::new();
        collect_unread_keys(
            &input,
            &serde_json::to_value(&file)?,
            &mut String::new(),
            &mut unread,
        );
        if unread.is_empty() {
            Ok(file)
        } else {
            Err(IndexesFileProblem::UnreadKeys(UnreadKeys { keys: unread }))
        }
    }
}

/// The paths of the keys of `input` that `parsed`, the file read from it and written out again,
/// does not have: the keys deserializing skipped.
fn collect_unread_keys(
    input: &serde_json::Value,
    parsed: &serde_json::Value,
    path: &mut String,
    unread: &mut Vec<String>,
) {
    match (input, parsed) {
        (serde_json::Value::Object(input), serde_json::Value::Object(parsed)) => {
            for (key, value) in input {
                let parent_length = path.len();
                if !path.is_empty() {
                    path.push('.');
                }
                path.push_str(key);
                match parsed.get(key) {
                    Some(parsed) => collect_unread_keys(value, parsed, path, unread),
                    None => unread.push(path.clone()),
                }
                path.truncate(parent_length);
            }
        }
        (serde_json::Value::Array(input), serde_json::Value::Array(parsed)) => {
            for (position, (value, parsed)) in input.iter().zip(parsed).enumerate() {
                let parent_length = path.len();
                path.push_str(&format!("[{position}]"));
                collect_unread_keys(value, parsed, path, unread);
                path.truncate(parent_length);
            }
        }
        _ => {}
    }
}

impl Display for IndexesFile {
    /// The file as the Firebase CLI writes it: two-space indentation, no trailing newline.
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let json = serde_json::to_string_pretty(self).map_err(|_| std::fmt::Error)?;
        f.write_str(&json)
    }
}

/// The declarations of a file, keyed by the collection group that each owns.
#[derive(Default)]
struct Declarations(BTreeMap<FirestoreCollectionId, FirestoreIndexParams>);

impl Declarations {
    fn group(&mut self, id: &str) -> Result<&mut FirestoreIndexParams, IndexesFileProblem> {
        let id = FirestoreCollectionId::new(id)?;
        Ok(self
            .0
            .entry(id.clone())
            .or_insert_with(|| FirestoreIndexParams::new(id)))
    }
}

impl IndexesFile {
    /// One declaration per collection group the file names, in the order of the groups' IDs.
    pub fn into_params(self) -> Result<Vec<FirestoreIndexParams>, IndexesFileProblem> {
        let mut declarations = Declarations::default();
        for index in self.indexes {
            declarations
                .group(&index.collection_group)?
                .composite_indexes
                .push(index.try_into()?);
        }
        for field_override in self.field_overrides {
            declarations
                .group(&field_override.collection_group)?
                .field_overrides
                .push(field_override.try_into()?);
        }
        for ttl in self.fsq.unwrap_or_default().ttl {
            declarations
                .group(&ttl.collection_group)?
                .ttl_fields
                .push(ttl.field_path);
        }
        Ok(declarations.0.into_values().collect())
    }
}

impl IndexesFile {
    /// What the listing shows that the file cannot declare, one line each: the items the firestore
    /// crate could not represent, and the overrides and TTL states of fields that it could not.
    pub fn unexported(listing: &FirestoreIndexListing) -> Vec<String> {
        let mut lines: Vec<String> = listing
            .unrecognised
            .iter()
            .map(ToString::to_string)
            .collect();
        for field in &listing.fields {
            if let Some(FirestoreFieldOverrideOutcome::Unrecognised(reason)) = &field.index_override
            {
                lines.push(format!("{}: {reason}", field.name));
            }
            if let Some(FirestoreFieldTtlOutcome::Unrecognised(reason)) = &field.ttl {
                lines.push(format!("{}: {reason}", field.name));
            }
        }
        lines
    }
}

impl TryFrom<FileIndex> for FirestoreCompositeIndex {
    type Error = IndexesFileProblem;

    fn try_from(index: FileIndex) -> Result<Self, Self::Error> {
        let extras = index.fsq.unwrap_or_default();
        Ok(Self {
            fields: index
                .fields
                .into_iter()
                .map(FirestoreIndexField::try_from)
                .collect::<Result<_, _>>()?,
            query_scope: index.query_scope.into(),
            search_options: extras.search.map(Into::into),
            unique: extras.unique.unwrap_or(false),
            density: extras.density.map(Into::into),
        })
    }
}

impl TryFrom<FileIndexField> for FirestoreIndexField {
    type Error = IndexesFileProblem;

    fn try_from(field: FileIndexField) -> Result<Self, Self::Error> {
        let mode = field
            .mode
            .try_into()
            .map_err(|problem: NoSingleMode| problem.in_field(&field.field_path))?;
        Ok(Self {
            field_path: field.field_path,
            mode,
        })
    }
}

impl TryFrom<FileFieldOverride> for FirestoreFieldOverride {
    type Error = IndexesFileProblem;

    fn try_from(field_override: FileFieldOverride) -> Result<Self, Self::Error> {
        let indexes = field_override
            .indexes
            .into_iter()
            .map(|index| {
                Ok(FirestoreFieldOverrideIndex {
                    query_scope: index.query_scope.into(),
                    mode: index.mode.try_into().map_err(|problem: NoSingleMode| {
                        problem.in_field(&field_override.field_path)
                    })?,
                })
            })
            .collect::<Result<_, IndexesFileProblem>>()?;
        let target = if field_override.field_path == ALL_FIELDS_PATH {
            FirestoreFieldOverrideTarget::AllFields
        } else {
            FirestoreFieldOverrideTarget::Field(field_override.field_path)
        };
        Ok(Self { target, indexes })
    }
}

impl TryFrom<FileMode> for FirestoreIndexFieldMode {
    type Error = NoSingleMode;

    fn try_from(mode: FileMode) -> Result<Self, Self::Error> {
        match (mode.order, mode.array_config, mode.vector_config, mode.fsq) {
            (Some(order), None, None, None) => Ok(Self::Order(order.into())),
            (None, Some(FileArrayConfig::Contains), None, None) => Ok(Self::ArrayContains),
            (None, None, Some(vector), None) => Ok(Self::Vector(FirestoreVectorIndexConfig::new(
                vector.dimension,
            ))),
            (None, None, None, Some(FileFieldExtras::Text)) => Ok(Self::Text),
            (None, None, None, Some(FileFieldExtras::Geo(geo))) => Ok(Self::Geo(
                FirestoreGeoIndexConfig::new()
                    .with_geo_json_indexing_disabled(geo.geo_json_indexing_disabled),
            )),
            _ => Err(NoSingleMode),
        }
    }
}

/// A field sets no mode, or more than one.
#[derive(Debug)]
pub struct NoSingleMode;

impl NoSingleMode {
    fn in_field(self, field_path: &str) -> IndexesFileProblem {
        IndexesFileProblem::FieldMode(InvalidFieldMode {
            field_path: field_path.to_string(),
        })
    }
}

impl From<FileQueryScope> for FirestoreIndexQueryScope {
    fn from(scope: FileQueryScope) -> Self {
        match scope {
            FileQueryScope::Collection => Self::Collection,
            FileQueryScope::CollectionGroup => Self::AllDescendants,
        }
    }
}

impl From<FirestoreIndexQueryScope> for FileQueryScope {
    fn from(scope: FirestoreIndexQueryScope) -> Self {
        match scope {
            FirestoreIndexQueryScope::Collection => Self::Collection,
            FirestoreIndexQueryScope::AllDescendants => Self::CollectionGroup,
        }
    }
}

impl From<FileOrder> for FirestoreQueryDirection {
    fn from(order: FileOrder) -> Self {
        match order {
            FileOrder::Ascending => Self::Ascending,
            FileOrder::Descending => Self::Descending,
        }
    }
}

impl From<FileDensity> for FirestoreIndexDensity {
    fn from(density: FileDensity) -> Self {
        match density {
            FileDensity::SparseAll => Self::SparseAll,
            FileDensity::SparseAny => Self::SparseAny,
            FileDensity::Dense => Self::Dense,
        }
    }
}

impl From<FirestoreIndexDensity> for FileDensity {
    fn from(density: FirestoreIndexDensity) -> Self {
        match density {
            FirestoreIndexDensity::SparseAll => Self::SparseAll,
            FirestoreIndexDensity::SparseAny => Self::SparseAny,
            FirestoreIndexDensity::Dense => Self::Dense,
        }
    }
}

impl From<FileSearchOptions> for FirestoreSearchIndexOptions {
    fn from(options: FileSearchOptions) -> Self {
        Self::new()
            .opt_text_language(options.text_language)
            .opt_text_language_override_field_path(options.text_language_override_field_path)
    }
}

impl From<&FirestoreSearchIndexOptions> for FileSearchOptions {
    fn from(options: &FirestoreSearchIndexOptions) -> Self {
        Self {
            text_language: options.text_language.clone(),
            text_language_override_field_path: options.text_language_override_field_path.clone(),
        }
    }
}

impl From<&FirestoreIndexFieldMode> for FileMode {
    fn from(mode: &FirestoreIndexFieldMode) -> Self {
        let mut file_mode = Self::default();
        match mode {
            FirestoreIndexFieldMode::Order(FirestoreQueryDirection::Ascending) => {
                file_mode.order = Some(FileOrder::Ascending)
            }
            FirestoreIndexFieldMode::Order(FirestoreQueryDirection::Descending) => {
                file_mode.order = Some(FileOrder::Descending)
            }
            FirestoreIndexFieldMode::ArrayContains => {
                file_mode.array_config = Some(FileArrayConfig::Contains)
            }
            FirestoreIndexFieldMode::Vector(vector) => {
                file_mode.vector_config = Some(FileVectorConfig {
                    dimension: vector.dimension,
                    flat: FlatVectorIndex {},
                })
            }
            FirestoreIndexFieldMode::Text => file_mode.fsq = Some(FileFieldExtras::Text),
            FirestoreIndexFieldMode::Geo(geo) => {
                file_mode.fsq = Some(FileFieldExtras::Geo(FileGeoOptions {
                    geo_json_indexing_disabled: geo.geo_json_indexing_disabled,
                }))
            }
        }
        file_mode
    }
}

impl From<&FirestoreListedCompositeIndex> for FileIndex {
    fn from(listed: &FirestoreListedCompositeIndex) -> Self {
        let index = &listed.index;
        let has_extras = index.unique || index.density.is_some() || index.search_options.is_some();
        Self {
            collection_group: listed.collection_group.to_string(),
            query_scope: index.query_scope.into(),
            fields: index
                .fields
                .iter()
                .map(|field| FileIndexField {
                    field_path: field.field_path.clone(),
                    mode: (&field.mode).into(),
                })
                .collect(),
            fsq: has_extras.then(|| FileIndexExtras {
                unique: index.unique.then_some(true),
                density: index.density.map(Into::into),
                search: index.search_options.as_ref().map(Into::into),
            }),
        }
    }
}

impl From<&FirestoreIndexListing> for IndexesFile {
    /// The file that declares what the listing shows. A field whose override Firestore is
    /// already reverting is left out, since it is on its way to having none.
    fn from(listing: &FirestoreIndexListing) -> Self {
        let mut indexes: Vec<FileIndex> = listing.indexes.iter().map(Into::into).collect();
        indexes.sort_by(|left, right| left.collection_group.cmp(&right.collection_group));
        let mut field_overrides = Vec::new();
        let mut ttl = Vec::new();
        for field in &listing.fields {
            if let Some(FirestoreFieldOverrideOutcome::Explicit(explicit)) = &field.index_override {
                if !explicit.reverting {
                    field_overrides.push(FileFieldOverride {
                        collection_group: field.collection_group.to_string(),
                        field_path: field.field_path.clone(),
                        indexes: explicit
                            .indexes
                            .iter()
                            .map(|index| FileOverrideIndex {
                                query_scope: index.query_scope.into(),
                                mode: (&index.mode).into(),
                            })
                            .collect(),
                    });
                }
            }
            if let Some(FirestoreFieldTtlOutcome::Configured(_)) = &field.ttl {
                ttl.push(FileTtlField {
                    collection_group: field.collection_group.to_string(),
                    field_path: field.field_path.clone(),
                });
            }
        }
        field_overrides.sort_by(|left, right| left.collection_group.cmp(&right.collection_group));
        ttl.sort_by(|left, right| left.collection_group.cmp(&right.collection_group));
        Self {
            indexes,
            field_overrides,
            fsq: (!ttl.is_empty()).then_some(FileExtras { ttl }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::errors::IndexesFileProblem;
    use firestore::{
        FirestoreCompositeIndex, FirestoreIndexId, FirestoreIndexState, FirestoreListedField,
    };

    /// Written by hand from the schema in the Firebase documentation of `firestore.indexes.json`:
    /// composite indexes of both scopes, an array and a vector field, and field overrides with
    /// explicit indexes, an exemption and the all-fields wildcard.
    const FIREBASE_INDEXES: &str = r#"{
  "indexes": [
    {
      "collectionGroup": "cities",
      "queryScope": "COLLECTION",
      "fields": [
        { "fieldPath": "country", "order": "ASCENDING" },
        { "fieldPath": "population", "order": "DESCENDING" }
      ]
    },
    {
      "collectionGroup": "cities",
      "queryScope": "COLLECTION_GROUP",
      "fields": [
        { "fieldPath": "landmarks", "arrayConfig": "CONTAINS" },
        { "fieldPath": "population", "order": "ASCENDING" }
      ]
    },
    {
      "collectionGroup": "landmarks",
      "queryScope": "COLLECTION",
      "fields": [
        { "fieldPath": "category", "order": "ASCENDING" },
        { "fieldPath": "embedding", "vectorConfig": { "dimension": 768, "flat": {} } }
      ]
    }
  ],
  "fieldOverrides": [
    {
      "collectionGroup": "cities",
      "fieldPath": "notes",
      "indexes": []
    },
    {
      "collectionGroup": "cities",
      "fieldPath": "country",
      "indexes": [
        { "order": "ASCENDING", "queryScope": "COLLECTION" },
        { "arrayConfig": "CONTAINS", "queryScope": "COLLECTION_GROUP" }
      ]
    },
    {
      "collectionGroup": "landmarks",
      "fieldPath": "*",
      "indexes": []
    }
  ]
}"#;

    fn group(id: &str) -> FirestoreCollectionId {
        id.parse().expect("a collection ID")
    }

    fn field(path: &str, mode: FirestoreIndexFieldMode) -> FirestoreIndexField {
        FirestoreIndexField {
            field_path: path.to_string(),
            mode,
        }
    }

    fn ascending() -> FirestoreIndexFieldMode {
        FirestoreIndexFieldMode::Order(FirestoreQueryDirection::Ascending)
    }

    fn index(
        fields: Vec<FirestoreIndexField>,
        query_scope: FirestoreIndexQueryScope,
    ) -> FirestoreCompositeIndex {
        FirestoreCompositeIndex {
            fields,
            query_scope,
            search_options: None,
            unique: false,
            density: None,
        }
    }

    fn params_of(text: &str) -> Vec<FirestoreIndexParams> {
        text.parse::<IndexesFile>()
            .expect("an indexes file")
            .into_params()
            .expect("declarations")
    }

    #[test]
    fn a_firebase_file_declares_the_indexes_and_overrides_of_each_group() {
        let params = params_of(FIREBASE_INDEXES);

        assert_eq!(
            params,
            vec![
                FirestoreIndexParams::new(group("cities"))
                    .with_composite_indexes(vec![
                        index(
                            vec![
                                field("country", ascending()),
                                field(
                                    "population",
                                    FirestoreIndexFieldMode::Order(
                                        FirestoreQueryDirection::Descending
                                    )
                                ),
                            ],
                            FirestoreIndexQueryScope::Collection
                        ),
                        index(
                            vec![
                                field("landmarks", FirestoreIndexFieldMode::ArrayContains),
                                field("population", ascending()),
                            ],
                            FirestoreIndexQueryScope::AllDescendants
                        ),
                    ])
                    .with_field_overrides(vec![
                        FirestoreFieldOverride {
                            target: FirestoreFieldOverrideTarget::Field("notes".to_string()),
                            indexes: Vec::new(),
                        },
                        FirestoreFieldOverride {
                            target: FirestoreFieldOverrideTarget::Field("country".to_string()),
                            indexes: vec![
                                FirestoreFieldOverrideIndex {
                                    query_scope: FirestoreIndexQueryScope::Collection,
                                    mode: ascending(),
                                },
                                FirestoreFieldOverrideIndex {
                                    query_scope: FirestoreIndexQueryScope::AllDescendants,
                                    mode: FirestoreIndexFieldMode::ArrayContains,
                                },
                            ],
                        },
                    ]),
                FirestoreIndexParams::new(group("landmarks"))
                    .with_composite_indexes(vec![index(
                        vec![
                            field("category", ascending()),
                            field(
                                "embedding",
                                FirestoreIndexFieldMode::Vector(FirestoreVectorIndexConfig::new(
                                    768
                                ))
                            ),
                        ],
                        FirestoreIndexQueryScope::Collection
                    )])
                    .with_field_overrides(vec![FirestoreFieldOverride {
                        target: FirestoreFieldOverrideTarget::AllFields,
                        indexes: Vec::new(),
                    }]),
            ]
        );
    }

    #[test]
    fn enterprise_settings_and_ttl_fields_live_under_fsq_keys() {
        let params = params_of(
            r#"{
  "indexes": [
    {
      "collectionGroup": "articles",
      "queryScope": "COLLECTION",
      "fields": [
        { "fieldPath": "body", "fsq": { "mode": "TEXT" } },
        { "fieldPath": "place", "fsq": { "mode": "GEO", "geoJsonIndexingDisabled": true } }
      ],
      "fsq": {
        "unique": true,
        "density": "SPARSE_ANY",
        "search": { "textLanguage": "en", "textLanguageOverrideFieldPath": "lang" }
      }
    }
  ],
  "fieldOverrides": [],
  "fsq": { "ttl": [ { "collectionGroup": "sessions", "fieldPath": "expires_at" } ] }
}"#,
        );

        assert_eq!(
            params,
            vec![
                FirestoreIndexParams::new(group("articles")).with_composite_indexes(vec![
                    FirestoreCompositeIndex {
                        fields: vec![
                            field("body", FirestoreIndexFieldMode::Text),
                            field(
                                "place",
                                FirestoreIndexFieldMode::Geo(
                                    FirestoreGeoIndexConfig::new()
                                        .with_geo_json_indexing_disabled(true)
                                )
                            ),
                        ],
                        query_scope: FirestoreIndexQueryScope::Collection,
                        search_options: Some(
                            FirestoreSearchIndexOptions::new()
                                .with_text_language("en".to_string())
                                .with_text_language_override_field_path("lang".to_string())
                        ),
                        unique: true,
                        density: Some(FirestoreIndexDensity::SparseAny),
                    }
                ]),
                FirestoreIndexParams::new(group("sessions"))
                    .with_ttl_fields(vec!["expires_at".to_string()]),
            ]
        );
    }

    #[test]
    fn a_field_with_two_modes_is_refused() {
        let problem = r#"{"indexes": [{"collectionGroup": "cities", "queryScope": "COLLECTION",
            "fields": [{"fieldPath": "name", "order": "ASCENDING", "arrayConfig": "CONTAINS"}]}]}"#
            .parse::<IndexesFile>()
            .expect("valid JSON of the right shape")
            .into_params()
            .expect_err("two modes");

        assert!(
            matches!(problem, IndexesFileProblem::FieldMode(InvalidFieldMode { field_path }) if field_path == "name")
        );
    }

    #[test]
    fn a_key_the_file_format_does_not_define_is_refused_with_its_path() {
        let problem = r#"{"indexes": [{"collectionGroup": "cities", "queryScope": "COLLECTION",
            "density": "SPARSE_ALL", "fields": []}],
            "fieldOverrides": [{"collectionGroup": "cities", "fieldPath": "expires_at",
            "ttl": true, "indexes": []}]}"#
            .parse::<IndexesFile>()
            .expect_err("unread keys");

        assert!(
            matches!(problem, IndexesFileProblem::UnreadKeys(UnreadKeys { keys }) if keys == ["indexes[0].density", "fieldOverrides[0].ttl"])
        );
    }

    #[test]
    fn explicit_defaults_are_not_unread_keys() {
        let file: IndexesFile = r#"{"indexes": [{"collectionGroup": "cities",
            "queryScope": "COLLECTION", "fields": [{"fieldPath": "name", "order": "ASCENDING"}],
            "fsq": {"unique": false}}], "fieldOverrides": []}"#
            .parse()
            .expect("an indexes file");

        assert_eq!(file.indexes.len(), 1);
    }

    #[test]
    fn a_listing_exports_to_a_file_that_reads_back_as_the_same_declarations() {
        let listed_index =
            |id: &str, composite: FirestoreCompositeIndex| FirestoreListedCompositeIndex {
                name: format!("projects/p/databases/d/collectionGroups/orders/indexes/{id}"),
                collection_group: group("orders"),
                id: id.parse::<FirestoreIndexId>().expect("an index ID"),
                state: FirestoreIndexState::Ready,
                index: composite,
            };
        let search = FirestoreCompositeIndex {
            fields: vec![field("body", FirestoreIndexFieldMode::Text)],
            query_scope: FirestoreIndexQueryScope::Collection,
            search_options: Some(
                FirestoreSearchIndexOptions::new().with_text_language("en".to_string()),
            ),
            unique: false,
            density: Some(FirestoreIndexDensity::SparseAny),
        };
        let listing = FirestoreIndexListing {
            indexes: vec![
                listed_index(
                    "CICAgJim14AK",
                    index(
                        vec![
                            field("customer_id", ascending()),
                            field("tags", FirestoreIndexFieldMode::ArrayContains),
                        ],
                        FirestoreIndexQueryScope::AllDescendants,
                    ),
                ),
                listed_index("CICAgOjXh4EK", search),
            ],
            fields: vec![
                FirestoreListedField {
                    name: "projects/p/databases/d/collectionGroups/orders/fields/notes".to_string(),
                    collection_group: group("orders"),
                    field_path: "notes".to_string(),
                    index_override: Some(FirestoreFieldOverrideOutcome::Explicit(
                        firestore::FirestoreExplicitFieldOverride {
                            indexes: Vec::new(),
                            reverting: false,
                        },
                    )),
                    ttl: None,
                },
                FirestoreListedField {
                    name: "projects/p/databases/d/collectionGroups/orders/fields/placed_at"
                        .to_string(),
                    collection_group: group("orders"),
                    field_path: "placed_at".to_string(),
                    index_override: Some(FirestoreFieldOverrideOutcome::Inherited),
                    ttl: Some(FirestoreFieldTtlOutcome::Configured(
                        firestore::FirestoreFieldTtlState::Active,
                    )),
                },
            ],
            unrecognised: Vec::new(),
        };

        let exported = IndexesFile::from(&listing).to_string();

        assert_eq!(
            params_of(&exported),
            vec![FirestoreIndexParams::new(group("orders"))
                .with_composite_indexes(vec![
                    index(
                        vec![
                            field("customer_id", ascending()),
                            field("tags", FirestoreIndexFieldMode::ArrayContains),
                        ],
                        FirestoreIndexQueryScope::AllDescendants,
                    ),
                    FirestoreCompositeIndex {
                        fields: vec![field("body", FirestoreIndexFieldMode::Text)],
                        query_scope: FirestoreIndexQueryScope::Collection,
                        search_options: Some(
                            FirestoreSearchIndexOptions::new().with_text_language("en".to_string())
                        ),
                        unique: false,
                        density: Some(FirestoreIndexDensity::SparseAny),
                    },
                ])
                .with_field_overrides(vec![FirestoreFieldOverride {
                    target: FirestoreFieldOverrideTarget::Field("notes".to_string()),
                    indexes: Vec::new(),
                }])
                .with_ttl_fields(vec!["placed_at".to_string()])]
        );
    }

    #[test]
    fn what_the_crate_cannot_represent_is_listed_as_unexported() {
        let listing = FirestoreIndexListing {
            indexes: Vec::new(),
            fields: vec![FirestoreListedField {
                name: "projects/p/databases/d/collectionGroups/orders/fields/note".to_string(),
                collection_group: group("orders"),
                field_path: "note".to_string(),
                index_override: Some(FirestoreFieldOverrideOutcome::Unrecognised(
                    "an unknown order".to_string(),
                )),
                ttl: None,
            }],
            unrecognised: vec![firestore::FirestoreUnrecognisedIndexItem {
                name: "projects/p/databases/d/collectionGroups/orders/indexes/CICAgJim14AK"
                    .to_string(),
                reason: "a multikey index".to_string(),
            }],
        };

        assert_eq!(
            IndexesFile::unexported(&listing),
            [
                "projects/p/databases/d/collectionGroups/orders/indexes/CICAgJim14AK: a multikey index",
                "projects/p/databases/d/collectionGroups/orders/fields/note: an unknown order",
            ]
        );
    }
}
