use crate::connection::{DatabaseId, ProjectId};
use crate::describe::DescribeTarget;
use crate::export::ExportArgs;
use crate::import::ImportArgs;
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
        help = "Google Cloud project. Default: GOOGLE_CLOUD_PROJECT, then the project the credentials name"
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
