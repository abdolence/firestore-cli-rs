use super::SQL_KEYWORDS;
use crate::tokens::SourceTokens;
use nu_ansi_term::{Color, Style};
use reedline::{Highlighter, StyledText};
use sqlparser::tokenizer::{Token, Whitespace};

pub struct SqlHighlighter;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Highlight {
    Keyword,
    String,
    QuotedName,
    Number,
    Parameter,
    Comment,
    Meta,
    Plain,
}

impl Highlight {
    fn style(self) -> Style {
        match self {
            Self::Keyword => Style::new().bold().fg(Color::Blue),
            Self::String => Style::new().fg(Color::Green),
            Self::QuotedName => Style::new().fg(Color::Cyan),
            Self::Number => Style::new().fg(Color::Cyan),
            Self::Parameter => Style::new().fg(Color::Magenta),
            Self::Comment => Style::new().dimmed(),
            Self::Meta => Style::new().fg(Color::Yellow),
            Self::Plain => Style::new(),
        }
    }

    /// Text the tokenizer could not finish, which is styled as what it is being typed as.
    fn unread(text: &str) -> Self {
        if text.starts_with('`') {
            Self::QuotedName
        } else if text.starts_with("/*") {
            Self::Comment
        } else {
            Self::String
        }
    }
}

impl From<&Token> for Highlight {
    fn from(token: &Token) -> Self {
        match token {
            Token::Word(word) if word.quote_style.is_some() => Self::QuotedName,
            Token::Word(word) if word.value.starts_with('@') => Self::Parameter,
            Token::Word(word)
                if SQL_KEYWORDS.contains(&word.value.to_ascii_uppercase().as_str()) =>
            {
                Self::Keyword
            }
            Token::Placeholder(_) => Self::Parameter,
            Token::Number(..) => Self::Number,
            Token::Whitespace(
                Whitespace::SingleLineComment { .. } | Whitespace::MultiLineComment(_),
            ) => Self::Comment,
            Token::SingleQuotedString(_)
            | Token::DoubleQuotedString(_)
            | Token::TripleSingleQuotedString(_)
            | Token::TripleDoubleQuotedString(_)
            | Token::SingleQuotedByteStringLiteral(_)
            | Token::DoubleQuotedByteStringLiteral(_)
            | Token::TripleSingleQuotedByteStringLiteral(_)
            | Token::TripleDoubleQuotedByteStringLiteral(_)
            | Token::SingleQuotedRawStringLiteral(_)
            | Token::DoubleQuotedRawStringLiteral(_)
            | Token::TripleSingleQuotedRawStringLiteral(_)
            | Token::TripleDoubleQuotedRawStringLiteral(_) => Self::String,
            _ => Self::Plain,
        }
    }
}

impl Highlighter for SqlHighlighter {
    fn highlight(&self, line: &str, _cursor: usize) -> StyledText {
        let mut styled = StyledText::new();
        for (highlight, text) in highlights(line) {
            styled.push((highlight.style(), text.to_string()));
        }
        styled
    }
}

fn highlights(line: &str) -> Vec<(Highlight, &str)> {
    if line.trim_start().starts_with('\\') {
        return vec![(Highlight::Meta, line)];
    }
    let SourceTokens { tokens, unread } = SourceTokens::from(line);
    let mut highlights: Vec<_> = tokens
        .into_iter()
        .map(|token| (Highlight::from(&token.kind), token.text))
        .collect();
    if !unread.is_empty() {
        highlights.push((Highlight::unread(unread), unread));
    }
    highlights
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_kind_of_token_is_told_apart_and_no_text_is_lost() {
        let line = "SELECT `unit-price`, 'a;b', r'\\d' FROM books WHERE stock > 10 AND genre = @genre /* x */ -- note";

        let tokenised = highlights(line);

        assert_eq!(
            tokenised.iter().map(|(_, text)| *text).collect::<String>(),
            line
        );
        let kinds: Vec<(Highlight, &str)> = tokenised
            .into_iter()
            .filter(|(highlight, _)| *highlight != Highlight::Plain)
            .collect();
        assert_eq!(
            kinds,
            vec![
                (Highlight::Keyword, "SELECT"),
                (Highlight::QuotedName, "`unit-price`"),
                (Highlight::String, "'a;b'"),
                (Highlight::String, "r'\\d'"),
                (Highlight::Keyword, "FROM"),
                (Highlight::Keyword, "WHERE"),
                (Highlight::Number, "10"),
                (Highlight::Keyword, "AND"),
                (Highlight::Parameter, "@genre"),
                (Highlight::Comment, "/* x */"),
                (Highlight::Comment, "-- note"),
            ]
        );
        assert_eq!(
            highlights("\\d books"),
            vec![(Highlight::Meta, "\\d books")]
        );
    }

    #[test]
    fn a_string_still_being_typed_is_styled_as_a_string() {
        assert_eq!(
            highlights("SELECT 'abc"),
            vec![
                (Highlight::Keyword, "SELECT"),
                (Highlight::Plain, " "),
                (Highlight::String, "'abc"),
            ]
        );
    }
}
