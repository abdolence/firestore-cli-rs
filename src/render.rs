//! Everything fsq prints, rendered into strings so that one statement's output is written at
//! once, through the pager when it is taller than the terminal.

mod cut;
mod fit;
#[cfg(test)]
pub mod fixtures;
mod footer;
mod grid;
mod index_changes;
mod listings;
mod output;
mod plan;
mod spinner;
mod value;

pub use cut::cut_middle;
pub use footer::*;
pub use grid::*;
pub use index_changes::*;
pub use output::*;
pub use plan::*;
pub use spinner::*;

use clap::ValueEnum;
use console::Style;
use indicatif::{ProgressBar, ProgressStyle};
use std::time::Duration;

/// `bar`, drawn with `template` and redrawn every 120 ms until it is finished.
pub fn animated(bar: ProgressBar, template: &str) -> ProgressBar {
    bar.set_style(
        ProgressStyle::with_template(template)
            .expect("the template is a literal with keys indicatif knows"),
    );
    bar.enable_steady_tick(Duration::from_millis(120));
    bar
}

/// `count` and the noun that fits it: `1 row`, `2 rows`.
pub fn counted<N>(count: N, singular: &str, plural: &str) -> String
where
    N: std::fmt::Display + PartialEq + From<u8>,
{
    let noun = if count == N::from(1) {
        singular
    } else {
        plural
    };
    format!("{count} {noun}")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum OutputFormat {
    Table,
    Json,
    Jsonl,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExpandedMode {
    Off,
    On,
    Auto,
}

impl ExpandedMode {
    /// The mode a `\x` argument names, in any case.
    pub fn from_argument(argument: &str) -> Option<Self> {
        match argument.to_ascii_lowercase().as_str() {
            "on" => Some(Self::On),
            "off" => Some(Self::Off),
            "auto" => Some(Self::Auto),
            _ => None,
        }
    }
}

impl std::fmt::Display for ExpandedMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Off => "off",
            Self::On => "on",
            Self::Auto => "auto",
        })
    }
}

#[derive(Debug, Clone)]
pub struct Theme {
    pub null: Style,
    pub boolean: Style,
    pub number: Style,
    pub string: Style,
    pub timestamp: Style,
    pub reference: Style,
    pub nested: Style,
    pub key: Style,
    pub header: Style,
    pub muted: Style,
    pub warning: Style,
    pub error: Style,
    pub success: Style,
    pub plan_node: Style,
}

impl Theme {
    pub fn new(colour: bool) -> Self {
        let style = |style: Style| style.force_styling(colour);
        Self {
            null: style(Style::new().dim().italic()),
            boolean: style(Style::new().yellow()),
            number: style(Style::new().cyan()),
            string: style(Style::new().green()),
            timestamp: style(Style::new().magenta()),
            reference: style(Style::new().blue().underlined()),
            nested: style(Style::new().dim()),
            key: style(Style::new().bold()),
            header: style(Style::new().bold()),
            muted: style(Style::new().dim()),
            warning: style(Style::new().yellow().bold()),
            error: style(Style::new().red().bold()),
            success: style(Style::new().green().bold()),
            plan_node: style(Style::new().cyan().bold()),
        }
    }
}

pub struct View<'a> {
    pub theme: &'a Theme,
    pub format: OutputFormat,
    pub expanded: ExpandedMode,
    /// The terminal width `\x auto` compares a table with; `None` when stdout is not a terminal.
    pub width: Option<usize>,
    /// The documents path of the database, which names and references are shown relative to.
    pub documents_path: &'a str,
}
