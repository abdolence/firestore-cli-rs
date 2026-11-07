use super::{interruptible, Interaction, Session};
use crate::errors::FsqResult;
use crate::meta::Switch;
use crate::render::{Counted, Footer, FooterNote, Grid, OutputFormat, QueryPlan};
use firestore::{
    FirestoreDocument, FirestoreExpr, FirestorePipeline, FirestorePipelineStage, FirestoreResult,
    FirestoreWithPipelineMetadata,
};
use futures::stream::{BoxStream, Peekable};
use futures::StreamExt;
use std::pin::Pin;
use std::time::Instant;

type RowStream<'b> =
    BoxStream<'b, FirestoreResult<FirestoreWithPipelineMetadata<FirestoreDocument>>>;

/// Lines a page of rows leaves for the table's borders, its header, the footer, the "more"
/// prompt and the REPL's prompt.
const PAGE_CHROME_LINES: usize = 7;
const MIN_PAGE_ROWS: usize = 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RowsOf {
    Select,
    Explain,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(super) struct ResultShape {
    /// The fields of the last stage that names its fields, in its order.
    column_order: Vec<String>,
    /// A statement with a `LIMIT` shows all its rows; one without shows a page at a time.
    has_limit: bool,
}

impl From<&FirestorePipeline> for ResultShape {
    fn from(pipeline: &FirestorePipeline) -> Self {
        let column_order = pipeline
            .stages
            .iter()
            .rev()
            .find_map(|stage| match stage {
                FirestorePipelineStage::Fields(fields) => Some(&fields.fields),
                _ => None,
            })
            .map(|selections| {
                selections
                    .iter()
                    .filter_map(|selection| match (&selection.alias, &selection.expr) {
                        (Some(alias), _) => Some(alias.clone()),
                        (None, FirestoreExpr::Field(path)) => Some(path.clone()),
                        (None, _) => None,
                    })
                    .collect()
            })
            .unwrap_or_default();
        Self {
            column_order,
            has_limit: pipeline
                .stages
                .iter()
                .any(|stage| matches!(stage, FirestorePipelineStage::Limit(_))),
        }
    }
}

pub(super) struct RowPages<'b> {
    rows: Peekable<RowStream<'b>>,
    first_response: Option<Instant>,
    plan: Option<QueryPlan>,
}

pub(super) struct RowPage {
    pub documents: Vec<FirestoreDocument>,
    /// Whether another document follows this page.
    pub more: bool,
}

impl<'b> RowPages<'b> {
    pub fn new(rows: RowStream<'b>) -> Self {
        Self {
            rows: rows.peekable(),
            first_response: None,
            plan: None,
        }
    }

    /// The next `size` documents, or every one that is left. Items without a document carry
    /// only metadata, Query Explain's plan among it, and are read past.
    pub async fn next_page(&mut self, size: Option<usize>) -> FirestoreResult<RowPage> {
        let mut documents = Vec::new();
        loop {
            if size.is_some_and(|size| documents.len() >= size) {
                let more = self.document_follows().await;
                return Ok(RowPage { documents, more });
            }
            match self.rows.next().await {
                None => {
                    return Ok(RowPage {
                        documents,
                        more: false,
                    })
                }
                Some(item) => {
                    let item = item?;
                    self.first_response.get_or_insert_with(Instant::now);
                    if let Some(stats) = item.metadata.explain_stats {
                        self.plan = Some(QueryPlan::new(stats.text));
                    }
                    documents.extend(item.document);
                }
            }
        }
    }

    async fn document_follows(&mut self) -> bool {
        loop {
            match Pin::new(&mut self.rows).peek().await {
                None => return false,
                Some(Ok(item)) if item.document.is_none() => {
                    if let Some(Ok(item)) = self.rows.next().await {
                        if let Some(stats) = item.metadata.explain_stats {
                            self.plan = Some(QueryPlan::new(stats.text));
                        }
                    }
                }
                // A document, or an error that the next page reports.
                Some(_) => return true,
            }
        }
    }
}

impl Session {
    /// Shows the rows of a `SELECT` or an `EXPLAIN`. At the REPL, in a table, a statement
    /// without a `LIMIT` shows a page that fits the terminal and asks before the next one.
    pub(super) async fn show_rows(
        &mut self,
        rows: RowStream<'_>,
        shape: &ResultShape,
        rows_of: RowsOf,
        started: Instant,
    ) -> FsqResult<()> {
        let page_size = self.page_size(shape);
        let mut pages = RowPages::new(rows);
        let mut shown = 0;
        loop {
            let page = interruptible(pages.next_page(page_size)).await??;
            shown += page.documents.len();
            let grid = Grid::from_documents(page.documents, &shape.column_order);
            self.remember_fields(&grid);
            let view = self.view();
            if page.more {
                let footer = Footer {
                    count: shown,
                    counted: Counted::Rows,
                    elapsed: started.elapsed(),
                    notes: vec![FooterNote::MorePages],
                };
                self.output
                    .print(&format!("{}{}", view.grid(&grid), view.footer(&footer)))?;
                if self.output.ask_for_more(&self.theme)? {
                    continue;
                }
                return Ok(());
            }
            let footer = Footer {
                count: shown,
                counted: Counted::Rows,
                elapsed: started.elapsed(),
                notes: self.timing_notes(pages.first_response, started, pages.plan.as_ref()),
            };
            let mut text = String::new();
            match (&pages.plan, rows_of) {
                (Some(plan), RowsOf::Explain) if self.settings.format != OutputFormat::Table => {
                    text.push_str(&view.grids(&[("rows", &grid), ("plan", &Grid::plan(plan))]));
                }
                (Some(plan), RowsOf::Explain) => {
                    if grid.len() > 0 {
                        text.push_str(&view.grid(&grid));
                    }
                    text.push_str(&view.plan(plan));
                }
                (None, RowsOf::Explain) => {
                    self.output.notice(
                        "Firestore returned no plan; the Firestore emulator returns none.",
                    )?;
                    text.push_str(&view.grid(&grid));
                }
                (_, RowsOf::Select) => text.push_str(&view.grid(&grid)),
            }
            text.push_str(&view.footer(&footer));
            return self.output.print(&text);
        }
    }

    fn page_size(&self, shape: &ResultShape) -> Option<usize> {
        let pages = self.interaction == Interaction::Repl
            && self.settings.format == OutputFormat::Table
            && !shape.has_limit;
        let height = self.output.height().filter(|_| pages)?;
        Some(height.saturating_sub(PAGE_CHROME_LINES).max(MIN_PAGE_ROWS))
    }

    fn timing_notes(
        &self,
        first_response: Option<Instant>,
        started: Instant,
        plan: Option<&QueryPlan>,
    ) -> Vec<FooterNote> {
        if self.settings.timing == Switch::Off {
            return Vec::new();
        }
        first_response
            .map(|first| FooterNote::RoundTrip(first.duration_since(started)))
            .into_iter()
            .chain(plan.map(|plan| FooterNote::Stats(plan.stats())))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::fixtures::{document, string};
    use firestore::{
        FirestoreCollectionSource, FirestoreFieldsStage, FirestoreFunction, FirestoreLimitStage,
        FirestorePipelineExplainStats, FirestorePipelineMetadata, FirestorePipelineSource,
        FirestoreSelection,
    };

    fn row(id: &str) -> FirestoreResult<FirestoreWithPipelineMetadata<FirestoreDocument>> {
        Ok(FirestoreWithPipelineMetadata {
            document: Some(document(id, vec![("title", string(id))])),
            metadata: FirestorePipelineMetadata::new(),
        })
    }

    fn end_of_response(
        plan: Option<&str>,
    ) -> FirestoreResult<FirestoreWithPipelineMetadata<FirestoreDocument>> {
        Ok(FirestoreWithPipelineMetadata {
            document: None,
            metadata: FirestorePipelineMetadata::new().opt_explain_stats(
                plan.map(|text| FirestorePipelineExplainStats::new(text.to_string())),
            ),
        })
    }

    fn ids(page: &RowPage) -> Vec<String> {
        page.documents
            .iter()
            .map(|document| {
                document
                    .name
                    .rsplit('/')
                    .next()
                    .unwrap_or_default()
                    .to_string()
            })
            .collect()
    }

    #[tokio::test]
    async fn rows_are_read_a_page_at_a_time_and_more_only_when_a_document_follows() {
        let rows = futures::stream::iter(vec![
            row("a"),
            row("b"),
            end_of_response(None),
            row("c"),
            row("d"),
            end_of_response(Some("Tree:")),
        ])
        .boxed();
        let mut pages = RowPages::new(rows);

        let first = pages.next_page(Some(2)).await.expect("a page");
        let second = pages.next_page(Some(2)).await.expect("a page");

        assert_eq!(
            (ids(&first), first.more),
            (vec!["a".to_string(), "b".to_string()], true)
        );
        assert_eq!(
            (ids(&second), second.more),
            (vec!["c".to_string(), "d".to_string()], false)
        );
        assert_eq!(pages.plan, Some(QueryPlan::new("Tree:".to_string())));
    }

    #[test]
    fn the_shape_of_a_select_follows_its_select_list_and_its_limit() {
        let pipeline = FirestorePipeline::new(FirestorePipelineSource::Collection(
            FirestoreCollectionSource::new("books".to_string()),
        ))
        .with_stage(FirestorePipelineStage::Fields(FirestoreFieldsStage::new(
            vec![
                FirestoreSelection::new(FirestoreExpr::Field("title".to_string())),
                FirestoreSelection::new(FirestoreExpr::Function(FirestoreFunction::new(
                    "to_upper".to_string(),
                    vec![],
                )))
                .with_alias("shouted".to_string()),
                FirestoreSelection::new(FirestoreExpr::Function(FirestoreFunction::new(
                    "length".to_string(),
                    vec![],
                ))),
            ],
        )))
        .with_stage(FirestorePipelineStage::Limit(FirestoreLimitStage::new(10)));

        assert_eq!(
            ResultShape::from(&pipeline),
            ResultShape {
                column_order: vec!["title".to_string(), "shouted".to_string()],
                has_limit: true,
            }
        );
    }
}
