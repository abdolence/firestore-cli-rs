//! Everything fsq prints, rendered into strings so that one statement's output is written at
//! once, through the pager when it is taller than the terminal.

mod cut;
mod detail;
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
pub use fit::TableText;
pub use footer::*;
pub use grid::*;
pub use index_changes::*;
pub use output::*;
pub use plan::*;
pub use spinner::*;
pub use value::{sorted_keys, type_name};

use clap::ValueEnum;
use console::Style;
use indicatif::{ProgressBar, ProgressStyle};
use std::time::Duration;

/// How often a spinner or a progress bar is redrawn while it runs.
pub const ANIMATION_TICK: Duration = Duration::from_millis(120);

/// `bar`, drawn with `template` and redrawn every [`ANIMATION_TICK`] until it is finished.
pub fn animated(bar: ProgressBar, template: &str) -> ProgressBar {
    bar.set_style(
        ProgressStyle::with_template(template)
            .expect("the template is a literal with keys indicatif knows"),
    );
    bar.enable_steady_tick(ANIMATION_TICK);
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
    /// Every field of every document, one row per leaf value, with its type and its whole value.
    Full,
}

impl ExpandedMode {
    /// The mode a `\x` argument names, in any case.
    pub fn from_argument(argument: &str) -> Option<Self> {
        match argument.to_ascii_lowercase().as_str() {
            "on" => Some(Self::On),
            "off" => Some(Self::Off),
            "auto" => Some(Self::Auto),
            "full" => Some(Self::Full),
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
            Self::Full => "full",
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
    pub const fn new(colour: bool) -> Self {
        Self {
            null: Style::new().dim().italic().force_styling(colour),
            boolean: Style::new().yellow().force_styling(colour),
            number: Style::new().cyan().force_styling(colour),
            string: Style::new().green().force_styling(colour),
            timestamp: Style::new().magenta().force_styling(colour),
            reference: Style::new().blue().underlined().force_styling(colour),
            nested: Style::new().dim().force_styling(colour),
            key: Style::new().bold().force_styling(colour),
            header: Style::new().bold().force_styling(colour),
            muted: Style::new().dim().force_styling(colour),
            warning: Style::new().yellow().bold().force_styling(colour),
            error: Style::new().red().bold().force_styling(colour),
            success: Style::new().green().bold().force_styling(colour),
            plan_node: Style::new().cyan().bold().force_styling(colour),
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
