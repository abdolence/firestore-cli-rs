use super::{counted, ExecutionStats, OutputFormat, View};
use std::time::Duration;

#[derive(Debug, Clone, PartialEq)]
pub enum FooterNote {
    /// The rows are the first pages of a statement without a `LIMIT`, and more remain.
    MorePages,
    Sampled(Sample),
    /// From sending the statement to the first response.
    RoundTrip(Duration),
    Stats(ExecutionStats),
}

/// The documents a listing was computed from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sample {
    pub documents: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Footer {
    /// What the footer counts, with its noun: `48 rows`.
    pub counted: String,
    pub elapsed: Duration,
    pub notes: Vec<FooterNote>,
}

impl View<'_> {
    /// The footer in a table; JSON output carries none, so that it stays valid JSON.
    pub fn footer(&self, footer: &Footer) -> String {
        if self.format != OutputFormat::Table {
            return String::new();
        }
        let mut parts = vec![
            footer.counted.clone(),
            DurationText(footer.elapsed).to_string(),
        ];
        for note in &footer.notes {
            parts.push(match note {
                FooterNote::MorePages => "more rows remain".to_string(),
                FooterNote::Sampled(sample) => format!(
                    "from a sample of {}",
                    counted(sample.documents, "document", "documents")
                ),
                FooterNote::RoundTrip(round_trip) => {
                    format!("round trip {}", DurationText(*round_trip))
                }
                FooterNote::Stats(stats) => stats.to_string(),
            });
        }
        format!("{}\n", self.theme.muted.apply_to(parts.join(" · ")))
    }
}

pub struct DurationText(pub Duration);

impl std::fmt::Display for DurationText {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.0 < Duration::from_secs(1) {
            write!(f, "{} ms", self.0.as_millis())
        } else {
            write!(f, "{:.2} s", self.0.as_secs_f64())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::{ExpandedMode, Theme};

    fn footer_text(footer: Footer) -> String {
        View {
            theme: &Theme::new(false),
            format: OutputFormat::Table,
            expanded: ExpandedMode::Off,
            width: None,
            documents_path: "",
        }
        .footer(&footer)
    }

    #[test]
    fn a_footer_counts_what_was_listed_with_its_time_and_notes() {
        assert_eq!(
            footer_text(Footer {
                counted: counted(48, "row", "rows"),
                elapsed: Duration::from_millis(124),
                notes: vec![
                    FooterNote::MorePages,
                    FooterNote::RoundTrip(Duration::from_millis(61)),
                    FooterNote::Stats(ExecutionStats {
                        documents_read: Some(11),
                        read_units: Some(1),
                    }),
                ],
            }),
            "48 rows · 124 ms · more rows remain · round trip 61 ms · 11 documents read · 1 read unit\n"
        );
        assert_eq!(
            footer_text(Footer {
                counted: counted(1, "field path", "field paths"),
                elapsed: Duration::from_millis(2_340),
                notes: vec![FooterNote::Sampled(Sample { documents: 1 })],
            }),
            "1 field path · 2.34 s · from a sample of 1 document\n"
        );
    }
}
