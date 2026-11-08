use crate::codegen::GenerateCommand;
use crate::collection_tree::CollectionTreeTarget;
use crate::connection::DatabaseId;
use crate::describe::{CollectionPath, DescribeTarget, SampleSize};
use crate::errors::{FsqError, FsqResult, UnknownMetaCommand};
use crate::render::ExpandedMode;
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
        usage: "\\gen [SELECT] [--param name=value] [--sample N] [--offline]",
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

/// The text of `\?`: a line per meta-command, with the summaries in one column.
pub fn meta_help() -> String {
    let width = META_COMMANDS
        .iter()
        .map(|command| command.usage.len())
        .max()
        .unwrap_or_default();
    META_COMMANDS
        .iter()
        .map(|command| format!("{:<width$}  {}\n", command.usage, command.summary))
        .collect()
}

impl FromStr for MetaCommand {
    type Err = FsqError;

    fn from_str(line: &str) -> FsqResult<Self> {
        let mut words = line.split_whitespace();
        let command = words.next().unwrap_or_default();
        let arguments: Vec<&str> = words.collect();
        let command = match command {
            "\\q" | "\\quit" => no_arguments("\\q", &arguments, Self::Quit)?,
            "\\?" => no_arguments("\\?", &arguments, Self::Help)?,
            "\\l" => Self::ListCollections(CollectionTreeTarget::from_arguments(&arguments)?),
            "\\d" => Self::Describe(DescribeTarget::try_from(arguments.as_slice())?),
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
            "\\gen" => Self::Generate(line.trim_start().trim_start_matches("\\gen").parse()?),
            other => {
                return Err(FsqError::UnknownMetaCommand(UnknownMetaCommand {
                    command: other.to_string(),
                }))
            }
        };
        Ok(command)
    }
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

impl CollectionTreeTarget {
    /// The arguments of `\l`: none, or `--recursive` with an optional `--depth N` and
    /// `--sample K`.
    fn from_arguments(arguments: &[&str]) -> FsqResult<Option<Self>> {
        const EXPECTED: &str = "--recursive with an optional --depth N and --sample K";
        let mut target = Self::default();
        let mut recursive = false;
        let mut first_option = None;
        let mut arguments = arguments.iter().copied();
        while let Some(argument) = arguments.next() {
            match argument {
                "--recursive" => recursive = true,
                "--depth" | "--sample" => {
                    let value = arguments.next().ok_or_else(|| {
                        FsqError::missing_meta_argument("\\l", "a number after --depth or --sample")
                    })?;
                    if argument == "--depth" {
                        target.depth = value.parse()?;
                    } else {
                        target.sample = value.parse()?;
                    }
                    first_option.get_or_insert(argument);
                }
                other => return Err(FsqError::invalid_meta_argument("\\l", other, EXPECTED)),
            }
        }
        match (recursive, first_option) {
            (true, _) => Ok(Some(target)),
            (false, None) => Ok(None),
            (false, Some(option)) => Err(FsqError::invalid_meta_argument("\\l", option, EXPECTED)),
        }
    }
}

impl TryFrom<&[&str]> for DescribeTarget {
    type Error = FsqError;

    /// The arguments of `\d`: a collection and an optional `--sample N`.
    fn try_from(arguments: &[&str]) -> FsqResult<Self> {
        let (collection, sample) = match arguments {
            [collection] => (collection, None),
            [collection, "--sample", size] => (collection, Some(size)),
            [] => return Err(FsqError::missing_meta_argument("\\d", "a collection")),
            [_, argument, ..] => {
                return Err(FsqError::invalid_meta_argument(
                    "\\d",
                    argument,
                    "a collection and an optional --sample N",
                ))
            }
        };
        Ok(Self {
            collection: collection.parse::<CollectionPath>()?,
            sample: sample
                .map(|size| size.parse::<SampleSize>())
                .transpose()?
                .unwrap_or_default(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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

    #[test]
    fn a_recursive_listing_takes_a_depth_and_a_sample_in_any_order() {
        assert_eq!(
            parsed("\\l --recursive"),
            MetaCommand::ListCollections(Some(CollectionTreeTarget::default()))
        );
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
    fn a_depth_or_a_sample_needs_recursive() {
        match "\\l --depth 3".parse::<MetaCommand>() {
            Err(FsqError::InvalidMetaArgument(InvalidMetaArgument {
                command, argument, ..
            })) => {
                assert_eq!(command, "\\l");
                assert_eq!(argument, "--depth");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_depth_is_a_whole_number_of_one_or_more() {
        assert!(matches!(
            "\\l --recursive --depth 0".parse::<MetaCommand>(),
            Err(FsqError::InvalidTreeDepth(_))
        ));
        assert!(matches!(
            "\\l --recursive --depth".parse::<MetaCommand>(),
            Err(FsqError::MissingMetaArgument(_))
        ));
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
