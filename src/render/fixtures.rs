//! Documents for the rendering tests, holding a value of every type Firestore stores.

use firestore::gcloud_sdk::google::firestore::v1::value::ValueType;
use firestore::gcloud_sdk::google::firestore::v1::{ArrayValue, MapValue, Value};
use firestore::gcloud_sdk::google::r#type::LatLng;
use firestore::gcloud_sdk::prost_types::Timestamp;
use firestore::FirestoreDocument;
use std::collections::HashMap;

pub const DOCUMENTS_PATH: &str = "projects/latestbit/databases/books-db/documents";

pub fn value(value_type: ValueType) -> Value {
    Value {
        value_type: Some(value_type),
    }
}

pub fn string(text: &str) -> Value {
    value(ValueType::StringValue(text.to_string()))
}

pub fn document(id: &str, fields: Vec<(&str, Value)>) -> FirestoreDocument {
    FirestoreDocument {
        name: format!("{DOCUMENTS_PATH}/books/{id}"),
        fields: fields
            .into_iter()
            .map(|(name, value)| (name.to_string(), value))
            .collect(),
        create_time: None,
        update_time: None,
    }
}

pub fn dune() -> FirestoreDocument {
    let address = HashMap::from([
        ("city".to_string(), string("London")),
        ("zip".to_string(), string("N1 9GU")),
    ]);
    document(
        "dune",
        vec![
            ("title", string("Dune")),
            ("rating", value(ValueType::DoubleValue(4.5))),
            ("pages", value(ValueType::IntegerValue(412))),
            ("in_print", value(ValueType::BooleanValue(true))),
            ("subtitle", value(ValueType::NullValue(0))),
            (
                "published",
                value(ValueType::TimestampValue(Timestamp {
                    seconds: 1_700_000_000,
                    nanos: 0,
                })),
            ),
            (
                "address",
                value(ValueType::MapValue(MapValue { fields: address })),
            ),
            (
                "tags",
                value(ValueType::ArrayValue(ArrayValue {
                    values: vec![string("sf"), string("classic")],
                })),
            ),
            (
                "author",
                value(ValueType::ReferenceValue(format!(
                    "{DOCUMENTS_PATH}/authors/herbert"
                ))),
            ),
            (
                "location",
                value(ValueType::GeoPointValue(LatLng {
                    latitude: 51.5,
                    longitude: -0.12,
                })),
            ),
            ("cover", value(ValueType::BytesValue(vec![1, 2, 3, 255]))),
            (
                "blurb",
                string("A desert planet, a spice that extends life,\nand a duke's son"),
            ),
        ],
    )
}

/// A document that lacks most of the fields of [`dune`].
pub fn draft() -> FirestoreDocument {
    document("draft", vec![("title", string("Untitled"))])
}
