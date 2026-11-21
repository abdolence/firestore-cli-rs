use super::tree::{Expansion, Tree};
use super::BrowseTarget;
use crate::describe::CollectionPath;
use crate::errors::{FsqError, FsqResult};
use firestore::{FirestoreCollectionId, FirestoreDb, FirestoreDocument};
use sqlparser::ast::{LimitClause, Statement};
use sqlparser::dialect::BigQueryDialect;
use sqlparser::parser::Parser;
use std::fmt::{Display, Formatter};

/// How many documents a level shows when its filter has no `LIMIT` of its own.
pub const BROWSE_DOCUMENT_LIMIT: usize = 100;

/// The end of a `SELECT` after its `FROM`, such as `WHERE …`, `ORDER BY …` and `LIMIT …`, as
/// typed, without the `;` that ends a statement. An empty one reads the collection as it is.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FilterTail(String);

impl FilterTail {
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Whether the query this filter ends has a `LIMIT` count, as the parser the firestore crate
    /// translates with reads it. A field named `limit`, the `LIMIT` of a subquery and an `OFFSET`
    /// alone bound nothing, and a filter that does not parse has no limit.
    pub fn has_limit(&self) -> bool {
        let statement = format!("SELECT * FROM t\n{}", self.0);
        let Ok(statements) = Parser::parse_sql(&BigQueryDialect {}, &statement) else {
            return false;
        };
        let [Statement::Query(query)] = statements.as_slice() else {
            return false;
        };
        match &query.limit_clause {
            Some(LimitClause::LimitOffset { limit, .. }) => limit.is_some(),
            Some(LimitClause::OffsetCommaLimit { .. }) => true,
            None => false,
        }
    }
}

impl From<&str> for FilterTail {
    fn from(text: &str) -> Self {
        let text = text.trim();
        Self(
            text.strip_suffix(';')
                .unwrap_or(text)
                .trim_end()
                .to_string(),
        )
    }
}

impl Display for FilterTail {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
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
            sql.push_str(&self.filter.0);
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
    documents: Vec<FirestoreDocument>,
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
            documents: Vec::new(),
            selected: 0,
            tree: Tree::default(),
            load: pending,
        }
    }

    /// Stops waiting for the pending query, if there is one: the documents before it stay, and
    /// the query counts as interrupted. `false` when no query was pending.
    fn stop_waiting(&mut self) -> bool {
        let Load::Pending { filter, .. } = std::mem::replace(&mut self.load, Load::Done) else {
            return false;
        };
        self.load = Load::Failed {
            error: FsqError::Interrupted,
            filter,
        };
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
            Load::Pending { filter, .. } | Load::Failed { filter, .. } => filter,
            Load::Done => &self.filter,
        }
    }

    pub fn path(&self) -> &CollectionPath {
        &self.path
    }

    pub fn filter(&self) -> &FilterTail {
        &self.filter
    }

    pub fn documents(&self) -> &[FirestoreDocument] {
        &self.documents
    }

    pub fn selected(&self) -> usize {
        self.selected
    }

    pub fn selected_document(&self) -> Option<&FirestoreDocument> {
        self.documents.get(self.selected)
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

/// A move of the cursor in the focused pane, by rows.
#[derive(Debug, Clone, Copy)]
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

#[derive(Debug)]
pub enum Message {
    Move(Movement),
    /// Expands the row under the cursor, opens a subcollection, or in the documents pane moves
    /// to the tree.
    Expand,
    /// Collapses the row under the cursor or moves to the row it is in, and from a top-level row
    /// back to the documents.
    Collapse,
    SwitchPane,
    /// Goes back up to the collection the current one was opened from.
    Back,
    EditFilter,
    FilterInput(char),
    FilterDelete,
    /// Text pasted at once, which goes into the filter being edited as one line. With no filter
    /// being edited it is dropped.
    Paste(String),
    RunFilter,
    CancelFilter,
    /// Ctrl-C, which ends the innermost thing going on: stops waiting for Firestore, else closes
    /// the filter editor, else quits.
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
    /// The text of the filter being edited, while it is.
    filter_editor: Option<String>,
    documents_path: String,
    last_request: RequestId,
}

impl Browser {
    /// A browser of `target`, and the query that reads its first documents.
    pub fn open(target: BrowseTarget, documents_path: &str) -> (Self, Effect) {
        let request = RequestId::default().following();
        let current = Level::loading(target.collection, target.filter, request);
        let query = current.query(request, current.filter.clone());
        let browser = Self {
            current,
            parents: Vec::new(),
            focus: Pane::Documents,
            filter_editor: None,
            documents_path: documents_path.to_string(),
            last_request: request,
        };
        (browser, Effect::Query(query))
    }

    pub fn update(&mut self, message: Message) -> Option<Effect> {
        match message {
            Message::Move(movement) => self.move_cursor(movement),
            Message::Expand => return self.expand(),
            Message::Collapse => self.collapse(),
            Message::SwitchPane => {
                self.focus = match self.focus {
                    Pane::Documents => Pane::Tree,
                    Pane::Tree => Pane::Documents,
                }
            }
            Message::Back => return self.back(),
            Message::EditFilter => {
                self.filter_editor = Some(self.current.filter_to_edit().to_string())
            }
            Message::FilterInput(typed) => {
                if let Some(text) = &mut self.filter_editor {
                    text.push(typed);
                }
            }
            Message::FilterDelete => {
                if let Some(text) = &mut self.filter_editor {
                    text.pop();
                }
            }
            Message::Paste(pasted) => {
                if let Some(text) = &mut self.filter_editor {
                    text.push_str(&pasted.replace("\r\n", " ").replace(['\r', '\n'], " "));
                }
            }
            Message::RunFilter => return self.run_filter(),
            Message::CancelFilter => self.filter_editor = None,
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
                let selected = movement.applied_to(level.selected, level.documents.len());
                if selected != level.selected {
                    level.selected = selected;
                    level.tree = Tree::default();
                }
            }
            Pane::Tree => {
                if let Some(document) = level.documents.get(level.selected) {
                    let rows = level.tree.rows(document).len();
                    let row = movement.applied_to(level.tree.cursor(), rows);
                    level.tree.select(row);
                }
            }
        }
    }

    fn expand(&mut self) -> Option<Effect> {
        let level = &mut self.current;
        let document = level.documents.get(level.selected)?;
        if self.focus == Pane::Documents {
            self.focus = Pane::Tree;
            return None;
        }
        match level.tree.expand(document)? {
            Expansion::ListSubcollections => {
                let document = document.name.clone();
                let request = self.next_request();
                self.current.tree.listing_started(request);
                Some(Effect::ListSubcollections { request, document })
            }
            Expansion::OpenSubcollection(collection_id) => {
                let parent =
                    FirestoreDb::relative_document_name(&self.documents_path, &document.name);
                let path = CollectionPath::in_document(parent, collection_id);
                Some(self.go_down(path))
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

    fn collapse(&mut self) {
        let level = &mut self.current;
        let Some(document) = level.documents.get(level.selected) else {
            return;
        };
        if self.focus == Pane::Tree && !level.tree.collapse(document) {
            self.focus = Pane::Documents;
        }
    }

    /// Returns to the level the current one was opened from, as it was left. Whatever the
    /// current level still waits for is of no use any more.
    fn back(&mut self) -> Option<Effect> {
        self.current = self.parents.pop()?;
        self.focus = Pane::Tree;
        Some(Effect::Cancel)
    }

    fn run_filter(&mut self) -> Option<Effect> {
        let text = self.filter_editor.take()?;
        let request = self.next_request();
        let level = &mut self.current;
        let filter = FilterTail::from(text.as_str());
        let query = level.query(request, filter.clone());
        level.load = Load::Pending { request, filter };
        Some(Effect::Query(query))
    }

    fn interrupt(&mut self) -> Option<Effect> {
        let level = &mut self.current;
        let query_stopped = level.stop_waiting();
        if level.tree.cancel_listing() || query_stopped {
            return Some(Effect::Cancel);
        }
        match self.filter_editor.take() {
            Some(_) => None,
            None => Some(Effect::Quit),
        }
    }

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
                level.documents = documents;
                level.filter = filter;
                level.selected = 0;
                level.tree = Tree::default();
            }
            Err(error) => level.load = Load::Failed { error, filter },
        }
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

    pub fn filter_editor(&self) -> Option<&str> {
        self.filter_editor.as_deref()
    }

    pub fn has_parent(&self) -> bool {
        !self.parents.is_empty()
    }

    pub fn documents_path(&self) -> &str {
        &self.documents_path
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::browse::tree::Row;
    use crate::render::fixtures::{draft, dune, DOCUMENTS_PATH};

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
        let target = BrowseTarget {
            collection: "books".parse().expect("a collection path"),
            filter: FilterTail::default(),
        };
        let (mut browser, first) = Browser::open(target, DOCUMENTS_PATH);
        let Effect::Query(query) = first else {
            panic!("no query: {first:?}")
        };
        browser.update(Message::DocumentsLoaded {
            request: query.request,
            outcome: Ok(vec![draft(), dune()]),
        });
        browser
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
    fn a_failed_query_keeps_the_documents_read_before_it() {
        let mut browser = browser();
        browser.update(Message::EditFilter);
        for typed in "WHERE".chars() {
            browser.update(Message::FilterInput(typed));
        }

        let query = query_of(browser.update(Message::RunFilter));
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
        assert_eq!(browser.level().documents().len(), 2);
    }

    #[test]
    fn the_filter_of_the_documents_shown_changes_only_when_the_new_one_has_answered() {
        let mut browser = browser();
        let type_filter = |browser: &mut Browser, text: &str| {
            browser.update(Message::EditFilter);
            for typed in text.chars() {
                browser.update(Message::FilterInput(typed));
            }
            query_of(browser.update(Message::RunFilter))
        };

        let failing = type_filter(&mut browser, "WHERE genre = 'fantasy'");
        assert_eq!(browser.level().filter(), &FilterTail::default());
        browser.update(Message::DocumentsLoaded {
            request: failing.request,
            outcome: Err(FsqError::NoProject),
        });
        assert_eq!(browser.level().filter(), &FilterTail::default());

        browser.update(Message::EditFilter);
        assert_eq!(browser.filter_editor(), Some("WHERE genre = 'fantasy'"));
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

        assert!(matches!(
            browser.level().load(),
            Load::Failed {
                error: FsqError::Interrupted,
                ..
            }
        ));
        assert_eq!(browser.level().documents().len(), 2);
        assert_eq!(browser.update(Message::Interrupt), Some(Effect::Quit));
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
        assert_eq!(browser.filter_editor(), None);
        assert_eq!(browser.update(Message::Interrupt), Some(Effect::Quit));
    }

    #[test]
    fn a_paste_goes_into_the_filter_editor_as_one_line() {
        let mut browser = browser();
        assert_eq!(browser.update(Message::Paste("q".to_string())), None);
        assert_eq!(browser.filter_editor(), None);

        browser.update(Message::EditFilter);
        browser.update(Message::Paste(
            "WHERE genre = 'sf'\nAND pages > 100\r\nORDER BY title\rLIMIT 5".to_string(),
        ));

        assert_eq!(
            browser.filter_editor(),
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

    #[test]
    fn another_document_opens_with_its_tree_collapsed() {
        let mut browser = browser();
        browser.update(Message::Move(Movement::Down(1)));
        browser.update(Message::Expand);
        browser.update(Message::Expand);
        let expanded = |browser: &Browser| {
            let level = browser.level();
            let document = level.selected_document().expect("a document");
            level
                .tree()
                .rows(document)
                .iter()
                .any(|row| row.depth() > 0 && matches!(row, Row::Field { .. }))
        };
        assert!(expanded(&browser));

        browser.update(Message::SwitchPane);
        browser.update(Message::Move(Movement::Up(1)));
        browser.update(Message::Move(Movement::Down(1)));

        assert!(!expanded(&browser));
    }
}
