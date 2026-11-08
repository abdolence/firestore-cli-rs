use comfy_table::{presets, Cell as TableCell, ContentArrangement, Table};

/// What the table preset adds around every column: a space on each side and a separator.
const COLUMN_FRAME_WIDTH: usize = 3;
/// The border that closes the table, beside the separators.
const TABLE_FRAME_WIDTH: usize = 1;
/// The narrowest a column is cut to when the headers alone do not fit the terminal.
const MIN_COLUMN_WIDTH: usize = 1;

/// The text of a table's cells, which can be drawn as it is or cut to fit a terminal width.
pub struct TableText {
    header: Vec<String>,
    rows: Vec<Vec<String>>,
}

impl TableText {
    pub fn new(header: Vec<String>, rows: Vec<Vec<String>>) -> Self {
        Self { header, rows }
    }

    /// The width of the table as drawn, borders included.
    pub fn width(&self) -> usize {
        self.column_widths().iter().sum::<usize>() + self.frame_width()
    }

    /// Cuts the longest cells with `…` until the table is `width` wide. Columns shrink down to
    /// their headers first, and below them only when the headers themselves cannot fit, so a
    /// table with more columns than the terminal has room for still overflows by the frame.
    pub fn fit_to(&mut self, width: usize) {
        let available = width.saturating_sub(self.frame_width());
        let natural = self.column_widths();
        let header_widths: Vec<usize> = self.header.iter().map(|cell| line_width(cell)).collect();
        let narrowest = vec![MIN_COLUMN_WIDTH; natural.len()];
        let fitted = shrink(
            shrink(natural, &header_widths, available),
            &narrowest,
            available,
        );
        for (column, width) in fitted.into_iter().enumerate() {
            self.header[column] = cut(&self.header[column], width);
            for row in &mut self.rows {
                row[column] = cut(&row[column], width);
            }
        }
    }

    pub fn into_table(self) -> Table {
        let mut table = Table::new();
        table
            .load_style(presets::UTF8_FULL_CONDENSED.with_rounded_corners())
            .set_content_arrangement(ContentArrangement::Disabled)
            .set_header(self.header.into_iter().map(TableCell::new));
        for row in self.rows {
            table.add_row(row.into_iter().map(TableCell::new));
        }
        table
    }

    fn frame_width(&self) -> usize {
        self.header.len() * COLUMN_FRAME_WIDTH + TABLE_FRAME_WIDTH
    }

    fn column_widths(&self) -> Vec<usize> {
        (0..self.header.len())
            .map(|column| {
                std::iter::once(&self.header[column])
                    .chain(self.rows.iter().map(|row| &row[column]))
                    .map(|cell| line_width(cell))
                    .max()
                    .unwrap_or_default()
            })
            .collect()
    }
}

/// The width of the widest line of a cell, which may span lines.
fn line_width(cell: &str) -> usize {
    cell.lines()
        .map(console::measure_text_width)
        .max()
        .unwrap_or_default()
}

/// Narrows the widest column one character at a time, never below its floor, until the widths
/// sum to at most `available` or every column is at its floor.
fn shrink(mut widths: Vec<usize>, floors: &[usize], available: usize) -> Vec<usize> {
    while widths.iter().sum::<usize>() > available {
        let widest = (0..widths.len())
            .filter(|&column| widths[column] > floors[column])
            .max_by_key(|&column| widths[column]);
        match widest {
            Some(column) => widths[column] -= 1,
            None => break,
        }
    }
    widths
}

fn cut(cell: &str, width: usize) -> String {
    cell.lines()
        .map(|line| console::truncate_str(line, width, "…"))
        .collect::<Vec<_>>()
        .join("\n")
}
