use super::fit::TableText;
use super::{ExpandedMode, OutputFormat, View};
use firestore::gcloud_sdk::google::firestore::v1::value::ValueType;
use firestore::gcloud_sdk::google::firestore::v1::Value;
use firestore::FirestoreDocument;
use std::collections::BTreeSet;

/// The column a document's name is shown under, as SQL selects it.
pub const NAME_COLUMN: &str = "__name__";

#[derive(Debug, Clone, PartialEq)]
pub enum Cell {
    Value(Value),
    Text(String),
    Count(u64),
    /// A share from 0 to 100: `87.5%` in a table and `87.5` in JSON.
    Percent(f64),
}

/// Rows under named columns, the one shape every tabular result is drawn from: a table, the
/// expanded view, JSON or JSON lines. A row holds `None` where it has no value for a column,
/// such as a field its document lacks.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Grid {
    columns: Vec<String>,
    rows: Vec<Vec<Option<Cell>>>,
}

impl Grid {
    pub fn new<I, S>(columns: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            columns: columns.into_iter().map(Into::into).collect(),
            rows: Vec::new(),
        }
    }

    pub fn push_row(&mut self, row: Vec<Option<Cell>>) {
        debug_assert_eq!(row.len(), self.columns.len());
        self.rows.push(row);
    }

    pub fn columns(&self) -> &[String] {
        &self.columns
    }

    #[cfg(test)]
    pub fn rows(&self) -> &[Vec<Option<Cell>>] {
        &self.rows
    }

    pub fn len(&self) -> usize {
        self.rows.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// One row per document, under the union of their fields. Firestore returns a document's
    /// fields unordered, so the columns come in `order`, the order the statement selects them in,
    /// and then by name, with the document name first when the documents have one.
    pub fn from_documents(documents: Vec<FirestoreDocument>, order: &[String]) -> Self {
        let mut fields = BTreeSet::new();
        for document in &documents {
            fields.extend(document.fields.keys().cloned());
        }
        let has_names = documents.iter().any(|document| !document.name.is_empty());
        let mut columns: Vec<String> = has_names
            .then(|| NAME_COLUMN.to_string())
            .into_iter()
            .collect();
        for name in order {
            if fields.remove(name) {
                columns.push(name.clone());
            }
        }
        columns.extend(fields);
        let rows = documents
            .into_iter()
            .map(|mut document| {
                columns
                    .iter()
                    .map(|column| match column.as_str() {
                        NAME_COLUMN if has_names => (!document.name.is_empty()).then(|| {
                            Cell::Value(Value {
                                value_type: Some(ValueType::ReferenceValue(std::mem::take(
                                    &mut document.name,
                                ))),
                            })
                        }),
                        _ => document.fields.remove(column).map(Cell::Value),
                    })
                    .collect()
            })
            .collect();
        Self { columns, rows }
    }
}

impl View<'_> {
    pub fn grid(&self, grid: &Grid) -> String {
        self.grids(&[("", grid)])
    }

    /// Several grids of one statement. A table per non-empty grid, titled when several are
    /// non-empty; in JSON one array, or an object of arrays by title; in JSON lines every row.
    pub fn grids(&self, grids: &[(&str, &Grid)]) -> String {
        match self.format {
            OutputFormat::Table if grids.len() == 1 => self.grid_text(grids[0].1),
            OutputFormat::Table => {
                let shown: Vec<_> = grids.iter().filter(|(_, grid)| !grid.is_empty()).collect();
                let mut out = String::new();
                for (title, grid) in &shown {
                    if shown.len() > 1 {
                        out.push_str(&self.theme.header.apply_to(title).to_string());
                        out.push('\n');
                    }
                    out.push_str(&self.grid_text(grid));
                }
                out
            }
            OutputFormat::Json => {
                let json = match grids {
                    [(_, grid)] => self.json_rows(grid),
                    _ => serde_json::Value::Object(
                        grids
                            .iter()
                            .map(|(title, grid)| (title.to_string(), self.json_rows(grid)))
                            .collect(),
                    ),
                };
                format!("{json:#}\n")
            }
            OutputFormat::Jsonl => {
                let mut out = String::new();
                for (_, grid) in grids {
                    for row in self.json_objects(grid) {
                        out.push_str(&row.to_string());
                        out.push('\n');
                    }
                }
                out
            }
        }
    }

    fn grid_text(&self, grid: &Grid) -> String {
        match self.expanded {
            ExpandedMode::On => self.expanded_text(grid),
            ExpandedMode::Off => {
                let mut cells = self.table_cells(grid);
                if let Some(width) = self.width {
                    cells.fit_to(width);
                }
                format!("{}\n", cells.into_table())
            }
            ExpandedMode::Auto => {
                let cells = self.table_cells(grid);
                match self.width {
                    Some(width) if cells.width() > width => self.expanded_text(grid),
                    _ => format!("{}\n", cells.into_table()),
                }
            }
        }
    }

    fn table_cells(&self, grid: &Grid) -> TableText {
        TableText::new(
            grid.columns
                .iter()
                .map(|column| self.theme.header.apply_to(column).to_string())
                .collect(),
            grid.rows
                .iter()
                .map(|row| {
                    row.iter()
                        .map(|cell| {
                            cell.as_ref()
                                .map(|cell| self.cell_text(cell, CellLayout::Compact))
                                .unwrap_or_default()
                        })
                        .collect()
                })
                .collect(),
        )
    }

    fn expanded_text(&self, grid: &Grid) -> String {
        let key_width = grid
            .columns
            .iter()
            .map(|column| console::measure_text_width(column))
            .max()
            .unwrap_or_default();
        let mut out = String::new();
        for (position, row) in grid.rows.iter().enumerate() {
            let mut lines = Vec::new();
            for (column, cell) in grid.columns.iter().zip(row) {
                let Some(cell) = cell else { continue };
                let text = self.cell_text(cell, CellLayout::Pretty);
                for (line_number, line) in text.lines().enumerate() {
                    let key = if line_number == 0 {
                        column.as_str()
                    } else {
                        ""
                    };
                    lines.push(format!(
                        "{}{} │ {line}",
                        self.theme.key.apply_to(key),
                        " ".repeat(key_width - console::measure_text_width(key)),
                    ));
                }
            }
            let widest = lines
                .iter()
                .map(|line| console::measure_text_width(line))
                .max()
                .unwrap_or_default();
            let title = format!("-[ RECORD {} ]", position + 1);
            let padding = widest.saturating_sub(title.len()).max(1);
            out.push_str(
                &self
                    .theme
                    .muted
                    .apply_to(format!("{title}{}", "-".repeat(padding)))
                    .to_string(),
            );
            out.push('\n');
            for line in lines {
                out.push_str(&line);
                out.push('\n');
            }
        }
        out
    }

    fn cell_text(&self, cell: &Cell, layout: CellLayout) -> String {
        match (cell, layout) {
            (Cell::Value(value), CellLayout::Compact) => self.compact_value(value),
            (Cell::Value(value), CellLayout::Pretty) => self.pretty_value(value),
            (Cell::Text(text), _) => text.clone(),
            (Cell::Count(count), _) => self.theme.number.apply_to(count).to_string(),
            (Cell::Percent(share), _) => format!("{share:.1}%"),
        }
    }

    fn json_rows(&self, grid: &Grid) -> serde_json::Value {
        serde_json::Value::Array(self.json_objects(grid))
    }

    fn json_objects(&self, grid: &Grid) -> Vec<serde_json::Value> {
        grid.rows
            .iter()
            .map(|row| {
                serde_json::Value::Object(
                    grid.columns
                        .iter()
                        .zip(row)
                        .filter_map(|(column, cell)| {
                            cell.as_ref()
                                .map(|cell| (column.clone(), self.json_cell(cell)))
                        })
                        .collect(),
                )
            })
            .collect()
    }

    fn json_cell(&self, cell: &Cell) -> serde_json::Value {
        match cell {
            Cell::Value(value) => self.json_value(value),
            Cell::Text(text) => serde_json::Value::String(text.clone()),
            Cell::Count(count) => serde_json::Value::from(*count),
            Cell::Percent(share) => serde_json::json!((share * 10.0).round() / 10.0),
        }
    }
}

#[derive(Clone, Copy)]
enum CellLayout {
    Compact,
    Pretty,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::fixtures::{document, draft, dune, string, value, DOCUMENTS_PATH};
    use crate::render::Theme;
    use firestore::gcloud_sdk::google::firestore::v1::value::ValueType;
    use firestore::gcloud_sdk::google::firestore::v1::{ArrayValue, MapValue};
    use firestore::gcloud_sdk::google::r#type::LatLng;
    use firestore::gcloud_sdk::prost_types::Timestamp;
    use std::collections::HashMap;

    fn view(theme: &Theme, format: OutputFormat, expanded: ExpandedMode) -> View<'_> {
        View {
            theme,
            format,
            expanded,
            width: None,
            documents_path: DOCUMENTS_PATH,
        }
    }

    fn books() -> Grid {
        Grid::from_documents(vec![dune(), draft()], &["title".to_string()])
    }

    #[test]
    fn documents_render_as_a_table_of_compact_cells_under_the_union_of_their_fields() {
        let theme = Theme::new(false);

        let table = view(&theme, OutputFormat::Table, ExpandedMode::Off).grid(&books());

        assert_eq!(table, TABLE);
    }

    #[test]
    fn the_expanded_view_shows_a_block_per_row_with_nested_values_pretty_printed() {
        let theme = Theme::new(false);
        let grid = Grid::from_documents(vec![draft(), dune()], &[]);

        let expanded = view(&theme, OutputFormat::Table, ExpandedMode::On).grid(&grid);

        assert_eq!(expanded, EXPANDED);
    }

    fn auto_expanded_at(width: usize, grid: &Grid) -> String {
        View {
            width: Some(width),
            ..view(&Theme::new(false), OutputFormat::Table, ExpandedMode::Auto)
        }
        .grid(grid)
    }

    #[test]
    fn auto_expands_only_a_table_wider_than_the_terminal() {
        let grid = Grid::from_documents(vec![draft()], &[]);

        assert!(auto_expanded_at(80, &grid).starts_with('╭'));
        assert!(auto_expanded_at(10, &grid).starts_with("-[ RECORD 1 ]"));
    }

    fn restaurants() -> Grid {
        let restaurant = |id: &str, cuisine: &str, description: &str, menu: &str| {
            document(
                id,
                vec![
                    ("cuisine", string(cuisine)),
                    ("description", string(description)),
                    (
                        "location",
                        value(ValueType::GeoPointValue(LatLng {
                            latitude: 38.990077,
                            longitude: -107.065076,
                        })),
                    ),
                    ("menu", string(menu)),
                    ("name", string("Gluten-Free Kitchen")),
                    ("price_level", value(ValueType::IntegerValue(3))),
                    ("rating", value(ValueType::DoubleValue(4.3))),
                ],
            )
        };
        Grid::from_documents(
            vec![
                restaurant(
                    "rest-08",
                    "health",
                    "Gluten-free waffles and e-mail order specialities for every taste",
                    "gluten-free waffles, quinoa bowls, c++ and more of the same",
                ),
                restaurant(
                    "rest-04",
                    "pizza",
                    "New York style pizza by the slice.",
                    "pizza",
                ),
            ],
            &[],
        )
    }

    fn table_at(width: Option<usize>, colour: bool) -> String {
        let theme = Theme::new(colour);
        View {
            width,
            ..view(&theme, OutputFormat::Table, ExpandedMode::Off)
        }
        .grid(&restaurants())
    }

    fn line_widths(text: &str) -> Vec<usize> {
        text.lines().map(console::measure_text_width).collect()
    }

    #[test]
    fn a_table_wider_than_the_terminal_is_shrunk_to_the_terminal_width_with_its_borders_aligned() {
        for colour in [false, true] {
            let natural = line_widths(&table_at(None, colour))[0];
            for width in [80, 120, 200] {
                let table = table_at(Some(width), colour);

                assert_eq!(
                    line_widths(&table),
                    vec![width.min(natural); table.lines().count()],
                    "width {width}, colour {colour}:\n{table}"
                );
            }
        }
    }

    #[test]
    fn shrinking_cuts_cell_text_before_it_cuts_a_header() {
        let table = table_at(Some(120), false);

        let header_line = table.lines().nth(1).unwrap_or_default();
        for header in restaurants().columns() {
            assert!(header_line.contains(header.as_str()), "{header}:\n{table}");
        }
        assert_eq!(line_widths(&table).first(), Some(&120));
    }

    #[test]
    fn a_table_that_fits_the_terminal_is_drawn_at_its_natural_width() {
        assert_eq!(table_at(Some(400), true), table_at(None, true));
    }

    #[test]
    fn json_lines_hold_one_plain_json_object_per_row() {
        let theme = Theme::new(false);

        let lines = view(&theme, OutputFormat::Jsonl, ExpandedMode::Off).grid(&books());

        assert_eq!(lines, JSON_LINES);
    }

    #[test]
    fn json_writes_infinity_a_fractional_timestamp_and_a_vector_in_the_plain_encoding() {
        let theme = Theme::new(false);
        let mut grid = Grid::new(["score", "seen", "embedding"]);
        grid.push_row(vec![
            Some(Cell::Value(value(ValueType::DoubleValue(f64::INFINITY)))),
            Some(Cell::Value(value(ValueType::TimestampValue(Timestamp {
                seconds: 1_700_000_000,
                nanos: 500_000_000,
            })))),
            Some(Cell::Value(value(ValueType::MapValue(MapValue {
                fields: HashMap::from([
                    ("__type__".to_string(), string("__vector__")),
                    (
                        "value".to_string(),
                        value(ValueType::ArrayValue(ArrayValue {
                            values: vec![
                                value(ValueType::DoubleValue(1.0)),
                                value(ValueType::DoubleValue(2.5)),
                            ],
                        })),
                    ),
                ]),
            })))),
        ]);

        let lines = view(&theme, OutputFormat::Jsonl, ExpandedMode::Off).grid(&grid);

        assert_eq!(
            lines,
            "{\"score\":\"Infinity\",\"seen\":\"2023-11-14T22:13:20.500Z\",\"embedding\":[1.0,2.5]}\n"
        );
    }

    #[test]
    fn several_grids_render_as_one_json_object_of_arrays_by_title() {
        let theme = Theme::new(false);
        let mut first = Grid::new(["id"]);
        first.push_row(vec![Some(Cell::Count(1))]);
        let second = Grid::new(["name"]);

        let json = view(&theme, OutputFormat::Json, ExpandedMode::Off)
            .grids(&[("indexes", &first), ("fields", &second)]);

        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&json).expect("JSON"),
            serde_json::json!({"indexes": [{"id": 1}], "fields": []})
        );
    }

    const TABLE: &str = r#"╭─────────────┬──────────┬─────────────┬─────────────────┬──────────────────────────────────────────┬─────────┬──────────┬───────────────┬───────┬──────────────────────┬────────┬──────────┬───────────╮
│ __name__    ┆ title    ┆ address     ┆ author          ┆ blurb                                    ┆ cover   ┆ in_print ┆ location      ┆ pages ┆ published            ┆ rating ┆ subtitle ┆ tags      │
╞═════════════╪══════════╪═════════════╪═════════════════╪══════════════════════════════════════════╪═════════╪══════════╪═══════════════╪═══════╪══════════════════════╪════════╪══════════╪═══════════╡
│ books/dune  ┆ Dune     ┆ {city, zip} ┆ authors/herbert ┆ A desert planet, a spice that extends l… ┆ 4 bytes ┆ true     ┆ (51.5, -0.12) ┆ 412   ┆ 2023-11-14T22:13:20Z ┆ 4.5    ┆ NULL     ┆ [2 items] │
│ books/draft ┆ Untitled ┆             ┆                 ┆                                          ┆         ┆          ┆               ┆       ┆                      ┆        ┆          ┆           │
╰─────────────┴──────────┴─────────────┴─────────────────┴──────────────────────────────────────────┴─────────┴──────────┴───────────────┴───────┴──────────────────────┴────────┴──────────┴───────────╯
"#;

    const EXPANDED: &str = r#"-[ RECORD 1 ]----------
__name__  │ books/draft
title     │ Untitled
-[ RECORD 2 ]------------------------------------------
__name__  │ books/dune
address   │ {
          │   "city": "London",
          │   "zip": "N1 9GU"
          │ }
author    │ authors/herbert
blurb     │ A desert planet, a spice that extends life,
          │ and a duke's son
cover     │ 4 bytes
in_print  │ true
location  │ (51.5, -0.12)
pages     │ 412
published │ 2023-11-14T22:13:20Z
rating    │ 4.5
subtitle  │ NULL
tags      │ [
          │   "sf",
          │   "classic"
          │ ]
title     │ Dune
"#;

    const JSON_LINES: &str = r#"{"__name__":"books/dune","title":"Dune","address":{"city":"London","zip":"N1 9GU"},"author":"authors/herbert","blurb":"A desert planet, a spice that extends life,\nand a duke's son","cover":"AQID/w==","in_print":true,"location":{"latitude":51.5,"longitude":-0.12},"pages":412,"published":"2023-11-14T22:13:20Z","rating":4.5,"subtitle":null,"tags":["sf","classic"]}
{"__name__":"books/draft","title":"Untitled"}
"#;
}
