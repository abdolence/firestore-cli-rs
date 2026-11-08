use crate::codegen::GenerateCommand;
use crate::collection_tree::CollectionTreeTarget;
use crate::connection::DatabaseId;
use crate::describe::DescribeTarget;
use crate::errors::{FsqError, FsqResult, UnclosedQuote, UnknownMetaCommand};
use crate::render::ExpandedMode;
use clap::{Args, Command, FromArgMatches};
use std::str::FromStr;

#[derive(Debug, Clone, PartialEq)]
pub enum MetaCommand {
    Quit,
    Help,
    /// `\l`, or with `--recursive` a walk of the collections below the root.
    ListCollections(Option<CollectionTreeTarget>),
    Describe(DescribeTarget),
    /// `\x` without an argument toggles between the table and the expanded view.
    Expanded(Option<ExpandedMode>),
    Connect(DatabaseId),
    /// Without an argument, toggles.
    Timing(Option<Switch>),
    /// Without an argument, toggles.
    Wait(Option<Switch>),
    Generate(GenerateCommand),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Switch {
    On,
    Off,
}

impl Switch {
    pub fn toggled(self) -> Self {
        match self {
            Self::On => Self::Off,
            Self::Off => Self::On,
        }
    }
}

impl std::fmt::Display for Switch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::On => "on",
            Self::Off => "off",
        })
    }
}

/// A meta-command as the help lists it and the REPL completes it.
pub struct MetaCommandHelp {
    pub usage: &'static str,
    pub summary: &'static str,
}

impl MetaCommandHelp {
    pub fn name(&self) -> &'static str {
        self.usage
            .split_once(' ')
            .map_or(self.usage, |(name, _)| name)
    }
}

pub const META_COMMANDS: &[MetaCommandHelp] = &[
    MetaCommandHelp {
        usage: "\\l [--recursive [--depth N] [--sample K]]",
        summary: "list the collections under the root; --recursive samples K documents (20) per collection, down to depth N (2)",
    },
    MetaCommandHelp {
        usage: "\\d <collection> [--sample N]",
        summary: "list a collection's field paths, types, % present and an example",
    },
    MetaCommandHelp {
        usage: "\\x [on|off|auto]",
        summary: "expanded view, one block per document; auto when a table is too wide",
    },
    MetaCommandHelp {
        usage: "\\c <database>",
        summary: "connect to another database of the project",
    },
    MetaCommandHelp {
        usage: "\\timing [on|off]",
        summary: "round trip time and execution stats in the footer",
    },
    MetaCommandHelp {
        usage: "\\wait [on|off]",
        summary: "wait for CREATE INDEX and ALTER COLLECTION to finish",
    },
    MetaCommandHelp {
        usage: "\\gen ['SELECT'] [--param NAME=VALUE] [--sample N] [--offline]",
        summary: "fluent Rust code of a SELECT, or of the last one run",
    },
    MetaCommandHelp {
        usage: "\\?",
        summary: "this help",
    },
    MetaCommandHelp {
        usage: "\\q",
        summary: "quit",
    },
];

const QUOTING_NOTE: &str = "The arguments of \\l, \\d and \\gen are split as a shell splits them: quote the SELECT, and a value that holds spaces or quotes, as in\n  \\gen 'SELECT title FROM books WHERE genre = @genre' --param 'genre=\"science fiction\"'\n";

/// The text of `\?`: a line per meta-command, with the summaries in one column, and a note on
/// quoting.
pub fn meta_help() -> String {
    let width = META_COMMANDS
        .iter()
        .map(|command| command.usage.len())
        .max()
        .unwrap_or_default();
    META_COMMANDS
        .iter()
        .map(|command| format!("{:<width$}  {}\n", command.usage, command.summary))
        .chain([format!("\n{QUOTING_NOTE}")])
        .collect()
}

impl FromStr for MetaCommand {
    type Err = FsqError;

    fn from_str(line: &str) -> FsqResult<Self> {
        let line = line.trim_start();
        let (command, rest) = line.split_once(char::is_whitespace).unwrap_or((line, ""));
        let arguments: Vec<&str> = rest.split_whitespace().collect();
        let command = match command {
            "\\q" | "\\quit" => no_arguments("\\q", &arguments, Self::Quit)?,
            "\\?" => no_arguments("\\?", &arguments, Self::Help)?,
            "\\l" => {
                let ListCollections { recursive, target } = parse_arguments("\\l", rest)?;
                Self::ListCollections(recursive.then_some(target))
            }
            "\\d" => Self::Describe(parse_arguments("\\d", rest)?),
            "\\x" => Self::Expanded(optional_argument(
                "\\x",
                &arguments,
                "on, off or auto",
                ExpandedMode::from_argument,
            )?),
            "\\c" => match arguments.as_slice() {
                [database] => Self::Connect(DatabaseId::from(*database)),
                [] => return Err(FsqError::missing_meta_argument("\\c", "a database ID")),
                [_, extra, ..] => {
                    return Err(FsqError::invalid_meta_argument(
                        "\\c",
                        extra,
                        "one database ID",
                    ))
                }
            },
            "\\timing" => Self::Timing(optional_argument(
                "\\timing",
                &arguments,
                "on or off",
                Switch::from_argument,
            )?),
            "\\wait" => Self::Wait(optional_argument(
                "\\wait",
                &arguments,
                "on or off",
                Switch::from_argument,
            )?),
            "\\gen" => Self::Generate(parse_arguments("\\gen", rest)?),
            other => {
                return Err(FsqError::UnknownMetaCommand(UnknownMetaCommand {
                    command: other.to_string(),
                }))
            }
        };
        Ok(command)
    }
}

/// The arguments of `\l`: the options of a recursive listing only make sense with `--recursive`.
#[derive(Args)]
struct ListCollections {
    #[arg(long)]
    recursive: bool,

    #[command(flatten)]
    target: CollectionTreeTarget,
}

/// Reads the arguments of a meta-command as clap reads those of a command, after the quoting of
/// a shell has split them into words.
fn parse_arguments<A: Args + FromArgMatches>(
    command: &'static str,
    arguments: &str,
) -> FsqResult<A> {
    let words = shlex::split(arguments).ok_or_else(|| {
        FsqError::UnclosedQuote(UnclosedQuote {
            command,
            arguments: arguments.to_string(),
        })
    })?;
    let matches = A::augment_args(
        Command::new(command)
            .no_binary_name(true)
            .disable_help_flag(true),
    )
    .try_get_matches_from(words)?;
    Ok(A::from_arg_matches(&matches)?)
}

impl Switch {
    /// The switch a `\timing` or `\wait` argument names, in any case.
    fn from_argument(argument: &str) -> Option<Self> {
        match argument.to_ascii_lowercase().as_str() {
            "on" => Some(Self::On),
            "off" => Some(Self::Off),
            _ => None,
        }
    }
}

fn no_arguments(
    command: &'static str,
    arguments: &[&str],
    parsed: MetaCommand,
) -> FsqResult<MetaCommand> {
    match arguments.first() {
        None => Ok(parsed),
        Some(argument) => Err(FsqError::invalid_meta_argument(
            command,
            argument,
            "no arguments",
        )),
    }
}

fn optional_argument<T>(
    command: &'static str,
    arguments: &[&str],
    expected: &'static str,
    from_argument: fn(&str) -> Option<T>,
) -> FsqResult<Option<T>> {
    match arguments {
        [] => Ok(None),
        [argument] => from_argument(argument)
            .map(Some)
            .ok_or_else(|| FsqError::invalid_meta_argument(command, argument, expected)),
        [_, extra, ..] => Err(FsqError::invalid_meta_argument(command, extra, expected)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codegen::ParameterBinding;
    use crate::describe::SampleSize;
    use crate::errors::{InvalidMetaArgument, MissingMetaArgument};

    fn parsed(line: &str) -> MetaCommand {
        line.parse().expect(line)
    }

    #[test]
    fn meta_commands_parse_with_their_arguments() {
        assert_eq!(parsed("\\q"), MetaCommand::Quit);
        assert_eq!(parsed("\\l"), MetaCommand::ListCollections(None));
        assert_eq!(parsed("\\x"), MetaCommand::Expanded(None));
        assert_eq!(
            parsed("\\x auto"),
            MetaCommand::Expanded(Some(ExpandedMode::Auto))
        );
        assert_eq!(parsed("\\timing on"), MetaCommand::Timing(Some(Switch::On)));
        assert_eq!(parsed("\\wait OFF"), MetaCommand::Wait(Some(Switch::Off)));
        assert_eq!(
            parsed("\\c firestore-rs-enterprise"),
            MetaCommand::Connect(DatabaseId::from("firestore-rs-enterprise"))
        );
        assert_eq!(
            parsed("\\d users/alice/orders --sample 20"),
            MetaCommand::Describe(DescribeTarget {
                collection: "users/alice/orders".parse().expect("a collection path"),
                sample: "20".parse().expect("a sample size"),
            })
        );
        assert_eq!(
            parsed("\\d books"),
            MetaCommand::Describe(DescribeTarget {
                collection: "books".parse().expect("a collection path"),
                sample: SampleSize::default(),
            })
        );
    }

    fn generated(line: &str) -> GenerateCommand {
        match parsed(line) {
            MetaCommand::Generate(command) => command,
            other => panic!("{other:?}"),
        }
    }

    fn binding(argument: &str) -> ParameterBinding {
        argument.parse().expect(argument)
    }

    #[test]
    fn an_option_inside_a_string_of_the_select_belongs_to_the_select() {
        let command =
            generated(r#"\gen "SELECT title FROM books WHERE note = 'see --param x'" --param a=1"#);

        assert_eq!(
            command.sql.as_deref(),
            Some("SELECT title FROM books WHERE note = 'see --param x'")
        );
        assert_eq!(command.options.parameters, vec![binding("a=1")]);
    }

    #[test]
    fn an_option_inside_a_comment_of_the_select_belongs_to_the_select() {
        let command = generated("\\gen 'SELECT title FROM books -- --sample 5'");

        assert_eq!(
            command.sql.as_deref(),
            Some("SELECT title FROM books -- --sample 5")
        );
        assert!(command.options.parameters.is_empty());
        assert_eq!(
            command.options.sample_rows(),
            Some(SampleSize::CODEGEN_DEFAULT)
        );
    }

    #[test]
    fn gen_without_a_select_leaves_it_to_the_last_one_that_ran() {
        assert_eq!(generated("\\gen").sql, None);
        assert_eq!(generated("\\gen --offline").options.sample_rows(), None);
    }

    #[test]
    fn a_json_parameter_with_a_space_is_quoted_like_a_shell_argument() {
        let command = generated(r#"\gen 'SELECT 1' --param 'genre="science fiction"'"#);

        assert_eq!(
            command.options.parameters,
            vec![binding("genre=\"science fiction\"")]
        );
    }

    #[test]
    fn a_quote_that_is_never_closed_is_refused() {
        assert!(matches!(
            "\\gen 'SELECT 1".parse::<MetaCommand>(),
            Err(FsqError::UnclosedQuote(_))
        ));
    }

    #[test]
    fn a_recursive_listing_takes_a_depth_and_a_sample_in_any_order() {
        let target = CollectionTreeTarget {
            depth: "3".parse().expect("a depth"),
            sample: "5".parse().expect("a sample size"),
        };
        assert_eq!(
            parsed("\\l --recursive --depth 3 --sample 5"),
            MetaCommand::ListCollections(Some(target))
        );
        assert_eq!(
            parsed("\\l --sample 5 --depth 3 --recursive"),
            MetaCommand::ListCollections(Some(target))
        );
    }

    #[test]
    fn a_wrong_argument_names_the_command_and_the_argument() {
        match "\\x sideways".parse::<MetaCommand>() {
            Err(FsqError::InvalidMetaArgument(InvalidMetaArgument {
                command, argument, ..
            })) => {
                assert_eq!(command, "\\x");
                assert_eq!(argument, "sideways");
            }
            other => panic!("{other:?}"),
        }
        assert!(matches!(
            "\\c".parse::<MetaCommand>(),
            Err(FsqError::MissingMetaArgument(MissingMetaArgument {
                command: "\\c",
                ..
            }))
        ));
        assert!(matches!(
            "\\frobnicate".parse::<MetaCommand>(),
            Err(FsqError::UnknownMetaCommand(UnknownMetaCommand { command })) if command == "\\frobnicate"
        ));
    }

    #[test]
    fn every_listed_command_is_one_the_parser_knows() {
        for command in META_COMMANDS {
            assert!(!matches!(
                command.name().parse::<MetaCommand>(),
                Err(FsqError::UnknownMetaCommand(_))
            ));
        }
    }
}
