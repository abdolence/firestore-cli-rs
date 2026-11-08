//! The `firestore.indexes.json` format of the Firebase CLI, read into the declarations the
//! firestore crate plans and syncs, and written from a listing of a live database.
//!
//! Every key the Firebase CLI reads and writes is the Firebase key. Only what the format cannot
//! carry goes under an `fsq` key, which the Firebase CLI ignores: the language options of a search
//! index, `unique` (the Firebase CLI refuses it), and a TTL field whose indexes are inherited, since
//! Firebase writes `ttl` on a field override only. Keys that neither Firebase nor `fsq` defines are
//! refused rather than ignored, so that no setting is dropped without a word.

use crate::errors::{IndexesFileProblem, InvalidFieldMode, UnsupportedSetting};
use clap::ValueEnum;
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
use std::str::FromStr;

/// The `fieldPath` that names every field of a collection group, as in the Firebase format.
const ALL_FIELDS_PATH: &str = "*";

#[derive(Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct IndexesFile {
    #[serde(default)]
    pub indexes: Vec<FileIndex>,
    #[serde(default)]
    pub field_overrides: Vec<FileFieldOverride>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fsq: Option<FileExtras>,
}

#[derive(Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FileExtras {
    /// The TTL fields that have no field override to carry `ttl`.
    #[serde(default)]
    pub ttl: Vec<FileTtlField>,
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FileTtlField {
    pub collection_group: FirestoreCollectionId,
    pub field_path: String,
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FileIndex {
    pub collection_group: FirestoreCollectionId,
    pub query_scope: IndexScope,
    pub fields: Vec<FileIndexField>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_scope: Option<FileApiScope>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub density: Option<FileDensity>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub multikey: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unique: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fsq: Option<FileIndexExtras>,
}

/// Whether an index serves queries against one collection or across every collection with the
/// group's ID.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, ValueEnum, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum IndexScope {
    /// Queries against one collection.
    #[default]
    Collection,
    /// Queries across every collection with the group's ID.
    #[value(name = "group")]
    CollectionGroup,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FileApiScope {
    #[serde(rename = "ANY_API")]
    Any,
    #[serde(rename = "DATASTORE_MODE_API")]
    DatastoreMode,
    #[serde(rename = "MONGODB_COMPATIBLE_API")]
    MongodbCompatible,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FileIndexField {
    pub field_path: String,
    #[serde(flatten)]
    pub mode: FileMode,
}

/// How a field participates in an index. Exactly one of the four keys is set.
#[derive(Debug, Clone, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileMode {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order: Option<FileOrder>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub array_config: Option<FileArrayConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub search_config: Option<FileSearchConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vector_config: Option<FileVectorConfig>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum FileOrder {
    Ascending,
    Descending,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum FileArrayConfig {
    Contains,
}

/// A text or a geo search field: exactly one of the two specs is set.
#[derive(Debug, Clone, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FileSearchConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text_spec: Option<FileTextSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub geo_spec: Option<FileGeoSpec>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FileTextSpec {
    pub index_specs: Vec<FileTextIndexSpec>,
}

/// The one kind of text index Firestore has.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FileTextIndexSpec {
    pub index_type: FileTextIndexType,
    pub match_type: FileTextMatchType,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum FileTextIndexType {
    Tokenized,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum FileTextMatchType {
    MatchGlobally,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FileGeoSpec {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub geo_json_indexing_disabled: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileVectorConfig {
    pub dimension: u32,
    #[serde(default)]
    pub flat: FlatVectorIndex,
}

/// The `flat` key of a vector config, the one kind of vector index Firestore has.
#[derive(Debug, Clone, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FlatVectorIndex {}

#[derive(Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FileIndexExtras {
    /// Firebase's own `unique` key is refused by the Firebase CLI, so fsq writes it here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unique: Option<bool>,
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
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FileSearchOptions {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text_language: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text_language_override_field_path: Option<String>,
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FileFieldOverride {
    pub collection_group: FirestoreCollectionId,
    pub field_path: String,
    /// Whether the field's timestamp expires the document. Firebase writes it on every override.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ttl: Option<bool>,
    pub indexes: Vec<FileOverrideIndex>,
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FileOverrideIndex {
    #[serde(flatten)]
    pub mode: FileMode,
    pub query_scope: IndexScope,
}

impl FromStr for IndexesFile {
    type Err = IndexesFileProblem;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        Ok(serde_json::from_str(text)?)
    }
}

impl TryFrom<IndexesFile> for Vec<FirestoreIndexParams> {
    type Error = IndexesFileProblem;

    /// One declaration per collection group the file names, in the order of the groups' IDs.
    fn try_from(file: IndexesFile) -> Result<Self, Self::Error> {
        fn group<'a>(
            declarations: &'a mut BTreeMap<FirestoreCollectionId, FirestoreIndexParams>,
            id: &FirestoreCollectionId,
        ) -> &'a mut FirestoreIndexParams {
            declarations
                .entry(id.clone())
                .or_insert_with(|| FirestoreIndexParams::new(id.clone()))
        }

        let mut declarations = BTreeMap::new();
        for index in file.indexes {
            group(&mut declarations, &index.collection_group)
                .composite_indexes
                .push(index.try_into()?);
        }
        for field_override in file.field_overrides {
            let params = group(&mut declarations, &field_override.collection_group);
            if field_override.ttl == Some(true) {
                params.ttl_fields.push(field_override.field_path.clone());
            }
            params.field_overrides.push(field_override.try_into()?);
        }
        for ttl in file.fsq.unwrap_or_default().ttl {
            group(&mut declarations, &ttl.collection_group)
                .ttl_fields
                .push(ttl.field_path);
        }
        Ok(declarations.into_values().collect())
    }
}

impl TryFrom<FileIndex> for FirestoreCompositeIndex {
    type Error = IndexesFileProblem;

    fn try_from(index: FileIndex) -> Result<Self, Self::Error> {
        let unsupported = |setting| {
            IndexesFileProblem::UnsupportedSetting(UnsupportedSetting {
                collection_group: index.collection_group.clone(),
                setting,
            })
        };
        if index
            .api_scope
            .is_some_and(|scope| scope != FileApiScope::Any)
        {
            return Err(unsupported("apiScope other than ANY_API"));
        }
        if index.multikey == Some(true) {
            return Err(unsupported("multikey"));
        }
        let extras = index.fsq.unwrap_or_default();
        Ok(Self {
            fields: index
                .fields
                .into_iter()
                .map(FirestoreIndexField::try_from)
                .collect::<Result<_, _>>()?,
            query_scope: index.query_scope.into(),
            search_options: extras.search.map(Into::into),
            unique: index.unique.or(extras.unique).unwrap_or(false),
            density: index.density.map(Into::into),
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
        match (
            mode.order,
            mode.array_config,
            mode.search_config,
            mode.vector_config,
        ) {
            (Some(order), None, None, None) => Ok(Self::Order(order.into())),
            (None, Some(FileArrayConfig::Contains), None, None) => Ok(Self::ArrayContains),
            (None, None, Some(search), None) => search.try_into(),
            (None, None, None, Some(vector)) => Ok(Self::Vector(FirestoreVectorIndexConfig::new(
                vector.dimension,
            ))),
            _ => Err(NoSingleMode),
        }
    }
}

impl TryFrom<FileSearchConfig> for FirestoreIndexFieldMode {
    type Error = NoSingleMode;

    fn try_from(config: FileSearchConfig) -> Result<Self, Self::Error> {
        match (config.text_spec, config.geo_spec) {
            (Some(_), None) => Ok(Self::Text),
            (None, Some(geo)) => Ok(Self::Geo(
                FirestoreGeoIndexConfig::new()
                    .with_geo_json_indexing_disabled(geo.geo_json_indexing_disabled == Some(true)),
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

impl From<IndexScope> for FirestoreIndexQueryScope {
    fn from(scope: IndexScope) -> Self {
        match scope {
            IndexScope::Collection => Self::Collection,
            IndexScope::CollectionGroup => Self::AllDescendants,
        }
    }
}

impl From<FirestoreIndexQueryScope> for IndexScope {
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
            FirestoreIndexFieldMode::Text => {
                file_mode.search_config = Some(FileSearchConfig {
                    text_spec: Some(FileTextSpec {
                        index_specs: vec![FileTextIndexSpec {
                            index_type: FileTextIndexType::Tokenized,
                            match_type: FileTextMatchType::MatchGlobally,
                        }],
                    }),
                    geo_spec: None,
                })
            }
            FirestoreIndexFieldMode::Geo(geo) => {
                file_mode.search_config = Some(FileSearchConfig {
                    text_spec: None,
                    geo_spec: Some(FileGeoSpec {
                        geo_json_indexing_disabled: geo.geo_json_indexing_disabled.then_some(true),
                    }),
                })
            }
        }
        file_mode
    }
}

impl FileIndex {
    /// The position the Firebase CLI gives the index in its file: by group, the collection-group
    /// scope before the collection scope, then by fields.
    fn order_key(&self) -> (&FirestoreCollectionId, bool, &[FileIndexField]) {
        (
            &self.collection_group,
            self.query_scope == IndexScope::Collection,
            &self.fields,
        )
    }
}

impl FileFieldOverride {
    /// The position the Firebase CLI gives the override in its file: by group, the TTL fields
    /// last, then by field path.
    fn order_key(&self) -> (&FirestoreCollectionId, bool, &str) {
        (
            &self.collection_group,
            self.ttl == Some(true),
            &self.field_path,
        )
    }
}

impl From<&FirestoreListedCompositeIndex> for FileIndex {
    fn from(listed: &FirestoreListedCompositeIndex) -> Self {
        let index = &listed.index;
        let has_extras = index.unique || index.search_options.is_some();
        Self {
            collection_group: listed.collection_group.clone(),
            query_scope: index.query_scope.into(),
            fields: index
                .fields
                .iter()
                .map(|field| FileIndexField {
                    field_path: field.field_path.clone(),
                    mode: (&field.mode).into(),
                })
                .collect(),
            api_scope: None,
            density: index.density.map(Into::into),
            multikey: None,
            unique: None,
            fsq: has_extras.then(|| FileIndexExtras {
                unique: index.unique.then_some(true),
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
        indexes.sort_by(|left, right| left.order_key().cmp(&right.order_key()));
        let mut field_overrides = Vec::new();
        let mut inherited_ttl = Vec::new();
        for field in &listing.fields {
            let has_ttl = matches!(field.ttl, Some(FirestoreFieldTtlOutcome::Configured(_)));
            match &field.index_override {
                Some(FirestoreFieldOverrideOutcome::Explicit(explicit)) if !explicit.reverting => {
                    field_overrides.push(FileFieldOverride {
                        collection_group: field.collection_group.clone(),
                        field_path: field.field_path.clone(),
                        ttl: Some(has_ttl),
                        indexes: explicit
                            .indexes
                            .iter()
                            .map(|index| FileOverrideIndex {
                                mode: (&index.mode).into(),
                                query_scope: index.query_scope.into(),
                            })
                            .collect(),
                    });
                }
                _ if has_ttl => inherited_ttl.push(FileTtlField {
                    collection_group: field.collection_group.clone(),
                    field_path: field.field_path.clone(),
                }),
                _ => {}
            }
        }
        field_overrides.sort_by(|left, right| left.order_key().cmp(&right.order_key()));
        inherited_ttl.sort_by(|left, right| {
            (&left.collection_group, &left.field_path)
                .cmp(&(&right.collection_group, &right.field_path))
        });
        Self {
            indexes,
            field_overrides,
            fsq: (!inherited_ttl.is_empty()).then_some(FileExtras { ttl: inherited_ttl }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::errors::{IndexesFileProblem, UnsupportedSetting};
    use firestore::{
        FirestoreCompositeIndex, FirestoreExplicitFieldOverride, FirestoreFieldTtlState,
        FirestoreIndexId, FirestoreIndexState, FirestoreListedField,
    };

    /// The output of `firebase firestore:indexes` (firebase-tools 15.35.0) for a Standard edition
    /// database: `density` on every index, a vector index, and field overrides with `ttl`.
    const FIREBASE_STANDARD: &str = include_str!("../tests/fixtures/firebase/standard.json");

    /// The output of `firebase firestore:indexes` (firebase-tools 15.35.0) for an Enterprise
    /// edition database: text and geo `searchConfig` fields and `SPARSE_ANY` and `DENSE` density.
    const FIREBASE_ENTERPRISE: &str = include_str!("../tests/fixtures/firebase/enterprise.json");

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
        density: Option<FirestoreIndexDensity>,
    ) -> FirestoreCompositeIndex {
        FirestoreCompositeIndex {
            fields,
            query_scope,
            search_options: None,
            unique: false,
            density,
        }
    }

    fn try_params_of(text: &str) -> Result<Vec<FirestoreIndexParams>, IndexesFileProblem> {
        let file = text.parse::<IndexesFile>().expect("an indexes file");
        Vec::try_from(file)
    }

    fn params_of(text: &str) -> Vec<FirestoreIndexParams> {
        try_params_of(text).expect("declarations")
    }

    fn params_of_group(text: &str, id: &str) -> FirestoreIndexParams {
        params_of(text)
            .into_iter()
            .find(|params| params.collection_group == group(id))
            .expect("the group is declared")
    }

    fn written(file: &IndexesFile) -> serde_json::Value {
        serde_json::to_value(file).expect("a JSON value")
    }

    #[test]
    fn what_the_firebase_cli_writes_is_written_back_unchanged() {
        for text in [FIREBASE_STANDARD, FIREBASE_ENTERPRISE] {
            let file: IndexesFile = text.parse().expect("an indexes file");

            assert_eq!(
                written(&file),
                serde_json::from_str::<serde_json::Value>(text).expect("JSON")
            );
        }
    }

    #[test]
    fn a_standard_file_declares_its_indexes_and_overrides() {
        let versions = params_of_group(FIREBASE_STANDARD, "versions");
        let vectors = params_of_group(FIREBASE_STANDARD, "test-query-vec");
        let childs = params_of_group(FIREBASE_STANDARD, "test-childs");

        assert_eq!(
            versions.composite_indexes[1],
            index(
                vec![
                    field("status", ascending()),
                    field(
                        "createdAt",
                        FirestoreIndexFieldMode::Order(FirestoreQueryDirection::Descending)
                    ),
                    field(
                        "__name__",
                        FirestoreIndexFieldMode::Order(FirestoreQueryDirection::Descending)
                    ),
                ],
                FirestoreIndexQueryScope::Collection,
                Some(FirestoreIndexDensity::SparseAll),
            )
        );
        assert_eq!(
            versions.composite_indexes[0].query_scope,
            FirestoreIndexQueryScope::AllDescendants
        );
        assert_eq!(
            vectors.composite_indexes[0].fields[1],
            field(
                "some_vec",
                FirestoreIndexFieldMode::Vector(FirestoreVectorIndexConfig::new(3))
            )
        );
        assert_eq!(
            childs.field_overrides,
            vec![FirestoreFieldOverride {
                target: FirestoreFieldOverrideTarget::Field("another_string".to_string()),
                indexes: vec![
                    FirestoreFieldOverrideIndex {
                        query_scope: FirestoreIndexQueryScope::Collection,
                        mode: ascending(),
                    },
                    FirestoreFieldOverrideIndex {
                        query_scope: FirestoreIndexQueryScope::Collection,
                        mode: FirestoreIndexFieldMode::Order(FirestoreQueryDirection::Descending),
                    },
                    FirestoreFieldOverrideIndex {
                        query_scope: FirestoreIndexQueryScope::Collection,
                        mode: FirestoreIndexFieldMode::ArrayContains,
                    },
                    FirestoreFieldOverrideIndex {
                        query_scope: FirestoreIndexQueryScope::AllDescendants,
                        mode: ascending(),
                    },
                ],
            }]
        );
        assert_eq!(childs.ttl_fields, Vec::<String>::new());
    }

    #[test]
    fn an_enterprise_file_declares_its_search_fields_and_densities() {
        let restaurants = params_of_group(FIREBASE_ENTERPRISE, "pipeline-restaurants");
        let books = params_of_group(FIREBASE_ENTERPRISE, "pipeline-books");

        assert_eq!(
            restaurants.composite_indexes,
            vec![
                index(
                    vec![field(
                        "location",
                        FirestoreIndexFieldMode::Geo(FirestoreGeoIndexConfig::new())
                    )],
                    FirestoreIndexQueryScope::Collection,
                    Some(FirestoreIndexDensity::SparseAny),
                ),
                index(
                    vec![
                        field("menu", FirestoreIndexFieldMode::Text),
                        field("description", FirestoreIndexFieldMode::Text),
                    ],
                    FirestoreIndexQueryScope::Collection,
                    Some(FirestoreIndexDensity::SparseAny),
                ),
            ]
        );
        assert_eq!(
            books.composite_indexes[1].density,
            Some(FirestoreIndexDensity::Dense)
        );
    }

    #[test]
    fn a_field_override_with_ttl_declares_the_ttl_field() {
        let params = params_of(
            r#"{"indexes": [], "fieldOverrides": [
                {"collectionGroup": "sessions", "fieldPath": "expires_at", "ttl": true,
                 "indexes": [{"order": "ASCENDING", "queryScope": "COLLECTION"}]},
                {"collectionGroup": "sessions", "fieldPath": "seen_at", "ttl": false,
                 "indexes": []}]}"#,
        );

        assert_eq!(params[0].ttl_fields, ["expires_at"]);
        assert_eq!(params[0].field_overrides.len(), 2);
    }

    #[test]
    fn what_firebase_cannot_express_lives_under_fsq_keys() {
        let params = params_of(
            r#"{
  "indexes": [
    {
      "collectionGroup": "articles",
      "queryScope": "COLLECTION",
      "fields": [
        { "fieldPath": "body", "searchConfig": { "geoSpec": { "geoJsonIndexingDisabled": true } } }
      ],
      "density": "SPARSE_ANY",
      "fsq": {
        "unique": true,
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
                        fields: vec![field(
                            "body",
                            FirestoreIndexFieldMode::Geo(
                                FirestoreGeoIndexConfig::new()
                                    .with_geo_json_indexing_disabled(true)
                            )
                        )],
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
    fn unique_is_read_from_the_firebase_key_too() {
        let params = params_of(
            r#"{"indexes": [{"collectionGroup": "cities", "queryScope": "COLLECTION",
            "fields": [{"fieldPath": "name", "order": "ASCENDING"}], "unique": true}]}"#,
        );

        assert!(params[0].composite_indexes[0].unique);
    }

    #[test]
    fn settings_the_crate_cannot_declare_are_refused() {
        for (setting, text) in [
            ("apiScope", r#""apiScope": "DATASTORE_MODE_API""#),
            ("multikey", r#""multikey": true"#),
        ] {
            let problem = try_params_of(&format!(
                r#"{{"indexes": [{{"collectionGroup": "cities", "queryScope": "COLLECTION",
                "fields": [{{"fieldPath": "name", "order": "ASCENDING"}}], {text}}}]}}"#
            ))
            .expect_err("an unsupported setting");

            assert!(
                matches!(
                    problem,
                    IndexesFileProblem::UnsupportedSetting(UnsupportedSetting { collection_group, setting: refused })
                        if collection_group == group("cities") && refused.starts_with(setting)
                ),
                "{setting}"
            );
        }
    }

    #[test]
    fn the_default_api_scope_and_multikey_are_read() {
        let params = params_of(
            r#"{"indexes": [{"collectionGroup": "cities", "queryScope": "COLLECTION",
            "fields": [{"fieldPath": "name", "order": "ASCENDING"}],
            "apiScope": "ANY_API", "multikey": false}]}"#,
        );

        assert_eq!(params[0].composite_indexes.len(), 1);
    }

    #[test]
    fn a_field_with_two_modes_is_refused() {
        let problem = try_params_of(
            r#"{"indexes": [{"collectionGroup": "cities", "queryScope": "COLLECTION",
            "fields": [{"fieldPath": "name", "order": "ASCENDING", "arrayConfig": "CONTAINS"}]}]}"#,
        )
        .expect_err("two modes");

        assert!(
            matches!(problem, IndexesFileProblem::FieldMode(InvalidFieldMode { field_path }) if field_path == "name")
        );
    }

    #[test]
    fn a_key_the_file_format_does_not_define_is_refused() {
        let problem = r#"{"indexes": [{"collectionGroup": "cities", "queryScope": "COLLECTION",
            "sharded": true, "fields": []}]}"#
            .parse::<IndexesFile>()
            .expect_err("an unknown key");

        assert!(matches!(problem, IndexesFileProblem::Json(error) if error.is_data()));
    }

    #[test]
    fn a_misspelt_mode_of_a_field_is_refused() {
        let problem = r#"{"indexes": [{"collectionGroup": "cities", "queryScope": "COLLECTION",
            "fields": [{"fieldPath": "name", "ordr": "ASCENDING"}]}]}"#
            .parse::<IndexesFile>()
            .expect_err("an unknown key beside a flattened mode");

        assert!(matches!(problem, IndexesFileProblem::Json(error) if error.is_data()));
    }

    #[test]
    fn explicit_defaults_and_nulls_are_read() {
        let file: IndexesFile = r#"{"indexes": [{"collectionGroup": "cities",
            "queryScope": "COLLECTION", "fields": [{"fieldPath": "name", "order": "ASCENDING"}],
            "density": null, "fsq": {"unique": false}}], "fieldOverrides": []}"#
            .parse()
            .expect("an indexes file");

        assert_eq!(file.indexes.len(), 1);
    }

    fn listed_index(id: &str, composite: FirestoreCompositeIndex) -> FirestoreListedCompositeIndex {
        FirestoreListedCompositeIndex {
            name: format!("projects/p/databases/d/collectionGroups/orders/indexes/{id}"),
            collection_group: group("orders"),
            id: id.parse::<FirestoreIndexId>().expect("an index ID"),
            state: FirestoreIndexState::Ready,
            index: composite,
        }
    }

    fn listed_field(
        path: &str,
        index_override: Option<FirestoreFieldOverrideOutcome>,
        ttl: bool,
    ) -> FirestoreListedField {
        FirestoreListedField {
            name: format!("projects/p/databases/d/collectionGroups/orders/fields/{path}"),
            collection_group: group("orders"),
            field_path: path.to_string(),
            index_override,
            ttl: ttl.then_some(FirestoreFieldTtlOutcome::Configured(
                FirestoreFieldTtlState::Active,
            )),
        }
    }

    fn exempt() -> Option<FirestoreFieldOverrideOutcome> {
        Some(FirestoreFieldOverrideOutcome::Explicit(
            FirestoreExplicitFieldOverride {
                indexes: Vec::new(),
                reverting: false,
            },
        ))
    }

    fn orders_listing() -> FirestoreIndexListing {
        let search = FirestoreCompositeIndex {
            fields: vec![
                field("body", FirestoreIndexFieldMode::Text),
                field(
                    "place",
                    FirestoreIndexFieldMode::Geo(
                        FirestoreGeoIndexConfig::new().with_geo_json_indexing_disabled(true),
                    ),
                ),
            ],
            query_scope: FirestoreIndexQueryScope::Collection,
            search_options: Some(
                FirestoreSearchIndexOptions::new().with_text_language("en".to_string()),
            ),
            unique: true,
            density: Some(FirestoreIndexDensity::SparseAny),
        };
        FirestoreIndexListing {
            indexes: vec![
                listed_index(
                    "CICAgOjXh4EK",
                    index(
                        vec![field("customer_id", ascending())],
                        FirestoreIndexQueryScope::Collection,
                        Some(FirestoreIndexDensity::SparseAll),
                    ),
                ),
                listed_index("CICAgJim14AK", search),
                listed_index(
                    "CICAgNiv44AK",
                    index(
                        vec![
                            field("customer_id", ascending()),
                            field("tags", FirestoreIndexFieldMode::ArrayContains),
                        ],
                        FirestoreIndexQueryScope::AllDescendants,
                        Some(FirestoreIndexDensity::SparseAll),
                    ),
                ),
            ],
            fields: vec![
                listed_field("expires_at", exempt(), true),
                listed_field("notes", exempt(), false),
                listed_field(
                    "placed_at",
                    Some(FirestoreFieldOverrideOutcome::Inherited),
                    true,
                ),
            ],
            unrecognised: Vec::new(),
        }
    }

    #[test]
    fn a_listing_exports_as_firebase_writes_it() {
        let exported = written(&IndexesFile::from(&orders_listing()));

        assert_eq!(
            exported,
            serde_json::json!({
                "indexes": [
                    {
                        "collectionGroup": "orders",
                        "queryScope": "COLLECTION_GROUP",
                        "fields": [
                            { "fieldPath": "customer_id", "order": "ASCENDING" },
                            { "fieldPath": "tags", "arrayConfig": "CONTAINS" }
                        ],
                        "density": "SPARSE_ALL"
                    },
                    {
                        "collectionGroup": "orders",
                        "queryScope": "COLLECTION",
                        "fields": [
                            { "fieldPath": "body", "searchConfig": { "textSpec": { "indexSpecs": [
                                { "indexType": "TOKENIZED", "matchType": "MATCH_GLOBALLY" }
                            ] } } },
                            { "fieldPath": "place", "searchConfig": { "geoSpec": {
                                "geoJsonIndexingDisabled": true
                            } } }
                        ],
                        "density": "SPARSE_ANY",
                        "fsq": { "unique": true, "search": { "textLanguage": "en" } }
                    },
                    {
                        "collectionGroup": "orders",
                        "queryScope": "COLLECTION",
                        "fields": [{ "fieldPath": "customer_id", "order": "ASCENDING" }],
                        "density": "SPARSE_ALL"
                    }
                ],
                "fieldOverrides": [
                    { "collectionGroup": "orders", "fieldPath": "notes", "ttl": false, "indexes": [] },
                    { "collectionGroup": "orders", "fieldPath": "expires_at", "ttl": true, "indexes": [] }
                ],
                "fsq": { "ttl": [{ "collectionGroup": "orders", "fieldPath": "placed_at" }] }
            })
        );
    }

    #[test]
    fn an_exported_file_reads_back_as_the_declarations_it_came_from() {
        let listing = orders_listing();
        let exported =
            serde_json::to_string_pretty(&IndexesFile::from(&listing)).expect("a file serialises");

        let params = params_of(&exported);

        assert_eq!(params.len(), 1);
        assert_eq!(
            params[0].composite_indexes,
            vec![
                listing.indexes[2].index.clone(),
                listing.indexes[1].index.clone(),
                listing.indexes[0].index.clone(),
            ]
        );
        assert_eq!(params[0].ttl_fields, ["expires_at", "placed_at"]);
    }

    #[test]
    fn inherited_ttl_fields_of_a_group_export_in_field_path_order() {
        let listing = FirestoreIndexListing {
            indexes: Vec::new(),
            fields: vec![
                listed_field(
                    "placed_at",
                    Some(FirestoreFieldOverrideOutcome::Inherited),
                    true,
                ),
                listed_field(
                    "expires_at",
                    Some(FirestoreFieldOverrideOutcome::Inherited),
                    true,
                ),
            ],
            unrecognised: Vec::new(),
        };

        let exported = written(&IndexesFile::from(&listing));

        assert_eq!(
            exported["fsq"]["ttl"],
            serde_json::json!([
                { "collectionGroup": "orders", "fieldPath": "expires_at" },
                { "collectionGroup": "orders", "fieldPath": "placed_at" }
            ])
        );
    }
}
