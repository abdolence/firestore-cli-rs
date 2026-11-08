use super::{counted, cut_middle, View};
use firestore::gcloud_sdk::google::firestore::v1::value::ValueType;
use firestore::gcloud_sdk::google::firestore::v1::Value;
use firestore::gcloud_sdk::prost_types::Timestamp;
use firestore::timestamp_utils::from_timestamp;
use firestore::{FirestoreDb, FirestoreJsonEncoding};
use std::collections::HashMap;
use std::fmt::{Display, Formatter, Write};

const CELL_TEXT_LIMIT: usize = 40;

impl View<'_> {
    pub fn compact_value(&self, value: &Value) -> String {
        let theme = self.theme;
        match &value.value_type {
            None | Some(ValueType::NullValue(_)) => theme.null.apply_to("NULL").to_string(),
            Some(ValueType::StringValue(text)) => {
                theme.string.apply_to(cut(&single_line(text))).to_string()
            }
            Some(ValueType::ArrayValue(array)) => {
                let summary = match array.values.len() {
                    0 => "[]".to_string(),
                    count => format!("[{}]", counted(count, "item", "items")),
                };
                theme.nested.apply_to(summary).to_string()
            }
            Some(ValueType::MapValue(map)) => {
                let keys = sorted_keys(&map.fields)
                    .into_iter()
                    .map(String::as_str)
                    .collect::<Vec<_>>()
                    .join(", ");
                theme
                    .nested
                    .apply_to(format!("{{{}}}", cut(&keys)))
                    .to_string()
            }
            Some(_) => self.scalar(value),
        }
    }

    pub fn pretty_value(&self, value: &Value) -> String {
        let mut out = String::new();
        match &value.value_type {
            Some(ValueType::StringValue(text)) => {
                out.push_str(&self.theme.string.apply_to(text).to_string())
            }
            _ => self.pretty_into(value, 0, &mut out),
        }
        out
    }

    fn pretty_into(&self, value: &Value, depth: usize, out: &mut String) {
        let theme = self.theme;
        let indent = "  ".repeat(depth + 1);
        let closing = "  ".repeat(depth);
        match &value.value_type {
            Some(ValueType::StringValue(text)) => {
                out.push_str(&theme.string.apply_to(format!("{text:?}")).to_string())
            }
            Some(ValueType::ArrayValue(array)) if array.values.is_empty() => out.push_str("[]"),
            Some(ValueType::ArrayValue(array)) => {
                out.push_str("[\n");
                for (position, element) in array.values.iter().enumerate() {
                    out.push_str(&indent);
                    self.pretty_into(element, depth + 1, out);
                    out.push_str(if position + 1 < array.values.len() {
                        ",\n"
                    } else {
                        "\n"
                    });
                }
                out.push_str(&closing);
                out.push(']');
            }
            Some(ValueType::MapValue(map)) if map.fields.is_empty() => out.push_str("{}"),
            Some(ValueType::MapValue(map)) => {
                out.push_str("{\n");
                let keys = sorted_keys(&map.fields);
                for (position, key) in keys.iter().enumerate() {
                    let _ = write!(out, "{indent}{}: ", theme.key.apply_to(format!("{key:?}")));
                    self.pretty_into(&map.fields[*key], depth + 1, out);
                    out.push_str(if position + 1 < keys.len() {
                        ",\n"
                    } else {
                        "\n"
                    });
                }
                out.push_str(&closing);
                out.push('}');
            }
            _ => out.push_str(&self.scalar(value)),
        }
    }

    fn scalar(&self, value: &Value) -> String {
        let theme = self.theme;
        match &value.value_type {
            None | Some(ValueType::NullValue(_)) => theme.null.apply_to("NULL").to_string(),
            Some(ValueType::BooleanValue(flag)) => theme.boolean.apply_to(flag).to_string(),
            Some(ValueType::IntegerValue(number)) => theme.number.apply_to(number).to_string(),
            Some(ValueType::DoubleValue(number)) => {
                theme.number.apply_to(format!("{number:?}")).to_string()
            }
            Some(ValueType::TimestampValue(timestamp)) => theme
                .timestamp
                .apply_to(TimestampText(timestamp))
                .to_string(),
            Some(ValueType::StringValue(text)) => theme.string.apply_to(text).to_string(),
            Some(ValueType::BytesValue(bytes)) => theme
                .muted
                .apply_to(counted(bytes.len(), "byte", "bytes"))
                .to_string(),
            Some(ValueType::ReferenceValue(name)) => theme
                .reference
                .apply_to(FirestoreDb::relative_document_name(
                    self.documents_path,
                    name,
                ))
                .to_string(),
            Some(ValueType::GeoPointValue(point)) => theme
                .number
                .apply_to(format!("({:?}, {:?})", point.latitude, point.longitude))
                .to_string(),
            Some(ValueType::ArrayValue(_) | ValueType::MapValue(_)) => self.compact_value(value),
            Some(
                ValueType::FieldReferenceValue(text) | ValueType::VariableReferenceValue(text),
            ) => theme.muted.apply_to(text).to_string(),
            Some(ValueType::FunctionValue(function)) => theme
                .muted
                .apply_to(format!("{}(..)", function.name))
                .to_string(),
            Some(ValueType::PipelineValue(_)) => theme.muted.apply_to("pipeline").to_string(),
        }
    }

    /// A value as plain JSON, in the firestore crate's plain encoding.
    pub fn json_value(&self, value: &Value) -> serde_json::Value {
        FirestoreDb::serialize_value_to_json(
            self.documents_path,
            value,
            FirestoreJsonEncoding::Plain,
        )
        .expect("the plain encoding has no value it refuses")
    }
}

struct TimestampText<'a>(&'a Timestamp);

impl Display for TimestampText<'_> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match from_timestamp(*self.0) {
            Ok(instant) => instant.fmt(f),
            Err(_) => write!(f, "{}s {}ns", self.0.seconds, self.0.nanos),
        }
    }
}

pub(super) fn sorted_keys(fields: &HashMap<String, Value>) -> Vec<&String> {
    let mut keys: Vec<&String> = fields.keys().collect();
    keys.sort();
    keys
}

fn single_line(text: &str) -> String {
    text.chars().map(single_line_char).collect()
}

/// Whether the compact form of `value` leaves something out that the detail view shows: a string
/// it cuts, the contents of a map, an array or bytes.
pub(super) fn is_summarised(value: &Value) -> bool {
    match &value.value_type {
        Some(ValueType::StringValue(text)) => {
            console::measure_text_width(&single_line(text)) > CELL_TEXT_LIMIT
        }
        Some(ValueType::ArrayValue(array)) => !array.values.is_empty(),
        Some(ValueType::MapValue(map)) => !map.fields.is_empty(),
        Some(ValueType::BytesValue(_)) => true,
        _ => false,
    }
}

/// The components of a vector, a map that Firestore stores as a marker and an array of doubles.
/// The recognition is the firestore crate's, which is not public: the plain JSON encoding
/// writes a vector as an array and every other map as an object.
pub(super) fn vector_components(value: &Value) -> Option<Vec<serde_json::Value>> {
    if !matches!(value.value_type, Some(ValueType::MapValue(_))) {
        return None;
    }
    match FirestoreDb::serialize_value_to_json("", value, FirestoreJsonEncoding::Plain)
        .expect("the plain encoding has no value it refuses")
    {
        serde_json::Value::Array(components) => Some(components),
        _ => None,
    }
}

/// Names the type of a value as `\d` and the detail view list it. An expression value is one
/// that a pipeline takes as input and never returns in a document.
pub fn type_name(value: &Value) -> &'static str {
    match &value.value_type {
        None | Some(ValueType::NullValue(_)) => "null",
        Some(ValueType::BooleanValue(_)) => "boolean",
        Some(ValueType::IntegerValue(_)) => "integer",
        Some(ValueType::DoubleValue(_)) => "double",
        Some(ValueType::TimestampValue(_)) => "timestamp",
        Some(ValueType::StringValue(_)) => "string",
        Some(ValueType::BytesValue(_)) => "bytes",
        Some(ValueType::ReferenceValue(_)) => "reference",
        Some(ValueType::GeoPointValue(_)) => "geopoint",
        Some(ValueType::ArrayValue(_)) => "array",
        Some(ValueType::MapValue(_)) if vector_components(value).is_some() => "vector",
        Some(ValueType::MapValue(_)) => "map",
        Some(
            ValueType::FieldReferenceValue(_)
            | ValueType::VariableReferenceValue(_)
            | ValueType::FunctionValue(_)
            | ValueType::PipelineValue(_),
        ) => "expression",
    }
}

fn single_line_char(c: char) -> char {
    match c {
        '\n' => '↵',
        c if c.is_control() => ' ',
        c => c,
    }
}

fn cut(text: &str) -> String {
    cut_middle(text, CELL_TEXT_LIMIT)
}
