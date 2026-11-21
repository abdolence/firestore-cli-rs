use super::editor::{Edit, FilterEditor, FilterHistory, Recall};
use super::tree::{Expansion, Fold, Listing, NodePath, Row, Tree};
use super::BrowseTarget;
use crate::describe::CollectionPath;
use crate::errors::{FsqError, FsqResult};
use crate::field_path::FieldPath;
use firestore::gcloud_sdk::google::firestore::v1::value::ValueType;
use firestore::gcloud_sdk::google::firestore::v1::Value;
use firestore::{FirestoreCollectionId, FirestoreDb, FirestoreDocument, FirestoreJsonEncoding};
use sqlparser::ast::{self, Expr, LimitClause, OrderByKind, Query, Statement, ValueWithSpan};
use sqlparser::dialect::BigQueryDialect;
use sqlparser::parser::Parser;
use std::fmt::{Display, Formatter};

/// How many documents a level shows when its filter has no `LIMIT` of its own.
pub const BROWSE_DOCUMENT_LIMIT: usize = 100;

/// The end of a `SELECT` after its `FROM`, such as `WHERE …`, `ORDER BY …` and `LIMIT …`, as
/// typed, without the `;` that ends a statement, and what the browser reads from it. An empty one
/// reads the collection as it is.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FilterTail {
    text: String,
    /// The `LIMIT` of the query this filter ends, `None` when it has none.
    limit: Option<Limit>,
    /// The field the documents are ordered by first, when the first key of the `ORDER BY` of the
    /// query is a field path, such as `rating` or `address.city`.
    order_key: Option<NodePath>,
}

/// The `LIMIT` of the query a filter ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Limit {
    Count(usize),
    /// A limit that is not a number, such as `LIMIT 2 + 3`.
    Uncounted,
}

impl Limit {
    /// The `LIMIT` of `query`. A field named `limit`, the `LIMIT` of a subquery and an `OFFSET`
    /// alone bound nothing.
    fn of(query: &Query) -> Option<Self> {
        let limit = match query.limit_clause.as_ref()? {
            LimitClause::LimitOffset { limit, .. } => limit.as_ref()?,
            LimitClause::OffsetCommaLimit { limit, .. } => limit,
        };
        Some(match limit {
            Expr::Value(ValueWithSpan {
                value: ast::Value::Number(count, _),
                ..
            }) => count.parse().map_or(Self::Uncounted, Self::Count),
            _ => Self::Uncounted,
        })
    }
}

impl FilterTail {
    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// Whether the query this filter ends has a `LIMIT` of its own.
    pub fn has_limit(&self) -> bool {
        self.limit.is_some()
    }

    /// The most documents the query of this filter reads: its own `LIMIT` when that is a number,
    /// else [`BROWSE_DOCUMENT_LIMIT`] when it has none. `None` for a `LIMIT` that is not a number.
    pub fn effective_limit(&self) -> Option<usize> {
        match self.limit {
            None => Some(BROWSE_DOCUMENT_LIMIT),
            Some(Limit::Count(count)) => Some(count),
            Some(Limit::Uncounted) => None,
        }
    }

    pub fn order_key(&self) -> Option<&NodePath> {
        self.order_key.as_ref()
    }
}

/// The field path of the first key of the `ORDER BY` of `query`.
fn first_order_key(query: &Query) -> Option<NodePath> {
    let OrderByKind::Expressions(keys) = &query.order_by.as_ref()?.kind else {
        return None;
    };
    let names = match &keys.first()?.expr {
        Expr::Identifier(name) => std::slice::from_ref(name),
        Expr::CompoundIdentifier(names) => names.as_slice(),
        _ => return None,
    };
    Some(
        names
            .iter()
            .fold(NodePath::default(), |path, name| path.field(&name.value)),
    )
}

impl From<&str> for FilterTail {
    /// The filter `text`, read as the parser the firestore crate translates with reads it. A
    /// filter that does not parse has no limit and no order key.
    fn from(text: &str) -> Self {
        let text = text.trim();
        let text = text.strip_suffix(';').unwrap_or(text).trim_end();
        let statement = format!("SELECT * FROM t\n{text}");
        let parsed = Parser::parse_sql(&BigQueryDialect {}, &statement);
        let query = match parsed.as_deref() {
            Ok([Statement::Query(query)]) => Some(query.as_ref()),
            _ => None,
        };
        Self {
            text: text.to_string(),
            limit: query.and_then(Limit::of),
            order_key: query.and_then(first_order_key),
        }
    }
}

impl Display for FilterTail {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.text)
    }
}

/// Names one Firestore call of the browser, so that an answer that arrives after the browser
/// stopped waiting for it is told apart from the one it waits for.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RequestId(u64);

impl RequestId {
    fn following(self) -> Self {
        Self(self.0 + 1)
    }
}

/// The query of the documents of one level.
#[derive(Debug, PartialEq, Eq)]
pub struct DocumentsQuery {
    pub request: RequestId,
    pub collection: CollectionPath,
    pub filter: FilterTail,
}

impl DocumentsQuery {
    /// ``SELECT * FROM `<collection>` <filter>``, limited to [`BROWSE_DOCUMENT_LIMIT`] documents
    /// when the filter sets no limit.
    pub fn sql(&self) -> String {
        let collection = self.collection.to_string().replace('`', "``");
        let mut sql = format!("SELECT * FROM `{collection}`");
        // The filter and the limit each start a line: an error in the filter then names the
        // column of the filter as typed, and a comment that ends the filter cannot swallow the
        // limit.
        if !self.filter.is_empty() {
            sql.push('\n');
            sql.push_str(&self.filter.text);
        }
        if !self.filter.has_limit() {
            sql.push_str(&format!("\nLIMIT {BROWSE_DOCUMENT_LIMIT}"));
        }
        sql
    }
}

#[derive(Debug)]
pub enum Load {
    Done,
    Pending {
        request: RequestId,
        filter: FilterTail,
    },
    /// Ctrl-C stopped the query of `filter`, and the documents before it stay.
    Stopped {
        filter: FilterTail,
    },
    /// The query of `filter` failed, and the documents before it stay.
    Failed {
        error: FsqError,
        filter: FilterTail,
    },
}

/// One collection on the way down from the collection the browser opened.
#[derive(Debug)]
pub struct Level {
    path: CollectionPath,
    /// The filter the documents were read with. A filter that is still running or failed is in
    /// [`Load`] instead, so that this one never names documents it did not read.
    filter: FilterTail,
    /// `None` until the first query of the level answers.
    documents: Option<Vec<FirestoreDocument>>,
    /// The position of the selected document, 0 when there are none.
    selected: usize,
    tree: Tree,
    load: Load,
}

impl Level {
    /// A level that has read nothing yet, so that it names the filter it is reading with.
    fn loading(path: CollectionPath, filter: FilterTail, request: RequestId) -> Self {
        let pending = Load::Pending {
            request,
            filter: filter.clone(),
        };
        Self {
            path,
            filter,
            documents: None,
            selected: 0,
            tree: Tree::default(),
            load: pending,
        }
    }

    /// Stops waiting for the pending query, if there is one, and the documents before it stay.
    /// `false` when no query was pending.
    fn stop_waiting(&mut self) -> bool {
        let Load::Pending { filter, .. } = std::mem::replace(&mut self.load, Load::Done) else {
            return false;
        };
        self.load = Load::Stopped { filter };
        true
    }

    fn query(&self, request: RequestId, filter: FilterTail) -> DocumentsQuery {
        DocumentsQuery {
            request,
            collection: self.path.clone(),
            filter,
        }
    }

    /// The filter the next edit starts from: the one that last ran, whether or not it answered.
    fn filter_to_edit(&self) -> &FilterTail {
        match &self.load {
            Load::Pending { filter, .. }
            | Load::Stopped { filter }
            | Load::Failed { filter, .. } => filter,
            Load::Done => &self.filter,
        }
    }

    pub fn path(&self) -> &CollectionPath {
        &self.path
    }

    pub fn filter(&self) -> &FilterTail {
        &self.filter
    }

    /// The documents of the last query that answered, `None` before the first one has.
    pub fn documents(&self) -> Option<&[FirestoreDocument]> {
        self.documents.as_deref()
    }

    pub fn selected(&self) -> usize {
        self.selected
    }

    pub fn selected_document(&self) -> Option<&FirestoreDocument> {
        self.documents.as_ref()?.get(self.selected)
    }

    pub fn tree(&self) -> &Tree {
        &self.tree
    }

    pub fn load(&self) -> &Load {
        &self.load
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pane {
    Documents,
    Tree,
}

/// A move of the cursor in the focused pane, or of the text of a popup, by rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Movement {
    Up(usize),
    Down(usize),
    First,
    Last,
}

impl Movement {
    /// The position this leads to from `position` in a list of `len` rows, never past either end.
    pub fn applied_to(self, position: usize, len: usize) -> usize {
        let last = len.saturating_sub(1);
        match self {
            Self::Up(rows) => position.saturating_sub(rows),
            Self::Down(rows) => position.saturating_add(rows).min(last),
            Self::First => 0,
            Self::Last => last,
        }
    }
}

/// What the keys go to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Panes,
    FilterEditor,
    /// A popup, over the panes and over the filter editor when that is open too.
    Popup,
}

/// A box over the panes that takes every key until Esc or Ctrl-C closes it.
#[derive(Debug)]
pub enum Popup {
    /// The value at `path` in the selected document, whole, as it was when the popup opened.
    Value { path: NodePath, value: Value },
    /// The whole error of a failed query or listing.
    Error(Failure),
    /// Every key of the browser.
    Help,
}

/// What failed on the current level.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Failure {
    Query,
    Listing,
}

/// What `y` and `Y` put on the clipboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Copied {
    Value,
    /// The path of the value, as the bottom border of the tree shows it.
    Path,
}

#[derive(Debug)]
pub enum Message {
    Move(Movement),
    /// Expands the row under the cursor, opens a subcollection, or in the documents pane moves
    /// to the tree.
    Expand,
    /// Shows the value under the cursor whole, a leaf or an expanded map or array, and else
    /// does what [`Message::Expand`] does.
    Open,
    /// Expands the row under the cursor, or collapses it when it is expanded.
    Toggle,
    /// Collapses the row under the cursor or moves to the row it is in, and from a top-level row
    /// back to the documents. In the documents pane it goes back up as [`Message::Back`] does.
    Collapse,
    SwitchPane,
    /// Goes back up to the collection the current one was opened from.
    Back,
    /// Reads the documents of the current level again, with the filter they were read with.
    Reload,
    /// Shows the whole error of what failed, as [`Browser::failure`] names it.
    ShowError,
    ShowHelp,
    ClosePopup,
    EditFilter,
    Edit(Edit),
    /// Text pasted at once, which goes into the filter being edited as one line. With no filter
    /// being edited it is dropped.
    Paste(String),
    RunFilter,
    CancelFilter,
    /// Copies the value under the tree cursor, or in the value popup, or its path.
    Copy(Copied),
    /// Opens the filter editor with the filter of the documents whose field at the tree cursor
    /// holds the same value, when the value has a literal in SQL.
    FilterOnValue,
    /// Shows an earlier filter that ran, or a later one, in the filter editor.
    Recall(Recall),
    /// Ctrl-C, which ends the innermost thing going on: stops waiting for Firestore, else closes
    /// the popup, else the filter editor, else quits.
    Interrupt,
    Quit,
    DocumentsLoaded {
        request: RequestId,
        outcome: FsqResult<Vec<FirestoreDocument>>,
    },
    SubcollectionsListed {
        request: RequestId,
        outcome: FsqResult<Vec<FirestoreCollectionId>>,
    },
}

/// What the browser asks its caller to do.
#[derive(Debug, PartialEq, Eq)]
pub enum Effect {
    Query(DocumentsQuery),
    /// Lists the collections under `document`, a full resource name.
    ListSubcollections {
        request: RequestId,
        document: String,
    },
    /// Puts `text`, the value or the path `copied` names, on the terminal's clipboard.
    Copy {
        copied: Copied,
        text: String,
    },
    /// Stops every call still pending.
    Cancel,
    Quit,
}

/// The state of a browser, changed only by [`Browser::update`] so that it can be tested without a
/// terminal or a database.
#[derive(Debug)]
pub struct Browser {
    current: Level,
    /// The levels the current one was opened from, the first one the browser opened first.
    parents: Vec<Level>,
    focus: Pane,
    filter_editor: Option<FilterEditor>,
    popup: Option<Popup>,
    history: FilterHistory,
    documents_path: String,
    last_request: RequestId,
}

impl Browser {
    /// A browser of `target`, which recalls the filters of `history`, and the query that reads
    /// its first documents.
    pub fn open(
        target: BrowseTarget,
        documents_path: &str,
        history: FilterHistory,
    ) -> (Self, Effect) {
        let request = RequestId::default().following();
        let current = Level::loading(target.collection, target.filter, request);
        let query = current.query(request, current.filter.clone());
        let browser = Self {
            current,
            parents: Vec::new(),
            focus: Pane::Documents,
            filter_editor: None,
            popup: None,
            history,
            documents_path: documents_path.to_string(),
            last_request: request,
        };
        (browser, Effect::Query(query))
    }

    pub fn update(&mut self, message: Message) -> Option<Effect> {
        match message {
            Message::Move(movement) => self.move_cursor(movement),
            Message::Expand => return self.expand(),
            Message::Open => return self.open_at_cursor(),
            Message::Toggle => return self.toggle(),
            Message::Collapse => return self.collapse(),
            Message::SwitchPane => {
                self.focus = match self.focus {
                    Pane::Documents => Pane::Tree,
                    Pane::Tree => Pane::Documents,
                }
            }
            Message::Back => return self.back(),
            Message::Reload => {
                let filter = self.current.filter.clone();
                return Some(self.read(filter));
            }
            Message::ShowError => {
                if let Some(failure) = self.failure() {
                    self.popup = Some(Popup::Error(failure));
                }
            }
            Message::ShowHelp => self.popup = Some(Popup::Help),
            Message::ClosePopup => self.popup = None,
            Message::EditFilter => {
                self.filter_editor = Some(FilterEditor::from(self.current.filter_to_edit()))
            }
            Message::Edit(edit) => {
                if let Some(editor) = &mut self.filter_editor {
                    editor.apply(edit);
                }
            }
            Message::Paste(pasted) => {
                if let (None, Some(editor)) = (&self.popup, &mut self.filter_editor) {
                    editor.paste(&pasted);
                }
            }
            Message::RunFilter => return self.run_filter(),
            Message::CancelFilter => self.filter_editor = None,
            Message::Copy(copied) => {
                return self
                    .copy_text(copied)
                    .map(|text| Effect::Copy { copied, text })
            }
            Message::FilterOnValue => {
                if let Some(filter) = self.filter_on_cursor() {
                    self.filter_editor = Some(FilterEditor::from(&filter));
                }
            }
            Message::Recall(recall) => {
                if let Some(editor) = &mut self.filter_editor {
                    editor.recall(recall, &self.history);
                }
            }
            Message::Interrupt => return self.interrupt(),
            Message::Quit => return Some(Effect::Quit),
            Message::DocumentsLoaded { request, outcome } => self.loaded(request, outcome),
            Message::SubcollectionsListed { request, outcome } => {
                self.current.tree.listed(request, outcome)
            }
        }
        None
    }

    fn next_request(&mut self) -> RequestId {
        self.last_request = self.last_request.following();
        self.last_request
    }

    fn move_cursor(&mut self, movement: Movement) {
        let level = &mut self.current;
        match self.focus {
            Pane::Documents => {
                let Some(documents) = level.documents.as_deref() else {
                    return;
                };
                let selected = movement.applied_to(level.selected, documents.len());
                if selected != level.selected {
                    level
                        .tree
                        .switch_document(&documents[level.selected], &documents[selected]);
                    level.selected = selected;
                }
            }
            Pane::Tree => {
                if let Some(document) = level.selected_document() {
                    let rows = level.tree.rows(document).len();
                    let row = movement.applied_to(level.tree.cursor(), rows);
                    level.tree.select(row);
                }
            }
        }
    }

    fn expand(&mut self) -> Option<Effect> {
        let level = &mut self.current;
        let document = level.documents.as_ref()?.get(level.selected)?;
        if self.focus == Pane::Documents {
            self.focus = Pane::Tree;
            return None;
        }
        let expansion = level.tree.expand(document)?;
        let document = document.name.clone();
        Some(self.expanded(expansion, document))
    }

    fn open_at_cursor(&mut self) -> Option<Effect> {
        let level = &self.current;
        let document = level.selected_document()?;
        if self.focus == Pane::Tree {
            if let Some(Row::Field {
                path,
                value,
                fold: Fold::Leaf | Fold::Expanded,
            }) = level.tree.cursor_row(document)
            {
                let value = value.clone();
                self.popup = Some(Popup::Value { path, value });
                return None;
            }
        }
        self.expand()
    }

    fn toggle(&mut self) -> Option<Effect> {
        let level = &mut self.current;
        let document = level.documents.as_ref()?.get(level.selected)?;
        if self.focus == Pane::Documents {
            return None;
        }
        let expansion = level.tree.toggle(document)?;
        let document = document.name.clone();
        Some(self.expanded(expansion, document))
    }

    /// What `expansion` of the tree of `document`, a full resource name, asks of Firestore.
    fn expanded(&mut self, expansion: Expansion, document: String) -> Effect {
        match expansion {
            Expansion::ListSubcollections => {
                let request = self.next_request();
                self.current.tree.listing_started(request);
                Effect::ListSubcollections { request, document }
            }
            Expansion::OpenSubcollection(collection_id) => {
                let parent = FirestoreDb::relative_document_name(&self.documents_path, &document);
                let path = CollectionPath::in_document(parent, collection_id);
                self.go_down(path)
            }
        }
    }

    /// Goes down into the collection at `path`. The query of the level it leaves, if one is
    /// pending, gives way to the query of the new level.
    fn go_down(&mut self, path: CollectionPath) -> Effect {
        let request = self.next_request();
        let level = Level::loading(path, FilterTail::default(), request);
        let query = level.query(request, FilterTail::default());
        self.current.stop_waiting();
        self.parents
            .push(std::mem::replace(&mut self.current, level));
        self.focus = Pane::Documents;
        Effect::Query(query)
    }

    fn collapse(&mut self) -> Option<Effect> {
        if self.focus == Pane::Documents {
            return self.back();
        }
        let level = &mut self.current;
        let document = level.documents.as_ref()?.get(level.selected)?;
        if !level.tree.collapse(document) {
            self.focus = Pane::Documents;
        }
        None
    }

    /// Returns to the level the current one was opened from, as it was left. Whatever the
    /// current level still waits for is of no use any more.
    fn back(&mut self) -> Option<Effect> {
        self.current = self.parents.pop()?;
        self.focus = Pane::Tree;
        Some(Effect::Cancel)
    }

    fn run_filter(&mut self) -> Option<Effect> {
        let editor = self.filter_editor.take()?;
        Some(self.read(FilterTail::from(editor.text())))
    }

    /// Reads the documents of the current level with `filter`.
    fn read(&mut self, filter: FilterTail) -> Effect {
        let request = self.next_request();
        let level = &mut self.current;
        let query = level.query(request, filter.clone());
        level.load = Load::Pending { request, filter };
        Effect::Query(query)
    }

    fn interrupt(&mut self) -> Option<Effect> {
        let level = &mut self.current;
        let query_stopped = level.stop_waiting();
        if level.tree.cancel_listing() || query_stopped {
            return Some(Effect::Cancel);
        }
        if self.popup.take().is_some() || self.filter_editor.take().is_some() {
            return None;
        }
        Some(Effect::Quit)
    }

    /// Keeps the answer to the query `request`, unless the browser stopped waiting for it. A new
    /// filter that fails opens in the editor to be fixed, unless the editor is open already with
    /// what the user is typing. A read with the filter the documents were read with, such as the
    /// first one or a reload, fails for a reason the filter cannot fix, so the keys stay with
    /// the panes. Documents read again with the filter they were read with keep the selected
    /// document and its tree, while it is among them.
    fn loaded(&mut self, request: RequestId, outcome: FsqResult<Vec<FirestoreDocument>>) {
        let level = &mut self.current;
        if !matches!(&level.load, Load::Pending { request: pending, .. } if *pending == request) {
            return;
        }
        let Load::Pending { filter, .. } = std::mem::replace(&mut level.load, Load::Done) else {
            return;
        };
        match outcome {
            Ok(documents) => {
                let kept = level
                    .selected_document()
                    .filter(|_| level.filter == filter)
                    .and_then(|selected| {
                        documents
                            .iter()
                            .position(|document| document.name == selected.name)
                    });
                self.history.add(&filter);
                level.documents = Some(documents);
                level.filter = filter;
                match kept {
                    Some(position) => level.selected = position,
                    None => {
                        level.selected = 0;
                        level.tree = Tree::default();
                    }
                }
                let selected = level
                    .documents
                    .as_ref()
                    .and_then(|documents| documents.get(level.selected));
                if let Some(document) = selected {
                    level.tree.keep_cursor_in(document);
                }
            }
            Err(error) => {
                if filter != level.filter {
                    self.filter_editor
                        .get_or_insert_with(|| FilterEditor::from(&filter));
                }
                level.load = Load::Failed { error, filter };
            }
        }
    }

    /// What Ctrl-G shows the whole error of: while the filter is in the editor, its failed query;
    /// else a failed listing, else a failed query.
    pub fn failure(&self) -> Option<Failure> {
        let level = &self.current;
        let query_failed = matches!(level.load, Load::Failed { .. });
        let listing_failed = matches!(level.tree.listing(), Listing::Failed(_));
        match (self.filter_editor.is_some(), query_failed, listing_failed) {
            (true, true, _) | (false, true, false) => Some(Failure::Query),
            (false, _, true) => Some(Failure::Listing),
            (true, false, _) | (false, false, false) => None,
        }
    }

    /// The error of `failure`, while the current level still has it.
    pub fn error(&self, failure: Failure) -> Option<&FsqError> {
        let level = &self.current;
        match (failure, &level.load, level.tree.listing()) {
            (Failure::Query, Load::Failed { error, .. }, _)
            | (Failure::Listing, _, Listing::Failed(error)) => Some(error),
            _ => None,
        }
    }

    pub fn mode(&self) -> Mode {
        match (&self.popup, &self.filter_editor) {
            (Some(_), _) => Mode::Popup,
            (None, Some(_)) => Mode::FilterEditor,
            (None, None) => Mode::Panes,
        }
    }

    pub fn popup(&self) -> Option<&Popup> {
        self.popup.as_ref()
    }

    pub fn level(&self) -> &Level {
        &self.current
    }

    /// Whether the current level waits for its documents or its subcollections.
    pub fn is_waiting(&self) -> bool {
        matches!(self.current.load, Load::Pending { .. }) || self.current.tree.is_listing()
    }

    pub fn focus(&self) -> Pane {
        self.focus
    }

    pub fn filter_editor(&self) -> Option<&FilterEditor> {
        self.filter_editor.as_ref()
    }

    pub fn has_parent(&self) -> bool {
        !self.parents.is_empty()
    }

    pub fn documents_path(&self) -> &str {
        &self.documents_path
    }

    /// The filters this browser and the ones before it ran, for the next browser.
    pub fn into_history(self) -> FilterHistory {
        self.history
    }

    /// The row under the tree cursor and its document, while the tree has the focus.
    pub fn tree_cursor(&self) -> Option<(&FirestoreDocument, Row<'_>)> {
        let level = &self.current;
        let document = level
            .selected_document()
            .filter(|_| self.focus == Pane::Tree)?;
        Some((document, level.tree.cursor_row(document)?))
    }

    /// The filter `f` opens the editor with: the documents whose field at the tree cursor holds
    /// the same leaf value.
    pub fn filter_on_cursor(&self) -> Option<FilterTail> {
        match self.tree_cursor()? {
            (
                _,
                Row::Field {
                    path,
                    value,
                    fold: Fold::Leaf,
                },
            ) => FilterTail::matching(&path, value),
            _ => None,
        }
    }

    /// What `y` or `Y` puts on the clipboard: the value under the tree cursor or in the value
    /// popup, or its path as the bottom border of the tree shows it.
    fn copy_text(&self, copied: Copied) -> Option<String> {
        if let Some(popup) = &self.popup {
            return match (popup, copied) {
                (Popup::Value { value, .. }, Copied::Value) => Some(self.clipboard_value(value)),
                (Popup::Value { path, .. }, Copied::Path) => {
                    Some(FieldPath::from(path).to_string())
                }
                (Popup::Error(_) | Popup::Help, _) => None,
            };
        }
        match (copied, self.tree_cursor()?) {
            (Copied::Value, (_, Row::Field { value, .. })) => Some(self.clipboard_value(value)),
            (Copied::Value, _) => None,
            (Copied::Path, (document, row)) => Some(self.row_path(&row, document)),
        }
    }

    /// `value` as it pastes: a string as its text, and any other value as compact JSON in the
    /// tagged encoding `fsq export` writes, which reads back to the same Firestore type.
    fn clipboard_value(&self, value: &Value) -> String {
        if let Some(ValueType::StringValue(text)) = &value.value_type {
            return text.clone();
        }
        FirestoreDb::serialize_value_to_json(
            &self.documents_path,
            value,
            FirestoreJsonEncoding::Tagged,
        )
        .expect("Firestore returns no expression value, the one kind the tagged encoding refuses")
        .to_string()
    }

    /// Where `row` of the tree of `document` points: a field path as SQL reads it, a collection
    /// path, and for the group of subcollections, the path of the document they are under.
    pub fn row_path(&self, row: &Row<'_>, document: &FirestoreDocument) -> String {
        let document_path =
            FirestoreDb::relative_document_name(self.documents_path(), &document.name);
        match row {
            Row::Field { path, .. } => FieldPath::from(path).to_string(),
            Row::Subcollections { .. } => document_path.to_string(),
            Row::Subcollection(collection_id) => {
                CollectionPath::in_document(document_path, (*collection_id).clone()).to_string()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::browse::tree::Row;
    use crate::render::fixtures::{draft, dune, string, DOCUMENTS_PATH};
    use firestore::gcloud_sdk::google::firestore::v1::value::ValueType;
    use firestore::gcloud_sdk::google::firestore::v1::ArrayValue;

    fn books(filter: &str) -> DocumentsQuery {
        DocumentsQuery {
            request: RequestId::default(),
            collection: "books".parse().expect("a collection path"),
            filter: FilterTail::from(filter),
        }
    }

    fn orders() -> FirestoreCollectionId {
        "orders".parse().expect("a collection ID")
    }

    fn query_of(effect: Option<Effect>) -> DocumentsQuery {
        match effect {
            Some(Effect::Query(query)) => query,
            other => panic!("no query: {other:?}"),
        }
    }

    /// A browser of `books` that has read `draft` and `dune`, in that order.
    fn browser() -> Browser {
        browser_of(vec![draft(), dune()])
    }

    /// A browser of `books` that has read `documents`.
    fn browser_of(documents: Vec<FirestoreDocument>) -> Browser {
        let target = BrowseTarget {
            collection: "books".parse().expect("a collection path"),
            filter: FilterTail::default(),
        };
        let (mut browser, first) = Browser::open(target, DOCUMENTS_PATH, FilterHistory::default());
        let Effect::Query(query) = first else {
            panic!("no query: {first:?}")
        };
        browser.update(Message::DocumentsLoaded {
            request: query.request,
            outcome: Ok(documents),
        });
        browser
    }

    /// Opens the filter editor, types `text` into it and runs it.
    fn run_filter(browser: &mut Browser, text: &str) -> DocumentsQuery {
        browser.update(Message::EditFilter);
        for typed in text.chars() {
            browser.update(Message::Edit(Edit::Insert(typed)));
        }
        query_of(browser.update(Message::RunFilter))
    }

    fn edited_text(browser: &Browser) -> Option<&str> {
        browser.filter_editor().map(FilterEditor::text)
    }

    fn selected_id(browser: &Browser) -> Option<&str> {
        browser
            .level()
            .selected_document()
            .and_then(|document| document.name.rsplit('/').next())
    }

    /// Selects `dune`, lists its subcollections as `orders`, and opens `orders`.
    fn open_orders(browser: &mut Browser) -> Option<Effect> {
        browser.update(Message::Move(Movement::Down(1)));
        browser.update(Message::Expand);
        browser.update(Message::Move(Movement::Last));
        let Some(Effect::ListSubcollections { request, .. }) = browser.update(Message::Expand)
        else {
            panic!("no listing")
        };
        browser.update(Message::SubcollectionsListed {
            request,
            outcome: Ok(vec![orders()]),
        });
        browser.update(Message::Move(Movement::Last));
        browser.update(Message::Expand)
    }

    #[test]
    fn a_filter_without_a_limit_reads_the_first_hundred_documents() {
        assert_eq!(books("").sql(), "SELECT * FROM `books`\nLIMIT 100");
        assert_eq!(
            books("WHERE note = 'no limit' -- or offset").sql(),
            "SELECT * FROM `books`\nWHERE note = 'no limit' -- or offset\nLIMIT 100"
        );
        assert_eq!(
            books("WHERE genre = 'sf' ORDER BY title limit 5").sql(),
            "SELECT * FROM `books`\nWHERE genre = 'sf' ORDER BY title limit 5"
        );
        assert_eq!(
            books("ORDER BY title OFFSET 10").sql(),
            "SELECT * FROM `books`\nORDER BY title OFFSET 10\nLIMIT 100"
        );
    }

    #[test]
    fn a_field_named_limit_is_not_a_limit_of_the_query() {
        for filter in ["WHERE quota.limit > 5", "WHERE limit > 5", "ORDER BY limit"] {
            assert_eq!(
                books(filter).sql(),
                format!("SELECT * FROM `books`\n{filter}\nLIMIT 100")
            );
        }
    }

    #[test]
    fn a_filter_ends_without_the_semicolon_that_ends_the_statement() {
        assert_eq!(
            books("WHERE genre = 'sf';").sql(),
            "SELECT * FROM `books`\nWHERE genre = 'sf'\nLIMIT 100"
        );
        assert_eq!(
            books("WHERE genre = 'sf' LIMIT 5 ;").sql(),
            "SELECT * FROM `books`\nWHERE genre = 'sf' LIMIT 5"
        );
    }

    #[test]
    fn a_limit_inside_parentheses_is_not_the_limit_of_the_query() {
        let filter = "WHERE EXISTS(SELECT * FROM subcollection('orders') LIMIT 1)";

        assert_eq!(
            books(filter).sql(),
            format!("SELECT * FROM `books`\n{filter}\nLIMIT 100")
        );
    }

    #[test]
    fn a_subcollection_is_read_by_its_full_quoted_path() {
        let mut browser = browser();

        let query = query_of(open_orders(&mut browser));

        assert_eq!(query.sql(), "SELECT * FROM `books/dune/orders`\nLIMIT 100");
        assert_eq!(browser.level().path(), &query.collection);
        assert_eq!(browser.focus(), Pane::Documents);
    }

    #[test]
    fn going_back_from_a_subcollection_returns_to_the_document_it_was_opened_from() {
        let mut browser = browser();
        open_orders(&mut browser);

        let back = browser.update(Message::Back);

        assert_eq!(back, Some(Effect::Cancel));
        assert_eq!(browser.level().path().to_string(), "books");
        assert_eq!(selected_id(&browser), Some("dune"));
        assert!(!browser.has_parent());
        assert_eq!(browser.update(Message::Back), None);
    }

    #[test]
    fn the_effective_limit_is_the_count_of_the_filter_else_the_default() {
        assert_eq!(FilterTail::default().effective_limit(), Some(100));
        assert_eq!(
            FilterTail::from("WHERE quota.limit > 5").effective_limit(),
            Some(100)
        );
        assert_eq!(
            FilterTail::from("ORDER BY title LIMIT 5").effective_limit(),
            Some(5)
        );
        assert_eq!(
            FilterTail::from("ORDER BY title LIMIT 2 + 3").effective_limit(),
            None
        );
    }

    #[test]
    fn a_failed_query_keeps_the_documents_read_before_it() {
        let mut browser = browser();

        let query = run_filter(&mut browser, "WHERE");
        browser.update(Message::DocumentsLoaded {
            request: query.request,
            outcome: Err(FsqError::NoProject),
        });

        assert_eq!(query.filter, FilterTail::from("WHERE"));
        assert!(matches!(
            browser.level().load(),
            Load::Failed {
                error: FsqError::NoProject,
                ..
            }
        ));
        assert_eq!(browser.level().documents().map(<[_]>::len), Some(2));
    }

    #[test]
    fn a_failed_filter_reopens_in_the_editor() {
        let mut browser = browser();

        let query = run_filter(&mut browser, "WHERE pagez > 100");
        browser.update(Message::DocumentsLoaded {
            request: query.request,
            outcome: Err(FsqError::NoProject),
        });

        assert_eq!(edited_text(&browser), Some("WHERE pagez > 100"));
    }

    #[test]
    fn a_failure_leaves_a_filter_being_typed_as_it_is() {
        let mut browser = browser();
        let query = run_filter(&mut browser, "WHERE pagez > 100");
        browser.update(Message::EditFilter);
        browser.update(Message::Edit(Edit::DeleteWordBefore));

        browser.update(Message::DocumentsLoaded {
            request: query.request,
            outcome: Err(FsqError::NoProject),
        });

        assert_eq!(edited_text(&browser), Some("WHERE pagez > "));
    }

    #[test]
    fn the_filter_of_the_documents_shown_changes_only_when_the_new_one_has_answered() {
        let mut browser = browser();

        let failing = run_filter(&mut browser, "WHERE genre = 'fantasy'");
        assert_eq!(browser.level().filter(), &FilterTail::default());
        browser.update(Message::DocumentsLoaded {
            request: failing.request,
            outcome: Err(FsqError::NoProject),
        });
        assert_eq!(browser.level().filter(), &FilterTail::default());

        browser.update(Message::CancelFilter);
        browser.update(Message::EditFilter);
        assert_eq!(edited_text(&browser), Some("WHERE genre = 'fantasy'"));
        let query = query_of(browser.update(Message::RunFilter));
        browser.update(Message::DocumentsLoaded {
            request: query.request,
            outcome: Ok(vec![dune()]),
        });
        assert_eq!(
            browser.level().filter(),
            &FilterTail::from("WHERE genre = 'fantasy'")
        );
    }

    #[test]
    fn an_answer_the_browser_stopped_waiting_for_changes_nothing() {
        let mut browser = browser();
        browser.update(Message::EditFilter);
        let query = query_of(browser.update(Message::RunFilter));

        assert_eq!(browser.update(Message::Interrupt), Some(Effect::Cancel));
        browser.update(Message::DocumentsLoaded {
            request: query.request,
            outcome: Ok(Vec::new()),
        });

        assert!(matches!(browser.level().load(), Load::Stopped { .. }));
        assert_eq!(browser.level().documents().map(<[_]>::len), Some(2));
        assert_eq!(browser.update(Message::Interrupt), Some(Effect::Quit));
    }

    #[test]
    fn a_stopped_filter_is_not_a_failure_and_reopens_with_slash() {
        let mut browser = browser();
        run_filter(&mut browser, "WHERE pages > 100");

        browser.update(Message::Interrupt);

        assert!(matches!(browser.level().load(), Load::Stopped { .. }));
        assert_eq!(browser.filter_editor().map(FilterEditor::text), None);
        browser.update(Message::EditFilter);
        assert_eq!(edited_text(&browser), Some("WHERE pages > 100"));
    }

    #[test]
    fn ctrl_c_stops_a_load_then_closes_the_filter_editor_then_quits() {
        let mut browser = browser();
        browser.update(Message::EditFilter);
        query_of(browser.update(Message::RunFilter));
        browser.update(Message::EditFilter);

        assert_eq!(browser.update(Message::Interrupt), Some(Effect::Cancel));
        assert!(browser.filter_editor().is_some());
        assert_eq!(browser.update(Message::Interrupt), None);
        assert!(browser.filter_editor().is_none());
        assert_eq!(browser.update(Message::Interrupt), Some(Effect::Quit));
    }

    #[test]
    fn ctrl_c_closes_the_popup_before_the_filter_editor_under_it() {
        let mut browser = browser();
        let query = run_filter(&mut browser, "WHERE pagez > 100");
        browser.update(Message::DocumentsLoaded {
            request: query.request,
            outcome: Err(FsqError::NoProject),
        });

        browser.update(Message::ShowError);
        assert!(matches!(
            browser.popup(),
            Some(Popup::Error(Failure::Query))
        ));
        assert_eq!(browser.mode(), Mode::Popup);
        browser.update(Message::Paste(" AND".to_string()));

        assert_eq!(browser.update(Message::Interrupt), None);
        assert!(browser.popup().is_none());
        assert_eq!(edited_text(&browser), Some("WHERE pagez > 100"));
        assert_eq!(browser.update(Message::Interrupt), None);
        assert!(browser.filter_editor().is_none());
        assert_eq!(browser.update(Message::Interrupt), Some(Effect::Quit));
    }

    #[test]
    fn a_paste_goes_into_the_filter_editor_as_one_line() {
        let mut browser = browser();
        assert_eq!(browser.update(Message::Paste("q".to_string())), None);
        assert!(browser.filter_editor().is_none());

        browser.update(Message::EditFilter);
        browser.update(Message::Paste(
            "WHERE genre = 'sf'\nAND pages > 100\r\nORDER BY title\rLIMIT 5".to_string(),
        ));

        assert_eq!(
            edited_text(&browser),
            Some("WHERE genre = 'sf' AND pages > 100 ORDER BY title LIMIT 5")
        );
    }

    #[test]
    fn the_cursor_stops_at_the_first_and_the_last_document() {
        let mut browser = browser();

        browser.update(Message::Move(Movement::Up(1)));
        assert_eq!(selected_id(&browser), Some("draft"));
        browser.update(Message::Move(Movement::Down(10)));
        assert_eq!(selected_id(&browser), Some("dune"));
        browser.update(Message::Move(Movement::Down(1)));
        assert_eq!(selected_id(&browser), Some("dune"));
        browser.update(Message::Move(Movement::First));
        assert_eq!(selected_id(&browser), Some("draft"));
    }

    fn path(names: &[&str]) -> NodePath {
        names
            .iter()
            .fold(NodePath::default(), |path, name| path.field(name))
    }

    /// The path of the field under the tree cursor of the selected document.
    fn cursor_path(browser: &Browser) -> Option<NodePath> {
        let level = browser.level();
        match level.tree().cursor_row(level.selected_document()?)? {
            Row::Field { path, .. } => Some(path),
            _ => None,
        }
    }

    fn has_open_map(browser: &Browser) -> bool {
        let level = browser.level();
        let document = level.selected_document().expect("a document");
        level
            .tree()
            .rows(document)
            .iter()
            .any(|row| row.depth() > 0 && matches!(row, Row::Field { .. }))
    }

    /// `dune` under another ID.
    fn dune_messiah() -> FirestoreDocument {
        let mut sequel = dune();
        sequel.name = sequel.name.replace("dune", "dune-messiah");
        sequel
    }

    /// Opens `address` in the tree of the first document, puts the cursor on its city, and gives
    /// the focus back to the documents.
    fn on_city(browser: &mut Browser) {
        browser.update(Message::Expand);
        browser.update(Message::Expand);
        browser.update(Message::Move(Movement::Down(1)));
        browser.update(Message::SwitchPane);
    }

    #[test]
    fn another_document_keeps_the_open_maps_and_the_cursor_on_the_same_path() {
        let mut browser = browser_of(vec![dune(), dune_messiah()]);
        on_city(&mut browser);
        assert_eq!(cursor_path(&browser), Some(path(&["address", "city"])));

        browser.update(Message::Move(Movement::Down(1)));

        assert_eq!(selected_id(&browser), Some("dune-messiah"));
        assert!(has_open_map(&browser));
        assert_eq!(cursor_path(&browser), Some(path(&["address", "city"])));
    }

    #[test]
    fn the_cursor_falls_back_to_the_nearest_path_the_next_document_has() {
        let mut one_tag = dune();
        one_tag.name = one_tag.name.replace("dune", "one-tag");
        let tags = Value {
            value_type: Some(ValueType::ArrayValue(ArrayValue {
                values: vec![string("sf")],
            })),
        };
        one_tag.fields.insert("tags".to_string(), tags);
        let mut browser = browser_of(vec![dune(), one_tag]);
        browser.update(Message::Expand);
        browser.update(Message::Move(Movement::Down(10)));
        browser.update(Message::Expand);
        browser.update(Message::Move(Movement::Down(2)));
        assert_eq!(cursor_path(&browser).map(|at| at.depth()), Some(1));
        browser.update(Message::SwitchPane);

        browser.update(Message::Move(Movement::Down(1)));

        assert_eq!(cursor_path(&browser), Some(path(&["tags"])));
        assert_eq!(browser.level().tree().cursor(), 10);
    }

    #[test]
    fn a_new_query_starts_with_the_tree_collapsed() {
        let mut browser = browser_of(vec![dune(), dune_messiah()]);
        on_city(&mut browser);

        let query = run_filter(&mut browser, "WHERE pages > 100");
        browser.update(Message::DocumentsLoaded {
            request: query.request,
            outcome: Ok(vec![dune(), dune_messiah()]),
        });

        assert!(!has_open_map(&browser));
        assert_eq!(browser.level().tree().cursor(), 0);
    }

    /// Runs `filter` in place of the one in the editor, and answers it with `outcome`.
    fn answered(browser: &mut Browser, filter: &str, outcome: FsqResult<Vec<FirestoreDocument>>) {
        browser.update(Message::EditFilter);
        browser.update(Message::Edit(Edit::DeleteToStart));
        browser.update(Message::Paste(filter.to_string()));
        let query = query_of(browser.update(Message::RunFilter));
        browser.update(Message::DocumentsLoaded {
            request: query.request,
            outcome,
        });
    }

    /// The text of the editor after each of `recalls`.
    fn recalled(browser: &mut Browser, recalls: &[Recall]) -> Vec<String> {
        recalls
            .iter()
            .map(|recall| {
                browser.update(Message::Recall(*recall));
                edited_text(browser).unwrap_or_default().to_string()
            })
            .collect()
    }

    #[test]
    fn up_and_down_recall_the_filters_that_ran_most_recent_first_and_keep_the_draft() {
        let mut browser = browser();
        for filter in [
            "WHERE pages > 100",
            "WHERE genre = 'sf'",
            "WHERE pages > 100",
        ] {
            answered(&mut browser, filter, Ok(vec![dune()]));
        }
        browser.update(Message::EditFilter);
        browser.update(Message::Edit(Edit::DeleteToStart));
        browser.update(Message::Paste("ORDER BY".to_string()));

        let texts = recalled(
            &mut browser,
            &[
                Recall::Older,
                Recall::Older,
                Recall::Older,
                Recall::Newer,
                Recall::Newer,
                Recall::Newer,
            ],
        );

        assert_eq!(
            texts,
            [
                "WHERE pages > 100",
                "WHERE genre = 'sf'",
                "WHERE genre = 'sf'",
                "WHERE pages > 100",
                "ORDER BY",
                "ORDER BY",
            ]
        );
    }

    #[test]
    fn a_filter_that_failed_is_not_recalled() {
        let mut browser = browser();
        answered(&mut browser, "WHERE pages > 100", Ok(vec![dune()]));
        answered(&mut browser, "WHERE pagez > 100", Err(FsqError::NoProject));

        let texts = recalled(&mut browser, &[Recall::Older, Recall::Older, Recall::Newer]);

        assert_eq!(
            texts,
            [
                "WHERE pages > 100",
                "WHERE pages > 100",
                "WHERE pagez > 100"
            ]
        );
    }

    #[test]
    fn the_next_browser_recalls_the_filters_of_the_one_before() {
        let mut first = browser();
        answered(&mut first, "WHERE pages > 100", Ok(vec![dune()]));
        let target = BrowseTarget {
            collection: "books".parse().expect("a collection path"),
            filter: FilterTail::default(),
        };

        let (mut next, _) = Browser::open(target, DOCUMENTS_PATH, first.into_history());
        next.update(Message::EditFilter);

        assert_eq!(recalled(&mut next, &[Recall::Older]), ["WHERE pages > 100"]);
    }

    #[test]
    fn the_order_key_is_the_first_field_path_the_documents_are_ordered_by() {
        let order_key = |filter: &str| FilterTail::from(filter).order_key().cloned();

        assert_eq!(
            order_key("WHERE pages > 100 ORDER BY rating DESC, title LIMIT 5"),
            Some(path(&["rating"]))
        );
        assert_eq!(
            order_key("ORDER BY address.city"),
            Some(path(&["address", "city"]))
        );
        assert_eq!(
            order_key("ORDER BY `unit-price`"),
            Some(path(&["unit-price"]))
        );
        for unordered in [
            "",
            "WHERE note = 'ORDER BY rating'",
            "ORDER BY LENGTH(title)",
            "ORDER BY",
        ] {
            assert_eq!(order_key(unordered), None, "{unordered}");
        }
    }
}
