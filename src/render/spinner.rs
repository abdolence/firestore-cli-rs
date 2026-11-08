use super::animated;
use indicatif::{ProgressBar, ProgressDrawTarget};
use std::time::Duration;
use tokio::task::JoinHandle;

/// How long an operation runs before its spinner appears, so that fast calls do not flicker.
pub const SPINNER_DELAY: Duration = Duration::from_millis(250);

/// What decides whether a spinner may be drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpinnerSurroundings {
    pub stderr_is_terminal: bool,
    /// JSON or JSON lines are written to a pipe, where a reader expects nothing else to show.
    pub json_to_pipe: bool,
}

impl SpinnerSurroundings {
    /// The delay before the spinner appears, or `None` when it must not appear at all.
    pub fn reveal_after(&self) -> Option<Duration> {
        (self.stderr_is_terminal && !self.json_to_pipe).then_some(SPINNER_DELAY)
    }
}

/// A spinner on stderr for the length of a wait. It is cleared when dropped, so that nothing is
/// left on the line the next output starts on, whether the wait ended, failed or was interrupted.
/// Must be started inside a Tokio runtime.
pub struct Spinner {
    bar: ProgressBar,
    reveal: Option<JoinHandle<()>>,
}

impl Spinner {
    pub fn start(message: &str, surroundings: SpinnerSurroundings) -> Self {
        let bar = animated(
            ProgressBar::hidden().with_message(message.to_string()),
            "{spinner} {msg} {elapsed}",
        );
        let reveal = surroundings.reveal_after().map(|delay| {
            let bar = bar.clone();
            tokio::spawn(async move {
                tokio::time::sleep(delay).await;
                bar.set_draw_target(ProgressDrawTarget::stderr());
            })
        });
        Self { bar, reveal }
    }
}

impl Drop for Spinner {
    fn drop(&mut self) {
        if let Some(reveal) = self.reveal.take() {
            reveal.abort();
        }
        self.bar.finish_and_clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_spinner_appears_after_the_delay_and_only_on_a_terminal_that_is_not_a_pipe_of_json() {
        let terminal = SpinnerSurroundings {
            stderr_is_terminal: true,
            json_to_pipe: false,
        };

        assert_eq!(terminal.reveal_after(), Some(SPINNER_DELAY));
        assert_eq!(
            SpinnerSurroundings {
                stderr_is_terminal: false,
                ..terminal
            }
            .reveal_after(),
            None
        );
        assert_eq!(
            SpinnerSurroundings {
                json_to_pipe: true,
                ..terminal
            }
            .reveal_after(),
            None
        );
    }
}
