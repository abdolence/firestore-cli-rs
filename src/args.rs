use crate::codegen::CodegenArgs;
use crate::connection::{DatabaseId, ProjectId};
use crate::describe::DescribeTarget;
use crate::export::ExportArgs;
use crate::import::ImportArgs;
use crate::index_command::IndexCommand;
use crate::render::OutputFormat;
use clap::{Args, Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(
    name = "fsq",
    version,
    about = "A command line client for Google Firestore: a SQL REPL, scripts and JSON output"
)]
pub struct CliArgs {
    #[arg(
        short = 'p',
        long,
        global = true,
        help = "Google Cloud project. Default: the first of GCP_PROJECT, PROJECT_ID, GCP_PROJECT_ID and GOOGLE_CLOUD_PROJECT that is set, then the project of the credentials file, then of the metadata server"
    )]
    pub project: Option<ProjectId>,

    #[arg(
        short = 'd',
        long,
        global = true,
        default_value_t = DatabaseId::default(),
        help = "Database ID"
    )]
    pub database: DatabaseId,

    #[arg(long, global = true, value_enum, default_value_t = OutputFormat::Table)]
    pub format: OutputFormat,

    #[arg(
        long,
        global = true,
        help = "Return from CREATE INDEX and ALTER COLLECTION once the build starts, without waiting for it"
    )]
    pub no_wait: bool,

    #[command(subcommand)]
    pub command: Option<CliCommand>,
}

#[derive(Subcommand, Debug)]
pub enum CliCommand {
    #[command(
        about = "Run SQL: the REPL, or a statement with -e, a script with -f, or a script from stdin"
    )]
    Sql(SqlArgs),
    #[command(
        about = "List the field paths of a collection, with their types, % present and an example value"
    )]
    Describe(DescribeTarget),
    #[command(
        about = "Write the documents of a collection, or the rows of a SELECT, as JSON lines"
    )]
    Export(ExportArgs),
    #[command(about = "Write the documents of a JSON lines file, as `export` writes them")]
    Import(ImportArgs),
    #[command(
        about = "Show the fluent Rust code of the firestore crate for a SELECT, with a row struct typed from sample rows"
    )]
    Codegen(CodegenArgs),
    #[command(
        subcommand,
        about = "Plan, sync, list, export, create and delete indexes, from a firestore.indexes.json"
    )]
    Index(IndexCommand),
}

#[cfg(test)]
impl CliArgs {
    /// Parses the arguments that follow `fsq` and returns the subcommand they select.
    pub fn parse_command(arguments: &[&str]) -> Result<Option<CliCommand>, clap::Error> {
        let arguments = ["fsq"].into_iter().chain(arguments.iter().copied());
        Ok(Self::try_parse_from(arguments)?.command)
    }
}

#[derive(Args, Debug, Default)]
pub struct SqlArgs {
    #[arg(
        short = 'e',
        long = "execute",
        conflicts_with = "file",
        help = "Run these statements, separated by `;`, and exit"
    )]
    pub execute: Option<String>,

    #[arg(
        short = 'f',
        long,
        help = "Run the statements of this file, one at a time, and exit"
    )]
    pub file: Option<PathBuf>,
}
