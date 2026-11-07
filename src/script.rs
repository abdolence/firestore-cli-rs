use crate::tokens::SourceTokens;
use sqlparser::tokenizer::Token;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Input {
    Statement(String),
    Meta(String),
}

/// Text split into inputs. The crate takes exactly one statement per call, so a script runs
/// statement by statement: each ends at a `;` outside a string, a quoted name or a comment. A
/// line that starts with `\` where a statement would start, after any comments, is a
/// meta-command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Script {
    pub inputs: Vec<Input>,
    /// Statement text after the last `;`, which a REPL keeps reading lines for.
    pub unterminated: Option<String>,
}

impl Script {
    pub fn parse(text: &str) -> Self {
        let mut inputs = Vec::new();
        let mut rest = text;
        'restart: loop {
            let SourceTokens { tokens, unread } = SourceTokens::from(rest);
            let mut statement_start = 0;
            let mut position = 0;
            let mut has_content = false;
            for token in &tokens {
                let start = position;
                position += token.text.len();
                match token.kind {
                    Token::Whitespace(_) => {}
                    Token::Backslash if !has_content => {
                        let end = rest[start..]
                            .find('\n')
                            .map_or(rest.len(), |offset| start + offset);
                        inputs.push(Input::Meta(rest[start..end].trim_end().to_string()));
                        rest = &rest[end..];
                        continue 'restart;
                    }
                    Token::SemiColon => {
                        if has_content {
                            inputs.push(Input::Statement(
                                rest[statement_start..start].trim().to_string(),
                            ));
                        }
                        statement_start = position;
                        has_content = false;
                    }
                    _ => has_content = true,
                }
            }
            let unterminated = (has_content || !unread.is_empty())
                .then(|| rest[statement_start..].trim().to_string());
            return Self {
                inputs,
                unterminated,
            };
        }
    }

    pub fn into_inputs(self) -> Vec<Input> {
        let mut inputs = self.inputs;
        inputs.extend(self.unterminated.map(Input::Statement));
        inputs
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn statement(text: &str) -> Input {
        Input::Statement(text.to_string())
    }

    #[test]
    fn statements_end_at_semicolons_outside_strings_names_and_comments() {
        let script = Script::parse(
            "SELECT 'a;b', \"c\\\";d\" FROM `e;f`; -- g;h\nSELECT 1 /* i;j */ FROM books;\n;\nSELECT 2",
        );

        assert_eq!(
            script.inputs,
            vec![
                statement("SELECT 'a;b', \"c\\\";d\" FROM `e;f`"),
                statement("-- g;h\nSELECT 1 /* i;j */ FROM books"),
            ]
        );
        assert_eq!(script.unterminated, Some("SELECT 2".to_string()));
    }

    #[test]
    fn a_backslash_line_where_a_statement_starts_is_a_meta_command() {
        let script = Script::parse(
            "\\x auto\nSELECT a\n  FROM books; \\l\n-- note\n\\d books --sample 10\n",
        );

        assert_eq!(
            script.into_inputs(),
            vec![
                Input::Meta("\\x auto".to_string()),
                statement("SELECT a\n  FROM books"),
                Input::Meta("\\l".to_string()),
                Input::Meta("\\d books --sample 10".to_string()),
            ]
        );
    }

    #[test]
    fn a_comment_after_the_last_semicolon_is_not_a_statement() {
        let script = Script::parse("SELECT 1; /* done */");

        assert_eq!(script.inputs, vec![statement("SELECT 1")]);
        assert_eq!(script.unterminated, None);
    }

    #[test]
    fn a_meta_command_after_a_leading_block_comment_is_a_meta_command() {
        let script = Script::parse("/* header */\n\\x auto\nSELECT 1;");

        assert_eq!(
            script.inputs,
            vec![Input::Meta("\\x auto".to_string()), statement("SELECT 1")]
        );
    }

    #[test]
    fn a_raw_string_ends_at_its_closing_quote() {
        let script = Script::parse("SELECT r'\\' AS x; SELECT 2;");

        assert_eq!(
            script.inputs,
            vec![statement("SELECT r'\\' AS x"), statement("SELECT 2")]
        );
    }

    #[test]
    fn a_statement_without_its_semicolon_is_unterminated() {
        assert_eq!(
            Script::parse("SELECT 'it''s; still open").unterminated,
            Some("SELECT 'it''s; still open".to_string())
        );
        assert_eq!(Script::parse("SELECT 1;\n-- done\n").unterminated, None);
    }
}
