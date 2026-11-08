use firestore::FirestoreSqlFieldName;
use std::fmt::{Display, Formatter};

/// The path of a field inside a document, written so that SQL reads the same field when it is
/// pasted into a statement: `address.city`, `` `unit-price` ``, `tags[0]`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FieldPath(String);

impl FieldPath {
    /// The field `name` of the map this path names, or a top-level field of the document when
    /// this is the empty path.
    pub fn field(&self, name: &str) -> Self {
        let name = FirestoreSqlFieldName::from(name);
        if self.0.is_empty() {
            Self(name.to_string())
        } else {
            Self(format!("{}.{name}", self.0))
        }
    }

    /// The element at `index` of the array this path names.
    pub fn element(&self, index: usize) -> Self {
        Self(format!("{}[{index}]", self.0))
    }
}

impl Display for FieldPath {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_path_quotes_each_name_and_marks_array_elements_by_position() {
        let path = FieldPath::default()
            .field("order")
            .field("unit-price")
            .element(2)
            .field("it`s");

        assert_eq!(path.to_string(), "`order`.`unit-price`[2].`it``s`");
    }
}
