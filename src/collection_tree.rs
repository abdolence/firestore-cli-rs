use crate::describe::{CollectionPath, SampleSize};
use crate::errors::{FsqError, FsqResult, InvalidTreeDepth};
use crate::render::{Cell, Grid, Sample};
use crate::walk::Walk;
use firestore::{FirestoreCollectionId, FirestoreDb};
use futures::StreamExt;
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CollectionTreeTarget {
    pub depth: TreeDepth,
    pub sample: SampleSize,
}

impl Default for CollectionTreeTarget {
    fn default() -> Self {
        Self {
            depth: TreeDepth::default(),
            sample: SampleSize::COLLECTION_TREE_DEFAULT,
        }
    }
}

/// How many of the sampled documents of a collection have a subcollection.
#[derive(Debug, Clone, PartialEq, Eq)]
struct SeenUnder {
    documents_with_it: usize,
    sampled: usize,
    parent: FirestoreCollectionId,
}

impl Display for SeenUnder {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} of {} sampled {}",
            self.documents_with_it, self.sampled, self.parent
        )
    }
}

/// A collection by its path with `*` for each document ID on the way: `users/*/addresses`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct TreeCollection {
    path: String,
    id: FirestoreCollectionId,
    depth: usize,
    /// `None` for a collection under the root, which no sample is taken to find.
    seen_under: Option<SeenUnder>,
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

impl CollectionTree {
    /// Lists the collections under the root, then the subcollections of a sample of the
    /// documents of each, down to `target.depth`. A sample is the first `target.sample`
    /// documents, existing or not, across all collections of one path, so no path is read for
    /// more.
    pub async fn walk(db: &FirestoreDb, target: CollectionTreeTarget) -> FsqResult<Self> {
        let walk = Walk::new(db);
        let converter = db.json();
        let limit = target.sample.documents() as usize;
        let mut collections = Vec::new();
        let mut levels = Vec::new();
        for id in walk.collection_ids(None).await? {
            collections.push(TreeCollection {
                path: id.to_string(),
                id: id.clone(),
                depth: 1,
                seen_under: None,
            });
            levels.push(Level {
                path: id.to_string(),
                instances: vec![CollectionPath::from(id.clone())],
                id,
            });
        }
        for depth in 2..=target.depth.levels() as usize {
            let mut next_levels = Vec::new();
            for level in levels {
                let documents = walk.first_document_names(&level.instances, limit).await?;
                let listings: Vec<_> = futures::stream::iter(&documents)
                    .map(|document| walk.collection_ids(Some(document)))
                    .buffered(COLLECTION_ID_CALLS_IN_FLIGHT)
                    .collect()
                    .await;
                let mut found: BTreeMap<FirestoreCollectionId, Vec<CollectionPath>> =
                    BTreeMap::new();
                for (document, listing) in documents.iter().zip(listings) {
                    let relative = converter.relative_name(document);
                    for id in listing? {
                        let path = format!("{relative}/{id}").parse()?;
                        found.entry(id).or_default().push(path);
                    }
                }
                for (id, instances) in found {
                    let path = format!("{}/*/{id}", level.path);
                    collections.push(TreeCollection {
                        path: path.clone(),
                        id: id.clone(),
                        depth,
                        seen_under: Some(SeenUnder {
                            documents_with_it: instances.len(),
                            sampled: documents.len(),
                            parent: level.id.clone(),
                        }),
                    });
                    next_levels.push(Level {
                        path,
                        id,
                        instances,
                    });
                }
            }
            levels = next_levels;
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
                collection
                    .seen_under
                    .map(|seen_under| Cell::Text(seen_under.to_string())),
            ]);
        }
        grid
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn collection_id(id: &str) -> FirestoreCollectionId {
        id.parse().expect("a collection ID")
    }

    #[test]
    fn the_grid_has_a_row_per_collection_with_where_it_was_seen() {
        let tree = CollectionTree {
            target: CollectionTreeTarget::default(),
            collections: vec![
                TreeCollection {
                    path: "users".to_string(),
                    id: collection_id("users"),
                    depth: 1,
                    seen_under: None,
                },
                TreeCollection {
                    path: "users/*/addresses".to_string(),
                    id: collection_id("addresses"),
                    depth: 2,
                    seen_under: Some(SeenUnder {
                        documents_with_it: 12,
                        sampled: 20,
                        parent: collection_id("users"),
                    }),
                },
            ],
        };

        let grid = tree.into_grid();

        assert_eq!(
            grid.columns(),
            ["collection", "path", "depth", "seen_under"]
        );
        assert_eq!(
            grid.rows(),
            [
                vec![
                    Some(Cell::Text("users".to_string())),
                    Some(Cell::Text("users".to_string())),
                    Some(Cell::Count(1)),
                    None,
                ],
                vec![
                    Some(Cell::Text("addresses".to_string())),
                    Some(Cell::Text("users/*/addresses".to_string())),
                    Some(Cell::Count(2)),
                    Some(Cell::Text("12 of 20 sampled users".to_string())),
                ],
            ]
        );
    }
}
