#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Input {
    Statement(String),
    Meta(String),
}

/// Text split into inputs. The crate takes exactly one statement per call, so a script runs
/// statement by statement: each ends at a `;` outside a string, a quoted name or a comment. A
/// line that starts with `\` where a statement would start is a meta-command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Script {
    pub inputs: Vec<Input>,
    /// Statement text after the last `;`, which a REPL keeps reading lines for.
    pub unterminated: Option<String>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Code,
    Quoted(char),
    LineComment,
    BlockComment,
}

impl Script {
    pub fn parse(text: &str) -> Self {
        let chars: Vec<char> = text.chars().collect();
        let mut inputs = Vec::new();
        let mut statement = String::new();
        let mut has_content = false;
        let mut mode = Mode::Code;
        let mut position = 0;
        while let Some(&current) = chars.get(position) {
            let next = chars.get(position + 1).copied();
            match mode {
                Mode::Code if current == '\\' && !has_content => {
                    let end = chars[position..]
                        .iter()
                        .position(|&c| c == '\n')
                        .map_or(chars.len(), |offset| position + offset);
                    let line: String = chars[position..end].iter().collect();
                    inputs.push(Input::Meta(line.trim_end().to_string()));
                    statement.clear();
                    position = end;
                    continue;
                }
                Mode::Code if current == ';' => {
                    if has_content {
                        inputs.push(Input::Statement(statement.trim().to_string()));
                    }
                    statement.clear();
                    has_content = false;
                    position += 1;
                    continue;
                }
                Mode::Code => match (current, next) {
                    ('\'' | '"' | '`', _) => mode = Mode::Quoted(current),
                    ('-', Some('-')) | ('#', _) => mode = Mode::LineComment,
                    ('/', Some('*')) => {
                        statement.push(current);
                        position += 1;
                        mode = Mode::BlockComment;
                    }
                    _ => {}
                },
                Mode::Quoted(quote) if current == '\\' && quote != '`' => {
                    statement.push(current);
                    if let Some(escaped) = next {
                        statement.push(escaped);
                    }
                    position += 2;
                    continue;
                }
                Mode::Quoted(quote) if current == quote => mode = Mode::Code,
                Mode::LineComment if current == '\n' => mode = Mode::Code,
                Mode::BlockComment if current == '*' && next == Some('/') => {
                    statement.push(current);
                    position += 1;
                    mode = Mode::Code;
                }
                Mode::Quoted(_) | Mode::LineComment | Mode::BlockComment => {}
            }
            let in_comment = matches!(mode, Mode::LineComment | Mode::BlockComment);
            if !in_comment && !current.is_whitespace() {
                has_content = true;
            }
            statement.push(chars[position]);
            position += 1;
        }
        Self {
            inputs,
            unterminated: has_content.then(|| statement.trim().to_string()),
        }
    }

    pub fn into_inputs(self) -> Vec<Input> {
        let mut inputs = self.inputs;
        inputs.extend(self.unterminated.map(Input::Statement));
        inputs
    }
}

pub struct SqlName<'a>(pub &'a str);

impl std::fmt::Display for SqlName<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut chars = self.0.chars();
        let plain = chars
            .next()
            .is_some_and(|first| first.is_ascii_alphabetic() || first == '_')
            && chars.all(|c| c.is_ascii_alphanumeric() || c == '_');
        if plain {
            f.write_str(self.0)
        } else {
            write!(f, "`{}`", self.0.replace('`', "\\`"))
        }
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
    fn a_statement_without_its_semicolon_is_unterminated() {
        assert_eq!(
            Script::parse("SELECT 'it''s; still open").unterminated,
            Some("SELECT 'it''s; still open".to_string())
        );
        assert_eq!(Script::parse("SELECT 1;\n-- done\n").unterminated, None);
    }
}
