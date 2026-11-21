use super::draw::Viewport;
use super::editor::FilterEditor;
use super::state::{Browser, Copied, Failure, FilterTail, BROWSE_DOCUMENT_LIMIT};
use super::tree::Row;
use crate::errors::FsqError;
use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::Stylize;
use ratatui::text::{Line, Span};
use ratatui::Frame;
use std::borrow::Cow;

/// The most lines the footer gives an error, which can be a long message from Firestore.
const BROWSE_ERROR_LINES: usize = 4;

const FILTER_PROMPT: &str = "/ ";

/// What the filter editor shows while it is empty.
const FILTER_PLACEHOLDER: &str = "WHERE … ORDER BY … LIMIT …";

const HINT_SEPARATOR: &str = " · ";

impl Browser {
    /// Draws the filter editor while it is open, else the keys after the note of a copy. A popup
    /// over the panes keeps its own keys, and only the stop of a load shows here, beside an
    /// editor without its cursor.
    pub(super) fn draw_footer(&self, frame: &mut Frame, area: Rect, viewport: &mut Viewport) {
        match (self.filter_editor(), self.popup()) {
            (Some(editor), popup) => {
                let mut hints = vec![Hint::new("Enter", "run", 2), Hint::new("Esc", "cancel", 1)];
                if self.failure() == Some(Failure::Query) {
                    hints.push(Hint::new("Ctrl-G", "error", 3));
                }
                let cursor = editor.draw(frame, area, viewport, Hints(hints));
                if popup.is_none() {
                    frame.set_cursor_position(cursor);
                }
            }
            (None, Some(_)) => {
                let mut hints = Vec::new();
                if self.is_waiting() {
                    hints.push(Hint::new("Ctrl-C", "stop", 0));
                }
                let width = usize::from(area.width);
                frame.render_widget(Hints(hints).after_note(viewport.copied, width), area);
            }
            (None, None) => self.draw_key_hints(frame, area, viewport.copied),
        }
    }

    fn draw_key_hints(&self, frame: &mut Frame, area: Rect, copied: Option<Copied>) {
        let mut hints = vec![
            Hint::new("↑↓", "move", 1),
            Hint::new("→", "open", 5),
            Hint::new("Tab", "pane", 4),
            Hint::new("/", "filter", 6),
        ];
        if self.filter_on_cursor().is_some() {
            hints.push(Hint::new("f", "where", 3));
        }
        if matches!(self.tree_cursor(), Some((_, Row::Field { .. }))) {
            hints.push(Hint::new("y", "copy", 2));
        }
        if self.has_parent() {
            hints.push(Hint::new("Esc", "back", 7));
        }
        if self.failure().is_some() {
            hints.push(Hint::new("Ctrl-G", "error", 9));
        }
        if self.is_waiting() {
            hints.push(Hint::new("Ctrl-C", "stop", 8));
        }
        let pinned = Hints(vec![Hint::new("?", "help", 0), Hint::new("q", "quit", 0)]);
        let [left, right] =
            Layout::horizontal([Constraint::Fill(1), Constraint::Length(pinned.columns())])
                .spacing(2)
                .areas(area);
        let hints = Hints(hints).after_note(copied, usize::from(left.width));
        frame.render_widget(hints, left);
        frame.render_widget(pinned.line(), right);
    }

    /// The error under the footer, the one Ctrl-G shows whole, wrapped at words to `width`
    /// columns.
    pub(super) fn footer_error(&self, width: usize) -> Vec<Line<'static>> {
        self.failure()
            .and_then(|failure| self.error(failure))
            .map(|error| error.footer_lines(width))
            .unwrap_or_default()
    }
}

impl FilterEditor {
    /// Draws the filter being edited after its prompt, scrolled to keep the cursor in view, and
    /// `hints`, the keys that end the edit, on the right. Returns where the cursor goes.
    fn draw(
        &self,
        frame: &mut Frame,
        area: Rect,
        viewport: &mut Viewport,
        hints: Hints,
    ) -> Position {
        let hints = hints.fitted(usize::from(area.width) / 2);
        let [line_area, hints_area] =
            Layout::horizontal([Constraint::Fill(1), Constraint::Length(hints.columns())])
                .spacing(2)
                .areas(area);
        frame.render_widget(hints.line(), hints_area);

        let text = self.text();
        let room = usize::from(line_area.width).saturating_sub(FILTER_PROMPT.len());
        // The limit the query gets when the filter has none, after the column of the cursor at
        // the end, as the query would run. It takes room from the text only when the end of the
        // text shows, and only on a line at least twice its width, so that a narrow line keeps
        // its room for the text.
        let ghost = format!("  LIMIT {BROWSE_DOCUMENT_LIMIT}");
        let mut window = self.window(room, viewport.editor_start);
        let shows_ghost = !text.is_empty()
            && window.end == text.len()
            && room >= 2 * ghost.len()
            && !FilterTail::from(text).has_limit();
        if shows_ghost {
            window = self.window(room - (ghost.len() - 1), viewport.editor_start);
        }
        viewport.editor_start = window.start;

        let mut spans = vec![Span::from(FILTER_PROMPT)];
        if text.is_empty() {
            spans.push(Span::from(FILTER_PLACEHOLDER).dim());
        }
        if window.start > 0 {
            spans.push(Span::from("…").dim());
        }
        spans.push(Span::from(text[window.start..window.end].to_string()));
        if window.end < text.len() {
            spans.push(Span::from("…").dim());
        } else if shows_ghost {
            spans.push(Span::from(ghost).dim());
        }
        frame.render_widget(Line::from(spans), line_area);
        let column = u16::try_from(FILTER_PROMPT.len() + window.cursor_column).unwrap_or(u16::MAX);
        Position::new(line_area.x.saturating_add(column), line_area.y)
    }
}

impl FsqError {
    /// This error wrapped at words to `width` columns, in at most [`BROWSE_ERROR_LINES`] lines,
    /// the last of which ends in `…` when more of the error remains.
    fn footer_lines(&self, width: usize) -> Vec<Line<'static>> {
        let text = format!("error: {self}");
        let mut lines: Vec<String> = textwrap::wrap(&text, width.max(1))
            .into_iter()
            .map(Cow::into_owned)
            .collect();
        if lines.len() > BROWSE_ERROR_LINES {
            lines.truncate(BROWSE_ERROR_LINES);
            if let Some(last) = lines.last_mut() {
                *last = format!(
                    "{}…",
                    console::truncate_str(last, width.saturating_sub(1), "")
                );
            }
        }
        lines
            .into_iter()
            .map(|line| Line::from(line).red())
            .collect()
    }
}

/// A key and what it does, as the footer names it.
#[derive(Debug, Clone, Copy)]
pub(super) struct Hint {
    key: &'static str,
    action: &'static str,
    /// The hints that do not fit go lowest first.
    priority: u8,
}

impl Hint {
    pub(super) fn new(key: &'static str, action: &'static str, priority: u8) -> Self {
        Self {
            key,
            action,
            priority,
        }
    }

    fn width(&self) -> usize {
        Span::from(self.key).width() + 1 + Span::from(self.action).width()
    }
}

/// Hints in the order they show, their keys bright and what they do dim.
pub(super) struct Hints(pub(super) Vec<Hint>);

impl Hints {
    /// These hints without those of the lowest priority that do not fit in `width` columns.
    pub(super) fn fitted(mut self, width: usize) -> Self {
        while self.width() > width {
            let Some(lowest) = (0..self.0.len()).min_by_key(|&position| self.0[position].priority)
            else {
                break;
            };
            self.0.remove(lowest);
        }
        self
    }

    fn width(&self) -> usize {
        let separators = self.0.len().saturating_sub(1) * Span::from(HINT_SEPARATOR).width();
        self.0.iter().map(Hint::width).sum::<usize>() + separators
    }

    fn columns(&self) -> u16 {
        u16::try_from(self.width()).unwrap_or(u16::MAX)
    }

    /// These hints fitted to `width` columns after the note of what the last key `copied`, if it
    /// copied anything. The note is never dropped.
    fn after_note(self, copied: Option<Copied>, width: usize) -> Line<'static> {
        let Some(copied) = copied else {
            return self.fitted(width).line();
        };
        let note = match copied {
            Copied::Value => "copied value",
            Copied::Path => "copied path",
        };
        let separator = Span::from(HINT_SEPARATOR).dim();
        let hints = self.fitted(width.saturating_sub(Span::from(note).width() + separator.width()));
        let mut spans = vec![Span::from(note).dim()];
        if !hints.0.is_empty() {
            spans.push(separator);
        }
        spans.extend(hints.line().spans);
        Line::from(spans)
    }

    pub(super) fn line(&self) -> Line<'static> {
        let mut spans = Vec::new();
        for (position, hint) in self.0.iter().enumerate() {
            if position > 0 {
                spans.push(Span::from(HINT_SEPARATOR).dim());
            }
            spans.push(Span::from(hint.key));
            spans.push(Span::from(format!(" {}", hint.action)).dim());
        }
        Line::from(spans)
    }
}
