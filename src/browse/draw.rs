use super::state::{Browser, Load, Pane, BROWSE_DOCUMENT_LIMIT};
use super::tree::{Fold, Listing, Row};
use crate::render::{counted, type_name, ExpandedMode, OutputFormat, Theme, View};
use firestore::gcloud_sdk::google::firestore::v1::value::ValueType;
use firestore::gcloud_sdk::google::firestore::v1::Value;
use firestore::{FirestoreDb, FirestoreDocument};
use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::{Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Cell, List, ListItem, ListState, Paragraph, Row as TableRow, Table, TableState, Wrap,
};
use ratatui::Frame;

/// The most lines the browser gives an error, which can be a long message from Firestore.
const BROWSE_ERROR_LINES: u16 = 4;

/// What the panes keep from one frame to the next: where each list is scrolled to, so that the
/// cursor moves inside the visible rows rather than dragging them along, and how many rows a page
/// of the focused pane holds.
#[derive(Debug, Default)]
pub struct Viewport {
    documents: ListState,
    tree: TableState,
    page_rows: usize,
}

impl Viewport {
    /// The rows of the focused pane in the last frame drawn, at least one.
    pub fn page_rows(&self) -> usize {
        self.page_rows.max(1)
    }
}

impl Browser {
    /// Draws the header with the collection and its filter, the documents and the tree of the
    /// selected one side by side, and the footer with the keys, the filter editor or an error.
    pub fn draw(&self, frame: &mut Frame, viewport: &mut Viewport) {
        let area = frame.area();
        let status = self.status(area.width);
        let status_height = status.as_ref().map_or(0, |status| status.height);
        let [header, body, status_area, keys] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Fill(1),
            Constraint::Length(status_height),
            Constraint::Length(1),
        ])
        .areas(area);
        let [documents, tree] =
            Layout::horizontal([Constraint::Percentage(30), Constraint::Percentage(70)])
                .areas(body);
        frame.render_widget(self.header(), header);
        self.draw_documents(frame, documents, viewport);
        self.draw_tree(frame, tree, viewport);
        if let Some(status) = status {
            if let Some(column) = status.cursor_column {
                frame.set_cursor_position(Position::new(
                    status_area.x.saturating_add(column),
                    status_area.y,
                ));
            }
            frame.render_widget(status.paragraph, status_area);
        }
        frame.render_widget(Paragraph::new(self.key_hints()).dim(), keys);
    }

    fn header(&self) -> Line<'_> {
        let level = self.level();
        let filter = level.filter();
        let mut spans = vec![
            Span::from(level.path().to_string()).bold(),
            Span::from("  "),
        ];
        if !filter.is_empty() {
            spans.push(Span::from(filter.to_string()));
        }
        if !filter.has_limit() {
            let space = if filter.is_empty() { "" } else { " " };
            spans.push(Span::from(format!("{space}LIMIT {BROWSE_DOCUMENT_LIMIT}")).dim());
        }
        let documents = counted(level.documents().len(), "document", "documents");
        spans.push(Span::from(format!("  · {documents}")).dim());
        if matches!(level.load(), Load::Pending(_)) {
            spans.push(Span::from("  · loading…").yellow());
        }
        Line::from(spans)
    }

    fn draw_documents(&self, frame: &mut Frame, area: Rect, viewport: &mut Viewport) {
        let level = self.level();
        let block = self.pane_block(Pane::Documents, " documents ".to_string());
        if self.focus() == Pane::Documents {
            viewport.page_rows = usize::from(block.inner(area).height);
        }
        if level.documents().is_empty() {
            let empty = match level.load() {
                Load::Pending(_) => "loading…",
                Load::Done | Load::Failed(_) => "no documents",
            };
            frame.render_widget(Paragraph::new(empty).dim().block(block), area);
            return;
        }
        let items: Vec<ListItem> = level
            .documents()
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

    fn draw_tree(&self, frame: &mut Frame, area: Rect, viewport: &mut Viewport) {
        let level = self.level();
        let Some(document) = level.selected_document() else {
            frame.render_widget(self.pane_block(Pane::Tree, String::new()), area);
            return;
        };
        let title = FirestoreDb::relative_document_name(self.documents_path(), &document.name);
        let block = self.pane_block(Pane::Tree, format!(" {title} "));
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
            .map(|row| TreeLine::new(row, &view))
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

    /// The line above the key hints: the filter being edited, or the error of the last read.
    fn status(&self, width: u16) -> Option<StatusLine> {
        if let Some(text) = self.filter_editor() {
            let prompt = format!("/ {text}");
            let column = u16::try_from(Line::from(prompt.as_str()).width()).unwrap_or(u16::MAX);
            return Some(StatusLine {
                paragraph: Paragraph::new(prompt),
                height: 1,
                cursor_column: Some(column),
            });
        }
        let error = match self.level().load() {
            Load::Failed(error) => error.to_string(),
            Load::Done | Load::Pending(_) => return None,
        };
        let text = format!("error: {error}");
        let lines: usize = text
            .lines()
            .map(|line| {
                Line::from(line)
                    .width()
                    .div_ceil(usize::from(width.max(1)))
                    .max(1)
            })
            .sum();
        Some(StatusLine {
            height: u16::try_from(lines)
                .unwrap_or(u16::MAX)
                .min(BROWSE_ERROR_LINES),
            paragraph: Paragraph::new(text).red().wrap(Wrap { trim: false }),
            cursor_column: None,
        })
    }

    fn key_hints(&self) -> String {
        if self.filter_editor().is_some() {
            return "Enter runs the filter · Esc cancels".to_string();
        }
        let mut hints = vec![
            "↑↓ move",
            "→ open",
            "← close",
            "Tab switches pane",
            "/ filter",
        ];
        if self.has_parent() {
            hints.push("Esc back");
        }
        if matches!(self.level().load(), Load::Pending(_)) {
            hints.push("Ctrl-C stops waiting");
        }
        hints.push("q quits");
        hints.join(" · ")
    }

    /// The frame of `pane`, in colour while it has the focus.
    fn pane_block(&self, pane: Pane, title: String) -> Block<'static> {
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

/// The footer line above the key hints.
struct StatusLine {
    paragraph: Paragraph<'static>,
    height: u16,
    /// Where the terminal cursor goes on the first line, while the filter is edited.
    cursor_column: Option<u16>,
}

/// A row of the tree as the three columns of its pane.
struct TreeLine {
    name: String,
    type_name: &'static str,
    value: String,
    value_style: Style,
}

impl TreeLine {
    fn new(row: &Row<'_>, view: &View<'_>) -> Self {
        let indent = "  ".repeat(row.depth());
        match row {
            Row::Field { path, value, fold } => Self {
                name: format!(
                    "{indent}{} {}",
                    fold.marker(),
                    path.last().map(ToString::to_string).unwrap_or_default()
                ),
                type_name: type_name(value),
                value: view.compact_value(value),
                value_style: value_style(value),
            },
            Row::Subcollections { listing, fold } => {
                let (value, value_style) = match listing {
                    Listing::NotListed => (String::new(), Style::new()),
                    Listing::Pending(_) => ("listing…".to_string(), Style::new().yellow()),
                    Listing::Listed(collection_ids) => (
                        counted(collection_ids.len(), "collection", "collections"),
                        Style::new().dim(),
                    ),
                    Listing::Failed(error) => (error.to_string(), Style::new().red()),
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

/// The ID of `document`, the last segment of its name.
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
    use crate::browse::state::{Effect, Message, Movement};
    use crate::browse::BrowseTarget;
    use crate::render::fixtures::{draft, dune, DOCUMENTS_PATH};
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    #[test]
    fn the_documents_and_the_tree_of_the_selected_one_show_side_by_side() {
        let target = BrowseTarget {
            collection: "books".parse().expect("a collection path"),
            filter: "WHERE pages > 100".into(),
        };
        let (mut browser, Effect::Query(query)) = Browser::open(target, DOCUMENTS_PATH) else {
            panic!("no query")
        };
        browser.update(Message::DocumentsLoaded {
            request: query.request,
            outcome: Ok(vec![draft(), dune()]),
        });
        browser.update(Message::Move(Movement::Down(1)));
        browser.update(Message::Expand);
        browser.update(Message::Expand);
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).expect("a test terminal");

        terminal
            .draw(|frame| browser.draw(frame, &mut Viewport::default()))
            .expect("a frame");

        let buffer = terminal.backend().buffer();
        let lines: Vec<String> = (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect()
            })
            .collect();
        let line_with = |text: &str| {
            lines
                .iter()
                .find(|line| line.contains(text))
                .unwrap_or_else(|| panic!("no `{text}` in\n{}", lines.join("\n")))
        };
        assert!(line_with("books").contains("WHERE pages > 100"));
        assert!(line_with("▾ address").contains("draft"));
        assert!(line_with("London").contains("city"));
        assert!(line_with("▸ tags").contains("[2 items]"));
        line_with("▸ subcollections");
    }
}
