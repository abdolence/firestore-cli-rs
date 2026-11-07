use crate::connection::DatabaseId;
use crate::describe::{CollectionPath, DescribeTarget, SampleSize};
use crate::errors::{FsqError, FsqResult, UnknownChoice, UnknownMetaCommand};
use crate::render::ExpandedMode;
use std::str::FromStr;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MetaCommand {
    Quit,
    Help,
    ListCollections,
    Describe(DescribeTarget),
    /// `\x` without an argument toggles between the table and the expanded view.
    Expanded(Option<ExpandedMode>),
    Connect(DatabaseId),
    /// Without an argument, toggles.
    Timing(Option<Switch>),
    /// Without an argument, toggles.
    Wait(Option<Switch>),
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

pub const META_HELP: &str = "\
\\l                         list the collections under the root, as SHOW COLLECTIONS
\\d <collection> [--sample N] list a collection's field paths, types, % present and an example
\\x [on|off|auto]           expanded view, one block per document; auto when a table is too wide
\\c <database>              connect to another database of the project
\\timing [on|off]           round trip time and execution stats in the footer
\\wait [on|off]             wait for CREATE INDEX and ALTER COLLECTION to finish
\\?                         this help
\\q                         quit";

impl FromStr for MetaCommand {
    type Err = FsqError;

    fn from_str(line: &str) -> FsqResult<Self> {
        let mut words = line.split_whitespace();
        let command = words.next().unwrap_or_default();
        let arguments: Vec<&str> = words.collect();
        let command = match command {
            "\\q" | "\\quit" => no_arguments("\\q", &arguments, Self::Quit)?,
            "\\?" => no_arguments("\\?", &arguments, Self::Help)?,
            "\\l" => no_arguments("\\l", &arguments, Self::ListCollections)?,
            "\\d" => Self::Describe(DescribeTarget::try_from(arguments.as_slice())?),
            "\\x" => Self::Expanded(optional_argument("\\x", &arguments, "on, off or auto")?),
            "\\c" => match arguments.as_slice() {
                [database] => {
                    let Ok(database) = database.parse();
                    Self::Connect(database)
                }
                [] => return Err(FsqError::missing_meta_argument("\\c", "a database ID")),
                [_, extra, ..] => {
                    return Err(FsqError::invalid_meta_argument(
                        "\\c",
                        extra,
                        "one database ID",
                    ))
                }
            },
            "\\timing" => Self::Timing(optional_argument("\\timing", &arguments, "on or off")?),
            "\\wait" => Self::Wait(optional_argument("\\wait", &arguments, "on or off")?),
            other => {
                return Err(FsqError::UnknownMetaCommand(UnknownMetaCommand {
                    command: other.to_string(),
                }))
            }
        };
        Ok(command)
    }
}

impl FromStr for Switch {
    type Err = UnknownChoice;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.to_ascii_lowercase().as_str() {
            "on" => Ok(Self::On),
            "off" => Ok(Self::Off),
            _ => Err(UnknownChoice(value.to_string())),
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

fn optional_argument<T: FromStr>(
    command: &'static str,
    arguments: &[&str],
    expected: &'static str,
) -> FsqResult<Option<T>> {
    match arguments {
        [] => Ok(None),
        [argument] => argument
            .parse()
            .map(Some)
            .map_err(|_| FsqError::invalid_meta_argument(command, argument, expected)),
        [_, extra, ..] => Err(FsqError::invalid_meta_argument(command, extra, expected)),
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
        assert_eq!(parsed("\\l"), MetaCommand::ListCollections);
        assert_eq!(parsed("\\x"), MetaCommand::Expanded(None));
        assert_eq!(
            parsed("\\x auto"),
            MetaCommand::Expanded(Some(ExpandedMode::Auto))
        );
        assert_eq!(parsed("\\timing on"), MetaCommand::Timing(Some(Switch::On)));
        assert_eq!(parsed("\\wait OFF"), MetaCommand::Wait(Some(Switch::Off)));
        assert_eq!(
            parsed("\\c firestore-rs-enterprise"),
            MetaCommand::Connect("firestore-rs-enterprise".parse().expect("infallible"))
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
}
