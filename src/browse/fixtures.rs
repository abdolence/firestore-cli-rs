//! Browsers, documents and paths for the browser's tests.

use super::editor::FilterHistory;
use super::state::{Browser, DocumentsQuery, Effect, Message};
use super::tree::NodePath;
use super::{BrowseTarget, FilterTail};
use crate::render::fixtures::{document, string, DOCUMENTS_PATH};
use firestore::FirestoreDocument;

/// A browser of `collection` read with `filter`, before its first documents, and the query that
/// reads them.
pub fn opened(collection: &str, filter: &str) -> (Browser, DocumentsQuery) {
    let target = BrowseTarget {
        collection: collection.parse().expect("a collection path"),
        filter: FilterTail::from(filter),
    };
    match Browser::open(target, DOCUMENTS_PATH, FilterHistory::default()) {
        (browser, Effect::Query(query)) => (browser, query),
        (_, other) => panic!("no query: {other:?}"),
    }
}

/// A browser of `collection` read with `filter` that has read `documents`.
pub fn browser_of(collection: &str, filter: &str, documents: Vec<FirestoreDocument>) -> Browser {
    let (mut browser, query) = opened(collection, filter);
    browser.update(Message::DocumentsLoaded {
        request: query.request,
        outcome: Ok(documents),
    });
    browser
}

/// `count` books from `book-001` on, each with a title.
pub fn books(count: usize) -> Vec<FirestoreDocument> {
    (1..=count)
        .map(|number| document(&format!("book-{number:03}"), vec![("title", string("t"))]))
        .collect()
}

/// The path of the field `names` leads to, each name a field of the map before it.
pub fn field_path(names: &[&str]) -> NodePath {
    names
        .iter()
        .fold(NodePath::default(), |path, name| path.field(name))
}
