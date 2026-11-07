use super::Theme;
use crate::errors::{FsqError, FsqResult, PagerFailed};
use console::{Key, Term};
use std::io::Write;
use std::process::{Command, Stdio};

const DEFAULT_PAGER: &str = "less -RFX";

/// Where results go: stdout, through a pager when stdout is a terminal and a result is taller than
/// it, and notices to stderr.
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

    pub fn width(&self) -> Option<usize> {
        self.is_terminal.then(|| usize::from(self.stdout.size().1))
    }

    pub fn height(&self) -> Option<usize> {
        self.is_terminal.then(|| usize::from(self.stdout.size().0))
    }

    pub fn print(&self, text: &str) -> FsqResult<()> {
        if text.is_empty() {
            return Ok(());
        }
        if self.is_terminal && !self.fits_height(text) {
            if let Some(pager) = Pager::from_env() {
                match pager.show(text) {
                    // No such pager installed, as `less` on Windows: print as without one.
                    Err(FsqError::PagerFailed(PagerFailed { source, .. }))
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

    /// An error on stderr as `error: ..`, and Ctrl-C as `Interrupted.`
    pub fn report(&self, error: &FsqError) {
        let theme = self.theme();
        let text = match error {
            FsqError::Interrupted => theme.muted.apply_to("Interrupted.").to_string(),
            error => format!("{} {error}", theme.error.apply_to("error:")),
        };
        if let Err(write_error) = self.notice(&text) {
            tracing::warn!(%write_error, %error, "cannot write to stderr");
        }
    }

    /// Asks whether to show the next page of rows: Enter or space for yes, anything else for no.
    pub fn ask_for_more(&self, theme: &Theme) -> FsqResult<bool> {
        self.stdout.write_str(
            &theme
                .muted
                .apply_to("-- more: Enter or space for the next page, any other key to stop --")
                .to_string(),
        )?;
        let key = self.stdout.read_key()?;
        self.stdout.clear_line()?;
        Ok(matches!(key, Key::Enter | Key::Char(' ')))
    }

    fn fits_height(&self, text: &str) -> bool {
        text.lines().count() < usize::from(self.stdout.size().0)
    }
}

/// The command that results taller than the terminal are piped through.
struct Pager {
    command: String,
}

impl Pager {
    /// `$PAGER`, or `less -RFX` when it is unset. An empty `$PAGER` turns the pager off.
    fn from_env() -> Option<Self> {
        let command = match std::env::var("PAGER") {
            Ok(pager) if pager.trim().is_empty() => return None,
            Ok(pager) => pager,
            Err(_) => DEFAULT_PAGER.to_string(),
        };
        Some(Self { command })
    }

    fn show(&self, text: &str) -> FsqResult<()> {
        let pager_error = |source| {
            FsqError::PagerFailed(PagerFailed {
                command: self.command.clone(),
                source,
            })
        };
        let mut words = self.command.split_whitespace();
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
}
