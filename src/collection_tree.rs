use crate::describe::{CollectionPath, SampleSize};
use crate::errors::{FsqError, FsqResult, InvalidTreeDepth};
use crate::render::{Cell, Grid, Sample};
use crate::walk::Walk;
use firestore::{FirestoreCollectionId, FirestoreDb};
use futures::{StreamExt, TryStreamExt};
use std::collections::BTreeMap;
use std::fmt::{Display, Formatter};
use std::num::NonZeroU32;
use std::str::FromStr;

/// How many levels of collections `\l --recursive` lists: 1 is the root collections alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TreeDepth(NonZeroU32);

impl TreeDepth {
    const DEFAULT: NonZeroU32 = NonZeroU32::new(2).unwrap();

    pub fn levels(self) -> u32 {
        self.0.get()
    }
}

impl Default for TreeDepth {
    fn default() -> Self {
        Self(Self::DEFAULT)
    }
}

impl Display for TreeDepth {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl FromStr for TreeDepth {
    type Err = FsqError;

    fn from_str(value: &str) -> FsqResult<Self> {
        value.parse().map(Self).map_err(|source| {
            FsqError::InvalidTreeDepth(InvalidTreeDepth {
                value: value.to_string(),
                source,
            })
        })
    }
}

/// The options of a recursive `\l`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::Args)]
pub struct CollectionTreeTarget {
    #[arg(
        long,
        requires = "recursive",
        default_value_t = TreeDepth::default(),
        help = "How many levels of collections to list; 1 is the root collections alone"
    )]
    pub depth: TreeDepth,

    #[arg(
        long,
        requires = "recursive",
        default_value_t = SampleSize::COLLECTION_TREE_DEFAULT,
        help = "How many documents of each collection to read to find its subcollections"
    )]
    pub sample: SampleSize,
}

/// A collection by its path with `*` for each document ID on the way: `users/*/addresses`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct TreeCollection {
    path: String,
    id: FirestoreCollectionId,
    depth: usize,
    /// How many of the sampled documents of the parent collection have this one, such as
    /// `3 of 20 sampled users`; `None` for a collection under the root, which no sample is taken
    /// to find.
    seen_under: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CollectionTree {
    target: CollectionTreeTarget,
    collections: Vec<TreeCollection>,
}

/// How many ListCollectionIds calls of one sample run at once; each is a round trip, and a
/// sample can be hundreds of documents.
const COLLECTION_ID_CALLS_IN_FLIGHT: usize = 8;

/// A collection by its path with `*`, and every collection of that path whose documents the
/// next level is sampled from.
struct Level {
    path: String,
    id: FirestoreCollectionId,
    instances: Vec<CollectionPath>,
}

/// How many collection paths of one level are sampled at once. Each path costs a listing of its
/// documents and then its calls of [`COLLECTION_ID_CALLS_IN_FLIGHT`] at a time, and Firestore can
/// take seconds to answer each of them.
const LEVELS_IN_FLIGHT: usize = 8;

impl Level {
    /// Samples the first `limit` documents across the collections of this path, and lists the
    /// collections under them as the next `depth`: the rows they make and the levels they open.
    async fn sample(
        self,
        walk: &Walk<'_>,
        documents_path: &str,
        limit: usize,
        depth: usize,
    ) -> FsqResult<(Vec<TreeCollection>, Vec<Level>)> {
        let documents = walk.first_document_names(&self.instances, limit).await?;
        let listings: Vec<_> = futures::stream::iter(&documents)
            .map(|document| walk.collection_ids(Some(document)))
            .buffered(COLLECTION_ID_CALLS_IN_FLIGHT)
            .collect()
            .await;
        let mut found: BTreeMap<FirestoreCollectionId, Vec<CollectionPath>> = BTreeMap::new();
        for (document, listing) in documents.iter().zip(listings) {
            let relative = FirestoreDb::relative_document_name(documents_path, document);
            for id in listing? {
                let path = CollectionPath::in_document(relative, id.clone());
                found.entry(id).or_default().push(path);
            }
        }
        let mut collections = Vec::new();
        let mut next_levels = Vec::new();
        for (id, instances) in found {
            let path = format!("{}/*/{id}", self.path);
            collections.push(TreeCollection {
                path: path.clone(),
                id: id.clone(),
                depth,
                seen_under: Some(format!(
                    "{} of {} sampled {}",
                    instances.len(),
                    documents.len(),
                    self.id
                )),
            });
            next_levels.push(Level {
                path,
                id,
                instances,
            });
        }
        Ok((collections, next_levels))
    }
}

impl CollectionTree {
    /// Lists the collections under the root, then the subcollections of a sample of the
    /// documents of each, down to `target.depth`. A sample is the first `target.sample`
    /// documents, existing or not, across all collections of one path, so no path is read for
    /// more.
    pub async fn walk(db: &FirestoreDb, target: CollectionTreeTarget) -> FsqResult<Self> {
        let walk = Walk::new(db);
        let documents_path = db.get_documents_path();
        let limit = target.sample.documents() as usize;
        let mut collections = Vec::new();
        let mut levels = Vec::new();
        for id in walk.collection_ids(None).await? {
            let path = id.to_string();
            collections.push(TreeCollection {
                path: path.clone(),
                id: id.clone(),
                depth: 1,
                seen_under: None,
            });
            levels.push(Level {
                path,
                instances: vec![CollectionPath::from(id.clone())],
                id,
            });
        }
        for depth in 2..=target.depth.levels() as usize {
            let sampled: Vec<_> = futures::stream::iter(levels)
                .map(|level| level.sample(&walk, documents_path, limit, depth))
                .buffered(LEVELS_IN_FLIGHT)
                .try_collect()
                .await?;
            levels = Vec::new();
            for (found, opened) in sampled {
                collections.extend(found);
                levels.extend(opened);
            }
        }
        Ok(Self {
            target,
            collections,
        })
    }

    /// The sample every collection above the last level was walked with; none when the depth
    /// is 1 and nothing was sampled.
    pub fn sample(&self) -> Option<Sample> {
        (self.target.depth.levels() > 1).then(|| Sample {
            documents: self.target.sample.documents() as usize,
        })
    }

    /// One row per collection, level by level.
    pub fn into_grid(self) -> Grid {
        let mut grid = Grid::new(["collection", "path", "depth", "seen_under"]);
        for collection in self.collections {
            grid.push_row(vec![
                Some(Cell::Text(collection.id.to_string())),
                Some(Cell::Text(collection.path)),
                Some(Cell::Count(collection.depth as u64)),
                collection.seen_under.map(Cell::Text),
            ]);
        }
        grid
    }
}
