use super::{Cell, Grid};
use firestore::{
    FirestoreCollectionId, FirestoreCompositeIndex, FirestoreFieldOverrideOutcome,
    FirestoreFieldTtlOutcome, FirestoreListedCompositeIndex, FirestoreListedField,
    FirestoreUnrecognisedIndexItem,
};

impl Grid {
    /// The collections `SHOW COLLECTIONS` lists. Under `parent`, the path of a document, each
    /// comes with its path from the documents root; at the root the path is the ID.
    pub fn collections(collection_ids: &[FirestoreCollectionId], parent: Option<&str>) -> Self {
        let mut grid = match parent {
            Some(_) => Grid::new(["collection", "path"]),
            None => Grid::new(["collection"]),
        };
        for collection_id in collection_ids {
            let mut row = vec![Some(Cell::Text(collection_id.to_string()))];
            if let Some(parent) = parent {
                row.push(Some(Cell::Text(format!("{parent}/{collection_id}"))));
            }
            grid.push_row(row);
        }
        grid
    }

    pub fn indexes<'a, I>(indexes: I) -> Self
    where
        I: IntoIterator<Item = &'a FirestoreListedCompositeIndex>,
    {
        let mut grid = Grid::new([
            "collection_group",
            "id",
            "state",
            "scope",
            "fields",
            "options",
        ]);
        for listed in indexes {
            grid.push_row(vec![
                Some(Cell::Text(listed.collection_group.to_string())),
                Some(Cell::Text(listed.id.to_string())),
                Some(Cell::Text(listed.state.to_string())),
                Some(Cell::Text(listed.index.query_scope.to_string())),
                Some(Cell::Text(
                    listed
                        .index
                        .fields
                        .iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()
                        .join(", "),
                )),
                index_options(&listed.index).map(Cell::Text),
            ]);
        }
        grid
    }

    pub fn fields<'a, I>(fields: I) -> Self
    where
        I: IntoIterator<Item = &'a FirestoreListedField>,
    {
        let mut grid = Grid::new(["collection_group", "field", "indexes", "ttl"]);
        for field in fields {
            let indexes = field.index_override.as_ref().map(|outcome| match outcome {
                FirestoreFieldOverrideOutcome::Inherited => "inherited".to_string(),
                FirestoreFieldOverrideOutcome::Explicit(explicit) => {
                    let mut text = match explicit.indexes.as_slice() {
                        [] => "exempt".to_string(),
                        indexes => indexes
                            .iter()
                            .map(ToString::to_string)
                            .collect::<Vec<_>>()
                            .join(", "),
                    };
                    if explicit.reverting {
                        text.push_str(" (reverting)");
                    }
                    text
                }
                FirestoreFieldOverrideOutcome::Unrecognised(reason) => {
                    format!("unrecognised: {reason}")
                }
            });
            let ttl = field.ttl.as_ref().map(|outcome| match outcome {
                FirestoreFieldTtlOutcome::Configured(state) => state.to_string(),
                FirestoreFieldTtlOutcome::Unrecognised(reason) => {
                    format!("unrecognised: {reason}")
                }
            });
            grid.push_row(vec![
                Some(Cell::Text(field.collection_group.to_string())),
                Some(Cell::Text(field.field_path.clone())),
                indexes.map(Cell::Text),
                ttl.map(Cell::Text),
            ]);
        }
        grid
    }

    /// Listed indexes and fields the firestore crate cannot represent, kept so that none is
    /// hidden.
    pub fn unrecognised(items: &[FirestoreUnrecognisedIndexItem]) -> Self {
        let mut grid = Grid::new(["name", "reason"]);
        for item in items {
            grid.push_row(vec![
                Some(Cell::Text(item.name.clone())),
                Some(Cell::Text(item.reason.clone())),
            ]);
        }
        grid
    }
}

fn index_options(index: &FirestoreCompositeIndex) -> Option<String> {
    let mut options = Vec::new();
    if index.unique {
        options.push("unique".to_string());
    }
    if let Some(density) = &index.density {
        options.push(format!("density={density}"));
    }
    if let Some(search) = &index.search_options {
        if let Some(language) = &search.text_language {
            options.push(format!("text_language={language}"));
        }
        if let Some(path) = &search.text_language_override_field_path {
            options.push(format!("text_language_override_field_path={path}"));
        }
    }
    (!options.is_empty()).then(|| options.join(" "))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::{ExpandedMode, OutputFormat, Theme, View};
    use firestore::{
        FirestoreIndexDensity, FirestoreIndexField, FirestoreIndexFieldMode,
        FirestoreIndexQueryScope, FirestoreIndexState, FirestoreQueryDirection,
    };

    fn table(grid: &Grid) -> String {
        View {
            theme: &Theme::new(false),
            format: OutputFormat::Table,
            expanded: ExpandedMode::Off,
            width: None,
            documents_path: "",
        }
        .grid(grid)
    }

    #[test]
    fn collections_under_a_document_carry_their_path_from_the_root() {
        let orders: FirestoreCollectionId = "orders".parse().expect("a collection ID");

        assert_eq!(
            table(&Grid::collections(
                std::slice::from_ref(&orders),
                Some("users/alice")
            )),
            r#"╭────────────┬────────────────────╮
│ collection ┆ path               │
╞════════════╪════════════════════╡
│ orders     ┆ users/alice/orders │
╰────────────┴────────────────────╯
"#
        );
        assert_eq!(
            Grid::collections(std::slice::from_ref(&orders), None).columns(),
            ["collection"]
        );
    }

    #[test]
    fn an_index_lists_its_scope_fields_and_options() {
        let index = FirestoreListedCompositeIndex {
            name: "projects/p/databases/d/collectionGroups/orders/indexes/CICAgJim14AK".to_string(),
            collection_group: "orders".parse().expect("a collection ID"),
            id: "CICAgJim14AK".parse().expect("an index ID"),
            state: FirestoreIndexState::Ready,
            index: FirestoreCompositeIndex {
                fields: vec![
                    FirestoreIndexField {
                        field_path: "customer_id".to_string(),
                        mode: FirestoreIndexFieldMode::Order(FirestoreQueryDirection::Ascending),
                    },
                    FirestoreIndexField {
                        field_path: "tags".to_string(),
                        mode: FirestoreIndexFieldMode::ArrayContains,
                    },
                ],
                query_scope: FirestoreIndexQueryScope::AllDescendants,
                search_options: None,
                unique: true,
                density: Some(FirestoreIndexDensity::SparseAny),
            },
        };

        assert_eq!(
            table(&Grid::indexes([&index])),
            r#"╭──────────────────┬──────────────┬───────┬──────────────────┬────────────────────────────────┬───────────────────────────╮
│ collection_group ┆ id           ┆ state ┆ scope            ┆ fields                         ┆ options                   │
╞══════════════════╪══════════════╪═══════╪══════════════════╪════════════════════════════════╪═══════════════════════════╡
│ orders           ┆ CICAgJim14AK ┆ READY ┆ COLLECTION_GROUP ┆ customer_id ASC, tags CONTAINS ┆ unique density=SPARSE_ANY │
╰──────────────────┴──────────────┴───────┴──────────────────┴────────────────────────────────┴───────────────────────────╯
"#
        );
    }
}
