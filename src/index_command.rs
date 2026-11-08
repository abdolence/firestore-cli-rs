use crate::errors::{FsqError, InvalidIndexField};
use crate::index_file::IndexScope;
use clap::{Args, Subcommand};
use firestore::{
    FirestoreCollectionId, FirestoreCompositeIndex, FirestoreIndexDefinition,
    FirestoreIndexDeleteTarget, FirestoreIndexField, FirestoreIndexFieldMode, FirestoreIndexId,
    FirestoreIndexIdTarget, FirestoreIndexQueryScope, FirestoreQueryDirection,
};
use std::path::PathBuf;
use std::str::FromStr;

#[derive(Subcommand, Debug)]
pub enum IndexCommand {
    #[command(
        about = "Show what a firestore.indexes.json file would change, without writing anything"
    )]
    Plan(IndexFileArgs),
    #[command(
        about = "Make the database match a firestore.indexes.json file, waiting for the builds"
    )]
    Sync(IndexFileArgs),
    #[command(about = "List the live indexes and field overrides with their states")]
    List(IndexListArgs),
    #[command(about = "Write the live indexes and field overrides as a firestore.indexes.json")]
    Export(IndexExportArgs),
    #[command(about = "Create one composite index")]
    Create(IndexCreateArgs),
    #[command(about = "Delete one composite index, by its ID or by its group and fields")]
    Delete(IndexDeleteArgs),
}

#[derive(Args, Debug)]
pub struct IndexFileArgs {
    #[arg(help = "A firestore.indexes.json file")]
    pub file: PathBuf,

    #[arg(
        long,
        help = "Also delete or revert what the live database has and the file does not declare, \
                in every collection group"
    )]
    pub prune: bool,
}

#[derive(Args, Debug)]
pub struct IndexListArgs {
    #[arg(
        long,
        value_name = "COLLECTION_GROUP",
        help = "List this collection group only"
    )]
    pub group: Option<FirestoreCollectionId>,
}

#[derive(Args, Debug)]
pub struct IndexExportArgs {
    #[arg(
        short,
        long,
        value_name = "FILE",
        help = "Write the file here instead of to stdout"
    )]
    pub output: Option<PathBuf>,
}

#[derive(Args, Debug)]
pub struct IndexCreateArgs {
    #[arg(value_name = "COLLECTION_GROUP")]
    pub group: FirestoreCollectionId,

    #[arg(
        required = true,
        value_name = "FIELD:MODE",
        help = "The indexed fields in order, each <field>:asc, <field>:desc or <field>:contains"
    )]
    pub fields: Vec<IndexFieldSpec>,

    #[arg(long, value_enum, default_value_t)]
    pub scope: IndexScope,

    #[arg(long, help = "Succeed when the index already exists")]
    pub if_not_exists: bool,
}

#[derive(Args, Debug)]
pub struct IndexDeleteArgs {
    #[arg(
        value_name = "ID | COLLECTION_GROUP",
        help = "The ID of the index, or, with fields, its collection group"
    )]
    pub id_or_group: String,

    #[arg(
        value_name = "FIELD:MODE",
        help = "The indexed fields in order, as in `create`, when the index is named by them"
    )]
    pub fields: Vec<IndexFieldSpec>,

    #[arg(
        long,
        value_enum,
        default_value_t,
        help = "The query scope of the index, when named by its fields"
    )]
    pub scope: IndexScope,

    #[arg(long, help = "Succeed when there is no such index")]
    pub if_exists: bool,
}

/// An indexed field as the command line writes it: `<field>:asc`, `<field>:desc` or
/// `<field>:contains`.
#[derive(Debug, Clone, PartialEq)]
pub struct IndexFieldSpec(FirestoreIndexField);

impl FromStr for IndexFieldSpec {
    type Err = FsqError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let invalid = || {
            FsqError::InvalidIndexField(InvalidIndexField {
                value: text.to_string(),
            })
        };
        let (field_path, mode) = text.rsplit_once(':').ok_or_else(invalid)?;
        let mode = match mode.to_ascii_lowercase().as_str() {
            "asc" => FirestoreIndexFieldMode::Order(FirestoreQueryDirection::Ascending),
            "desc" => FirestoreIndexFieldMode::Order(FirestoreQueryDirection::Descending),
            "contains" => FirestoreIndexFieldMode::ArrayContains,
            _ => return Err(invalid()),
        };
        if field_path.is_empty() {
            return Err(invalid());
        }
        Ok(Self(FirestoreIndexField {
            field_path: field_path.to_string(),
            mode,
        }))
    }
}

impl From<IndexFieldSpec> for FirestoreIndexField {
    fn from(spec: IndexFieldSpec) -> Self {
        spec.0
    }
}

impl IndexCreateArgs {
    pub fn into_definition(self) -> FirestoreIndexDefinition {
        FirestoreIndexDefinition::new(
            self.group,
            composite_index(
                self.fields.into_iter().map(Into::into).collect(),
                self.scope.into(),
            ),
        )
    }
}

impl TryFrom<IndexDeleteArgs> for FirestoreIndexDeleteTarget {
    type Error = FsqError;

    fn try_from(args: IndexDeleteArgs) -> Result<Self, Self::Error> {
        if args.fields.is_empty() {
            return Ok(Self::Id(FirestoreIndexIdTarget::new(
                args.id_or_group.parse::<FirestoreIndexId>()?,
            )));
        }
        Ok(Self::Definition(FirestoreIndexDefinition::new(
            args.id_or_group.parse()?,
            composite_index(
                args.fields.into_iter().map(Into::into).collect(),
                args.scope.into(),
            ),
        )))
    }
}

fn composite_index(
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

#[cfg(test)]
mod tests {
    use super::*;

    fn field(path: &str, mode: FirestoreIndexFieldMode) -> FirestoreIndexField {
        FirestoreIndexField {
            field_path: path.to_string(),
            mode,
        }
    }

    fn delete_args(id_or_group: &str, fields: &[&str]) -> IndexDeleteArgs {
        IndexDeleteArgs {
            id_or_group: id_or_group.to_string(),
            fields: fields
                .iter()
                .map(|text| text.parse().expect("a field spec"))
                .collect(),
            scope: IndexScope::Collection,
            if_exists: false,
        }
    }

    #[test]
    fn a_field_spec_names_a_field_and_one_of_three_modes() {
        let parsed = |text: &str| {
            text.parse::<IndexFieldSpec>()
                .map(FirestoreIndexField::from)
        };

        assert_eq!(
            parsed("genre:asc").expect("ascending"),
            field(
                "genre",
                FirestoreIndexFieldMode::Order(FirestoreQueryDirection::Ascending)
            )
        );
        assert_eq!(
            parsed("rating:DESC").expect("descending"),
            field(
                "rating",
                FirestoreIndexFieldMode::Order(FirestoreQueryDirection::Descending)
            )
        );
        assert_eq!(
            parsed("address.tags:contains").expect("array contains"),
            field("address.tags", FirestoreIndexFieldMode::ArrayContains)
        );
    }

    #[test]
    fn a_field_spec_without_a_known_mode_is_refused() {
        for text in ["genre", "genre:", ":asc", "genre:up"] {
            assert!(
                matches!(
                    text.parse::<IndexFieldSpec>(),
                    Err(FsqError::InvalidIndexField(InvalidIndexField { value })) if value == text
                ),
                "{text} should be refused"
            );
        }
    }

    #[test]
    fn a_delete_with_one_word_names_an_index_by_its_id() {
        let target = FirestoreIndexDeleteTarget::try_from(delete_args("CICAgJim14AK", &[]))
            .expect("an index ID");

        assert_eq!(
            target,
            FirestoreIndexDeleteTarget::Id(FirestoreIndexIdTarget::new(
                "CICAgJim14AK"
                    .parse::<FirestoreIndexId>()
                    .expect("an index ID")
            ))
        );
    }

    #[test]
    fn a_delete_with_a_group_and_fields_names_an_index_by_its_definition() {
        let mut args = delete_args("books", &["genre:asc", "rating:desc"]);
        args.scope = IndexScope::CollectionGroup;

        let target = FirestoreIndexDeleteTarget::try_from(args).expect("a definition");

        assert_eq!(
            target,
            FirestoreIndexDeleteTarget::Definition(FirestoreIndexDefinition::new(
                "books".parse().expect("a collection ID"),
                composite_index(
                    vec![
                        field(
                            "genre",
                            FirestoreIndexFieldMode::Order(FirestoreQueryDirection::Ascending)
                        ),
                        field(
                            "rating",
                            FirestoreIndexFieldMode::Order(FirestoreQueryDirection::Descending)
                        ),
                    ],
                    FirestoreIndexQueryScope::AllDescendants
                )
            ))
        );
    }
}
