use crate::describe::CollectionPath;
use crate::errors::FsqResult;
use firestore::{FirestoreCollectionId, FirestoreDb, FirestoreDocument, FirestoreResult};
use futures::stream::BoxStream;
use futures::{StreamExt, TryStreamExt};

pub type DocumentStream<'b> = BoxStream<'b, FirestoreResult<FirestoreDocument>>;

/// The listings that walking down a tree of collections is made of: the documents of a
/// collection, and the collections under a document.
pub struct Walk<'a> {
    db: &'a FirestoreDb,
}

impl<'a> Walk<'a> {
    pub fn new(db: &'a FirestoreDb) -> Self {
        Self { db }
    }

    /// The documents of `path`. ListDocuments skips a document that does not exist, and with it
    /// every subcollection below it; `show_missing` lists such a document too. Firestore
    /// refuses `show_missing` together with a filter or an order, which this never adds.
    pub async fn documents(
        &self,
        path: &CollectionPath,
        show_missing: bool,
        page_size: Option<usize>,
    ) -> FsqResult<DocumentStream<'a>> {
        let listing = self.db.fluent().list().from(path.collection_id());
        let listing = match path.parent_name(self.db.get_documents_path()) {
            Some(parent) => listing.parent(parent),
            None => listing,
        };
        let listing = if show_missing {
            listing.show_missing()
        } else {
            listing
        };
        let listing = match page_size {
            Some(page_size) => listing.page_size(page_size),
            None => listing,
        };
        Ok(listing.stream_all_with_errors().await?)
    }

    /// The names of the first `limit` documents, existing or not, taken in turn from each of
    /// `collections` until `limit` are found. Reads no more than `limit` documents in all.
    pub async fn first_document_names(
        &self,
        collections: &[CollectionPath],
        limit: usize,
    ) -> FsqResult<Vec<String>> {
        let mut names = Vec::new();
        for collection in collections {
            let remaining = limit - names.len();
            if remaining == 0 {
                break;
            }
            let mut documents = self
                .documents(collection, true, Some(remaining))
                .await?
                .take(remaining);
            while let Some(document) = documents.try_next().await? {
                names.push(document.name);
            }
        }
        Ok(names)
    }

    /// The IDs of the collections under the document `parent`, a full resource name, or under
    /// the root when there is none.
    pub async fn collection_ids(
        &self,
        parent: Option<&str>,
    ) -> FsqResult<Vec<FirestoreCollectionId>> {
        let listing = self.db.fluent().list().collections();
        let listing = match parent {
            Some(parent) => listing.parent(parent),
            None => listing,
        };
        let mut collection_ids = Vec::new();
        let mut stream = listing.stream_all_with_errors().await?;
        while let Some(collection_id) = stream.try_next().await? {
            collection_ids.push(collection_id.parse()?);
        }
        Ok(collection_ids)
    }
}
