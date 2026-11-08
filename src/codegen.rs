use crate::describe::SampleSize;
use crate::errors::{FsqError, FsqResult, InvalidParameter, ParameterProblem};
use crate::render::Theme;
use firestore::{FirestoreDb, FirestoreSqlOptions, FirestoreSqlParameters, FirestoreSqlStatement};
use std::str::FromStr;

/// The rows `--sample` fetches to type the row struct when it is not given.
const DEFAULT_SAMPLE_ROWS: u32 = 100;

/// Words of the generated Rust that are styled on a terminal.
const RUST_KEYWORDS: &[&str] = &["use", "struct", "async", "fn", "await"];

#[derive(Debug, Clone, PartialEq, clap::Args)]
pub struct CodegenArgs {
    #[arg(
        value_name = "SELECT",
        help = "The SELECT to show the fluent Rust code of"
    )]
    pub sql: String,

    #[command(flatten)]
    pub options: CodegenOptions,
}

#[derive(Debug, Clone, Default, PartialEq, clap::Args)]
pub struct CodegenOptions {
    #[arg(
        long = "param",
        value_name = "NAME=VALUE",
        help = "Bind @name to a JSON literal, which gives the argument its type: 4, 4.0, true, \"text\""
    )]
    pub parameters: Vec<ParameterBinding>,

    #[arg(
        long,
        value_name = "N",
        conflicts_with = "offline",
        help = "Run the SELECT for at most this many rows to type the row struct. Default: 100"
    )]
    pub sample: Option<SampleSize>,

    #[arg(
        long,
        help = "Do not run the SELECT: every field is typed as unknown JSON"
    )]
    pub offline: bool,
}

impl CodegenOptions {
    /// How many rows to run the SELECT for, or `None` when `--offline` skips the run.
    pub fn sample_rows(&self) -> Option<u32> {
        (!self.offline).then(|| {
            self.sample
                .map_or(DEFAULT_SAMPLE_ROWS, SampleSize::documents)
        })
    }

    /// The code of `sql` with every field of the row struct typed as unknown, from the text alone.
    pub fn offline(&self, sql: &str) -> FsqResult<String> {
        Ok(FirestoreSqlStatement::fluent_rust(
            sql,
            &FirestoreSqlParameters::from(self),
            &[],
        )?)
    }

    /// The code of `sql`, with the row struct typed from the first `rows` rows that `db` returns
    /// for it. The statement is checked before it runs, so that a statement that is not a
    /// SELECT is refused and never sent.
    pub async fn sampled(&self, db: &FirestoreDb, sql: &str, rows: u32) -> FsqResult<String> {
        self.offline(sql)?;
        let sample = self
            .parameters
            .iter()
            .fold(db.fluent().sql(sql), |builder, parameter| {
                builder.param(parameter.name.clone(), &parameter.value)
            })
            .options(FirestoreSqlOptions::read_only().with_max_rows(rows))
            .query()
            .await?;
        Ok(FirestoreSqlStatement::fluent_rust(
            sql,
            &FirestoreSqlParameters::from(self),
            &sample,
        )?)
    }
}

impl From<&CodegenOptions> for FirestoreSqlParameters {
    fn from(options: &CodegenOptions) -> Self {
        options
            .parameters
            .iter()
            .fold(Self::new(), |parameters, parameter| {
                parameters.param(parameter.name.clone(), &parameter.value)
            })
    }
}

/// A `--param`: the name of a `@name` parameter and the JSON literal bound to it.
#[derive(Debug, Clone, PartialEq)]
pub struct ParameterBinding {
    pub name: String,
    pub value: serde_json::Value,
}

impl FromStr for ParameterBinding {
    type Err = FsqError;

    fn from_str(argument: &str) -> FsqResult<Self> {
        let invalid = |problem| {
            FsqError::InvalidParameter(InvalidParameter {
                argument: argument.to_string(),
                problem,
            })
        };
        let (name, value) = argument
            .split_once('=')
            .ok_or_else(|| invalid(ParameterProblem::MissingValue))?;
        if name.trim().is_empty() {
            return Err(invalid(ParameterProblem::EmptyName));
        }
        Ok(Self {
            name: name.trim().to_string(),
            value: serde_json::from_str(value.trim())
                .map_err(|source| invalid(ParameterProblem::NotJson(source)))?,
        })
    }
}

/// The arguments of `\gen`: a SELECT, or none for the last one that ran, and the options of
/// `fsq codegen`.
#[derive(Debug, Clone, PartialEq)]
pub struct GenerateCommand {
    pub sql: Option<String>,
    pub options: CodegenOptions,
}

impl FromStr for GenerateCommand {
    type Err = FsqError;

    /// The text after `\gen`. The SELECT runs up to the first `--param`, `--sample` or
    /// `--offline` that starts a word, and each option takes the text up to the next one, so
    /// that a JSON string can hold spaces.
    fn from_str(arguments: &str) -> FsqResult<Self> {
        let option_starts: Vec<usize> = arguments
            .match_indices("--")
            .map(|(start, _)| start)
            .filter(|start| {
                let starts_word = arguments[..*start]
                    .chars()
                    .next_back()
                    .is_none_or(char::is_whitespace);
                let word = arguments[*start..]
                    .split_whitespace()
                    .next()
                    .unwrap_or_default();
                starts_word && matches!(word, "--param" | "--sample" | "--offline")
            })
            .collect();
        let sql_end = option_starts.first().copied().unwrap_or(arguments.len());
        let sql = arguments[..sql_end].trim();
        let mut options = CodegenOptions::default();
        for (index, start) in option_starts.iter().enumerate() {
            let end = option_starts
                .get(index + 1)
                .copied()
                .unwrap_or(arguments.len());
            let (option, value) = arguments[*start..end]
                .split_once(char::is_whitespace)
                .map_or((&arguments[*start..end], ""), |(option, value)| {
                    (option, value.trim())
                });
            match option {
                "--param" => options.parameters.push(value.parse()?),
                "--sample" => options.sample = Some(value.parse()?),
                _ if !value.is_empty() => {
                    return Err(FsqError::invalid_meta_argument(
                        "\\gen",
                        value,
                        "no value after --offline",
                    ))
                }
                _ => options.offline = true,
            }
        }
        if options.offline && options.sample.is_some() {
            return Err(FsqError::invalid_meta_argument(
                "\\gen",
                "--offline",
                "--sample or --offline, not both",
            ));
        }
        Ok(Self {
            sql: (!sql.is_empty()).then(|| sql.to_string()),
            options,
        })
    }
}

/// `snippet` with the keywords styled and the comment lines muted.
pub fn highlighted(snippet: &str, theme: &Theme) -> String {
    let is_delimiter = |c: char| !(c.is_alphanumeric() || c == '_');
    let mut text = String::new();
    for line in snippet.lines() {
        if line.trim_start().starts_with("//") {
            text.push_str(&theme.muted.apply_to(line).to_string());
        } else {
            for piece in line.split_inclusive(is_delimiter) {
                let (word, delimiter) = piece.split_at(piece.trim_end_matches(is_delimiter).len());
                if RUST_KEYWORDS.contains(&word) {
                    text.push_str(&theme.key.apply_to(word).to_string());
                } else {
                    text.push_str(word);
                }
                text.push_str(delimiter);
            }
        }
        text.push('\n');
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::args::{CliArgs, CliCommand};
    use crate::errors::InvalidMetaArgument;
    use clap::Parser;
    use serde_json::json;

    fn binding(argument: &str) -> ParameterBinding {
        argument.parse().expect(argument)
    }

    fn generate(arguments: &str) -> GenerateCommand {
        arguments.parse().expect(arguments)
    }

    #[test]
    fn a_parameter_value_is_a_json_literal_that_keeps_its_type() {
        assert_eq!(binding("n=4").value, json!(4));
        assert_eq!(binding("min=4.0").value, json!(4.0));
        assert_eq!(binding("genre=\"sci fi\"").value, json!("sci fi"));
        assert_eq!(binding("subtitle=null").value, json!(null));
        assert_eq!(binding("min = 4.0").name, "min");
    }

    #[test]
    fn a_parameter_needs_a_name_and_a_json_value() {
        let problem = |argument: &str| match argument.parse::<ParameterBinding>() {
            Err(FsqError::InvalidParameter(InvalidParameter { problem, .. })) => problem,
            other => panic!("{other:?}"),
        };

        assert!(matches!(problem("min"), ParameterProblem::MissingValue));
        assert!(matches!(problem("=4"), ParameterProblem::EmptyName));
        assert!(matches!(
            problem("genre=Dystopian"),
            ParameterProblem::NotJson(_)
        ));
    }

    #[test]
    fn gen_arguments_split_into_the_select_and_its_options() {
        let command = generate(
            " SELECT title FROM books WHERE genre = @genre AND note = 'a --b' -- note --param genre=\"sci fi\" --param min=4.0  --sample 20",
        );

        assert_eq!(
            command.sql.as_deref(),
            Some("SELECT title FROM books WHERE genre = @genre AND note = 'a --b' -- note")
        );
        assert_eq!(
            command.options.parameters,
            vec![binding("genre=\"sci fi\""), binding("min=4.0")]
        );
        assert_eq!(command.options.sample_rows(), Some(20));
    }

    #[test]
    fn gen_without_a_select_leaves_it_to_the_last_one_that_ran() {
        assert_eq!(generate("").sql, None);
        let offline = generate("--offline");
        assert_eq!(offline.sql, None);
        assert_eq!(offline.options.sample_rows(), None);
    }

    #[test]
    fn gen_refuses_an_offline_run_that_also_asks_for_a_sample() {
        let error = "SELECT 1 --offline --sample 5"
            .parse::<GenerateCommand>()
            .expect_err("sample and offline conflict");

        assert!(matches!(
            error,
            FsqError::InvalidMetaArgument(InvalidMetaArgument {
                command: "\\gen",
                ..
            })
        ));
    }

    #[test]
    fn a_sample_is_a_hundred_rows_unless_given() {
        assert_eq!(CodegenOptions::default().sample_rows(), Some(100));
    }

    #[test]
    fn the_command_line_takes_parameters_and_refuses_sample_with_offline() {
        let parsed = CliArgs::try_parse_from([
            "fsq",
            "codegen",
            "SELECT 1",
            "--param",
            "min=4.0",
            "--offline",
        ])
        .expect("arguments parse");
        match parsed.command {
            Some(CliCommand::Codegen(args)) => {
                assert_eq!(args.sql, "SELECT 1");
                assert_eq!(args.options.parameters, vec![binding("min=4.0")]);
                assert_eq!(args.options.sample_rows(), None);
            }
            other => panic!("{other:?}"),
        }

        let error =
            CliArgs::try_parse_from(["fsq", "codegen", "SELECT 1", "--offline", "--sample", "5"])
                .expect_err("sample and offline conflict");
        assert_eq!(error.kind(), clap::error::ErrorKind::ArgumentConflict);
    }

    #[test]
    fn keywords_are_styled_and_comment_lines_are_muted() {
        let theme = Theme::new(true);
        let snippet = "// SELECT 1 FROM user\nuse firestore::*;\nstruct UserRow {}\n";

        assert_eq!(
            highlighted(snippet, &theme),
            format!(
                "{}\n{} firestore::*;\n{} UserRow {{}}\n",
                theme.muted.apply_to("// SELECT 1 FROM user"),
                theme.key.apply_to("use"),
                theme.key.apply_to("struct"),
            )
        );
        assert_eq!(highlighted(snippet, &Theme::new(false)), snippet);
    }
}
