use super::{counted, View};
use firestore::gcloud_sdk::google::firestore::v1::value::ValueType;
use firestore::gcloud_sdk::google::firestore::v1::Value;
use firestore::gcloud_sdk::prost_types::Timestamp;
use firestore::timestamp_utils::from_timestamp;
use firestore::FirestoreJsonConverter;
use std::collections::HashMap;
use std::fmt::{Display, Formatter, Write};

const CELL_TEXT_LIMIT: usize = 40;

impl View<'_> {
    pub fn compact_value(&self, value: &Value) -> String {
        let theme = self.theme;
        match &value.value_type {
            None | Some(ValueType::NullValue(_)) => theme.null.apply_to("NULL").to_string(),
            Some(ValueType::StringValue(text)) => {
                let flat: String = text.chars().map(single_line_char).collect();
                theme.string.apply_to(cut(&flat)).to_string()
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
                .apply_to(FirestoreJsonConverter::new(self.documents_path).relative_name(name))
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
        FirestoreJsonConverter::new(self.documents_path)
            .plain(value)
            .into_inner()
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

fn sorted_keys(fields: &HashMap<String, Value>) -> Vec<&String> {
    let mut keys: Vec<&String> = fields.keys().collect();
    keys.sort();
    keys
}

fn single_line_char(c: char) -> char {
    match c {
        '\n' => '↵',
        c if c.is_control() => ' ',
        c => c,
    }
}

fn cut(text: &str) -> String {
    if text.chars().count() <= CELL_TEXT_LIMIT {
        text.to_string()
    } else {
        let kept: String = text.chars().take(CELL_TEXT_LIMIT - 1).collect();
        format!("{kept}…")
    }
}
