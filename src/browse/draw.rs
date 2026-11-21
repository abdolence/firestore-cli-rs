mod footer;
mod order;
mod popup;

use super::state::{Browser, Copied, Load, Movement, Pane, RequestId, BROWSE_DOCUMENT_LIMIT};
use super::tree::{Fold, Listing, Row, Tree};
use crate::render::{
    counted, cut_middle, type_name, ExpandedMode, OutputFormat, Theme, View, ANIMATION_TICK,
};
use firestore::gcloud_sdk::google::firestore::v1::value::ValueType;
use firestore::gcloud_sdk::google::firestore::v1::Value;
use firestore::FirestoreDocument;
use indicatif::ProgressStyle;
use order::OrderColumn;
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Cell, HighlightSpacing, List, ListItem, ListState, Paragraph, Row as TableRow, Table,
    TableState, Wrap,
};
use ratatui::Frame;
use std::sync::LazyLock;
use std::time::{Duration, Instant};

/// The narrowest the documents pane gets, which holds a count such as `58/100+` in its title.
const DOCUMENTS_PANE_MIN_WIDTH: usize = 12;

/// The most of the terminal's width that the documents pane takes, in percent.
const DOCUMENTS_PANE_MAX_PERCENT: usize = 35;

/// The width of the type column of the tree, which holds the longest type name.
const TREE_TYPE_WIDTH: usize = 10;

const TREE_COLUMN_GAP: usize = 2;

/// The narrowest tree pane that keeps its type column, so that a narrow one keeps its room for
/// the values.
const TREE_PANE_MIN_WIDTH_WITH_TYPES: u16 = 50;

/// How many rows a pane keeps in view on either side of its cursor, where it has them.
const BROWSE_SCROLL_PADDING: usize = 2;

/// Marks the selected row of both panes. It shows the selection of the pane without the focus,
/// which has no bar, and shows without colour.
const SELECTION_MARKER: &str = "▌";

static PLAIN_THEME: LazyLock<Theme> = LazyLock::new(|| Theme::new(false));

/// What the browser keeps from one frame to the next: where each list and the popup are
/// scrolled to, so that the cursor moves inside the visible rows rather than dragging them
/// along, how many rows a page of the focused pane or the popup holds, where the line of the
/// filter editor starts, the waits on Firestore that are drawn, and what the last key copied.
#[derive(Debug, Default)]
pub struct Viewport {
    documents: ListState,
    tree: TableState,
    page_rows: usize,
    /// The byte of the filter being edited that its line starts at.
    editor_start: usize,
    query_wait: Option<Wait>,
    listing_wait: Option<Wait>,
    /// `None` while no popup shows, so that the next one opens at its top.
    popup: Option<PopupScroll>,
    /// What the last key put on the clipboard, which the footer notes until the next key.
    pub(super) copied: Option<Copied>,
}

impl Viewport {
    /// The rows of the focused pane, or of the popup, in the last frame drawn, at least one.
    pub fn page_rows(&self) -> usize {
        self.page_rows.max(1)
    }

    /// Scrolls the text of the popup drawn last, never past its first line or its last.
    pub fn scroll_popup(&mut self, movement: Movement) {
        if let Some(scroll) = &mut self.popup {
            scroll.top = movement.applied_to(scroll.top, scroll.last_top + 1);
        }
    }
}

/// Where the text of a popup is scrolled to.
#[derive(Debug, Default, Clone, Copy)]
struct PopupScroll {
    /// The first line of the text that shows.
    top: usize,
    /// The furthest `top` goes: the last line of the text then shows at the bottom.
    last_top: usize,
}

/// A request the browser was drawn waiting for, and when it first was: its spinner and its
/// loading time count from then.
#[derive(Debug, Clone, Copy)]
struct Wait {
    request: RequestId,
    since: Instant,
}

impl Wait {
    /// How long the browser has waited for `pending` at `now`, `None` while nothing is pending.
    /// `slot` keeps the wait from one frame to the next, and starts it again for another request.
    fn track(
        slot: &mut Option<Self>,
        pending: Option<RequestId>,
        now: Instant,
    ) -> Option<Duration> {
        *slot = pending.map(|request| match *slot {
            Some(wait) if wait.request == request => wait,
            _ => Self {
                request,
                since: now,
            },
        });
        slot.map(|wait| now.saturating_duration_since(wait.since))
    }
}

impl Browser {
    /// Draws the header with the collection and its filter, the documents and the tree of the
    /// selected one side by side, the footer with the keys or the filter editor, under it the
    /// error of a failed filter or listing, and over the panes the popup, if one is open.
    pub fn draw(&self, frame: &mut Frame, viewport: &mut Viewport, now: Instant) {
        let level = self.level();
        let pending_query = match level.load() {
            Load::Pending { request, .. } => Some(*request),
            Load::Done | Load::Stopped { .. } | Load::Failed { .. } => None,
        };
        let pending_listing = match level.tree().listing() {
            Listing::Pending(request) => Some(*request),
            Listing::NotListed | Listing::Listed(_) | Listing::Failed(_) => None,
        };
        let query_waited = Wait::track(&mut viewport.query_wait, pending_query, now);
        let listing_waited = Wait::track(&mut viewport.listing_wait, pending_listing, now);

        let area = frame.area();
        let error = self.footer_error(usize::from(area.width));
        let [header, body, footer, error_area] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Fill(1),
            Constraint::Length(1),
            Constraint::Length(u16::try_from(error.len()).unwrap_or(u16::MAX)),
        ])
        .areas(area);
        let order = OrderColumn::of(self);
        let documents_width = self.documents_width(body.width, query_waited, order.as_ref());
        let [documents, tree] =
            Layout::horizontal([Constraint::Length(documents_width), Constraint::Fill(1)])
                .areas(body);
        frame.render_widget(self.header(usize::from(area.width)), header);
        self.draw_documents(frame, documents, viewport, query_waited, order.as_ref());
        self.draw_tree(frame, tree, viewport, listing_waited);
        self.draw_footer(frame, footer, viewport);
        frame.render_widget(Paragraph::new(error), error_area);
        match self.popup() {
            Some(popup) => self.draw_popup(frame, popup, body, viewport),
            None => viewport.popup = None,
        }
    }

    /// The trail of collections down to the current one, and the filter its documents were read
    /// with, cut in the middle to fit `width` columns.
    fn header(&self, width: usize) -> Line<'static> {
        let level = self.level();
        let path = level.path();
        let collection_id = path.collection_id().to_string();
        let mut spans = Vec::new();
        if let Some(parent) = path.parent() {
            let room = width.saturating_sub(Span::from(collection_id.as_str()).width());
            spans.push(Span::from(cut_middle(&format!("{parent}/"), room)).dim());
        }
        spans.push(Span::from(collection_id).bold());
        spans.push(Span::from("  "));

        let filter = level.filter();
        let limit = (!filter.has_limit()).then(|| {
            let space = if filter.is_empty() { "" } else { " " };
            Span::from(format!("{space}LIMIT {BROWSE_DOCUMENT_LIMIT}")).dim()
        });
        if !filter.is_empty() {
            let taken = spans.iter().chain(&limit).map(Span::width).sum::<usize>();
            let room = width.saturating_sub(taken);
            spans.push(Span::from(cut_middle(&filter.to_string(), room)));
        }
        spans.extend(limit);
        Line::from(spans)
    }

    /// The columns of the documents pane: its longest ID with the borders and the selection
    /// marker, and its `order` column when that still has room, or its title while no documents
    /// have been read, at least [`DOCUMENTS_PANE_MIN_WIDTH`] and at most
    /// [`DOCUMENTS_PANE_MAX_PERCENT`] of `width`.
    fn documents_width(
        &self,
        width: u16,
        waited: Option<Duration>,
        order: Option<&OrderColumn>,
    ) -> u16 {
        let width = usize::from(width);
        let most = (width * DOCUMENTS_PANE_MAX_PERCENT / 100).max(DOCUMENTS_PANE_MIN_WIDTH);
        let fitted = |content: usize| content.clamp(DOCUMENTS_PANE_MIN_WIDTH, most).min(width);
        let frame = Span::from(SELECTION_MARKER).width() + 2;
        let columns = match self.level().documents() {
            Some(documents) => {
                let longest = longest_id(documents);
                order
                    .and_then(|order| {
                        let columns = fitted(frame + longest + order.width());
                        order
                            .room(longest, columns.saturating_sub(frame))
                            .map(|_| columns)
                    })
                    .unwrap_or_else(|| fitted(frame + longest))
            }
            None => fitted(self.documents_titles(waited)[0].width() + 2),
        };
        u16::try_from(columns).unwrap_or(u16::MAX)
    }

    /// Draws the IDs of the documents, and after them the `order` column where the pane has room
    /// for the longest ID and some of the values.
    fn draw_documents(
        &self,
        frame: &mut Frame,
        area: Rect,
        viewport: &mut Viewport,
        waited: Option<Duration>,
        order: Option<&OrderColumn>,
    ) {
        let level = self.level();
        let title = self.documents_title(waited, usize::from(area.width));
        let block = self.pane_block(Pane::Documents, title);
        let inner = block.inner(area);
        if self.focus() == Pane::Documents {
            viewport.page_rows = usize::from(inner.height);
        }
        let width = usize::from(inner.width).saturating_sub(Span::from(SELECTION_MARKER).width());
        let documents = level.documents().unwrap_or_default();
        let longest = longest_id(documents);
        let column = order.and_then(|order| Some((order, order.room(longest, width)?)));
        let items: Vec<ListItem> = documents
            .iter()
            .enumerate()
            .map(|(position, document)| {
                let id = document_id(document);
                match column {
                    Some((order, room)) => ListItem::new(order.line(position, id, longest, room)),
                    None => ListItem::new(cut_middle(id, width)),
                }
            })
            .collect();
        viewport.documents.select(Some(level.selected()));
        frame.render_stateful_widget(
            List::new(items)
                .block(block)
                .highlight_style(self.cursor_style(Pane::Documents))
                .highlight_symbol(SELECTION_MARKER)
                .highlight_spacing(HighlightSpacing::Always)
                .scroll_padding(BROWSE_SCROLL_PADDING),
            area,
            &mut viewport.documents,
        );
    }

    /// The fullest title of the documents pane that fits in `width` columns, else the shortest.
    fn documents_title(&self, waited: Option<Duration>, width: usize) -> Line<'static> {
        let mut titles = self.documents_titles(waited);
        let fitting = titles
            .iter()
            .position(|title| title.width() + 2 <= width)
            .unwrap_or(titles.len() - 1);
        titles.swap_remove(fitting)
    }

    /// The titles the documents pane can have, the fullest first: its name, then the position of
    /// the selected document and the count, once documents have been read and while no query is
    /// pending, then what the query is doing. A `+` after a count that is the limit says that
    /// more documents may exist.
    fn documents_titles(&self, waited: Option<Duration>) -> Vec<Line<'static>> {
        let level = self.level();
        let count = match (level.documents(), waited) {
            (Some(documents), None) => {
                let more = if level.filter().effective_limit() == Some(documents.len()) {
                    "+"
                } else {
                    ""
                };
                Some(match documents.len() {
                    0 => format!("0{more}"),
                    count => format!("{}/{count}{more}", level.selected() + 1),
                })
            }
            (None, _) | (_, Some(_)) => None,
        };
        let state = match (waited, level.load()) {
            (Some(waited), _) => {
                let loading = WaitLabel {
                    what: "loading",
                    waited,
                };
                Some(Span::from(loading.to_string()).yellow())
            }
            (None, Load::Stopped { .. }) => Some(Span::from("stopped").dim()),
            (None, Load::Failed { .. }) => Some(Span::from("failed").red()),
            (None, Load::Done | Load::Pending { .. }) => None,
        };
        let mut titles = vec![
            pane_title(Some("documents"), count.clone(), state.clone()),
            pane_title(None, count.clone(), state.clone()),
        ];
        if let (Some(_), Some(state)) = (count, state) {
            titles.push(pane_title(None, None, Some(state)));
        }
        titles
    }

    fn draw_tree(
        &self,
        frame: &mut Frame,
        area: Rect,
        viewport: &mut Viewport,
        listing_waited: Option<Duration>,
    ) {
        let level = self.level();
        let Some(document) = level.selected_document() else {
            let block = self.pane_block(Pane::Tree, Line::default());
            let note = self.nothing_to_show().unwrap_or_default();
            frame.render_widget(
                Paragraph::new(note)
                    .dim()
                    .wrap(Wrap { trim: true })
                    .block(block),
                area,
            );
            return;
        };
        let tree = level.tree();
        let rows = tree.rows(document);
        let cursor = tree.cursor();
        let title_width = usize::from(area.width).saturating_sub(4);
        let title = format!(" {} ", cut_middle(document_id(document), title_width));
        let mut block = self.pane_block(Pane::Tree, title.into());
        if let Some(row) = rows.get(cursor) {
            let path = self.row_path(row, document);
            block = block.title_bottom(format!(" {} ", cut_middle(&path, title_width)));
        }
        let inner = block.inner(area);
        if self.focus() == Pane::Tree {
            viewport.page_rows = usize::from(inner.height);
        }
        let view = self.value_view();
        let columns = TreeColumns::new(document, area.width);
        let gap = rows
            .iter()
            .position(|row| matches!(row, Row::Subcollections { .. }));
        let offset = scrolled(
            viewport.tree.offset(),
            cursor,
            rows.len(),
            usize::from(inner.height),
            gap,
        );
        let table_rows: Vec<TableRow> = rows
            .iter()
            .map(|row| columns.row(row, &view, listing_waited))
            .collect();
        viewport.tree.select(Some(cursor));
        *viewport.tree.offset_mut() = offset;
        frame.render_stateful_widget(
            Table::new(table_rows, columns.widths())
                .column_spacing(u16::try_from(TREE_COLUMN_GAP).unwrap_or(u16::MAX))
                .block(block)
                .row_highlight_style(self.cursor_style(Pane::Tree))
                .highlight_symbol(SELECTION_MARKER)
                .highlight_spacing(HighlightSpacing::Always),
            area,
            &mut viewport.tree,
        );
    }

    /// Why no document is selected, and what to do about it. `None` while the first documents
    /// are still loading, which the documents pane shows.
    fn nothing_to_show(&self) -> Option<String> {
        let level = self.level();
        let note = match (level.documents(), level.load()) {
            (Some(_), _) if level.filter().is_empty() => format!(
                "no documents in {} (a mistyped path looks the same)",
                level.path()
            ),
            (Some(_), _) => "no documents match · / edits the filter".to_string(),
            (None, Load::Stopped { .. }) => {
                "stopped before the first answer · / edits the filter".to_string()
            }
            (None, Load::Failed { .. }) => {
                "the query failed · / edits the filter · r reads again".to_string()
            }
            (None, Load::Done | Load::Pending { .. }) => return None,
        };
        Some(note)
    }

    /// How fsq's renderer writes the values of this browser's documents: without colour, which
    /// the browser adds itself, and with names and references relative to the database.
    fn value_view(&self) -> View<'_> {
        View {
            theme: &PLAIN_THEME,
            format: OutputFormat::Table,
            expanded: ExpandedMode::Off,
            width: None,
            documents_path: self.documents_path(),
        }
    }

    fn pane_block(&self, pane: Pane, title: Line<'static>) -> Block<'static> {
        let border = if self.focus() == pane {
            Style::new().cyan()
        } else {
            Style::new().dim()
        };
        Block::bordered().title(title).border_style(border)
    }

    /// The selected row of `pane`, besides its marker. In the focused pane it is a bar in the
    /// terminal's own colours, reversed, without the hue, dim, italic and underline of its cells,
    /// so that it reads the same across the row.
    fn cursor_style(&self, pane: Pane) -> Style {
        if self.focus() == pane {
            Style::new()
                .fg(Color::Reset)
                .bg(Color::Reset)
                .add_modifier(Modifier::REVERSED)
                .remove_modifier(Modifier::DIM | Modifier::ITALIC | Modifier::UNDERLINED)
        } else {
            Style::new()
        }
    }
}

/// The first of `len` rows to draw in `height` lines, from `offset` in the frame before. It
/// keeps [`BROWSE_SCROLL_PADDING`] rows on either side of the cursor where there are rows, and
/// moves from `offset` only as far as that needs, or as far back as fills the lines. The row at
/// `gap` takes a blank line above it.
fn scrolled(offset: usize, cursor: usize, len: usize, height: usize, gap: Option<usize>) -> usize {
    let Some(last) = len.checked_sub(1) else {
        return 0;
    };
    let lines = |first: usize, end: usize| {
        end + 1 - first + usize::from(gap.is_some_and(|gap| first <= gap && gap <= end))
    };
    let padding = BROWSE_SCROLL_PADDING.min(height.saturating_sub(1) / 2);
    let below = cursor.saturating_add(padding).min(last);
    let mut first = offset.min(cursor.saturating_sub(padding));
    while first < cursor && lines(first, below) > height {
        first += 1;
    }
    while first > 0 && lines(first - 1, last) <= height {
        first -= 1;
    }
    first
}

/// The title of a pane: its name and a count, then what it is doing after a `·`, with a space at
/// either end.
fn pane_title(
    name: Option<&'static str>,
    count: Option<String>,
    state: Option<Span<'static>>,
) -> Line<'static> {
    let head: Vec<String> = name.map(String::from).into_iter().chain(count).collect();
    let mut spans = vec![Span::from(format!(" {}", head.join(" ")))];
    if let Some(state) = state {
        if !head.is_empty() {
            spans.push(Span::from(" · "));
        }
        spans.push(state);
    }
    spans.push(Span::from(" "));
    Line::from(spans)
}

/// fsq's spinner and what the browser waits for, then how long it has waited once that is a
/// second or more.
struct WaitLabel {
    what: &'static str,
    waited: Duration,
}

impl std::fmt::Display for WaitLabel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let tick = self.waited.as_millis() / ANIMATION_TICK.as_millis();
        let spinner = ProgressStyle::default_spinner();
        let frame = spinner.get_tick_str(u64::try_from(tick).unwrap_or(u64::MAX));
        write!(f, "{frame} {}", self.what)?;
        match self.waited.as_secs() {
            0 => Ok(()),
            seconds => write!(f, " {seconds} s"),
        }
    }
}

/// The widths of the columns of the tree: the names, the types and the values, after the
/// selection marker and [`TREE_COLUMN_GAP`] apart.
struct TreeColumns {
    name: usize,
    types: bool,
    value: usize,
    /// The whole row, which the rows of subcollections take.
    row: usize,
}

impl TreeColumns {
    /// The columns of the tree of `document` in a pane `pane_width` wide. The name column fits
    /// the widest field name the document has at any depth, whatever is expanded, so that the
    /// values stay put while rows expand and from one document to another of the same shape. It
    /// takes at most a third of the pane.
    fn new(document: &FirestoreDocument, pane_width: u16) -> Self {
        let room = usize::from(pane_width).saturating_sub(2 + Span::from(SELECTION_MARKER).width());
        let widest = Tree::widest_field(document, RowName::indent_width);
        let name = widest.min(room / 3);
        let types = pane_width >= TREE_PANE_MIN_WIDTH_WITH_TYPES;
        let taken = name
            + TREE_COLUMN_GAP
            + if types {
                TREE_TYPE_WIDTH + TREE_COLUMN_GAP
            } else {
                0
            };
        Self {
            name,
            types,
            value: room.saturating_sub(taken),
            row: room,
        }
    }

    fn widths(&self) -> Vec<Constraint> {
        let length =
            |columns: usize| Constraint::Length(u16::try_from(columns).unwrap_or(u16::MAX));
        let mut widths = vec![length(self.name)];
        if self.types {
            widths.push(length(TREE_TYPE_WIDTH));
        }
        widths.push(Constraint::Fill(1));
        widths
    }

    fn column_count(&self) -> u16 {
        if self.types {
            3
        } else {
            2
        }
    }

    /// The cells of `row`, each cut to its column with a `…`. The subcollections and their group
    /// take the whole row, apart from the columns of the fields. `listing_waited` is how long the
    /// subcollections have been listed for, while they are.
    fn row(
        &self,
        row: &Row<'_>,
        view: &View<'_>,
        listing_waited: Option<Duration>,
    ) -> TableRow<'static> {
        let (kind, value, value_style) = match row {
            // What an expanded map or array holds shows on the rows under it.
            Row::Field {
                value,
                fold: Fold::Expanded,
                ..
            } => (type_name(value), String::new(), Style::new()),
            // The value says null, and the type would say it again.
            Row::Field { value, .. }
                if matches!(value.value_type, None | Some(ValueType::NullValue(_))) =>
            {
                ("", view.leaf_value(value), value_style(value))
            }
            Row::Field { value, .. } => {
                (type_name(value), view.leaf_value(value), value_style(value))
            }
            Row::Subcollections { listing, .. } => {
                return self.group_row(row, listing, listing_waited);
            }
            Row::Subcollection(_) => {
                let name = RowName::of(row).cut(self.row);
                return TableRow::new([Cell::from(name).column_span(self.column_count())]);
            }
        };
        let name = RowName::of(row).cut(self.name);
        let value = console::truncate_str(&value, self.value, "…").into_owned();
        let mut cells = vec![Cell::from(name)];
        if self.types {
            cells.push(Cell::from(kind).dim());
        }
        cells.push(Cell::from(value).style(value_style));
        TableRow::new(cells)
    }

    /// The row of the group of subcollections: its name in italics and what its listing is
    /// doing, a line apart from the fields, one of which can be named `subcollections` too.
    fn group_row(
        &self,
        row: &Row<'_>,
        listing: &Listing,
        listing_waited: Option<Duration>,
    ) -> TableRow<'static> {
        let state = match (listing, listing_waited) {
            (Listing::Pending(_), Some(waited)) => {
                let listing = WaitLabel {
                    what: "listing",
                    waited,
                };
                Some(Span::from(listing.to_string()).yellow())
            }
            (Listing::NotListed | Listing::Pending(_), _) => None,
            (Listing::Listed(collection_ids), _) => {
                let count = counted(collection_ids.len(), "collection", "collections");
                Some(Span::from(count).dim())
            }
            (Listing::Failed(_), _) => Some(Span::from("failed").red()),
        };
        let name = RowName::of(row);
        let state_width = state
            .as_ref()
            .map_or(0, |state| TREE_COLUMN_GAP + state.width());
        let name = name.cut(self.row.saturating_sub(state_width));
        let mut spans = vec![Span::from(name).italic()];
        if let Some(state) = state {
            spans.push(Span::from(" ".repeat(TREE_COLUMN_GAP)));
            spans.push(state);
        }
        TableRow::new([Cell::from(Line::from(spans)).column_span(self.column_count())])
            .top_margin(1)
    }
}

/// The name of a row as the tree shows it: two columns of indent a level, the fold marker and a
/// space, then the field name, the position in the array or the collection ID.
struct RowName {
    depth: usize,
    fold: Fold,
    label: String,
}

impl RowName {
    fn of(row: &Row<'_>) -> Self {
        match row {
            Row::Field { path, fold, .. } => Self {
                depth: path.depth(),
                fold: *fold,
                label: path.last().map(ToString::to_string).unwrap_or_default(),
            },
            Row::Subcollections { fold, .. } => Self {
                depth: row.depth(),
                fold: *fold,
                label: "subcollections".to_string(),
            },
            Row::Subcollection(collection_id) => Self {
                depth: row.depth(),
                fold: Fold::Collapsed,
                label: collection_id.to_string(),
            },
        }
    }

    /// The columns before the label of a name `depth` levels down.
    fn indent_width(depth: usize) -> usize {
        2 * depth + 2
    }

    /// This name in at most `width` columns, the label cut in the middle when it does not fit,
    /// and the indent too when the label would keep less than two columns.
    fn cut(&self, width: usize) -> String {
        let indent = format!("{}{} ", "  ".repeat(self.depth), self.fold.marker());
        match width.saturating_sub(Self::indent_width(self.depth)) {
            room if room >= 2 => format!("{indent}{}", cut_middle(&self.label, room)),
            _ => cut_middle(&format!("{indent}{}", self.label), width),
        }
    }
}

impl Fold {
    fn marker(self) -> char {
        match self {
            Self::Leaf => ' ',
            Self::Collapsed => '▸',
            Self::Expanded => '▾',
        }
    }
}

/// Takes every hue out of `buffer` and keeps bold, dim, reverse and the other modifiers, for a
/// terminal where colour is off.
pub fn drop_hues(buffer: &mut Buffer) {
    for cell in &mut buffer.content {
        cell.fg = Color::Reset;
        cell.bg = Color::Reset;
    }
}

fn document_id(document: &FirestoreDocument) -> &str {
    document.name.rsplit('/').next().unwrap_or_default()
}

/// The columns the longest ID of `documents` takes.
fn longest_id(documents: &[FirestoreDocument]) -> usize {
    documents
        .iter()
        .map(|document| Span::from(document_id(document)).width())
        .max()
        .unwrap_or_default()
}

/// The colour of a value, as [`Theme::new`] gives it in a table.
fn value_style(value: &Value) -> Style {
    match &value.value_type {
        None | Some(ValueType::NullValue(_)) => Style::new().dim().italic(),
        Some(ValueType::BooleanValue(_)) => Style::new().yellow(),
        Some(
            ValueType::IntegerValue(_) | ValueType::DoubleValue(_) | ValueType::GeoPointValue(_),
        ) => Style::new().cyan(),
        Some(ValueType::StringValue(_)) => Style::new().green(),
        Some(ValueType::TimestampValue(_)) => Style::new().magenta(),
        Some(ValueType::ReferenceValue(_)) => Style::new().blue().underlined(),
        Some(_) => Style::new().dim(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::browse::editor::Edit;
    use crate::browse::fixtures::{books, browser_of, opened};
    use crate::browse::pressed;
    use crate::browse::state::{Effect, Message, Movement, RequestId};
    use crate::errors::FsqError;
    use crate::render::fixtures::{document, draft, dune, string, value};
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use firestore::errors::FirestoreError;
    use firestore::gcloud_sdk::tonic::{Code, Status};
    use ratatui::backend::TestBackend;
    use ratatui::layout::Position;
    use ratatui::style::{Color, Modifier};
    use ratatui::Terminal;
    use std::time::Duration;

    const LONG_FILTER: &str = "WHERE genre = 'Science fiction' AND pages BETWEEN 200 AND 900 \
        AND language = 'English' AND in_print = true ORDER BY rating DESC, published DESC, title";

    /// A terminal the browser is drawn on, at a time that moves only when a test says so.
    struct TestScreen {
        terminal: Terminal<TestBackend>,
        viewport: Viewport,
        now: Instant,
    }

    impl TestScreen {
        fn new(width: u16, height: u16) -> Self {
            Self {
                terminal: Terminal::new(TestBackend::new(width, height)).expect("a test terminal"),
                viewport: Viewport::default(),
                now: Instant::now(),
            }
        }

        /// Draws `browser` and returns the lines of the frame.
        fn draw(&mut self, browser: &Browser) -> Vec<String> {
            self.terminal
                .draw(|frame| browser.draw(frame, &mut self.viewport, self.now))
                .expect("a frame");
            let buffer = self.terminal.backend().buffer();
            (0..buffer.area.height)
                .map(|y| {
                    (0..buffer.area.width)
                        .map(|x| buffer[(x, y)].symbol())
                        .collect()
                })
                .collect()
        }

        fn cursor(&mut self) -> Position {
            self.terminal.get_cursor_position().expect("a cursor")
        }

        /// Presses `code` in `browser` as the terminal would send it, after a frame that sizes
        /// the pages, and draws the frame after it.
        fn press(&mut self, browser: &mut Browser, code: KeyCode) -> Vec<String> {
            self.press_with(browser, code, KeyModifiers::NONE)
        }

        fn press_with(
            &mut self,
            browser: &mut Browser,
            code: KeyCode,
            modifiers: KeyModifiers,
        ) -> Vec<String> {
            self.draw(browser);
            pressed(browser, &mut self.viewport, KeyEvent::new(code, modifiers));
            self.draw(browser)
        }

        fn cell(&self, x: u16, y: u16) -> &ratatui::buffer::Cell {
            &self.terminal.backend().buffer()[(x, y)]
        }
    }

    /// The column of `text` in the line that holds `row`.
    fn column_of(lines: &[String], row: &str, text: &str) -> usize {
        let line = line_with(lines, row);
        let byte = line
            .find(text)
            .unwrap_or_else(|| panic!("no `{text}` in {line}"));
        line[..byte].chars().count()
    }

    fn row_of(lines: &[String], text: &str) -> u16 {
        let row = lines
            .iter()
            .position(|line| line.contains(text))
            .unwrap_or_else(|| panic!("no `{text}` in\n{}", lines.join("\n")));
        u16::try_from(row).expect("a row of the screen")
    }

    /// A browser of `books` that has read `documents`, with the cursor in the tree of the first
    /// one, on the field `name`.
    fn in_tree_at(documents: Vec<FirestoreDocument>, name: &str) -> Browser {
        let mut browser = browser_of("books", "", documents);
        browser.update(Message::Expand);
        let level = browser.level();
        let document = level.selected_document().expect("a document");
        let row = level
            .tree()
            .rows(document)
            .iter()
            .position(|row| matches!(row, Row::Field { path, .. } if path.depth() == 0 && path.last().is_some_and(|last| last.to_string() == name)))
            .unwrap_or_else(|| panic!("no field {name}"));
        browser.update(Message::Move(Movement::First));
        browser.update(Message::Move(Movement::Down(row)));
        browser
    }

    /// The request of the query the browser waits for.
    fn query_of(browser: &Browser) -> RequestId {
        match browser.level().load() {
            Load::Pending { request, .. } => *request,
            other => panic!("no query pending: {other:?}"),
        }
    }

    fn edit_filter(browser: &mut Browser, text: &str) {
        browser.update(Message::EditFilter);
        browser.update(Message::Edit(Edit::DeleteToStart));
        browser.update(Message::Paste(text.to_string()));
    }

    fn line_with<'a>(lines: &'a [String], text: &str) -> &'a str {
        lines
            .iter()
            .find(|line| line.contains(text))
            .unwrap_or_else(|| panic!("no `{text}` in\n{}", lines.join("\n")))
    }

    fn firestore_error(message: &str) -> FsqError {
        FirestoreError::from(Status::new(Code::InvalidArgument, message)).into()
    }

    #[test]
    fn the_documents_and_the_tree_of_the_selected_one_show_side_by_side() {
        let mut browser = browser_of("books", "WHERE pages > 100", vec![draft(), dune()]);
        browser.update(Message::Move(Movement::Down(1)));
        browser.update(Message::Expand);
        browser.update(Message::Move(Movement::First));
        browser.update(Message::Expand);

        let lines = TestScreen::new(80, 24).draw(&browser);

        assert!(lines[0].starts_with("books  WHERE pages > 100"));
        let titles = line_with(&lines, "┌ dune ");
        assert!(titles.starts_with("┌ 2/2 ─"));
        assert!(!titles.contains("books/dune"));
        assert!(line_with(&lines, "▾ address").contains("draft"));
        assert!(line_with(&lines, "London").contains("city"));
        assert!(line_with(&lines, "▸ tags").contains("[2 items]"));
        line_with(&lines, "▸ subcollections");
    }

    #[test]
    fn a_string_longer_than_a_table_cell_shows_whole_in_a_wide_pane() {
        let note = format!("start-{}-end", "x".repeat(90));
        let browser = browser_of(
            "books",
            "",
            vec![document("noted", vec![("note", string(&note))])],
        );

        let lines = TestScreen::new(200, 12).draw(&browser);

        assert!(lines.iter().any(|line| line.contains(&note)));
    }

    #[test]
    fn the_header_keeps_the_breadcrumb_and_cuts_the_filter_in_the_middle() {
        let browser = browser_of("books/dune/reviews", LONG_FILTER, vec![draft()]);

        let lines = TestScreen::new(80, 24).draw(&browser);

        let header = lines[0].trim_end();
        assert!(header.starts_with("books/dune/reviews  WHERE genre = "));
        assert!(header.ends_with("published DESC, title LIMIT 100"));
        assert_eq!(header.matches('…').count(), 1);
        assert!(!header.contains("document"));
    }

    #[test]
    fn the_documents_title_shows_the_position_and_whether_more_documents_may_exist() {
        let mut browser = browser_of("books", "", books(100));
        for _ in 1..58 {
            browser.update(Message::Move(Movement::Down(1)));
        }
        let lines = TestScreen::new(80, 24).draw(&browser);
        assert!(lines[1].starts_with("┌ 58/100+ ─"));

        let browser = browser_of("books", "WHERE pages > 100", books(7));
        let lines = TestScreen::new(80, 24).draw(&browser);
        assert!(lines[1].starts_with("┌ 1/7 ─"));

        let browser = browser_of("books", "ORDER BY title LIMIT 7", books(7));
        let lines = TestScreen::new(80, 24).draw(&browser);
        assert!(lines[1].starts_with("┌ 1/7+ ─"));
    }

    #[test]
    fn a_pending_load_shows_how_long_it_has_waited_and_no_count_before_the_first_answer() {
        let (browser, _) = opened("books", "");
        let mut screen = TestScreen::new(80, 24);

        let first = screen.draw(&browser);
        screen.now += Duration::from_millis(3_200);
        let later = screen.draw(&browser);

        let title = first[1].split('┐').next().unwrap_or_default();
        assert!(title.contains("loading"));
        assert!(!title.chars().any(|character| character.is_ascii_digit()));
        assert!(later[1].contains("loading 3 s"));
        assert!(!first[0].contains("loading"));
        assert_eq!(
            first.iter().filter(|line| line.contains("loading")).count(),
            1
        );
    }

    #[test]
    fn an_empty_result_says_what_to_do() {
        let filtered = browser_of("books", "WHERE pages > 10000", Vec::new());
        let lines = TestScreen::new(80, 24).draw(&filtered);
        assert!(line_with(&lines, "no documents match").contains("/ edits the filter"));

        let unfiltered = browser_of("books", "", Vec::new());
        let lines = TestScreen::new(80, 24).draw(&unfiltered);
        line_with(&lines, "no documents in books");
        assert!(!lines.iter().any(|line| line.contains("no documents match")));
    }

    #[test]
    fn a_stop_is_a_note_and_not_an_error() {
        let mut browser = browser_of("books", "", vec![draft(), dune()]);
        edit_filter(&mut browser, "WHERE pages > 100");
        browser.update(Message::RunFilter);
        browser.update(Message::Interrupt);
        let mut screen = TestScreen::new(80, 24);

        let lines = screen.draw(&browser);

        assert!(lines[1].contains("stopped"));
        assert!(!lines.iter().any(|line| line.contains("error")));
        let buffer = screen.terminal.backend().buffer();
        assert!(buffer.content.iter().all(|cell| cell.fg != Color::Red));
    }

    #[test]
    fn a_failed_filter_stays_in_the_editor_with_its_error_wrapped_whole() {
        let mut browser = browser_of("books", "", vec![draft(), dune()]);
        edit_filter(&mut browser, "WHERE pagez > 100 ORDER BY title");
        browser.update(Message::RunFilter);
        let error = FsqError::Io(std::io::Error::other(
            "Unrecognized field name 'pagez' in the filter of books; \
             no document has the field at [2:7]",
        ));
        let shown = format!("error: {error}");
        browser.update(Message::DocumentsLoaded {
            request: query_of(&browser),
            outcome: Err(error),
        });

        let lines = TestScreen::new(50, 20).draw(&browser);

        let editor = lines
            .iter()
            .position(|line| line.starts_with("/ "))
            .unwrap_or_else(|| panic!("no editor in\n{}", lines.join("\n")));
        assert!(lines[editor].contains("BY title"));
        assert_eq!(
            lines[editor + 1..]
                .join(" ")
                .split_whitespace()
                .collect::<Vec<_>>(),
            shown.split_whitespace().collect::<Vec<_>>()
        );
        line_with(&lines, "dune");
    }

    #[test]
    fn esc_on_a_failed_new_filter_hides_its_error_and_keeps_the_failed_title_and_ctrl_g() {
        let mut browser = browser_of("books", "", vec![draft(), dune()]);
        edit_filter(&mut browser, "WHERE pagez > 100");
        browser.update(Message::RunFilter);
        browser.update(Message::DocumentsLoaded {
            request: query_of(&browser),
            outcome: Err(firestore_error("Unrecognized field name 'pagez'")),
        });
        browser.update(Message::CancelFilter);

        let lines = TestScreen::new(80, 24).draw(&browser);

        assert!(lines[1].contains("failed"), "{}", lines[1]);
        let keys = lines[23].trim_end();
        assert!(
            keys.ends_with("q quit") && keys.contains("Ctrl-G"),
            "{keys}"
        );
        line_with(&lines, "dune");
    }

    #[test]
    fn a_failed_listing_shows_its_error_under_the_footer() {
        let mut browser = browser_of("books", "", vec![dune()]);
        browser.update(Message::Expand);
        browser.update(Message::Move(Movement::Last));
        let Some(Effect::ListSubcollections { request, .. }) = browser.update(Message::Expand)
        else {
            panic!("no listing")
        };
        let error = firestore_error("Missing or insufficient permissions to list collections");
        let shown = format!("error: {error}");
        browser.update(Message::SubcollectionsListed {
            request,
            outcome: Err(error),
        });

        let lines = TestScreen::new(60, 20).draw(&browser);

        let footer = lines
            .iter()
            .position(|line| line.trim_end().ends_with("q quit"))
            .unwrap_or_else(|| panic!("no footer in\n{}", lines.join("\n")));
        assert_eq!(
            lines[footer + 1..]
                .join(" ")
                .split_whitespace()
                .collect::<Vec<_>>(),
            shown.split_whitespace().collect::<Vec<_>>()
        );
    }

    #[test]
    fn a_failed_first_read_leaves_the_keys_and_shows_its_error_under_them() {
        let (mut browser, query) = opened("books", "");
        let error = firestore_error("The database is not reachable from here");
        let shown = format!("error: {error}");
        browser.update(Message::DocumentsLoaded {
            request: query.request,
            outcome: Err(error),
        });

        let lines = TestScreen::new(60, 20).draw(&browser);

        let footer = lines
            .iter()
            .position(|line| line.trim_end().ends_with("q quit"))
            .unwrap_or_else(|| panic!("no keys in\n{}", lines.join("\n")));
        assert_eq!(
            lines[footer + 1..]
                .join(" ")
                .split_whitespace()
                .collect::<Vec<_>>(),
            shown.split_whitespace().collect::<Vec<_>>()
        );
    }

    #[test]
    fn a_long_filter_keeps_the_cursor_in_view() {
        for width in [80, 60] {
            let mut browser = browser_of("books", "", vec![draft()]);
            edit_filter(&mut browser, LONG_FILTER);
            let mut screen = TestScreen::new(width, 20);

            let at_end = screen.draw(&browser);
            let cursor = screen.cursor();

            let editor = &at_end[19];
            assert!(editor.starts_with("/ …"), "{editor}");
            assert_eq!(cursor.y, 19);
            let before_cursor: String = editor.chars().take(usize::from(cursor.x)).collect();
            assert!(before_cursor.ends_with("published DESC, title"), "{editor}");

            browser.update(Message::Edit(Edit::Start));
            let at_start = screen.draw(&browser);
            assert!(
                at_start[19].starts_with("/ WHERE genre"),
                "{}",
                at_start[19]
            );
            assert_eq!(screen.cursor(), Position::new(2, 19));
            assert!(at_start[19].contains('…'));
        }
    }

    #[test]
    fn the_editor_shows_the_limit_100_only_while_the_filter_has_no_limit() {
        let mut browser = browser_of("books", "", vec![draft()]);
        let mut screen = TestScreen::new(80, 24);

        browser.update(Message::EditFilter);
        let empty = screen.draw(&browser);
        assert!(empty[23].starts_with("/ WHERE … ORDER BY … LIMIT …"));
        assert!(!empty[23].contains("LIMIT 100"));

        edit_filter(&mut browser, "WHERE pages > 100");
        let without_limit = screen.draw(&browser);
        assert!(without_limit[23].starts_with("/ WHERE pages > 100  LIMIT 100"));
        assert_eq!(screen.cursor(), Position::new(19, 23));

        edit_filter(&mut browser, "WHERE pages > 100 LIMIT 5");
        let with_limit = screen.draw(&browser);
        assert!(!with_limit[23].contains("LIMIT 100"));
    }

    #[test]
    fn the_key_hints_keep_q_quit_on_a_narrow_screen() {
        let browser = in_tree_at(vec![dune()], "title");

        let lines = TestScreen::new(40, 12).draw(&browser);

        assert!(lines[11].trim_end().ends_with("q quit"), "{}", lines[11]);
    }

    #[test]
    fn enter_on_a_leaf_shows_its_value_whole_and_esc_closes_it() {
        let mut browser = in_tree_at(vec![dune()], "blurb");
        let mut screen = TestScreen::new(80, 24);
        let before = screen.draw(&browser);
        assert!(!before.iter().any(|line| line.contains("and a duke's son")));

        let shown = screen.press(&mut browser, KeyCode::Enter);

        assert!(line_with(&shown, "┌ blurb · string ").contains("60 chars"));
        line_with(&shown, "A desert planet, a spice that extends life,");
        line_with(&shown, "and a duke's son");
        let closed = screen.press(&mut browser, KeyCode::Esc);
        assert!(browser.popup().is_none());
        assert!(!closed.iter().any(|line| line.contains("and a duke's son")));
    }

    #[test]
    fn a_popup_takes_every_key_but_esc() {
        let mut browser = in_tree_at(vec![dune()], "blurb");
        let mut screen = TestScreen::new(80, 24);
        screen.press(&mut browser, KeyCode::Enter);

        for code in [
            KeyCode::Char('q'),
            KeyCode::Char('/'),
            KeyCode::Tab,
            KeyCode::Left,
        ] {
            screen.press(&mut browser, code);
        }

        assert!(browser.popup().is_some());
        assert!(browser.filter_editor().is_none());
        assert_eq!(browser.focus(), Pane::Tree);
    }

    #[test]
    fn bytes_show_as_hex() {
        let mut browser = in_tree_at(vec![dune()], "cover");
        let mut screen = TestScreen::new(80, 24);

        let shown = screen.press(&mut browser, KeyCode::Enter);

        assert!(line_with(&shown, "01 02 03 ff").contains("00000000"));
    }

    #[test]
    fn an_open_map_shows_whole_as_indented_text() {
        let mut browser = in_tree_at(vec![dune()], "address");
        let mut screen = TestScreen::new(80, 24);
        screen.press(&mut browser, KeyCode::Enter);
        assert!(browser.popup().is_none());

        let shown = screen.press(&mut browser, KeyCode::Enter);

        line_with(&shown, r#"  "city": "London","#);
        line_with(&shown, r#"  "zip": "N1 9GU""#);
    }

    #[test]
    fn a_long_value_scrolls_in_its_popup() {
        let text: Vec<String> = (1..=40).map(|line| format!("line {line:02}")).collect();
        let noted = document("noted", vec![("note", string(&text.join("\n")))]);
        let mut browser = in_tree_at(vec![noted], "note");
        let mut screen = TestScreen::new(80, 24);

        let top = screen.press(&mut browser, KeyCode::Enter);
        let down = screen.press(&mut browser, KeyCode::Down);
        let end = screen.press(&mut browser, KeyCode::Char('G'));
        let past_end = screen.press(&mut browser, KeyCode::PageDown);
        let up = screen.press(&mut browser, KeyCode::Up);
        let start = screen.press(&mut browser, KeyCode::Char('g'));

        let first_line = |lines: &[String]| row_of(lines, "line ");
        assert_eq!(first_line(&top), first_line(&down));
        assert!(line_with(&top, "line 01").contains("│line 01"));
        assert!(!down.iter().any(|line| line.contains("line 01")));
        line_with(&down, "line 02");
        line_with(&end, "line 40");
        assert_eq!(end, past_end);
        assert!(!up.iter().any(|line| line.contains("line 40")));
        assert_eq!(start, top);
    }

    #[test]
    fn ctrl_g_shows_the_whole_error_in_the_editor_and_in_the_panes() {
        let long = format!(
            "The query requires an index. {} and then run it again",
            "x".repeat(400)
        );
        let mut browser = browser_of("books", "", vec![dune()]);
        let mut screen = TestScreen::new(80, 24);
        assert!(!line_with(&screen.draw(&browser), "q quit").contains("Ctrl-G"));
        edit_filter(&mut browser, "WHERE pagez > 100");
        browser.update(Message::RunFilter);
        browser.update(Message::DocumentsLoaded {
            request: query_of(&browser),
            outcome: Err(firestore_error(&long)),
        });

        let editor = screen.draw(&browser);
        assert!(line_with(&editor, "/ WHERE pagez").contains("Ctrl-G"));
        assert!(!editor.iter().any(|line| line.contains("run it again")));
        let in_editor = screen.press_with(&mut browser, KeyCode::Char('g'), KeyModifiers::CONTROL);
        line_with(&in_editor, "run it again");
        screen.press(&mut browser, KeyCode::Esc);
        assert_eq!(
            browser.filter_editor().map(|editor| editor.text()),
            Some("WHERE pagez > 100")
        );

        let mut browser = browser_of("books", "", vec![dune()]);
        browser.update(Message::Expand);
        browser.update(Message::Move(Movement::Last));
        let Some(Effect::ListSubcollections { request, .. }) = browser.update(Message::Expand)
        else {
            panic!("no listing")
        };
        browser.update(Message::SubcollectionsListed {
            request,
            outcome: Err(firestore_error(&long)),
        });
        let panes = screen.draw(&browser);
        assert!(line_with(&panes, "q quit").contains("Ctrl-G"));
        let in_panes = screen.press_with(&mut browser, KeyCode::Char('g'), KeyModifiers::CONTROL);
        line_with(&in_panes, "run it again");
    }

    #[test]
    fn the_bottom_border_of_the_tree_shows_the_path_of_the_cursor() {
        let mut browser = in_tree_at(vec![dune()], "tags");
        let mut screen = TestScreen::new(80, 24);
        screen.press(&mut browser, KeyCode::Right);

        let lines = screen.press(&mut browser, KeyCode::Down);

        assert!(line_with(&lines, "└ tags[0] ─").ends_with("┘"));
        let at_location = in_tree_at(vec![dune()], "location");
        line_with(&screen.draw(&at_location), "└ `location` ─");
    }

    #[test]
    fn the_cursor_is_one_bar_without_the_colours_and_dim_of_its_cells() {
        for field in ["author", "subtitle"] {
            let browser = in_tree_at(vec![dune()], field);
            let mut screen = TestScreen::new(80, 24);
            let lines = screen.draw(&browser);

            let y = row_of(&lines[2..], &format!(" {field} ")) + 2;
            let tree_x = u16::try_from(column_of(&lines, "┌ dune", "┌ dune")).expect("a column");
            for x in tree_x + 2..79 {
                let cell = screen.cell(x, y);
                assert_eq!(cell.fg, Color::Reset, "{field} at {x}");
                assert!(cell.modifier.contains(Modifier::REVERSED), "{field} at {x}");
                assert!(
                    !cell
                        .modifier
                        .intersects(Modifier::DIM | Modifier::ITALIC | Modifier::UNDERLINED),
                    "{field} at {x}"
                );
            }
        }
    }

    #[test]
    fn the_values_stay_put_while_rows_expand_and_between_documents_of_one_shape() {
        let mut sequel = dune();
        sequel.name = sequel.name.replace("dune", "dune-messiah");
        sequel
            .fields
            .insert("title".to_string(), string("Dune Messiah"));
        let mut browser = in_tree_at(vec![dune(), sequel], "address");
        let mut screen = TestScreen::new(80, 24);

        let collapsed = screen.draw(&browser);
        let expanded = screen.press(&mut browser, KeyCode::Right);
        browser.update(Message::SwitchPane);
        let other = screen.press(&mut browser, KeyCode::Down);

        let value_column = column_of(&collapsed, " title ", "Dune");
        assert_eq!(column_of(&expanded, " title ", "Dune"), value_column);
        assert_eq!(column_of(&expanded, " city ", "London"), value_column);
        assert_eq!(column_of(&other, " title ", "Dune Messiah"), value_column);
    }

    #[test]
    fn the_cursor_keeps_two_rows_in_view_below_it() {
        let fields: Vec<(String, Value)> = (0..40)
            .map(|number| (format!("field_{number:02}"), string("value")))
            .collect();
        let many = document(
            "many",
            fields
                .iter()
                .map(|(name, field)| (name.as_str(), field.clone()))
                .collect(),
        );
        let mut browser = in_tree_at(vec![many], "field_00");
        let mut screen = TestScreen::new(80, 12);

        for _ in 0..12 {
            screen.press(&mut browser, KeyCode::Down);
        }
        let lines = screen.draw(&browser);

        let bottom = lines
            .iter()
            .rposition(|line| line.starts_with('│') && line.contains("field_"))
            .expect("a field row");
        assert!(lines[bottom].contains("field_14"), "{}", lines.join("\n"));
    }

    /// A book with the ID `id` and the rating `rating`.
    fn rated(id: &str, rating: f64) -> FirestoreDocument {
        document(
            id,
            vec![
                ("title", string("t")),
                ("rating", value(ValueType::DoubleValue(rating))),
            ],
        )
    }

    /// The text inside the documents pane on `line`.
    fn in_documents_pane(line: &str) -> String {
        line.chars().skip(1).take_while(|&c| c != '│').collect()
    }

    #[test]
    fn an_order_by_shows_the_value_of_its_first_key_dim_after_each_id() {
        let ordered = browser_of(
            "books",
            "ORDER BY rating DESC",
            vec![rated("dune", 4.5), rated("emma", 3.25)],
        );
        let mut screen = TestScreen::new(80, 24);

        let lines = screen.draw(&ordered);

        assert_eq!(in_documents_pane(&lines[2]).trim_end(), "▌dune  4.5");
        assert_eq!(in_documents_pane(&lines[3]), " emma  3.25");
        let value_x = u16::try_from(column_of(&lines[3..], "emma", "3.25")).expect("a column");
        assert!(screen.cell(value_x, 3).modifier.contains(Modifier::DIM));
        assert!(!screen.cell(2, 3).modifier.contains(Modifier::DIM));
    }

    #[test]
    fn order_values_narrower_than_the_minimum_width_show_whole() {
        let ordered = browser_of(
            "books",
            "ORDER BY rating DESC",
            vec![
                rated("dune", 4.5),
                rated("emma", 4.3),
                rated("foundation", 4.0),
            ],
        );

        let lines = TestScreen::new(80, 24).draw(&ordered);

        for (line, rating) in [(2, "4.5"), (3, "4.3"), (4, "4.0")] {
            let shown = in_documents_pane(&lines[line]);
            assert!(shown.trim_end().ends_with(rating), "{shown}");
        }
    }

    #[test]
    fn the_order_column_is_left_out_without_an_order_by_and_in_a_narrow_pane() {
        let documents = || vec![rated("dune", 4.5), rated("emma", 3.25)];
        let unordered = browser_of("books", "WHERE rating > 3", documents());
        let lines = TestScreen::new(80, 24).draw(&unordered);
        assert_eq!(in_documents_pane(&lines[2]).trim_end(), "▌dune");

        let long_ids = vec![
            rated("Zk3QmX9aB7cD2eF4gH57", 4.5),
            rated("Zk3QmX9aB7cD2eF4gH58", 3.25),
        ];
        let ordered = browser_of("books", "ORDER BY rating", long_ids);
        let wide = TestScreen::new(120, 24).draw(&ordered);
        let narrow = TestScreen::new(80, 24).draw(&ordered);
        assert!(
            in_documents_pane(&wide[2]).trim_end().ends_with("4.5"),
            "{}",
            wide[2]
        );
        assert_eq!(in_documents_pane(&narrow[2]), "▌Zk3QmX9aB7cD2eF4gH57");
    }
}
