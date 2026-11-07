mod args;
mod connection;
mod describe;
mod errors;
mod meta;
mod render;
mod repl;
mod script;
mod session;
mod tokens;

use args::{CliArgs, CliCommand, SqlArgs};
use clap::Parser;
use connection::Connection;
use errors::{FsqError, FsqResult, ReadScript};
use meta::Switch;
use render::{ExpandedMode, Output};
use session::{Interaction, Session, Settings};
use std::io::{IsTerminal, Read};
use std::process::ExitCode;

/// The exit code of a run that Ctrl-C stopped, as a shell reports a process that SIGINT ended.
const INTERRUPTED_EXIT_CODE: u8 = 130;

fn init_tracing() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("off"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .try_init();
}

#[tokio::main]
async fn main() -> ExitCode {
    init_tracing();
    match run(CliArgs::parse()).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(FsqError::Interrupted) => ExitCode::from(INTERRUPTED_EXIT_CODE),
        Err(error) if error.is_broken_pipe() => ExitCode::SUCCESS,
        Err(error) => {
            Output::detect().report(&error);
            ExitCode::FAILURE
        }
    }
}

async fn run(args: CliArgs) -> FsqResult<()> {
    let settings = Settings {
        format: args.format,
        expanded: ExpandedMode::Off,
        timing: Switch::Off,
        wait: if args.no_wait {
            Switch::Off
        } else {
            Switch::On
        },
    };
    match args.command.unwrap_or(CliCommand::Sql(SqlArgs::default())) {
        CliCommand::Sql(sql) => {
            let script = match (sql.execute, sql.file) {
                (Some(statements), _) => Some(statements),
                (None, Some(path)) => Some(
                    std::fs::read_to_string(&path)
                        .map_err(|source| FsqError::ReadScript(ReadScript { path, source }))?,
                ),
                (None, None) => None,
            };
            let connection = Connection::open(args.project, args.database).await?;
            match script {
                Some(script) => {
                    Session::new(connection, settings, Interaction::Script)
                        .run_script(&script)
                        .await
                }
                None if std::io::stdin().is_terminal() => {
                    repl::run(Session::new(connection, settings, Interaction::Repl)).await
                }
                None => {
                    let mut piped = String::new();
                    std::io::stdin().read_to_string(&mut piped)?;
                    repl::run_piped(
                        Session::new(connection, settings, Interaction::Script),
                        &piped,
                    )
                    .await
                }
            }
        }
        CliCommand::Describe(target) => {
            let connection = Connection::open(args.project, args.database).await?;
            Session::new(connection, settings, Interaction::Script)
                .describe(&target)
                .await
        }
    }
}
