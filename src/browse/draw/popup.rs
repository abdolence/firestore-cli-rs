use super::{PopupScroll, Viewport};
use crate::browse::footer::{Hint, Hints};
use crate::browse::state::{Browser, Popup};
use crate::field_path::FieldPath;
use crate::render::{counted, cut_middle, type_name, ExpandedMode, OutputFormat, Theme, View};
use firestore::gcloud_sdk::google::firestore::v1::value::ValueType;
use firestore::gcloud_sdk::google::firestore::v1::Value;
use firestore::FirestoreVector;
use ratatui::layout::{Constraint, Rect};
use ratatui::style::{Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph};
use ratatui::Frame;
use std::borrow::Cow;

/// The columns a popup leaves free of the panes on either side of it.
const POPUP_MARGIN: usize = 2;

/// The columns of the offset that starts each line of bytes in hexadecimal.
const HEX_OFFSET_WIDTH: usize = 8;

/// The most bytes a line of hexadecimal holds, when the popup has room for them.
const HEX_BYTES_PER_LINE: usize = 16;

/// The columns a tab in a value takes in a popup.
const POPUP_TAB: &str = "    ";

impl Browser {
    /// Draws `popup` over `body`, the area of the panes: a box as wide as its text and its
    /// title, inside the panes, with its text wrapped to the box and scrolled to where
    /// `viewport` keeps it.
    pub(super) fn draw_popup(
        &self,
        frame: &mut Frame,
        popup: &Popup,
        body: Rect,
        viewport: &mut Viewport,
    ) {
        let theme = Theme::new(false);
        let view = View {
            theme: &theme,
            format: OutputFormat::Table,
            expanded: ExpandedMode::Off,
            width: None,
            documents_path: self.documents_path(),
        };
        let (title, text) = match popup {
            Popup::Value { path, value } => {
                let details = [Some(type_name(value).to_string()), size_of(value)];
                let title = PopupTitle {
                    name: FieldPath::from(path).to_string(),
                    details: details.into_iter().flatten().collect(),
                };
                (title, PopupText::of(value, &view))
            }
            Popup::Error(failure) => {
                let error = self.error(*failure).map(ToString::to_string);
                (
                    PopupTitle::named("error"),
                    PopupText::Text(error.unwrap_or_default()),
                )
            }
            Popup::Help => (PopupTitle::named("keys"), PopupText::Help),
        };

        let most = usize::from(body.width).saturating_sub(2 * POPUP_MARGIN + 2);
        let inner_width = text.natural_width().max(title.width()).min(most);
        let lines = text.lines(inner_width);
        let rows = lines.len().min(usize::from(body.height).saturating_sub(2));
        let area = body.centered(
            Constraint::Length(u16::try_from(inner_width + 2).unwrap_or(u16::MAX)),
            Constraint::Length(u16::try_from(rows + 2).unwrap_or(u16::MAX)),
        );
        let last_top = lines.len() - rows;
        let scroll = viewport.popup.get_or_insert_with(PopupScroll::default);
        scroll.top = scroll.top.min(last_top);
        scroll.last_top = last_top;
        let top = scroll.top;
        viewport.page_rows = rows;

        let mut hints = vec![Hint::new("Esc", "close", 3)];
        if let Popup::Value { .. } = popup {
            hints.insert(0, Hint::new("y", "copy", 2));
        }
        if last_top > 0 {
            hints.insert(0, Hint::new("↑↓", "scroll", 1));
        }
        let mut hints = Hints(hints).fitted(inner_width.saturating_sub(2)).line();
        hints.spans.insert(0, Span::from(" "));
        hints.spans.push(Span::from(" "));
        let block = Block::bordered()
            .title(title.line(inner_width))
            .title_bottom(hints)
            .border_style(Style::new().cyan());
        let shown: Vec<Line> = lines.into_iter().skip(top).take(rows).collect();
        frame.render_widget(Clear, area);
        frame.render_widget(Paragraph::new(shown).block(block), area);
    }
}

/// The title of a popup: its name, cut in the middle when the title is wider than the popup, and
/// the details after it, a `·` before each, with a space at either end.
struct PopupTitle {
    name: String,
    details: Vec<String>,
}

impl PopupTitle {
    fn named(name: &str) -> Self {
        Self {
            name: name.to_string(),
            details: Vec::new(),
        }
    }

    fn tail(&self) -> String {
        self.details
            .iter()
            .map(|detail| format!(" · {detail}"))
            .collect::<String>()
            + " "
    }

    fn width(&self) -> usize {
        1 + Span::from(self.name.as_str()).width() + Span::from(self.tail()).width()
    }

    /// This title in `width` columns.
    fn line(&self, width: usize) -> Line<'static> {
        let tail = self.tail();
        let room = width.saturating_sub(1 + Span::from(tail.as_str()).width());
        Line::from(format!(" {}{tail}", cut_middle(&self.name, room)))
    }
}

/// How much `value` holds, for the types whose value does not say it at a glance.
fn size_of(value: &Value) -> Option<String> {
    match &value.value_type {
        Some(ValueType::StringValue(text)) => Some(counted(text.chars().count(), "char", "chars")),
        Some(ValueType::BytesValue(bytes)) => Some(counted(bytes.len(), "byte", "bytes")),
        Some(ValueType::ArrayValue(array)) => Some(counted(array.values.len(), "item", "items")),
        Some(ValueType::MapValue(map)) => Some(match FirestoreVector::from_value(value) {
            Ok(vector) => counted(vector.as_vec().len(), "dimension", "dimensions"),
            Err(_) => counted(map.fields.len(), "field", "fields"),
        }),
        _ => None,
    }
}

/// The text of a popup, which it lays out to the width of the popup.
enum PopupText<'a> {
    /// Text wrapped at words, each of its lines on a line of its own.
    Text(String),
    /// Bytes in hexadecimal, after the offset of the first byte of each line.
    Hex(&'a [u8]),
    /// Every key of the browser, by where it works.
    Help,
}

impl<'a> PopupText<'a> {
    /// A value whole: bytes in hexadecimal, and any other value as [`View::pretty_value`] writes
    /// it, a map or an array as indented JSON-like text.
    fn of(value: &'a Value, view: &View<'_>) -> Self {
        match &value.value_type {
            Some(ValueType::BytesValue(bytes)) => Self::Hex(bytes),
            _ => Self::Text(view.pretty_value(value)),
        }
    }

    /// The columns the widest line takes, unwrapped.
    fn natural_width(&self) -> usize {
        match self {
            Self::Text(text) => text_lines(text)
                .map(|line| Span::from(line.as_ref()).width())
                .max()
                .unwrap_or_default(),
            Self::Hex(bytes) => hex_line_width(bytes.len().min(HEX_BYTES_PER_LINE)),
            Self::Help => {
                let action_column = help_action_column();
                help_keys()
                    .map(|key| action_column + Span::from(key.action).width())
                    .max()
                    .unwrap_or_default()
                    .min(HELP_MAX_WIDTH)
            }
        }
    }

    /// The lines of the text in `width` columns.
    fn lines(&self, width: usize) -> Vec<Line<'static>> {
        let width = width.max(1);
        match self {
            Self::Text(text) => text_lines(text)
                .flat_map(|line| {
                    textwrap::wrap(&line, width)
                        .into_iter()
                        .map(|wrapped| Line::from(wrapped.into_owned()))
                        .collect::<Vec<_>>()
                })
                .collect(),
            Self::Hex(bytes) => {
                let per_line = [HEX_BYTES_PER_LINE, 8, 4]
                    .into_iter()
                    .find(|&count| hex_line_width(count) <= width)
                    .unwrap_or(4);
                bytes
                    .chunks(per_line)
                    .enumerate()
                    .map(|(line, chunk)| {
                        let hex: Vec<String> =
                            chunk.iter().map(|byte| format!("{byte:02x}")).collect();
                        Line::from(format!(
                            "{:0width$x}  {}",
                            line * per_line,
                            hex.join(" "),
                            width = HEX_OFFSET_WIDTH
                        ))
                    })
                    .collect()
            }
            Self::Help => {
                let action_column = help_action_column();
                let indent = " ".repeat(action_column);
                let options = textwrap::Options::new(width).subsequent_indent(&indent);
                let mut lines = Vec::new();
                for (position, section) in HELP_SECTIONS.iter().enumerate() {
                    if position > 0 {
                        lines.push(Line::default());
                    }
                    lines.push(Line::from(section.title).bold());
                    for key in section.keys {
                        let padding = action_column - HELP_INDENT - Span::from(key.keys).width();
                        let line = format!(
                            "{}{}{}{}",
                            " ".repeat(HELP_INDENT),
                            key.keys,
                            " ".repeat(padding),
                            key.action
                        );
                        lines.extend(
                            textwrap::wrap(&line, &options)
                                .into_iter()
                                .map(|wrapped| Line::from(wrapped.into_owned())),
                        );
                    }
                }
                lines
            }
        }
    }
}

/// The lines of `text` as a popup shows them: a tab as spaces and any other control character
/// as one space, since the terminal would act on it.
fn text_lines(text: &str) -> impl Iterator<Item = Cow<'_, str>> {
    text.lines().map(|line| {
        if line.contains(char::is_control) {
            Cow::Owned(line.replace('\t', POPUP_TAB).replace(char::is_control, " "))
        } else {
            Cow::Borrowed(line)
        }
    })
}

/// The columns of a line of `count` bytes in hexadecimal, after its offset.
fn hex_line_width(count: usize) -> usize {
    HEX_OFFSET_WIDTH + 2 + (3 * count).saturating_sub(1)
}

/// The keys of one place of the browser, and what each does there.
struct HelpSection {
    title: &'static str,
    keys: &'static [HelpKey],
}

struct HelpKey {
    keys: &'static str,
    action: &'static str,
}

const HELP_INDENT: usize = 2;

/// The widest the help gets, so that its longer lines wrap rather than run across a wide
/// terminal.
const HELP_MAX_WIDTH: usize = 80;

const HELP_SECTIONS: &[HelpSection] = &[
    HelpSection {
        title: "Documents and tree",
        keys: &[
            HelpKey {
                keys: "↑ ↓  k j",
                action: "move the cursor",
            },
            HelpKey {
                keys: "PgUp PgDn",
                action: "move a page",
            },
            HelpKey {
                keys: "Ctrl-U Ctrl-D",
                action: "move half a page",
            },
            HelpKey {
                keys: "g G  Home End",
                action: "go to the first or the last row",
            },
            HelpKey {
                keys: "→ l",
                action: "open the map, array or subcollection; from the documents, go to the tree",
            },
            HelpKey {
                keys: "Enter",
                action: "show the value whole, or open what → opens",
            },
            HelpKey {
                keys: "Space",
                action: "open or close the map, array or subcollections",
            },
            HelpKey {
                keys: "← h",
                action: "close the map or array, or go to the one it is in; from the top of the tree, go to the documents; from the documents, go up a level",
            },
            HelpKey {
                keys: "Tab",
                action: "switch between the documents and the tree",
            },
            HelpKey {
                keys: "/",
                action: "edit the filter",
            },
            HelpKey {
                keys: "f",
                action: "open the editor with a filter on the value under the cursor, WHERE <path> = <value>",
            },
            HelpKey {
                keys: "y Y",
                action: "copy the value under the cursor, or its path, to the clipboard",
            },
            HelpKey {
                keys: "r",
                action: "read the documents again with their filter",
            },
            HelpKey {
                keys: "Esc Backspace",
                action: "go back up from a subcollection",
            },
            HelpKey {
                keys: "Ctrl-G",
                action: "show the whole error of a failed filter or listing",
            },
            HelpKey {
                keys: "?",
                action: "show these keys",
            },
            HelpKey {
                keys: "Ctrl-C",
                action: "stop waiting for Firestore, else close the popup or the editor, else quit",
            },
            HelpKey {
                keys: "q",
                action: "quit",
            },
        ],
    },
    HelpSection {
        title: "Filter editor",
        keys: &[
            HelpKey {
                keys: "← →  Home End",
                action: "move the cursor",
            },
            HelpKey {
                keys: "↑ ↓",
                action: "recall an earlier filter that ran, or a later one",
            },
            HelpKey {
                keys: "Backspace Delete",
                action: "delete the character before or after the cursor",
            },
            HelpKey {
                keys: "Ctrl-W Ctrl-U",
                action: "delete the word before the cursor, or everything before it",
            },
            HelpKey {
                keys: "Enter",
                action: "run the filter",
            },
            HelpKey {
                keys: "Esc",
                action: "close the editor and keep the filter before",
            },
            HelpKey {
                keys: "Ctrl-G",
                action: "show the whole error of the filter",
            },
        ],
    },
    HelpSection {
        title: "Popup",
        keys: &[
            HelpKey {
                keys: "↑ ↓  k j",
                action: "scroll a line",
            },
            HelpKey {
                keys: "PgUp PgDn",
                action: "scroll a page",
            },
            HelpKey {
                keys: "Ctrl-U Ctrl-D",
                action: "scroll half a page",
            },
            HelpKey {
                keys: "g G  Home End",
                action: "go to the start or the end",
            },
            HelpKey {
                keys: "y Y",
                action: "copy the value, or its path, to the clipboard",
            },
            HelpKey {
                keys: "Esc",
                action: "close the popup",
            },
        ],
    },
];

fn help_keys() -> impl Iterator<Item = &'static HelpKey> {
    HELP_SECTIONS.iter().flat_map(|section| section.keys)
}

/// The column the actions of the help start at, two after the longest keys.
fn help_action_column() -> usize {
    let keys = help_keys()
        .map(|key| Span::from(key.keys).width())
        .max()
        .unwrap_or_default();
    HELP_INDENT + keys + 2
}
