use super::grid::in_selected_order;
use super::value::{sorted_keys, type_name};
use super::{ExpandedMode, Grid, OutputFormat, TableText, View};
use crate::field_path::FieldPath;
use firestore::gcloud_sdk::google::firestore::v1::value::ValueType;
use firestore::gcloud_sdk::google::firestore::v1::Value;
use firestore::{FirestoreDb, FirestoreDocument, FirestoreVector};
use std::collections::BTreeSet;

/// The names JSON export gives the times of a document.
const CREATE_TIME_FIELD: &str = "__create_time__";
const UPDATE_TIME_FIELD: &str = "__update_time__";

impl View<'_> {
    /// The rows of a result as a table, or in the detail view under `\x full`. `documents` are
    /// the rows of `grid`, the first of them row `first_row` of the result.
    pub fn rows(
        &self,
        grid: &Grid,
        documents: &[FirestoreDocument],
        first_row: usize,
        order: &[String],
    ) -> String {
        if self.expanded == ExpandedMode::Full && self.format == OutputFormat::Table {
            self.details(documents, first_row, order)
        } else {
            self.grid(grid)
        }
    }

    /// Every field of `documents`, a block each, the first of them row `first_row` of its result:
    /// one row per leaf value under its path, its type and its whole value, wrapped inside the
    /// value column. Fields come in `order`, the order the statement selects them in, then by
    /// name, and the times of the document last, when it carries them.
    pub fn details(
        &self,
        documents: &[FirestoreDocument],
        first_row: usize,
        order: &[String],
    ) -> String {
        documents
            .iter()
            .enumerate()
            .map(|(position, document)| self.detail_block(document, first_row + position, order))
            .collect()
    }

    fn detail_block(&self, document: &FirestoreDocument, row: usize, order: &[String]) -> String {
        let title = if document.name.is_empty() {
            format!("row {row}")
        } else {
            FirestoreDb::relative_document_name(self.documents_path, &document.name).to_string()
        };
        let header = ["field", "type", "value"]
            .map(|header| self.theme.header.apply_to(header).to_string())
            .to_vec();
        let table = TableText::new(header, self.detail_rows(document, order))
            .into_borderless_table(self.width)
            .to_string();
        let lines: Vec<&str> = table.lines().map(str::trim_end).collect();
        let widest = lines
            .iter()
            .map(|line| console::measure_text_width(line))
            .max()
            .unwrap_or_default();
        let title = format!("─[ {title} ]");
        let padding = widest
            .saturating_sub(console::measure_text_width(&title))
            .max(1);
        let mut out = format!(
            "{}\n",
            self.theme
                .muted
                .apply_to(format!("{title}{}", "─".repeat(padding)))
        );
        for line in lines {
            out.push_str(line);
            out.push('\n');
        }
        out
    }

    fn detail_rows(&self, document: &FirestoreDocument, order: &[String]) -> Vec<Vec<String>> {
        let names: BTreeSet<String> = document.fields.keys().cloned().collect();
        let mut rows = Vec::new();
        for name in in_selected_order(names, order) {
            self.flatten_into(
                &FieldPath::default().field(&name),
                &document.fields[&name],
                &mut rows,
            );
        }
        for (name, time) in [
            (CREATE_TIME_FIELD, &document.create_time),
            (UPDATE_TIME_FIELD, &document.update_time),
        ] {
            if let Some(time) = time {
                let value = Value {
                    value_type: Some(ValueType::TimestampValue(*time)),
                };
                self.push_leaf(name.to_string(), &value, &mut rows);
            }
        }
        rows
    }

    /// Adds a row for every value below `value` that is not a map or an array with something in
    /// it. An empty map or array is a value of its own, and so is a vector.
    fn flatten_into(&self, path: &FieldPath, value: &Value, rows: &mut Vec<Vec<String>>) {
        match &value.value_type {
            Some(ValueType::MapValue(map))
                if !map.fields.is_empty() && !FirestoreVector::is_vector(value) =>
            {
                for name in sorted_keys(&map.fields) {
                    self.flatten_into(&path.field(name), &map.fields[name], rows);
                }
            }
            Some(ValueType::ArrayValue(array)) if !array.values.is_empty() => {
                for (index, element) in array.values.iter().enumerate() {
                    self.flatten_into(&path.element(index), element, rows);
                }
            }
            _ => self.push_leaf(path.to_string(), value, rows),
        }
    }

    fn push_leaf(&self, path: String, value: &Value, rows: &mut Vec<Vec<String>>) {
        rows.push(vec![
            self.theme.key.apply_to(path).to_string(),
            self.theme.muted.apply_to(type_name(value)).to_string(),
            self.whole_value(value),
        ]);
    }

    /// A leaf value as `\x` shows it: bytes in the base64 of the JSON output rather than as a
    /// size.
    fn whole_value(&self, value: &Value) -> String {
        match &value.value_type {
            Some(ValueType::BytesValue(_)) => match self.json_value(value) {
                serde_json::Value::String(base64) => self.theme.muted.apply_to(base64).to_string(),
                other => other.to_string(),
            },
            _ => self.pretty_value(value),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::fixtures::{document, string, value, vector, DOCUMENTS_PATH};
    use crate::render::Theme;
    use firestore::gcloud_sdk::google::firestore::v1::value::ValueType;
    use firestore::gcloud_sdk::google::firestore::v1::{ArrayValue, MapValue, Value};
    use firestore::gcloud_sdk::prost_types::Timestamp;

    fn view(theme: &Theme, width: Option<usize>) -> View<'_> {
        View {
            theme,
            format: OutputFormat::Table,
            expanded: ExpandedMode::Full,
            width,
            documents_path: DOCUMENTS_PATH,
        }
    }

    fn map(fields: Vec<(&str, Value)>) -> Value {
        value(ValueType::MapValue(MapValue {
            fields: fields
                .into_iter()
                .map(|(name, field)| (name.to_string(), field))
                .collect(),
        }))
    }

    fn timestamp(seconds: i64) -> Timestamp {
        Timestamp { seconds, nanos: 0 }
    }

    fn dune() -> FirestoreDocument {
        FirestoreDocument {
            create_time: Some(timestamp(1_700_000_000)),
            update_time: Some(timestamp(1_700_000_060)),
            ..document(
                "dune",
                vec![
                    ("title", string("Dune")),
                    ("rating", value(ValueType::DoubleValue(4.5))),
                    (
                        "published",
                        value(ValueType::TimestampValue(timestamp(1_700_000_000))),
                    ),
                    (
                        "author",
                        map(vec![
                            ("name", string("Frank Herbert")),
                            ("born", value(ValueType::IntegerValue(1920))),
                        ]),
                    ),
                    (
                        "tags",
                        value(ValueType::ArrayValue(ArrayValue {
                            values: vec![string("sf"), string("classic")],
                        })),
                    ),
                    ("extra", map(vec![])),
                    (
                        "notes",
                        value(ValueType::ArrayValue(ArrayValue { values: vec![] })),
                    ),
                    ("unit-price", value(ValueType::IntegerValue(12))),
                ],
            )
        }
    }

    #[test]
    fn a_document_is_one_row_per_leaf_value_with_its_path_and_type_and_its_times() {
        let theme = Theme::new(false);

        let text = view(&theme, None).details(&[dune()], 1, &["title".to_string()]);

        assert_eq!(text, DUNE_DETAILS);
    }

    #[test]
    fn a_document_without_a_name_is_titled_by_its_row() {
        let theme = Theme::new(false);
        let unnamed = FirestoreDocument {
            name: String::new(),
            ..document("x", vec![("title", string("Dune"))])
        };

        let text = view(&theme, None).details(&[unnamed], 7, &[]);

        assert!(text.starts_with("─[ row 7 ]"), "{text}");
    }

    #[test]
    fn a_long_value_wraps_inside_the_value_column_and_is_never_cut() {
        let words = "a desert planet, a spice that extends life, and a duke's son".repeat(3);
        let unbroken = "x".repeat(70);
        let document = document(
            "dune",
            vec![
                ("blurb", string(&words)),
                ("token", string(&unbroken)),
                ("note", string("first line\nsecond line")),
            ],
        );

        for colour in [false, true] {
            let theme = Theme::new(colour);
            let text = view(&theme, Some(50)).details(std::slice::from_ref(&document), 1, &[]);

            for line in text.lines() {
                assert!(console::measure_text_width(line) <= 50, "{text}");
            }
            assert!(!text.contains('…'), "{text}");
            let plain = console::strip_ansi_codes(&text).replace([' ', '\n'], "");
            assert!(plain.contains(&words.replace(' ', "")), "{text}");
            assert!(plain.contains(&unbroken), "{text}");
            assert!(plain.contains("firstlinesecondline"), "{text}");
        }
    }

    #[test]
    fn a_vector_is_one_row_of_type_vector_with_its_components() {
        let theme = Theme::new(false);
        let document = document(
            "dune",
            vec![
                ("embedding", vector(&[0.4, 0.2, 0.9])),
                ("title", string("Dune")),
            ],
        );

        let text = view(&theme, None).details(&[document], 1, &[]);

        assert!(
            text.contains(" embedding  vector  [0.4, 0.2, 0.9]\n"),
            "{text}"
        );
        assert!(!text.contains("__type__"), "{text}");
    }

    #[test]
    fn a_vector_with_an_infinite_component_shows_it_as_a_double() {
        let theme = Theme::new(false);
        let document = document(
            "dune",
            vec![(
                "embedding",
                vector(&[1.0, f64::INFINITY, f64::NEG_INFINITY]),
            )],
        );

        let text = view(&theme, None).details(&[document], 1, &[]);

        assert!(
            text.contains(" embedding  vector  [1.0, inf, -inf]\n"),
            "{text}"
        );
    }

    #[test]
    fn a_map_that_only_resembles_a_vector_is_flattened_as_a_map() {
        let theme = Theme::new(false);
        let resembling = map(vec![
            ("__type__", string("__vector__")),
            ("value", string("not components")),
        ]);
        let document = document("dune", vec![("embedding", resembling)]);

        let text = view(&theme, None).details(&[document], 1, &[]);

        assert!(!text.contains(" vector "), "{text}");
        assert!(text.contains("embedding.__type__"), "{text}");
    }

    #[test]
    fn bytes_are_shown_whole_as_base64() {
        let theme = Theme::new(false);
        let document = document(
            "dune",
            vec![("cover", value(ValueType::BytesValue(vec![1, 2, 3, 255])))],
        );

        let text = view(&theme, None).details(&[document], 1, &[]);

        assert!(text.contains("bytes"), "{text}");
        assert!(text.contains("AQID/w=="), "{text}");
    }

    const DUNE_DETAILS: &str = "─[ books/dune ]──────────────────────────────────
 field            type       value
 title            string     Dune
 author.born      integer    1920
 author.`name`    string     Frank Herbert
 extra            map        {}
 notes            array      []
 published        timestamp  2023-11-14T22:13:20Z
 rating           double     4.5
 tags[0]          string     sf
 tags[1]          string     classic
 `unit-price`     integer    12
 __create_time__  timestamp  2023-11-14T22:13:20Z
 __update_time__  timestamp  2023-11-14T22:14:20Z
";
}
