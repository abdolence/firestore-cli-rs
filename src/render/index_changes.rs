use super::{Cell, Grid};
use firestore::{
    FirestoreCollectionId, FirestoreIndexId, FirestoreIndexPlan, FirestoreIndexSyncReport,
    FirestoreListedCompositeIndex, FirestoreListedField,
};
use std::fmt::Display;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeAction {
    Create,
    Update,
    Delete,
    Revert,
    Enable,
    Disable,
    Unchanged,
    Pending,
    Reverting,
    NeedsRepair,
    /// Live, not declared, and left alone because the run does not prune.
    Undeclared,
    /// A delete a pruning sync held back.
    Withheld,
    /// Live, and of a kind the firestore crate cannot represent, so never touched.
    Unrecognised,
}

impl ChangeAction {
    fn writes(self) -> bool {
        matches!(
            self,
            Self::Create
                | Self::Update
                | Self::Delete
                | Self::Revert
                | Self::Enable
                | Self::Disable
        )
    }
}

impl Display for ChangeAction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Create => "create",
            Self::Update => "update",
            Self::Delete => "delete",
            Self::Revert => "revert",
            Self::Enable => "enable",
            Self::Disable => "disable",
            Self::Unchanged => "unchanged",
            Self::Pending => "pending",
            Self::Reverting => "reverting",
            Self::NeedsRepair => "needs repair",
            Self::Undeclared => "undeclared",
            Self::Withheld => "withheld",
            Self::Unrecognised => "unrecognised",
        })
    }
}

const INDEX_KIND: &str = "index";
const FIELD_OVERRIDE_KIND: &str = "field override";
const TTL_KIND: &str = "ttl";
const UNRECOGNISED_KIND: &str = "other";

#[derive(Debug, Clone, PartialEq)]
struct IndexChange {
    collection_group: FirestoreCollectionId,
    action: ChangeAction,
    kind: &'static str,
    definition: String,
    /// The ID of a live index, which `fsq index delete` takes.
    id: Option<FirestoreIndexId>,
}

/// What a plan or a sync did or would do to the collection groups of an indexes file, one row
/// per index, field override or TTL field.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct IndexChanges(Vec<IndexChange>);

impl IndexChanges {
    pub fn add_plan(&mut self, group: &FirestoreCollectionId, plan: &FirestoreIndexPlan) {
        use ChangeAction::*;
        self.displayed(group, Create, INDEX_KIND, &plan.create_indexes);
        self.displayed(group, Update, FIELD_OVERRIDE_KIND, &plan.update_fields);
        self.displayed(group, Enable, TTL_KIND, &plan.enable_ttl);
        self.listed_fields(group, Disable, TTL_KIND, &plan.disable_ttl);
        self.listed_indexes(group, Delete, &plan.delete_indexes);
        self.listed_fields(group, Revert, FIELD_OVERRIDE_KIND, &plan.revert_fields);
        self.listed_fields(
            group,
            Reverting,
            FIELD_OVERRIDE_KIND,
            &plan.reverting_fields,
        );
        self.displayed(
            group,
            Unchanged,
            FIELD_OVERRIDE_KIND,
            &plan.unchanged_fields,
        );
        self.displayed(group, Unchanged, INDEX_KIND, &plan.unchanged);
        self.displayed(group, Pending, INDEX_KIND, &plan.pending);
        self.displayed(group, NeedsRepair, INDEX_KIND, &plan.needs_repair);
        self.displayed(group, Unchanged, TTL_KIND, &plan.unchanged_ttl);
        self.displayed(group, Pending, TTL_KIND, &plan.pending_ttl);
        self.displayed(group, NeedsRepair, TTL_KIND, &plan.needs_repair_ttl);
        self.listed_indexes(group, Undeclared, &plan.kept_undeclared_indexes);
        self.listed_fields(
            group,
            Undeclared,
            FIELD_OVERRIDE_KIND,
            &plan.kept_undeclared_fields,
        );
        self.listed_fields(group, Undeclared, TTL_KIND, &plan.kept_undeclared_ttl);
        self.displayed(group, Unrecognised, UNRECOGNISED_KIND, &plan.unrecognised);
    }

    pub fn add_report(&mut self, group: &FirestoreCollectionId, report: &FirestoreIndexSyncReport) {
        use ChangeAction::*;
        self.displayed(group, Create, INDEX_KIND, &report.created_indexes);
        self.displayed(group, Update, FIELD_OVERRIDE_KIND, &report.updated_fields);
        self.displayed(group, Enable, TTL_KIND, &report.enabled_ttl);
        self.listed_fields(group, Disable, TTL_KIND, &report.disabled_ttl);
        self.listed_indexes(group, Delete, &report.deleted_indexes);
        if let Some(withheld) = &report.withheld_deletes {
            self.listed_indexes(group, Withheld, &withheld.indexes);
        }
        self.listed_fields(group, Revert, FIELD_OVERRIDE_KIND, &report.reverted_fields);
        self.listed_fields(
            group,
            Reverting,
            FIELD_OVERRIDE_KIND,
            &report.reverting_fields,
        );
        self.displayed(
            group,
            Unchanged,
            FIELD_OVERRIDE_KIND,
            &report.unchanged_fields,
        );
        self.displayed(group, Unchanged, INDEX_KIND, &report.unchanged);
        self.displayed(group, Pending, INDEX_KIND, &report.pending);
        self.displayed(group, NeedsRepair, INDEX_KIND, &report.needs_repair);
        self.displayed(group, Unchanged, TTL_KIND, &report.unchanged_ttl);
        self.displayed(group, Pending, TTL_KIND, &report.pending_ttl);
        self.displayed(group, NeedsRepair, TTL_KIND, &report.needs_repair_ttl);
        self.listed_indexes(group, Undeclared, &report.kept_undeclared_indexes);
        self.listed_fields(
            group,
            Undeclared,
            FIELD_OVERRIDE_KIND,
            &report.kept_undeclared_fields,
        );
        self.listed_fields(group, Undeclared, TTL_KIND, &report.kept_undeclared_ttl);
        self.displayed(group, Unrecognised, UNRECOGNISED_KIND, &report.unrecognised);
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn written(&self) -> usize {
        self.0
            .iter()
            .filter(|change| change.action.writes())
            .count()
    }

    fn displayed<T: Display>(
        &mut self,
        group: &FirestoreCollectionId,
        action: ChangeAction,
        kind: &'static str,
        items: &[T],
    ) {
        self.0.extend(items.iter().map(|item| IndexChange {
            collection_group: group.clone(),
            action,
            kind,
            definition: item.to_string(),
            id: None,
        }));
    }

    fn listed_indexes(
        &mut self,
        group: &FirestoreCollectionId,
        action: ChangeAction,
        indexes: &[FirestoreListedCompositeIndex],
    ) {
        self.0.extend(indexes.iter().map(|listed| IndexChange {
            collection_group: group.clone(),
            action,
            kind: INDEX_KIND,
            definition: listed.index.to_string(),
            id: Some(listed.id.clone()),
        }));
    }

    fn listed_fields(
        &mut self,
        group: &FirestoreCollectionId,
        action: ChangeAction,
        kind: &'static str,
        fields: &[FirestoreListedField],
    ) {
        self.0.extend(fields.iter().map(|field| IndexChange {
            collection_group: group.clone(),
            action,
            kind,
            definition: field.field_path.clone(),
            id: None,
        }));
    }
}

impl From<&IndexChanges> for Grid {
    fn from(changes: &IndexChanges) -> Self {
        let mut grid = Grid::new(["collection_group", "action", "kind", "definition", "id"]);
        for change in &changes.0 {
            grid.push_row(vec![
                Some(Cell::Text(change.collection_group.to_string())),
                Some(Cell::Text(change.action.to_string())),
                Some(Cell::Text(change.kind.to_string())),
                Some(Cell::Text(change.definition.clone())),
                change.id.as_ref().map(|id| Cell::Text(id.to_string())),
            ]);
        }
        grid
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::{ExpandedMode, OutputFormat, Theme, View};
    use firestore::{
        FirestoreCompositeIndex, FirestoreExplicitFieldOverride, FirestoreFieldOverride,
        FirestoreFieldOverrideOutcome, FirestoreFieldOverrideTarget, FirestoreIndexDeletesWithheld,
        FirestoreIndexDeletesWithheldReason, FirestoreIndexField, FirestoreIndexFieldMode,
        FirestoreIndexQueryScope, FirestoreIndexState, FirestoreListedCompositeIndex,
        FirestoreQueryDirection,
    };

    fn group(id: &str) -> FirestoreCollectionId {
        id.parse().expect("a collection ID")
    }

    fn ascending_index(field_path: &str) -> FirestoreCompositeIndex {
        FirestoreCompositeIndex {
            fields: vec![FirestoreIndexField {
                field_path: field_path.to_string(),
                mode: FirestoreIndexFieldMode::Order(FirestoreQueryDirection::Ascending),
            }],
            query_scope: FirestoreIndexQueryScope::Collection,
            search_options: None,
            unique: false,
            density: None,
        }
    }

    fn exempted(path: &str) -> FirestoreFieldOverride {
        FirestoreFieldOverride {
            target: FirestoreFieldOverrideTarget::Field(path.to_string()),
            indexes: Vec::new(),
        }
    }

    fn reverting_field(path: &str) -> FirestoreListedField {
        FirestoreListedField {
            name: format!("projects/p/databases/d/collectionGroups/books/fields/{path}"),
            collection_group: group("books"),
            field_path: path.to_string(),
            index_override: Some(FirestoreFieldOverrideOutcome::Explicit(
                FirestoreExplicitFieldOverride {
                    indexes: Vec::new(),
                    reverting: true,
                },
            )),
            ttl: None,
        }
    }

    fn table(changes: &IndexChanges) -> String {
        View {
            theme: &Theme::new(false),
            format: OutputFormat::Table,
            expanded: ExpandedMode::Off,
            width: None,
            documents_path: "",
        }
        .grid(&Grid::from(changes))
    }

    #[test]
    fn a_plan_lists_each_change_and_what_stays() {
        let live = FirestoreListedCompositeIndex {
            name: "projects/p/databases/d/collectionGroups/books/indexes/CICAgJim14AK".to_string(),
            collection_group: group("books"),
            id: "CICAgJim14AK".parse().expect("an index ID"),
            state: FirestoreIndexState::Ready,
            index: ascending_index("rating"),
        };
        let plan = FirestoreIndexPlan {
            create_indexes: vec![ascending_index("genre")],
            unchanged: vec![ascending_index("title")],
            delete_indexes: vec![live],
            update_fields: vec![exempted("notes")],
            unchanged_fields: vec![exempted("flags")],
            enable_ttl: vec!["expires_at".to_string()],
            ..FirestoreIndexPlan::default()
        };
        let mut changes = IndexChanges::default();

        changes.add_plan(&group("books"), &plan);

        assert_eq!(changes.written(), 4);
        assert_eq!(
            table(&changes),
            r#"╭──────────────────┬───────────┬────────────────┬───────────────────────────┬──────────────╮
│ collection_group ┆ action    ┆ kind           ┆ definition                ┆ id           │
╞══════════════════╪═══════════╪════════════════╪═══════════════════════════╪══════════════╡
│ books            ┆ create    ┆ index          ┆ [COLLECTION] (genre ASC)  ┆              │
│ books            ┆ update    ┆ field override ┆ notes EXEMPT              ┆              │
│ books            ┆ enable    ┆ ttl            ┆ expires_at                ┆              │
│ books            ┆ delete    ┆ index          ┆ [COLLECTION] (rating ASC) ┆ CICAgJim14AK │
│ books            ┆ unchanged ┆ field override ┆ flags EXEMPT              ┆              │
│ books            ┆ unchanged ┆ index          ┆ [COLLECTION] (title ASC)  ┆              │
╰──────────────────┴───────────┴────────────────┴───────────────────────────┴──────────────╯
"#
        );
    }

    #[test]
    fn a_sync_report_lists_the_deletes_it_withheld_apart_from_the_ones_it_made() {
        let listed = |id: &str, field_path: &str| FirestoreListedCompositeIndex {
            name: format!("projects/p/databases/d/collectionGroups/books/indexes/{id}"),
            collection_group: group("books"),
            id: id.parse().expect("an index ID"),
            state: FirestoreIndexState::Ready,
            index: ascending_index(field_path),
        };
        let report = FirestoreIndexSyncReport {
            deleted_indexes: vec![listed("CICAgJim14AK", "rating")],
            withheld_deletes: Some(FirestoreIndexDeletesWithheld {
                indexes: vec![listed("CICAgOjXh4EK", "genre")],
                reason: FirestoreIndexDeletesWithheldReason::AlreadyExists(Vec::new()),
            }),
            ..FirestoreIndexSyncReport::default()
        };
        let mut changes = IndexChanges::default();

        changes.add_report(&group("books"), &report);

        assert_eq!(changes.written(), 1);
        assert_eq!(
            Grid::from(&changes)
                .rows()
                .iter()
                .map(|row| row[1].clone())
                .collect::<Vec<_>>(),
            [
                Some(Cell::Text("delete".to_string())),
                Some(Cell::Text("withheld".to_string()))
            ]
        );
    }

    #[test]
    fn a_sync_report_lists_field_overrides_that_match_and_that_are_still_reverting() {
        let report = FirestoreIndexSyncReport {
            unchanged_fields: vec![exempted("flags")],
            reverting_fields: vec![reverting_field("notes")],
            ..FirestoreIndexSyncReport::default()
        };
        let mut changes = IndexChanges::default();

        changes.add_report(&group("books"), &report);

        assert_eq!(changes.written(), 0);
        assert_eq!(
            table(&changes),
            r#"╭──────────────────┬───────────┬────────────────┬──────────────┬────╮
│ collection_group ┆ action    ┆ kind           ┆ definition   ┆ id │
╞══════════════════╪═══════════╪════════════════╪══════════════╪════╡
│ books            ┆ reverting ┆ field override ┆ notes        ┆    │
│ books            ┆ unchanged ┆ field override ┆ flags EXEMPT ┆    │
╰──────────────────┴───────────┴────────────────┴──────────────┴────╯
"#
        );
    }
}
