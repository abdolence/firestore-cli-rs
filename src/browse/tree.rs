use super::state::RequestId;
use crate::errors::{FsqError, FsqResult};
use crate::field_path::FieldPath;
use crate::render::sorted_keys;
use firestore::gcloud_sdk::google::firestore::v1::value::ValueType;
use firestore::gcloud_sdk::google::firestore::v1::{ArrayValue, MapValue, Value};
use firestore::{FirestoreCollectionId, FirestoreDocument, FirestoreVector};
use std::collections::BTreeSet;
use std::fmt::{Display, Formatter};

/// Where a value sits inside a document: the field names and array positions on the way to it.
#[derive(Debug, Clone, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct NodePath(Vec<Segment>);

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Segment {
    Field(String),
    Index(usize),
}

impl NodePath {
    /// The field `name` of the map this path names, or a top-level field of the document when
    /// this is the empty path.
    pub fn field(&self, name: &str) -> Self {
        let mut segments = self.0.clone();
        segments.push(Segment::Field(name.to_string()));
        Self(segments)
    }

    fn element(&self, index: usize) -> Self {
        let mut segments = self.0.clone();
        segments.push(Segment::Index(index));
        Self(segments)
    }

    /// The field name or array position of the value itself.
    pub fn last(&self) -> Option<&Segment> {
        self.0.last()
    }

    /// How many maps and arrays the value is inside: 0 for a top-level field.
    pub fn depth(&self) -> usize {
        self.0.len().saturating_sub(1)
    }

    /// The value this path names in `document`, `None` where the document has none there.
    pub fn value_in<'a>(&self, document: &'a FirestoreDocument) -> Option<&'a Value> {
        let (Segment::Field(name), inside) = self.0.split_first()? else {
            return None;
        };
        inside
            .iter()
            .try_fold(document.fields.get(name)?, |value, segment| {
                match (segment, &value.value_type) {
                    (Segment::Field(name), Some(ValueType::MapValue(map))) => map.fields.get(name),
                    (Segment::Index(index), Some(ValueType::ArrayValue(array))) => {
                        array.values.get(*index)
                    }
                    _ => None,
                }
            })
    }
}

impl From<&NodePath> for FieldPath {
    fn from(path: &NodePath) -> Self {
        path.0
            .iter()
            .fold(Self::default(), |field_path, segment| match segment {
                Segment::Field(name) => field_path.field(name),
                Segment::Index(index) => field_path.element(*index),
            })
    }
}

impl Display for Segment {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Field(name) => f.write_str(name),
            Self::Index(index) => write!(f, "[{index}]"),
        }
    }
}

/// The columns the name of the element at `index` of an array takes, such as `[12]`.
fn index_width(index: usize) -> usize {
    let digits = std::iter::successors(Some(index), |rest| (*rest >= 10).then(|| rest / 10));
    digits.count() + 2
}

/// Whether a row has rows inside it, and whether they show.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fold {
    Leaf,
    Collapsed,
    Expanded,
}

/// The subcollections of a document, listed the first time their row is expanded.
#[derive(Debug, Default)]
pub enum Listing {
    #[default]
    NotListed,
    Pending(RequestId),
    Listed(Vec<FirestoreCollectionId>),
    Failed(FsqError),
}

/// A line of the tree.
#[derive(Debug)]
pub enum Row<'a> {
    Field {
        path: NodePath,
        value: &'a Value,
        fold: Fold,
    },
    /// The group that holds the subcollections, after the fields.
    Subcollections {
        listing: &'a Listing,
        fold: Fold,
    },
    Subcollection(&'a FirestoreCollectionId),
}

impl Row<'_> {
    pub fn depth(&self) -> usize {
        match self {
            Self::Field { path, .. } => path.depth(),
            Self::Subcollections { .. } => 0,
            Self::Subcollection(_) => 1,
        }
    }
}

/// What expanding a row asks of the browser beyond the tree itself.
#[derive(Debug, PartialEq, Eq)]
pub enum Expansion {
    ListSubcollections,
    OpenSubcollection(FirestoreCollectionId),
}

/// The values of one document as a tree that opens a map or an array at a time, with a cursor
/// on one of its rows.
#[derive(Debug, Default)]
pub struct Tree {
    expanded: BTreeSet<NodePath>,
    subcollections_open: bool,
    listing: Listing,
    cursor: usize,
}

impl Tree {
    /// The rows that show: every top-level field, the values inside each expanded map and array,
    /// and the subcollections group last. Map keys come in sorted order, and a vector is a leaf.
    pub fn rows<'a>(&'a self, document: &'a FirestoreDocument) -> Vec<Row<'a>> {
        let mut rows = Vec::new();
        for name in sorted_keys(&document.fields) {
            self.push_value(
                NodePath::default().field(name),
                &document.fields[name],
                &mut rows,
            );
        }
        rows.push(Row::Subcollections {
            listing: &self.listing,
            fold: if self.subcollections_open {
                Fold::Expanded
            } else {
                Fold::Collapsed
            },
        });
        if let (true, Listing::Listed(collection_ids)) = (self.subcollections_open, &self.listing) {
            rows.extend(collection_ids.iter().map(Row::Subcollection));
        }
        rows
    }

    /// The columns the widest name of a value in `document` takes after its indent, over every
    /// value at any depth: every field row the tree has with each map and array expanded.
    /// `indent` gives the columns before a name `depth` levels down.
    pub fn widest_field(document: &FirestoreDocument, indent: impl Fn(usize) -> usize) -> usize {
        fn widest_inside(value: &Value, depth: usize, indent: &impl Fn(usize) -> usize) -> usize {
            let widest = |name_width: usize, value: &Value| {
                (indent(depth) + name_width).max(widest_inside(value, depth + 1, indent))
            };
            match Inside::of(value) {
                Some(Inside::Map(map)) => map
                    .fields
                    .iter()
                    .map(|(name, value)| widest(console::measure_text_width(name), value))
                    .max(),
                Some(Inside::Array(array)) => array
                    .values
                    .iter()
                    .enumerate()
                    .map(|(index, value)| widest(index_width(index), value))
                    .max(),
                None => None,
            }
            .unwrap_or_default()
        }
        document
            .fields
            .iter()
            .map(|(name, value)| {
                (indent(0) + console::measure_text_width(name))
                    .max(widest_inside(value, 1, &indent))
            })
            .max()
            .unwrap_or_default()
    }

    fn push_value<'a>(&self, path: NodePath, value: &'a Value, rows: &mut Vec<Row<'a>>) {
        let inside = Inside::of(value);
        let fold = match inside {
            None => Fold::Leaf,
            Some(_) if self.expanded.contains(&path) => Fold::Expanded,
            Some(_) => Fold::Collapsed,
        };
        rows.push(Row::Field {
            path: path.clone(),
            value,
            fold,
        });
        match inside.filter(|_| fold == Fold::Expanded) {
            Some(Inside::Map(map)) => {
                for name in sorted_keys(&map.fields) {
                    self.push_value(path.field(name), &map.fields[name], rows);
                }
            }
            Some(Inside::Array(array)) => {
                for (index, element) in array.values.iter().enumerate() {
                    self.push_value(path.element(index), element, rows);
                }
            }
            None => {}
        }
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// Puts the cursor on `row`, which the caller keeps inside [`Self::rows`].
    pub fn select(&mut self, row: usize) {
        self.cursor = row;
    }

    /// Puts the cursor on the last row of `document` when it is past it, as after the document
    /// was read again with fewer fields.
    pub fn keep_cursor_in(&mut self, document: &FirestoreDocument) {
        let last = self.rows(document).len().saturating_sub(1);
        self.cursor = self.cursor.min(last);
    }

    /// Moves this tree from `from` to `to`, another document, so that one field can be compared
    /// across documents: the maps and arrays open in `from` stay open, and the cursor goes to the
    /// row of the same path, else of the nearest path around it that `to` has, else to the first
    /// row. The subcollections listed are those of `from`, so their group closes.
    pub fn switch_document(&mut self, from: &FirestoreDocument, to: &FirestoreDocument) {
        let at_cursor = self.at_cursor(from);
        self.subcollections_open = false;
        self.listing = Listing::NotListed;
        let rows = self.rows(to);
        let cursor = match at_cursor {
            Some(AtCursor::Field(path, _)) => (1..=path.0.len()).rev().find_map(|len| {
                let around = NodePath(path.0[..len].to_vec());
                rows.iter()
                    .position(|row| matches!(row, Row::Field { path, .. } if *path == around))
            }),
            Some(AtCursor::Subcollections(_) | AtCursor::Subcollection(_)) => rows
                .iter()
                .position(|row| matches!(row, Row::Subcollections { .. })),
            None => None,
        };
        self.cursor = cursor.unwrap_or_default();
    }

    pub fn cursor_row<'a>(&'a self, document: &'a FirestoreDocument) -> Option<Row<'a>> {
        self.rows(document).into_iter().nth(self.cursor)
    }

    /// Expands the row under the cursor.
    pub fn expand(&mut self, document: &FirestoreDocument) -> Option<Expansion> {
        match self.at_cursor(document)? {
            AtCursor::Field(path, Fold::Collapsed) => {
                self.expanded.insert(path);
                None
            }
            AtCursor::Subcollections(Fold::Collapsed) => {
                self.subcollections_open = true;
                matches!(self.listing, Listing::NotListed | Listing::Failed(_))
                    .then_some(Expansion::ListSubcollections)
            }
            AtCursor::Subcollection(collection_id) => {
                Some(Expansion::OpenSubcollection(collection_id))
            }
            AtCursor::Field(..) | AtCursor::Subcollections(_) => None,
        }
    }

    /// Expands the row under the cursor, or collapses it when it is expanded. A leaf stays as it
    /// is.
    pub fn toggle(&mut self, document: &FirestoreDocument) -> Option<Expansion> {
        match self.at_cursor(document)? {
            AtCursor::Field(path, Fold::Expanded) => {
                self.expanded.remove(&path);
                None
            }
            AtCursor::Subcollections(Fold::Expanded) => {
                self.subcollections_open = false;
                None
            }
            AtCursor::Field(_, Fold::Leaf) => None,
            AtCursor::Field(..) | AtCursor::Subcollections(_) | AtCursor::Subcollection(_) => {
                self.expand(document)
            }
        }
    }

    /// Collapses the row under the cursor, or moves the cursor to the row it is inside. `false`
    /// when there is neither: a top-level row that shows nothing inside it.
    pub fn collapse(&mut self, document: &FirestoreDocument) -> bool {
        match self.at_cursor(document) {
            Some(AtCursor::Field(path, Fold::Expanded)) => {
                self.expanded.remove(&path);
            }
            Some(AtCursor::Subcollections(Fold::Expanded)) => self.subcollections_open = false,
            Some(_) => match self.row_around_cursor(document) {
                Some(row) => self.cursor = row,
                None => return false,
            },
            None => return false,
        }
        true
    }

    fn at_cursor(&self, document: &FirestoreDocument) -> Option<AtCursor> {
        Some(match self.cursor_row(document)? {
            Row::Field { path, fold, .. } => AtCursor::Field(path, fold),
            Row::Subcollections { fold, .. } => AtCursor::Subcollections(fold),
            Row::Subcollection(collection_id) => AtCursor::Subcollection(collection_id.clone()),
        })
    }

    /// The position of the row the one under the cursor is inside.
    fn row_around_cursor(&self, document: &FirestoreDocument) -> Option<usize> {
        let rows = self.rows(document);
        let depth = rows.get(self.cursor)?.depth().checked_sub(1)?;
        rows[..self.cursor]
            .iter()
            .rposition(|row| row.depth() == depth)
    }

    pub fn listing_started(&mut self, request: RequestId) {
        self.listing = Listing::Pending(request);
    }

    pub fn listing(&self) -> &Listing {
        &self.listing
    }

    pub fn is_listing(&self) -> bool {
        matches!(self.listing, Listing::Pending(_))
    }

    /// Keeps the outcome of the listing `request`, unless the tree stopped waiting for it.
    pub fn listed(&mut self, request: RequestId, outcome: FsqResult<Vec<FirestoreCollectionId>>) {
        if matches!(self.listing, Listing::Pending(pending) if pending == request) {
            self.listing = match outcome {
                Ok(collection_ids) => Listing::Listed(collection_ids),
                Err(error) => Listing::Failed(error),
            };
        }
    }

    /// Stops waiting for the listing and closes its group. `false` when no listing was pending.
    pub fn cancel_listing(&mut self) -> bool {
        if !self.is_listing() {
            return false;
        }
        self.listing = Listing::NotListed;
        self.subcollections_open = false;
        true
    }
}

/// The row under the cursor, held apart from the tree so that the tree can change.
enum AtCursor {
    Field(NodePath, Fold),
    Subcollections(Fold),
    Subcollection(FirestoreCollectionId),
}

/// The values inside a map or an array that has some.
#[derive(Clone, Copy)]
enum Inside<'a> {
    Map(&'a MapValue),
    Array(&'a ArrayValue),
}

impl<'a> Inside<'a> {
    /// What `value` holds, unless it holds nothing or is a vector, which shows as one value.
    fn of(value: &'a Value) -> Option<Self> {
        match &value.value_type {
            Some(ValueType::MapValue(map))
                if !map.fields.is_empty() && !FirestoreVector::is_vector(value) =>
            {
                Some(Self::Map(map))
            }
            Some(ValueType::ArrayValue(array)) if !array.values.is_empty() => {
                Some(Self::Array(array))
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::browse::fixtures::field_path;
    use crate::render::fixtures::{document, dune, value, vector};

    /// The path and fold of every field row, in order.
    fn fields(tree: &Tree, document: &FirestoreDocument) -> Vec<(NodePath, Fold)> {
        tree.rows(document)
            .into_iter()
            .filter_map(|row| match row {
                Row::Field { path, fold, .. } => Some((path, fold)),
                _ => None,
            })
            .collect()
    }

    fn expand_at(tree: &mut Tree, document: &FirestoreDocument, path: &NodePath) {
        point_at(tree, document, path);
        tree.expand(document);
    }

    fn point_at(tree: &mut Tree, document: &FirestoreDocument, path: &NodePath) {
        let row = tree
            .rows(document)
            .iter()
            .position(|row| matches!(row, Row::Field { path: at, .. } if at == path))
            .unwrap_or_else(|| panic!("no row for {path:?}"));
        tree.select(row);
    }

    /// `shelf: {top: {left: 1}}`, a map inside a map inside a map.
    fn shelved() -> FirestoreDocument {
        let map = |name: &str, inner: Value| {
            value(ValueType::MapValue(MapValue {
                fields: [(name.to_string(), inner)].into(),
            }))
        };
        document(
            "shelved",
            vec![(
                "shelf",
                map("top", map("left", value(ValueType::IntegerValue(1)))),
            )],
        )
    }

    #[test]
    fn a_collapsed_map_shows_one_row() {
        let dune = dune();

        let rows = fields(&Tree::default(), &dune);

        assert_eq!(rows.len(), dune.fields.len());
        assert!(rows.contains(&(field_path(&["address"]), Fold::Collapsed)));
        assert!(rows.iter().all(|(path, _)| path.depth() == 0));
    }

    #[test]
    fn an_expanded_map_shows_its_keys_in_sorted_order_one_level_deeper() {
        let dune = dune();
        let mut tree = Tree::default();

        expand_at(&mut tree, &dune, &field_path(&["address"]));

        let rows = fields(&tree, &dune);
        let address = rows
            .iter()
            .position(|(path, _)| *path == field_path(&["address"]))
            .expect("the address row");
        assert_eq!(rows[address].1, Fold::Expanded);
        assert_eq!(
            rows[address + 1..address + 3],
            [
                (field_path(&["address", "city"]), Fold::Leaf),
                (field_path(&["address", "zip"]), Fold::Leaf),
            ]
        );
        assert_eq!(rows[address + 1].0.depth(), 1);
    }

    #[test]
    fn collapsing_a_map_hides_what_is_open_inside_it_and_reopening_shows_it_again() {
        let shelved = shelved();
        let mut tree = Tree::default();
        expand_at(&mut tree, &shelved, &field_path(&["shelf"]));
        expand_at(&mut tree, &shelved, &field_path(&["shelf", "top"]));
        let open = fields(&tree, &shelved);

        point_at(&mut tree, &shelved, &field_path(&["shelf"]));
        tree.collapse(&shelved);
        let collapsed = fields(&tree, &shelved);
        tree.expand(&shelved);

        assert_eq!(collapsed, [(field_path(&["shelf"]), Fold::Collapsed)]);
        assert_eq!(open.len(), 3);
        assert_eq!(fields(&tree, &shelved), open);
    }

    #[test]
    fn collapsing_a_value_without_rows_inside_moves_to_the_row_it_is_in() {
        let shelved = shelved();
        let mut tree = Tree::default();
        expand_at(&mut tree, &shelved, &field_path(&["shelf"]));
        expand_at(&mut tree, &shelved, &field_path(&["shelf", "top"]));
        point_at(&mut tree, &shelved, &field_path(&["shelf", "top", "left"]));

        assert!(tree.collapse(&shelved));
        assert_eq!(tree.cursor(), 1);
        point_at(&mut tree, &shelved, &field_path(&["shelf"]));
        tree.collapse(&shelved);
        assert!(!tree.collapse(&shelved));
    }

    #[test]
    fn the_elements_of_an_array_are_rows_named_by_their_position() {
        let dune = dune();
        let mut tree = Tree::default();

        expand_at(&mut tree, &dune, &field_path(&["tags"]));

        let elements: Vec<_> = fields(&tree, &dune)
            .into_iter()
            .filter(|(path, _)| path.depth() == 1)
            .map(|(path, _)| path)
            .collect();
        assert_eq!(
            elements,
            [
                NodePath::default().field("tags").element(0),
                NodePath::default().field("tags").element(1),
            ]
        );
        assert_eq!(
            elements[1].last().map(ToString::to_string).as_deref(),
            Some("[1]")
        );
    }

    #[test]
    fn a_vector_is_a_leaf() {
        let odd = document("odd", vec![("embedding", vector(&[0.4, 0.2]))]);
        let mut tree = Tree::default();

        expand_at(&mut tree, &odd, &field_path(&["embedding"]));

        assert_eq!(
            fields(&tree, &odd),
            [(field_path(&["embedding"]), Fold::Leaf)]
        );
    }

    #[test]
    fn the_subcollections_are_listed_the_first_time_their_group_opens() {
        let dune = dune();
        let orders: FirestoreCollectionId = "orders".parse().expect("a collection ID");
        let mut tree = Tree::default();
        let group = dune.fields.len();
        tree.select(group);

        let first = tree.expand(&dune);
        tree.listing_started(RequestId::default());
        tree.listed(RequestId::default(), Ok(vec![orders.clone()]));
        let rows = tree.rows(&dune);

        assert_eq!(first, Some(Expansion::ListSubcollections));
        assert!(matches!(
            rows[group],
            Row::Subcollections {
                fold: Fold::Expanded,
                ..
            }
        ));
        assert!(matches!(rows[group + 1], Row::Subcollection(id) if *id == orders));
        tree.select(group);
        tree.collapse(&dune);
        assert_eq!(tree.expand(&dune), None);
    }
}
