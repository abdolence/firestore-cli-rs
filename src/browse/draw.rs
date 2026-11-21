use super::state::{Browser, Load, Pane, RequestId, BROWSE_DOCUMENT_LIMIT};
use super::tree::{Fold, Listing, Row};
use crate::render::{
    counted, cut_middle, type_name, ExpandedMode, OutputFormat, Theme, View, ANIMATION_TICK,
};
use firestore::gcloud_sdk::google::firestore::v1::value::ValueType;
use firestore::gcloud_sdk::google::firestore::v1::Value;
use firestore::FirestoreDocument;
use indicatif::ProgressStyle;
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Cell, List, ListItem, ListState, Paragraph, Row as TableRow, Table, TableState, Wrap,
};
use ratatui::Frame;
use std::time::{Duration, Instant};

/// What the browser keeps from one frame to the next: where each list is scrolled to, so that the
/// cursor moves inside the visible rows rather than dragging them along, how many rows a page of
/// the focused pane holds, where the line of the filter editor starts, and the waits on
/// Firestore that are drawn.
#[derive(Debug, Default)]
pub struct Viewport {
    documents: ListState,
    tree: TableState,
    page_rows: usize,
    /// The byte of the filter being edited that its line starts at.
    pub(super) editor_start: usize,
    query_wait: Option<Wait>,
    listing_wait: Option<Wait>,
}

impl Viewport {
    /// The rows of the focused pane in the last frame drawn, at least one.
    pub fn page_rows(&self) -> usize {
        self.page_rows.max(1)
    }
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
    /// selected one side by side, the footer with the keys or the filter editor, and under it the
    /// error of a failed filter or listing.
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
        let [documents, tree] =
            Layout::horizontal([Constraint::Percentage(30), Constraint::Percentage(70)])
                .areas(body);
        frame.render_widget(self.header(usize::from(area.width)), header);
        self.draw_documents(frame, documents, viewport, query_waited);
        self.draw_tree(frame, tree, viewport, listing_waited);
        self.draw_footer(frame, footer, viewport);
        frame.render_widget(Paragraph::new(error), error_area);
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

    fn draw_documents(
        &self,
        frame: &mut Frame,
        area: Rect,
        viewport: &mut Viewport,
        waited: Option<Duration>,
    ) {
        let level = self.level();
        let title = self.documents_title(waited, usize::from(area.width));
        let block = self.pane_block(Pane::Documents, title);
        if self.focus() == Pane::Documents {
            viewport.page_rows = usize::from(block.inner(area).height);
        }
        let items: Vec<ListItem> = level
            .documents()
            .unwrap_or_default()
            .iter()
            .map(|document| ListItem::new(document_id(document)))
            .collect();
        viewport.documents.select(Some(level.selected()));
        frame.render_stateful_widget(
            List::new(items)
                .block(block)
                .highlight_style(self.cursor_style(Pane::Documents)),
            area,
            &mut viewport.documents,
        );
    }

    /// The position of the selected document and the count, once documents have been read and
    /// while no query is pending, and what the query is doing. A `+` after a count that is the
    /// limit says that more documents may exist. The name of the pane goes first when the title
    /// does not fit in `width` columns.
    fn documents_title(&self, waited: Option<Duration>, width: usize) -> Line<'static> {
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
        let named = pane_title(Some("documents"), count.clone(), state.clone());
        if named.width() + 2 <= width {
            named
        } else {
            pane_title(None, count, state)
        }
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
        let block = self.pane_block(Pane::Tree, format!(" {} ", document_id(document)).into());
        if self.focus() == Pane::Tree {
            viewport.page_rows = usize::from(block.inner(area).height);
        }
        let theme = Theme::new(false);
        let view = View {
            theme: &theme,
            format: OutputFormat::Table,
            expanded: ExpandedMode::Off,
            width: None,
            documents_path: self.documents_path(),
        };
        let lines: Vec<TreeLine> = level
            .tree()
            .rows(document)
            .iter()
            .map(|row| TreeLine::new(row, &view, listing_waited))
            .collect();
        let name_width = lines
            .iter()
            .map(|line| Span::from(line.name.as_str()).width())
            .max()
            .unwrap_or_default()
            .min(usize::from(area.width / 2));
        let type_width = lines
            .iter()
            .map(|line| line.type_name.len())
            .max()
            .unwrap_or_default();
        let widths = [
            Constraint::Length(u16::try_from(name_width).unwrap_or(u16::MAX)),
            Constraint::Length(u16::try_from(type_width).unwrap_or(u16::MAX)),
            Constraint::Fill(1),
        ];
        let rows = lines.into_iter().map(|line| {
            TableRow::new([
                Cell::from(line.name),
                Cell::from(line.type_name).dim(),
                Cell::from(line.value).style(line.value_style),
            ])
        });
        viewport.tree.select(Some(level.tree().cursor()));
        frame.render_stateful_widget(
            Table::new(rows, widths)
                .block(block)
                .row_highlight_style(self.cursor_style(Pane::Tree)),
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
            (None, Load::Failed { .. }) => "the query failed · / shows the error".to_string(),
            (None, Load::Done | Load::Pending { .. }) => return None,
        };
        Some(note)
    }

    fn pane_block(&self, pane: Pane, title: Line<'static>) -> Block<'static> {
        let border = if self.focus() == pane {
            Style::new().cyan()
        } else {
            Style::new().dim()
        };
        Block::bordered().title(title).border_style(border)
    }

    /// The selected row of `pane`, which stays marked while the other pane has the focus.
    fn cursor_style(&self, pane: Pane) -> Style {
        if self.focus() == pane {
            Style::new().reversed()
        } else {
            Style::new().bold()
        }
    }
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

/// A row of the tree as the three columns of its pane.
struct TreeLine {
    name: String,
    type_name: &'static str,
    value: String,
    value_style: Style,
}

impl TreeLine {
    /// The columns of `row`. `listing_waited` is how long the subcollections have been listed
    /// for, while they are.
    fn new(row: &Row<'_>, view: &View<'_>, listing_waited: Option<Duration>) -> Self {
        let indent = "  ".repeat(row.depth());
        match row {
            Row::Field { path, value, fold } => Self {
                name: format!(
                    "{indent}{} {}",
                    fold.marker(),
                    path.last().map(ToString::to_string).unwrap_or_default()
                ),
                type_name: type_name(value),
                value: view.leaf_value(value),
                value_style: value_style(value),
            },
            Row::Subcollections { listing, fold } => {
                let (value, value_style) = match (listing, listing_waited) {
                    (Listing::Pending(_), Some(waited)) => (
                        WaitLabel {
                            what: "listing",
                            waited,
                        }
                        .to_string(),
                        Style::new().yellow(),
                    ),
                    (Listing::NotListed | Listing::Pending(_), _) => (String::new(), Style::new()),
                    (Listing::Listed(collection_ids), _) => (
                        counted(collection_ids.len(), "collection", "collections"),
                        Style::new().dim(),
                    ),
                    (Listing::Failed(_), _) => ("failed".to_string(), Style::new().red()),
                };
                Self {
                    name: format!("{indent}{} subcollections", fold.marker()),
                    type_name: "",
                    value,
                    value_style,
                }
            }
            Row::Subcollection(collection_id) => Self {
                name: format!("{indent}{} {collection_id}", Fold::Collapsed.marker()),
                type_name: "collection",
                value: String::new(),
                value_style: Style::new(),
            },
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
    use crate::browse::state::{Effect, Message, Movement, RequestId};
    use crate::browse::{BrowseTarget, FilterTail};
    use crate::errors::FsqError;
    use crate::render::fixtures::{document, draft, dune, string, DOCUMENTS_PATH};
    use firestore::errors::FirestoreError;
    use firestore::gcloud_sdk::tonic::{Code, Status};
    use firestore::FirestoreCollectionId;
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
    }

    /// A browser of `collection` read with `filter`, which has read `documents`.
    fn browser_of(collection: &str, filter: &str, documents: Vec<FirestoreDocument>) -> Browser {
        let mut browser = opened(collection, filter);
        let query = query_of(&browser);
        browser.update(Message::DocumentsLoaded {
            request: query,
            outcome: Ok(documents),
        });
        browser
    }

    /// A browser of `collection` that waits for its first documents.
    fn opened(collection: &str, filter: &str) -> Browser {
        let target = BrowseTarget {
            collection: collection.parse().expect("a collection path"),
            filter: FilterTail::from(filter),
        };
        Browser::open(target, DOCUMENTS_PATH).0
    }

    /// The request of the query the browser waits for.
    fn query_of(browser: &Browser) -> RequestId {
        match browser.level().load() {
            Load::Pending { request, .. } => *request,
            other => panic!("no query pending: {other:?}"),
        }
    }

    fn books(count: usize) -> Vec<FirestoreDocument> {
        (1..=count)
            .map(|number| document(&format!("book-{number:03}"), vec![("title", string("t"))]))
            .collect()
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
        browser.update(Message::Expand);

        let lines = TestScreen::new(80, 24).draw(&browser);

        assert!(lines[0].starts_with("books  WHERE pages > 100"));
        let titles = line_with(&lines, "┌ dune ");
        assert!(titles.contains("documents 2/2"));
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
        assert!(lines[1].starts_with("┌ documents 58/100+ ─"));

        let browser = browser_of("books", "WHERE pages > 100", books(7));
        let lines = TestScreen::new(80, 24).draw(&browser);
        assert!(lines[1].starts_with("┌ documents 1/7 ─"));

        let browser = browser_of("books", "ORDER BY title LIMIT 7", books(7));
        let lines = TestScreen::new(80, 24).draw(&browser);
        assert!(lines[1].starts_with("┌ documents 1/7+ ─"));
    }

    #[test]
    fn a_pending_load_shows_how_long_it_has_waited_and_no_count_before_the_first_answer() {
        let browser = opened("books", "");
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
    fn the_key_hints_drop_by_priority_and_keep_q_quit() {
        let mut browser = browser_of("books", "", vec![dune()]);
        browser.update(Message::Expand);
        browser.update(Message::Move(Movement::Last));
        let Some(Effect::ListSubcollections { request, .. }) = browser.update(Message::Expand)
        else {
            panic!("no listing")
        };
        let reviews: FirestoreCollectionId = "reviews".parse().expect("a collection ID");
        browser.update(Message::SubcollectionsListed {
            request,
            outcome: Ok(vec![reviews]),
        });
        browser.update(Message::Move(Movement::Last));
        browser.update(Message::Expand);

        let wide = TestScreen::new(120, 24).draw(&browser);
        let narrow = TestScreen::new(60, 20).draw(&browser);

        let all = wide[23].trim_end();
        for hint in [
            "↑↓ move",
            "→ open",
            "Tab pane",
            "/ filter",
            "Esc back",
            "Ctrl-C stop",
        ] {
            assert!(all.contains(hint), "no `{hint}` in {all}");
        }
        let kept = narrow[19].trim_end();
        assert!(kept.ends_with("q quit"), "{kept}");
        for hint in ["/ filter", "Esc back", "Ctrl-C stop"] {
            assert!(kept.contains(hint), "no `{hint}` in {kept}");
        }
        assert!(!kept.contains("↑↓ move"), "{kept}");
    }

    #[test]
    fn with_colour_off_the_frame_keeps_its_modifiers_and_drops_every_hue() {
        let mut browser = browser_of("books", "", vec![dune()]);
        browser.update(Message::Expand);
        let mut screen = TestScreen::new(80, 24);
        screen.draw(&browser);
        let mut buffer = screen.terminal.backend().buffer().clone();
        assert!(buffer.content.iter().any(|cell| cell.fg != Color::Reset));

        drop_hues(&mut buffer);

        assert!(buffer
            .content
            .iter()
            .all(|cell| cell.fg == Color::Reset && cell.bg == Color::Reset));
        for modifier in [Modifier::BOLD, Modifier::DIM, Modifier::REVERSED] {
            assert!(buffer
                .content
                .iter()
                .any(|cell| cell.modifier.contains(modifier)));
        }
    }
}
