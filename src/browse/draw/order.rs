use crate::browse::state::Browser;
use ratatui::style::Stylize;
use ratatui::text::{Line, Span};

/// The columns between an ID and the value after it.
const ORDER_COLUMN_GAP: usize = 2;

/// The fewest columns of a cut value that make the column worth its room beside the IDs.
const ORDER_COLUMN_MIN_WIDTH: usize = 4;

/// The value of the first `ORDER BY` key of each document, dim after its ID, so that the order of
/// the list explains itself and documents with IDs alike tell apart.
pub(super) struct OrderColumn {
    /// The compact value of each document in order, `None` for one without the field.
    values: Vec<Option<String>>,
}

impl OrderColumn {
    /// The column of the documents of the current level. `None` when their filter orders them by
    /// no field path, or no document has the field.
    pub(super) fn of(browser: &Browser) -> Option<Self> {
        let level = browser.level();
        let key = level.filter().order_key()?;
        let view = browser.value_view();
        let values: Vec<Option<String>> = level
            .documents()?
            .iter()
            .map(|document| {
                key.value_in(document)
                    .map(|value| view.compact_value(value))
            })
            .collect();
        values
            .iter()
            .any(Option::is_some)
            .then_some(Self { values })
    }

    /// The columns the widest value takes.
    fn widest(&self) -> usize {
        self.values
            .iter()
            .flatten()
            .map(|value| console::measure_text_width(value))
            .max()
            .unwrap_or_default()
    }

    /// The columns the widest value takes, with the gap before it.
    pub(super) fn width(&self) -> usize {
        ORDER_COLUMN_GAP + self.widest()
    }

    /// The columns left for the values when the IDs take `id_width` of `width`, `None` when that
    /// is too few for the column: less than the widest value, and less than
    /// [`ORDER_COLUMN_MIN_WIDTH`].
    pub(super) fn room(&self, id_width: usize, width: usize) -> Option<usize> {
        let least = ORDER_COLUMN_MIN_WIDTH.min(self.widest());
        width
            .checked_sub(id_width + ORDER_COLUMN_GAP)
            .filter(|&room| room >= least)
    }

    /// The line of the document at `position`: `id` padded to `id_width`, then its value cut to
    /// `room` columns.
    pub(super) fn line(
        &self,
        position: usize,
        id: &str,
        id_width: usize,
        room: usize,
    ) -> Line<'static> {
        let padding = id_width.saturating_sub(console::measure_text_width(id)) + ORDER_COLUMN_GAP;
        let value = self.values[position].as_deref().unwrap_or_default();
        Line::from(vec![
            Span::from(format!("{id}{}", " ".repeat(padding))),
            Span::from(console::truncate_str(value, room, "…").into_owned()).dim(),
        ])
    }
}
