use super::state::FilterTail;
use ratatui::buffer::CellWidth;
use reedline::LineBuffer;
use unicode_segmentation::UnicodeSegmentation;

/// A change to the filter being edited. The cursor moves and deletes by grapheme, and by word as
/// Unicode splits words, the way fsq's prompt edits a line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edit {
    Insert(char),
    DeleteBefore,
    DeleteAfter,
    DeleteWordBefore,
    DeleteToStart,
    Left,
    Right,
    Start,
    End,
}

/// The most filters a [`FilterHistory`] keeps.
const FILTER_HISTORY_LIMIT: usize = 100;

/// The filters that ran and answered, the most recent last. It lasts for the process, so that
/// every browser of one REPL session recalls the filters of the browsers before it.
#[derive(Debug, Default)]
pub struct FilterHistory(Vec<FilterTail>);

impl FilterHistory {
    /// Keeps `filter` as the most recent, once: a filter that ran before moves up from where it
    /// was. An empty filter is not kept, since it recalls nothing.
    pub fn add(&mut self, filter: &FilterTail) {
        if filter.is_empty() {
            return;
        }
        self.0.retain(|kept| kept != filter);
        self.0.push(filter.clone());
        if self.0.len() > FILTER_HISTORY_LIMIT {
            self.0.remove(0);
        }
    }
}

/// A step through the [`FilterHistory`] from the filter editor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Recall {
    Older,
    Newer,
}

/// The text of a filter being edited and the cursor in it.
#[derive(Debug)]
pub struct FilterEditor {
    line: LineBuffer,
    /// `None` until a filter is recalled from the history.
    recalled: Option<Recalled>,
}

/// A filter of the history in the editor, and the text that was being typed before it.
#[derive(Debug)]
struct Recalled {
    /// How far back the filter is: 0 for the most recent.
    back: usize,
    draft: String,
}

impl FilterEditor {
    /// Replaces the text with the filter that ran before the one shown, or after it. Past the
    /// most recent filter, the text that was being typed before the first recall comes back.
    pub fn recall(&mut self, recall: Recall, history: &FilterHistory) {
        let Some(oldest) = history.0.len().checked_sub(1) else {
            return;
        };
        let back = match (self.recalled.as_ref().map(|recalled| recalled.back), recall) {
            (None, Recall::Older) => Some(0),
            (None, Recall::Newer) => return,
            (Some(back), Recall::Older) => Some((back + 1).min(oldest)),
            (Some(back), Recall::Newer) => back.checked_sub(1),
        };
        let draft = match self.recalled.take() {
            Some(recalled) => recalled.draft,
            None => self.text().to_string(),
        };
        let text = match back {
            Some(back) => {
                let filter = history.0[oldest - back].to_string();
                self.recalled = Some(Recalled { back, draft });
                filter
            }
            None => draft,
        };
        self.line = LineBuffer::from(text.as_str());
    }

    pub fn apply(&mut self, edit: Edit) {
        match edit {
            Edit::Insert(typed) => self.line.insert_char(typed),
            Edit::DeleteBefore => self.line.delete_left_grapheme(),
            Edit::DeleteAfter => self.line.delete_right_grapheme(),
            Edit::DeleteWordBefore => self.line.delete_word_left(),
            Edit::DeleteToStart => self.line.clear_to_insertion_point(),
            Edit::Left => self.line.move_left(),
            Edit::Right => self.line.move_right(),
            Edit::Start => self.line.move_to_start(),
            Edit::End => self.line.move_to_end(),
        }
    }

    /// Inserts `pasted` at the cursor as one line: a line break, a tab or another control
    /// character becomes a space.
    pub fn paste(&mut self, pasted: &str) {
        let line = pasted.replace("\r\n", " ").replace(char::is_control, " ");
        self.line.insert_str(&line);
    }

    pub fn text(&self) -> &str {
        self.line.get_buffer()
    }

    /// The byte position of the cursor in [`Self::text`], at the start of a grapheme.
    pub fn cursor(&self) -> usize {
        self.line.insertion_point()
    }

    /// The part of the text that shows in `width` columns. It starts from `from`, the start of
    /// the window in the frame before, scrolls only as far as keeps the cursor in view, and
    /// scrolls back while the end of the text shows with room to spare.
    pub fn window(&self, width: usize, from: usize) -> Window {
        let stops = Stops::of(self.text());
        let cursor = stops.at_or_after(self.cursor());
        let mut start = stops.at_or_before(from).min(cursor);
        while start < cursor && !stops.shows(start, cursor, width) {
            start += 1;
        }
        while start > 0
            && stops.end(start, width) == stops.last()
            && stops.need(start - 1, stops.last()) <= width
        {
            start -= 1;
        }
        Window {
            start: stops.byte(start),
            end: stops.byte(stops.end(start, width)),
            cursor_column: usize::from(start > 0) + stops.column(cursor) - stops.column(start),
        }
    }
}

/// The places a window of a text can start or end at: the start of every grapheme with a width,
/// as ratatui draws the line, and last the end of the text. A grapheme without width stays with
/// the one before it.
struct Stops(Vec<Stop>);

struct Stop {
    byte: usize,
    /// The column the grapheme is drawn at, from the start of the text.
    column: usize,
}

impl Stops {
    fn of(text: &str) -> Self {
        let mut stops = Vec::new();
        let mut column = 0;
        for (byte, grapheme) in text.grapheme_indices(true) {
            // ratatui draws nothing of a grapheme with a control character in it.
            let width = if grapheme.contains(char::is_control) {
                0
            } else {
                usize::from(grapheme.cell_width())
            };
            if width > 0 {
                stops.push(Stop { byte, column });
            }
            column += width;
        }
        stops.push(Stop {
            byte: text.len(),
            column,
        });
        Self(stops)
    }

    fn last(&self) -> usize {
        self.0.len() - 1
    }

    fn byte(&self, stop: usize) -> usize {
        self.0[stop].byte
    }

    fn column(&self, stop: usize) -> usize {
        self.0[stop].column
    }

    fn at_or_after(&self, byte: usize) -> usize {
        self.0
            .partition_point(|stop| stop.byte < byte)
            .min(self.last())
    }

    fn at_or_before(&self, byte: usize) -> usize {
        self.0
            .partition_point(|stop| stop.byte <= byte)
            .saturating_sub(1)
    }

    /// The columns the window from `start` to `end` takes: the `…` before it when it does not
    /// start the text, its characters, and one more for the `…` after it or for the cursor at the
    /// end of the text.
    fn need(&self, start: usize, end: usize) -> usize {
        usize::from(start > 0) + self.column(end) - self.column(start) + 1
    }

    /// The furthest end of a window from `start` that fits in `width` columns.
    fn end(&self, start: usize, width: usize) -> usize {
        (start + 1..=self.last())
            .take_while(|&end| self.need(start, end) <= width)
            .last()
            .unwrap_or(start)
    }

    /// Whether the window from `start` shows the cursor at `cursor`, which the `…` after the
    /// window would cover.
    fn shows(&self, start: usize, cursor: usize, width: usize) -> bool {
        let end = self.end(start, width);
        cursor < end || cursor == self.last() && end == self.last()
    }
}

impl From<&FilterTail> for FilterEditor {
    /// An editor of `filter`, with the cursor at its end.
    fn from(filter: &FilterTail) -> Self {
        Self {
            line: LineBuffer::from(filter.to_string().as_str()),
            recalled: None,
        }
    }
}

/// The part of an editor's text that shows on its line. A `…` of one column stands for the text
/// hidden before `start` and the text hidden after `end`. When the text shows to its end, one
/// more column after it holds the cursor at the end.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Window {
    /// The byte of the text the window starts at.
    pub start: usize,
    /// The byte of the text the window ends before.
    pub end: usize,
    /// The column of the cursor from the start of the window, counting the `…` before it.
    pub cursor_column: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    const LONG_FILTER: &str =
        "WHERE genre = 'Science fiction' AND pages BETWEEN 200 AND 900 ORDER BY rating DESC";

    fn editor(text: &str) -> FilterEditor {
        FilterEditor::from(&FilterTail::from(text))
    }

    fn edited(text: &str, edits: &[Edit]) -> FilterEditor {
        let mut editor = editor(text);
        for edit in edits {
            editor.apply(*edit);
        }
        editor
    }

    fn shown(editor: &FilterEditor, window: Window) -> &str {
        &editor.text()[window.start..window.end]
    }

    #[test]
    fn a_wide_character_takes_two_columns_of_the_window() {
        let editor = editor(&"名前".repeat(20));

        let window = editor.window(21, 0);

        assert_eq!(
            shown(&editor, window),
            "名前".repeat(5).chars().skip(1).collect::<String>()
        );
        assert_eq!(window.cursor_column, 19);
    }

    #[test]
    fn the_cursor_column_counts_each_grapheme_as_wide_as_it_is_drawn() {
        for emoji in ["👍🏽", "👨‍👩‍👧", "❤️"] {
            let editor = editor(&format!("WHERE t = '{emoji}'"));

            let window = editor.window(40, 0);

            assert_eq!(window.cursor_column, "WHERE t = ''".len() + 2, "{emoji}");
        }
    }

    #[test]
    fn a_paste_goes_in_at_the_cursor_as_one_line() {
        let mut editor = edited("WHERE  ORDER BY title", &[Edit::Start]);
        for _ in 0.."WHERE ".len() {
            editor.apply(Edit::Right);
        }

        editor.paste("genre = 'sf'\r\nAND\tpages > 100\n");

        assert_eq!(
            editor.text(),
            "WHERE genre = 'sf' AND pages > 100  ORDER BY title"
        );
    }

    #[test]
    fn a_text_that_fits_shows_whole() {
        let editor = editor("WHERE pages > 100");

        let window = editor.window(40, 0);

        assert_eq!(shown(&editor, window), "WHERE pages > 100");
        assert_eq!(window.cursor_column, "WHERE pages > 100".len());
    }

    #[test]
    fn a_long_text_shows_its_end_with_the_cursor_at_the_end() {
        let editor = editor(LONG_FILTER);

        let window = editor.window(40, 0);

        assert_eq!(window.end, LONG_FILTER.len());
        assert!(LONG_FILTER.ends_with(shown(&editor, window)));
        assert_eq!(window.cursor_column, 39);
        assert_eq!(1 + shown(&editor, window).len() + 1, 40);
    }

    #[test]
    fn the_window_scrolls_only_once_the_cursor_reaches_its_edge() {
        let mut editor = editor(LONG_FILTER);
        let at_end = editor.window(40, 0);

        editor.apply(Edit::Left);
        let one_left = editor.window(40, at_end.start);
        assert_eq!(one_left.start, at_end.start);
        assert_eq!(one_left.cursor_column, at_end.cursor_column - 1);

        editor.apply(Edit::Start);
        let at_start = editor.window(40, one_left.start);
        assert_eq!(at_start.start, 0);
        assert_eq!(at_start.cursor_column, 0);
        assert!(at_start.end < LONG_FILTER.len());

        for _ in 0..60 {
            editor.apply(Edit::Right);
        }
        let moved = editor.window(40, at_start.start);
        assert!(moved.start > 0);
        assert!(moved.start <= editor.cursor() && editor.cursor() < moved.end);
        assert_eq!(moved.cursor_column, 38);
    }

    #[test]
    fn a_text_that_shrinks_scrolls_back_to_fill_the_line() {
        let mut editor = editor(LONG_FILTER);
        let at_end = editor.window(40, 0);

        for _ in 0..50 {
            editor.apply(Edit::DeleteBefore);
        }
        let window = editor.window(40, at_end.start);

        assert_eq!(window.start, 0);
        assert_eq!(
            shown(&editor, window),
            &LONG_FILTER[..LONG_FILTER.len() - 50]
        );
    }
}
