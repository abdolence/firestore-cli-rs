use sqlparser::dialect::BigQueryDialect;
use sqlparser::tokenizer::{Location, Token, Tokenizer};

/// A token and the text it was read from.
#[derive(Debug, Clone, PartialEq)]
pub struct SourceToken<'a> {
    pub kind: Token,
    pub text: &'a str,
}

/// SQL text read by the tokenizer that the firestore crate translates with, so that fsq and the
/// translator agree on where a string, a quoted name or a comment starts and ends. Whitespace and
/// comments are tokens, so that the tokens cover the text.
#[derive(Debug, Clone, PartialEq)]
pub struct SourceTokens<'a> {
    pub tokens: Vec<SourceToken<'a>>,
    /// The text from the point where the tokenizer stopped: an unterminated string, quoted name
    /// or comment, which a REPL keeps reading lines for. Empty when all the text was read.
    pub unread: &'a str,
}

impl<'a> From<&'a str> for SourceTokens<'a> {
    fn from(text: &'a str) -> Self {
        let mut spanned = Vec::new();
        // The tokenizer keeps the tokens before an error in `spanned`; where it stopped is what
        // `unread` reports.
        let _ =
            Tokenizer::new(&BigQueryDialect, text).tokenize_with_location_into_buf(&mut spanned);
        let offsets = ByteOffsets::new(text);
        let mut read_to = 0;
        let tokens = spanned
            .into_iter()
            .map(|spanned| {
                let start = offsets.of(spanned.span.start);
                read_to = offsets.of(spanned.span.end);
                SourceToken {
                    kind: spanned.token,
                    text: &text[start..read_to],
                }
            })
            .collect();
        Self {
            tokens,
            unread: &text[read_to..],
        }
    }
}

/// Converts the tokenizer's line and column, counted in characters, to byte offsets.
struct ByteOffsets {
    text_len: usize,
    line_starts: Vec<usize>,
    char_starts: Vec<usize>,
}

impl ByteOffsets {
    fn new(text: &str) -> Self {
        let mut line_starts = vec![0];
        let mut char_starts = Vec::with_capacity(text.len());
        for (offset, c) in text.char_indices() {
            char_starts.push(offset);
            if c == '\n' {
                line_starts.push(char_starts.len());
            }
        }
        Self {
            text_len: text.len(),
            line_starts,
            char_starts,
        }
    }

    fn of(&self, location: Location) -> usize {
        let count = |number: u64| {
            usize::try_from(number)
                .unwrap_or(usize::MAX)
                .saturating_sub(1)
        };
        self.line_starts
            .get(count(location.line))
            .and_then(|line_start| self.char_starts.get(line_start + count(location.column)))
            .copied()
            .unwrap_or(self.text_len)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_cover_the_text_across_lines_and_multibyte_characters() {
        let text = "SELECT 'é;', -- ü\n  `a b` /* x\ny */ FROM books";

        let read = SourceTokens::from(text);

        assert_eq!(
            read.tokens
                .iter()
                .map(|token| token.text)
                .collect::<String>(),
            text
        );
        assert_eq!(read.unread, "");
    }

    #[test]
    fn an_unterminated_string_is_left_unread() {
        let read = SourceTokens::from("SELECT 'é; still open");

        assert_eq!(read.unread, "'é; still open");
        assert_eq!(
            read.tokens
                .iter()
                .map(|token| token.text)
                .collect::<String>(),
            "SELECT "
        );
    }
}
