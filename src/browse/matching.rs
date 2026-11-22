use super::state::FilterTail;
use super::tree::NodePath;
use crate::field_path::FieldPath;
use firestore::gcloud_sdk::google::firestore::v1::value::ValueType;
use firestore::gcloud_sdk::google::firestore::v1::Value;

impl FilterTail {
    /// The filter of the documents whose field at `path` holds `value`: `WHERE <path> = <value>`,
    /// or `WHERE <path> IS NULL`. `None` for a value SQL has no literal for, such as a map, an
    /// array, bytes, a reference, a double that is not finite or a string with a control
    /// character.
    pub fn matching(path: &NodePath, value: &Value) -> Option<Self> {
        let condition = match value.value_type.as_ref() {
            None | Some(ValueType::NullValue(_)) => "IS NULL".to_string(),
            Some(ValueType::BooleanValue(flag)) => format!("= {flag}"),
            Some(ValueType::IntegerValue(number)) => format!("= {number}"),
            // Debug keeps the `.0` of a whole double, so that SQL reads it as a double.
            Some(ValueType::DoubleValue(number)) if number.is_finite() => format!("= {number:?}"),
            Some(ValueType::StringValue(text)) => format!("= {}", quoted(text)?),
            // The firestore crate translates no `TIMESTAMP '..'` literal, and Firestore keeps
            // timestamps to the microsecond.
            Some(ValueType::TimestampValue(timestamp)) => {
                let micros = timestamp
                    .seconds
                    .checked_mul(1_000_000)?
                    .checked_add(i64::from(timestamp.nanos / 1_000))?;
                format!("= UNIX_MICROS_TO_TIMESTAMP({micros})")
            }
            Some(_) => return None,
        };
        Some(Self::from(
            format!("WHERE {} {condition}", FieldPath::from(path)).as_str(),
        ))
    }
}

/// `text` as a string literal on one line, with the backslash escapes of the BigQuery dialect the
/// firestore crate parses with. `None` for a text with a control character other than a line
/// break or a tab: the dialect has no escape for most of them, and the line of the filter editor
/// cannot show one as it is.
fn quoted(text: &str) -> Option<String> {
    let mut literal = String::from("'");
    for character in text.chars() {
        match character {
            '\'' | '\\' => {
                literal.push('\\');
                literal.push(character);
            }
            '\n' => literal.push_str("\\n"),
            '\r' => literal.push_str("\\r"),
            '\t' => literal.push_str("\\t"),
            control if control.is_control() => return None,
            _ => literal.push(character),
        }
    }
    literal.push('\'');
    Some(literal)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::fixtures::{string, value};
    use firestore::gcloud_sdk::google::firestore::v1::MapValue;
    use firestore::gcloud_sdk::prost_types::Timestamp;
    use sqlparser::ast::{self, BinaryOperator, Expr, SetExpr, Statement};
    use sqlparser::dialect::BigQueryDialect;
    use sqlparser::parser::Parser;

    fn matching(field: &str, value: &Value) -> Option<String> {
        FilterTail::matching(&NodePath::default().field(field), value)
            .map(|filter| filter.to_string())
    }

    /// The string on the right of the `=` of `filter`, as the parser the firestore crate
    /// translates with reads it.
    fn compared_string(filter: &str) -> String {
        let statement = format!("SELECT * FROM t\n{filter}");
        let statements = Parser::parse_sql(&BigQueryDialect {}, &statement).expect("a statement");
        let [Statement::Query(query)] = statements.as_slice() else {
            panic!("no query in {statement}")
        };
        let SetExpr::Select(select) = query.body.as_ref() else {
            panic!("no SELECT in {statement}")
        };
        match &select.selection {
            Some(Expr::BinaryOp {
                op: BinaryOperator::Eq,
                right,
                ..
            }) => match right.as_ref() {
                Expr::Value(literal) => match &literal.value {
                    ast::Value::SingleQuotedString(text) => text.clone(),
                    other => panic!("not a string: {other:?}"),
                },
                other => panic!("not a literal: {other:?}"),
            },
            other => panic!("not an `=`: {other:?}"),
        }
    }

    #[test]
    fn a_string_is_quoted_so_that_the_parser_reads_it_back_whole() {
        let text = "Paul's \"Dune\" \\ book\nof\tlines";

        let filter = matching("title", &string(text)).expect("a filter");

        assert_eq!(compared_string(&filter), text);
        assert!(!filter.contains(['\n', '\t']), "{filter}");
    }

    #[test]
    fn a_string_with_a_control_character_the_dialect_cannot_escape_has_no_filter() {
        for control in ['\u{1b}', '\u{7}', '\u{7f}', '\u{85}', '\u{8}', '\u{c}'] {
            let title = format!("a{control}[31mb");

            assert_eq!(matching("title", &string(&title)), None, "{control:?}");
        }
    }

    #[test]
    fn numbers_booleans_null_and_timestamps_are_literals_the_query_reads() {
        let published = Timestamp {
            seconds: 1_700_000_000,
            nanos: 250_000,
        };
        for (field, leaf, filter) in [
            ("pages", ValueType::IntegerValue(412), "WHERE pages = 412"),
            ("pages", ValueType::IntegerValue(-3), "WHERE pages = -3"),
            ("rating", ValueType::DoubleValue(4.5), "WHERE rating = 4.5"),
            ("rating", ValueType::DoubleValue(3.0), "WHERE rating = 3.0"),
            (
                "in_print",
                ValueType::BooleanValue(true),
                "WHERE in_print = true",
            ),
            (
                "subtitle",
                ValueType::NullValue(0),
                "WHERE subtitle IS NULL",
            ),
            (
                "published",
                ValueType::TimestampValue(published),
                "WHERE published = UNIX_MICROS_TO_TIMESTAMP(1700000000000250)",
            ),
        ] {
            assert_eq!(matching(field, &value(leaf)).as_deref(), Some(filter));
        }
    }

    #[test]
    fn a_value_without_a_literal_has_no_filter() {
        let address = value(ValueType::MapValue(MapValue::default()));

        assert_eq!(matching("address", &address), None);
        assert_eq!(
            matching("rating", &value(ValueType::DoubleValue(f64::NAN))),
            None
        );
        assert_eq!(
            matching("cover", &value(ValueType::BytesValue(vec![1, 2]))),
            None
        );
    }
}
