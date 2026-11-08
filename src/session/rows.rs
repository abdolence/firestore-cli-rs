use super::{Interaction, Session};
use crate::errors::{FsqError, FsqResult, RowOutOfRange};
use crate::meta::Switch;
use crate::render::{counted, Footer, FooterNote, Grid, OutputFormat, QueryPlan, View};
use firestore::{
    FirestoreDocument, FirestoreExpr, FirestorePipelineExplainStats, FirestorePipelineStage,
    FirestoreResult, FirestoreSqlStatement, FirestoreWithPipelineMetadata,
};
use futures::stream::{BoxStream, Peekable};
use futures::StreamExt;
use std::num::NonZeroUsize;
use std::pin::Pin;
use std::time::{Duration, Instant};

type RowStream<'b> =
    BoxStream<'b, FirestoreResult<FirestoreWithPipelineMetadata<FirestoreDocument>>>;

/// Lines a page of rows leaves for the table's borders, its header, the footer, the "more"
/// prompt and the REPL's prompt.
const PAGE_CHROME_LINES: usize = 7;
const MIN_PAGE_ROWS: usize = 10;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(super) struct ResultShape {
    /// The fields of the last stage that names its fields, in its order.
    column_order: Vec<String>,
    /// Whether the REPL shows the rows a page at a time: a `SELECT` without a `LIMIT` does, and an
    /// `EXPLAIN` does not.
    paged: bool,
}

impl From<&FirestoreSqlStatement> for ResultShape {
    fn from(statement: &FirestoreSqlStatement) -> Self {
        let (pipeline, pageable) = match statement {
            FirestoreSqlStatement::Query(pipeline) => (pipeline, true),
            // The plan comes with the last item, which a reader who stops paging never reaches.
            FirestoreSqlStatement::Explain(explain) => (&explain.pipeline, false),
            _ => return Self::default(),
        };
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
            paged: pageable
                && !pipeline
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
    pub more: bool,
}

/// The rows the last `SELECT` or `EXPLAIN` showed, kept for `\x full N`.
#[derive(Debug, Default)]
pub(super) struct LastResult(Option<ShownRows>);

#[derive(Debug)]
struct ShownRows {
    documents: Vec<FirestoreDocument>,
    column_order: Vec<String>,
}

impl LastResult {
    /// Forgets the rows of the previous result and starts one that has none yet.
    pub fn begin(&mut self, column_order: Vec<String>) {
        self.0 = Some(ShownRows {
            documents: Vec::new(),
            column_order,
        });
    }

    pub fn extend(&mut self, documents: &[FirestoreDocument]) {
        if let Some(shown) = &mut self.0 {
            shown.documents.extend_from_slice(documents);
        }
    }

    /// The rows from `first` (counting from 0) to the last one shown, and the order of the
    /// columns the statement selected them in.
    pub fn rows_from(&self, first: usize) -> (&[FirestoreDocument], &[String]) {
        match &self.0 {
            Some(shown) => (&shown.documents[first..], &shown.column_order),
            None => (&[], &[]),
        }
    }

    /// Row `number` of the result, counting from 1.
    pub fn row(&self, number: NonZeroUsize) -> FsqResult<(&[FirestoreDocument], &[String])> {
        let shown = self.0.as_ref().ok_or(FsqError::NoRowsToShow)?;
        let document = shown.documents.get(number.get() - 1).ok_or_else(|| {
            FsqError::RowOutOfRange(RowOutOfRange {
                row: number,
                rows: shown.documents.len(),
            })
        })?;
        Ok((std::slice::from_ref(document), &shown.column_order))
    }
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
                    self.keep_plan(item.metadata.explain_stats);
                    documents.extend(item.document);
                }
            }
        }
    }

    fn keep_plan(&mut self, explain_stats: Option<FirestorePipelineExplainStats>) {
        if let Some(stats) = explain_stats {
            self.plan = Some(QueryPlan::new(stats.text));
        }
    }

    async fn document_follows(&mut self) -> bool {
        loop {
            match Pin::new(&mut self.rows).peek().await {
                None => return false,
                Some(Ok(item)) if item.document.is_none() => {
                    if let Some(Ok(item)) = self.rows.next().await {
                        self.keep_plan(item.metadata.explain_stats);
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
        statement: &FirestoreSqlStatement,
        started: Instant,
    ) -> FsqResult<()> {
        let shape = ResultShape::from(statement);
        let explains = matches!(statement, FirestoreSqlStatement::Explain(_));
        let page_size = self.page_size(&shape);
        self.last_result.begin(shape.column_order.clone());
        let mut pages = RowPages::new(rows);
        let mut shown = 0;
        let mut waiting_for_reader = Duration::ZERO;
        loop {
            let page = self
                .waiting("waiting for rows", pages.next_page(page_size))
                .await??;
            let first_of_page = shown;
            shown += page.documents.len();
            self.last_result.extend(&page.documents);
            let grid = Grid::from_documents(page.documents, &shape.column_order);
            self.remember_fields(&grid);
            let view = self.view();
            let (page_documents, column_order) = self.last_result.rows_from(first_of_page);
            let rows_text = view.rows(&grid, page_documents, first_of_page + 1, column_order);
            let hint = self.detail_hint(&view, &grid);
            let elapsed = started.elapsed().saturating_sub(waiting_for_reader);
            if page.more {
                let footer = Footer {
                    counted: counted(shown, "row", "rows"),
                    elapsed,
                    notes: vec![FooterNote::MorePages],
                };
                self.output
                    .print(&format!("{rows_text}{}{hint}", view.footer(&footer)))?;
                let asked = Instant::now();
                let continues = self.output.ask_for_more(&self.theme)?;
                waiting_for_reader += asked.elapsed();
                if continues {
                    continue;
                }
                if self.settings.timing == Switch::On {
                    self.output.notice(
                        "Stopped before the last row, which carries the execution stats.",
                    )?;
                }
                return Ok(());
            }
            let footer = Footer {
                counted: counted(shown, "row", "rows"),
                elapsed,
                notes: self.timing_notes(pages.first_response, started, pages.plan.as_ref()),
            };
            let mut text = String::new();
            match (&pages.plan, explains) {
                (Some(plan), true) if self.settings.format != OutputFormat::Table => {
                    text.push_str(&view.grids(&[("rows", &grid), ("plan", &Grid::plan(plan))]));
                }
                (Some(plan), true) => {
                    if !grid.is_empty() {
                        text.push_str(&rows_text);
                    }
                    text.push_str(&view.plan(plan));
                }
                (None, true) => {
                    self.output.notice("Firestore returned no plan.")?;
                    text.push_str(&rows_text);
                }
                (_, false) => text.push_str(&rows_text),
            }
            text.push_str(&view.footer(&footer));
            text.push_str(&hint);
            return self.output.print(&text);
        }
    }

    /// The hint under a page of rows that `\x full N` shows more of. Only a person at the
    /// keyboard reads it, so scripts and piped input never get one.
    fn detail_hint(&self, view: &View<'_>, grid: &Grid) -> String {
        match self.interaction {
            Interaction::Repl => view.detail_hint(grid),
            Interaction::Script => String::new(),
        }
    }

    fn page_size(&self, shape: &ResultShape) -> Option<usize> {
        let pages = self.interaction == Interaction::Repl
            && self.settings.format == OutputFormat::Table
            && shape.paged;
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
        FirestoreCollectionSource, FirestoreExplainOptions, FirestoreFieldsStage,
        FirestoreFunction, FirestoreLimitStage, FirestorePipeline, FirestorePipelineExplainStats,
        FirestorePipelineMetadata, FirestorePipelineSource, FirestoreSelection,
        FirestoreSqlExplain,
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

    fn select_with_fields(limit: Option<u32>) -> FirestorePipeline {
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
        )));
        match limit {
            Some(limit) => pipeline.with_stage(FirestorePipelineStage::Limit(
                FirestoreLimitStage::new(limit),
            )),
            None => pipeline,
        }
    }

    fn shown(ids: &[&str]) -> LastResult {
        let mut last = LastResult::default();
        last.begin(vec!["title".to_string()]);
        let documents: Vec<_> = ids
            .iter()
            .map(|id| document(id, vec![("title", string(id))]))
            .collect();
        last.extend(&documents);
        last
    }

    fn row_number(number: usize) -> NonZeroUsize {
        NonZeroUsize::new(number).expect("a row number")
    }

    #[test]
    fn a_row_of_the_last_result_is_the_one_at_that_position_counting_from_one() {
        let last = shown(&["a", "b", "c"]);

        let (documents, order) = last.row(row_number(2)).expect("row 2");

        assert_eq!(documents.len(), 1);
        assert!(documents[0].name.ends_with("/b"));
        assert_eq!(order, ["title".to_string()]);
    }

    #[test]
    fn a_row_past_the_end_of_the_last_result_is_out_of_range() {
        let last = shown(&["a", "b"]);

        assert!(matches!(
            last.row(row_number(3)),
            Err(FsqError::RowOutOfRange(RowOutOfRange { row, rows: 2 })) if row == row_number(3)
        ));
    }

    #[test]
    fn a_result_without_rows_has_no_row_one() {
        assert!(matches!(
            shown(&[]).row(row_number(1)),
            Err(FsqError::RowOutOfRange(RowOutOfRange { rows: 0, .. }))
        ));
    }

    #[test]
    fn without_a_result_there_is_no_row_to_show() {
        assert!(matches!(
            LastResult::default().row(row_number(1)),
            Err(FsqError::NoRowsToShow)
        ));
    }

    #[test]
    fn a_new_result_replaces_the_rows_of_the_last_one() {
        let mut last = shown(&["a", "b"]);

        last.begin(Vec::new());
        last.extend(&[document("c", vec![])]);

        assert!(last.row(row_number(2)).is_err());
        assert!(last.row(row_number(1)).is_ok());
    }

    #[test]
    fn the_shape_follows_the_select_list_and_the_limit_and_never_pages_an_explain() {
        let limited = FirestoreSqlStatement::Query(select_with_fields(Some(10)));
        let unlimited = FirestoreSqlStatement::Query(select_with_fields(None));
        let explain = FirestoreSqlStatement::Explain(FirestoreSqlExplain::new(
            select_with_fields(None),
            FirestoreExplainOptions::new().with_analyze(true),
        ));

        assert_eq!(
            ResultShape::from(&limited),
            ResultShape {
                column_order: vec!["title".to_string(), "shouted".to_string()],
                paged: false,
            }
        );
        assert!(ResultShape::from(&unlimited).paged);
        assert!(!ResultShape::from(&explain).paged);
    }
}
