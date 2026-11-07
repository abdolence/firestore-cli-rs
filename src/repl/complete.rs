use super::SQL_KEYWORDS;
use crate::meta::META_COMMANDS;
use crate::session::SharedVocabulary;
use firestore::FirestoreSqlFieldName;
use reedline::{Completer, Span, Suggestion};

const COLLECTION_KEYWORDS: &[&str] = &["FROM", "JOIN", "INTO", "UPDATE", "COLLECTION", "\\D"];

pub struct SqlCompleter {
    vocabulary: SharedVocabulary,
}

impl SqlCompleter {
    pub fn new(vocabulary: SharedVocabulary) -> Self {
        Self { vocabulary }
    }

    fn candidates(&self, line: &str, word_start: usize) -> Vec<String> {
        let (before_word, word) = line.split_at(word_start);
        let before_word = before_word.trim_end();
        if before_word.is_empty() && word.starts_with('\\') {
            return META_COMMANDS
                .iter()
                .map(|command| command.name().to_string())
                .collect();
        }
        let previous = before_word
            .rsplit(|c: char| c.is_whitespace() || c == ',' || c == '(')
            .next()
            .unwrap_or_default()
            .to_ascii_uppercase();
        let Ok(vocabulary) = self.vocabulary.lock() else {
            return Vec::new();
        };
        // After the ON of a join a field belongs; after that of CREATE INDEX a collection does.
        let creates_index =
            previous == "ON" && line.trim_start().to_ascii_uppercase().starts_with("CREATE");
        if creates_index || COLLECTION_KEYWORDS.contains(&previous.as_str()) {
            return vocabulary
                .collections
                .iter()
                .map(|id| FirestoreSqlFieldName::from(id.as_str()).to_string())
                .collect();
        }
        SQL_KEYWORDS
            .iter()
            .map(ToString::to_string)
            .chain(
                vocabulary
                    .fields
                    .iter()
                    .map(|field| FirestoreSqlFieldName::from(field.as_str()).to_string()),
            )
            .collect()
    }
}

impl Completer for SqlCompleter {
    fn complete(&mut self, line: &str, pos: usize) -> Vec<Suggestion> {
        let line = &line[..pos];
        let start = line
            .rfind(|c: char| c.is_whitespace() || matches!(c, ',' | '(' | ')' | '=' | '<' | '>'))
            .map_or(0, |position| position + 1);
        let word = &line[start..];
        let prefix = word.trim_start_matches('`').to_ascii_lowercase();
        self.candidates(line, start)
            .into_iter()
            .filter(|candidate| {
                candidate
                    .trim_start_matches('`')
                    .to_ascii_lowercase()
                    .starts_with(&prefix)
            })
            .map(|value| Suggestion {
                value,
                display_override: None,
                description: None,
                style: None,
                extra: None,
                span: Span::new(start, pos),
                append_whitespace: true,
                match_indices: None,
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::Vocabulary;
    use std::sync::{Arc, Mutex};

    fn completions(line: &str) -> Vec<String> {
        let vocabulary = Vocabulary {
            collections: ["books", "order-lines"].map(String::from).into(),
            fields: ["rating", "title"].map(String::from).into(),
        };
        SqlCompleter::new(Arc::new(Mutex::new(vocabulary)))
            .complete(line, line.len())
            .into_iter()
            .map(|suggestion| suggestion.value)
            .collect()
    }

    #[test]
    fn the_word_before_the_cursor_completes_from_what_can_stand_there() {
        assert_eq!(
            completions("SELECT * FROM "),
            vec!["books", "`order-lines`"]
        );
        assert_eq!(completions("SELECT * FROM `ord"), vec!["`order-lines`"]);
        assert_eq!(completions("SELECT ti"), vec!["title"]);
        assert_eq!(completions("SELECT title FR"), vec!["FROM"]);
        assert_eq!(completions("\\ti"), vec!["\\timing"]);
        assert_eq!(completions("\\d bo"), vec!["books"]);
    }

    #[test]
    fn on_completes_collections_only_after_create_index() {
        assert_eq!(
            completions("CREATE INDEX ON "),
            vec!["books", "`order-lines`"]
        );
        let after_join = completions("SELECT * FROM books JOIN authors ON ");
        assert!(!after_join.contains(&"books".to_string()));
        assert!(after_join.contains(&"title".to_string()));
    }
}
