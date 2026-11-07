use super::Theme;
use crate::errors::{FsqError, FsqResult};
use console::{Key, Term};
use std::io::Write;
use std::process::{Command, Stdio};

const DEFAULT_PAGER: &str = "less -RFX";

/// Where results go: stdout, through a pager when stdout is a terminal and a result does not fit
/// on it, and notices to stderr.
pub struct Output {
    stdout: Term,
    stderr: Term,
    is_terminal: bool,
}

impl Output {
    pub fn detect() -> Self {
        let stdout = Term::stdout();
        Self {
            is_terminal: stdout.is_term(),
            stdout,
            stderr: Term::stderr(),
        }
    }

    pub fn theme(&self) -> Theme {
        Theme::new(self.is_terminal && console::colors_enabled())
    }

    /// The terminal's width, `None` when stdout is not a terminal.
    pub fn width(&self) -> Option<usize> {
        self.is_terminal.then(|| usize::from(self.stdout.size().1))
    }

    /// The terminal's height, `None` when stdout is not a terminal.
    pub fn height(&self) -> Option<usize> {
        self.is_terminal.then(|| usize::from(self.stdout.size().0))
    }

    pub fn print(&self, text: &str) -> FsqResult<()> {
        if text.is_empty() {
            return Ok(());
        }
        if self.is_terminal && !self.fits(text) {
            if let Some(command) = pager_command() {
                match run_pager(&command, text) {
                    // No such pager installed, as `less` on Windows: print as without one.
                    Err(FsqError::Pager { source, .. })
                        if source.kind() == std::io::ErrorKind::NotFound => {}
                    shown => return shown,
                }
            }
        }
        let mut stdout = std::io::stdout().lock();
        stdout.write_all(text.as_bytes())?;
        stdout.flush()?;
        Ok(())
    }

    /// A message beside the results, such as a notice or an error, on stderr so that piped
    /// results stay clean.
    pub fn notice(&self, text: &str) -> FsqResult<()> {
        self.stderr.write_line(text)?;
        Ok(())
    }

    /// Asks whether to show the next page of rows: Enter or space for yes, anything else for no.
    pub fn ask_for_more(&self, theme: &Theme) -> FsqResult<bool> {
        self.stdout.write_str(
            &theme
                .muted
                .apply_to("-- more: Enter for the next page, q to stop --")
                .to_string(),
        )?;
        let key = self.stdout.read_key()?;
        self.stdout.clear_line()?;
        Ok(matches!(key, Key::Enter | Key::Char(' ')))
    }

    fn fits(&self, text: &str) -> bool {
        let (rows, columns) = self.stdout.size();
        let mut lines = 0;
        for line in text.lines() {
            if console::measure_text_width(line) > usize::from(columns) {
                return false;
            }
            lines += 1;
        }
        lines < usize::from(rows)
    }
}

/// `$PAGER`, or `less -RFX` when it is unset. An empty `$PAGER` turns the pager off.
fn pager_command() -> Option<String> {
    match std::env::var("PAGER") {
        Ok(pager) if pager.trim().is_empty() => None,
        Ok(pager) => Some(pager),
        Err(_) => Some(DEFAULT_PAGER.to_string()),
    }
}

fn run_pager(command: &str, text: &str) -> FsqResult<()> {
    let pager_error = |source| FsqError::Pager {
        command: command.to_string(),
        source,
    };
    let mut words = command.split_whitespace();
    let program = words.next().unwrap_or(DEFAULT_PAGER);
    let mut child = Command::new(program)
        .args(words)
        .stdin(Stdio::piped())
        .spawn()
        .map_err(pager_error)?;
    if let Some(mut stdin) = child.stdin.take() {
        match stdin.write_all(text.as_bytes()) {
            // The pager quit before reading everything, as `q` in less does.
            Err(error) if error.kind() == std::io::ErrorKind::BrokenPipe => {}
            other => other.map_err(pager_error)?,
        }
    }
    child.wait().map_err(pager_error)?;
    Ok(())
}
