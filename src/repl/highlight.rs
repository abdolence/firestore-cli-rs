use super::SQL_KEYWORDS;
use nu_ansi_term::{Color, Style};
use reedline::{Highlighter, StyledText};

pub struct SqlHighlighter;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Token {
    Keyword,
    String,
    QuotedName,
    Number,
    Parameter,
    Comment,
    Meta,
    Plain,
}

impl Token {
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
}

impl Highlighter for SqlHighlighter {
    fn highlight(&self, line: &str, _cursor: usize) -> StyledText {
        let mut styled = StyledText::new();
        for (token, text) in tokens(line) {
            styled.push((token.style(), text.to_string()));
        }
        styled
    }
}

fn tokens(line: &str) -> Vec<(Token, &str)> {
    if line.trim_start().starts_with('\\') {
        return vec![(Token::Meta, line)];
    }
    let mut tokens = Vec::new();
    let mut rest = line;
    while let Some(first) = rest.chars().next() {
        let length = match first {
            '\'' | '"' | '`' => quoted_length(rest, first),
            '-' if rest.starts_with("--") => rest.find('\n').unwrap_or(rest.len()),
            '#' => rest.find('\n').unwrap_or(rest.len()),
            '/' if rest.starts_with("/*") => rest.find("*/").map_or(rest.len(), |end| end + 2),
            '@' => 1 + word_length(&rest[1..]),
            c if c.is_ascii_digit() => rest
                .find(|c: char| !c.is_ascii_alphanumeric() && c != '.')
                .unwrap_or(rest.len()),
            c if c.is_alphanumeric() || c == '_' => word_length(rest),
            c => c.len_utf8(),
        };
        let (text, remaining) = rest.split_at(length);
        let token = match first {
            '\'' | '"' => Token::String,
            '`' => Token::QuotedName,
            '-' | '#' | '/' if text.len() > 1 || first == '#' => Token::Comment,
            '@' | '?' => Token::Parameter,
            c if c.is_ascii_digit() => Token::Number,
            _ if SQL_KEYWORDS.contains(&text.to_ascii_uppercase().as_str()) => Token::Keyword,
            _ => Token::Plain,
        };
        tokens.push((token, text));
        rest = remaining;
    }
    tokens
}

fn word_length(text: &str) -> usize {
    text.find(|c: char| !c.is_alphanumeric() && c != '_')
        .unwrap_or(text.len())
}

/// The length of a quoted string or name at the start of `text`, to its closing quote or, while
/// it is still being typed, to the end.
fn quoted_length(text: &str, quote: char) -> usize {
    let mut escaped = false;
    for (position, c) in text.char_indices().skip(1) {
        match c {
            '\\' if quote != '`' && !escaped => escaped = true,
            c if c == quote && !escaped => return position + c.len_utf8(),
            _ => escaped = false,
        }
    }
    text.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_kind_of_token_is_told_apart_and_no_text_is_lost() {
        let line =
            "SELECT `unit-price`, 'a;b' FROM books WHERE stock > 10 AND genre = @genre -- note";

        let tokenised = tokens(line);

        assert_eq!(
            tokenised.iter().map(|(_, text)| *text).collect::<String>(),
            line
        );
        let kinds: Vec<(Token, &str)> = tokenised
            .into_iter()
            .filter(|(token, _)| *token != Token::Plain)
            .collect();
        assert_eq!(
            kinds,
            vec![
                (Token::Keyword, "SELECT"),
                (Token::QuotedName, "`unit-price`"),
                (Token::String, "'a;b'"),
                (Token::Keyword, "FROM"),
                (Token::Keyword, "WHERE"),
                (Token::Number, "10"),
                (Token::Keyword, "AND"),
                (Token::Parameter, "@genre"),
                (Token::Comment, "-- note"),
            ]
        );
        assert_eq!(tokens("\\d books"), vec![(Token::Meta, "\\d books")]);
    }
}
