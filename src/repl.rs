mod complete;
mod highlight;

use crate::errors::{FsqError, FsqResult};
use crate::script::Script;
use crate::session::{Flow, Session};
use complete::SqlCompleter;
use highlight::SqlHighlighter;
use reedline::{
    default_emacs_keybindings, ColumnarMenu, Emacs, FileBackedHistory, KeyCode, KeyModifiers,
    MenuBuilder, Prompt, PromptEditMode, PromptHistorySearch, PromptHistorySearchStatus, Reedline,
    ReedlineEvent, ReedlineMenu, Signal, ValidationResult, Validator,
};
use std::borrow::Cow;

const HISTORY_ENTRIES: usize = 1000;
const COMPLETION_MENU: &str = "completion_menu";

const SQL_KEYWORDS: &[&str] = &[
    "ALL",
    "ALTER",
    "ANALYZE",
    "AND",
    "ARRAY",
    "AS",
    "ASC",
    "BETWEEN",
    "BY",
    "CASE",
    "COLLECTION",
    "COLLECTIONS",
    "COLUMN",
    "CONFLICT",
    "CONTAINS",
    "CREATE",
    "CROSS",
    "DELETE",
    "DESC",
    "DISTINCT",
    "DO",
    "DROP",
    "ELSE",
    "END",
    "EXCLUDED",
    "EXISTS",
    "EXPLAIN",
    "FALSE",
    "FROM",
    "GEO",
    "GROUP",
    "HAVING",
    "IF",
    "IN",
    "INDEX",
    "INDEXES",
    "INNER",
    "INSERT",
    "INTO",
    "IS",
    "JOIN",
    "LEFT",
    "LIKE",
    "LIMIT",
    "NOT",
    "NOTHING",
    "NULL",
    "OFFSET",
    "ON",
    "OPTIONS",
    "OR",
    "ORDER",
    "REPLACE",
    "SEARCH",
    "SELECT",
    "SET",
    "SHOW",
    "THEN",
    "TRUE",
    "UNION",
    "UNIQUE",
    "UNNEST",
    "UPDATE",
    "VALUES",
    "VECTOR",
    "WHEN",
    "WHERE",
    "WITH",
];

pub async fn run(mut session: Session) -> FsqResult<()> {
    session.greet()?;
    session.hint("Type \\? for help and \\q to quit; a statement ends with `;`.")?;
    session.load_collections().await;
    let mut editor = Reedline::create()
        .with_validator(Box::new(StatementValidator))
        .with_highlighter(Box::new(SqlHighlighter))
        .with_completer(Box::new(SqlCompleter::new(session.vocabulary.clone())))
        .with_menu(ReedlineMenu::EngineCompleter(Box::new(
            ColumnarMenu::default().with_name(COMPLETION_MENU),
        )))
        .with_edit_mode(Box::new(Emacs::new(keybindings())));
    if let Some(history) = history() {
        editor = editor.with_history(Box::new(history));
    }
    loop {
        let prompt = SessionPrompt(session.connection.label());
        let signal = tokio::task::block_in_place(|| editor.read_line(&prompt))?;
        match signal {
            Signal::Success(buffer) => {
                if run_buffer(&mut session, &buffer).await.flow == Flow::Quit {
                    return Ok(());
                }
            }
            Signal::CtrlD => return Ok(()),
            _ => {}
        }
    }
}

/// Runs piped input as the REPL runs typed input: a failed statement is reported and the rest
/// still runs.
pub async fn run_piped(mut session: Session, buffer: &str) -> FsqResult<()> {
    session.greet()?;
    match run_buffer(&mut session, buffer).await.failed {
        0 => Ok(()),
        failed => Err(FsqError::InputsFailed { failed }),
    }
}

struct BufferRun {
    flow: Flow,
    failed: usize,
}

async fn run_buffer(session: &mut Session, buffer: &str) -> BufferRun {
    let mut failed = 0;
    for input in Script::parse(buffer).into_inputs() {
        match session.run_input(&input).await {
            Ok(Flow::Continue) => {}
            Ok(Flow::Quit) => {
                return BufferRun {
                    flow: Flow::Quit,
                    failed,
                }
            }
            Err(error) => {
                session.report(&error);
                failed += 1;
            }
        }
    }
    BufferRun {
        flow: Flow::Continue,
        failed,
    }
}

fn keybindings() -> reedline::Keybindings {
    let mut keybindings = default_emacs_keybindings();
    keybindings.add_binding(
        KeyModifiers::NONE,
        KeyCode::Tab,
        ReedlineEvent::UntilFound(vec![
            ReedlineEvent::Menu(COMPLETION_MENU.to_string()),
            ReedlineEvent::MenuNext,
        ]),
    );
    keybindings
}

fn history() -> Option<FileBackedHistory> {
    let path = dirs::data_dir()?.join("fsq").join("history.txt");
    let opened = path
        .parent()
        .map_or(Ok(()), std::fs::create_dir_all)
        .map_err(reedline::ReedlineError::from)
        .and_then(|_| FileBackedHistory::with_file(HISTORY_ENTRIES, path.clone()));
    match opened {
        Ok(history) => Some(history),
        Err(error) => {
            tracing::warn!(path = %path.display(), %error, "no history file");
            None
        }
    }
}

struct StatementValidator;

impl Validator for StatementValidator {
    fn validate(&self, line: &str) -> ValidationResult {
        match Script::parse(line).unterminated {
            Some(_) => ValidationResult::Incomplete,
            None => ValidationResult::Complete,
        }
    }
}

struct SessionPrompt(String);

impl Prompt for SessionPrompt {
    fn render_prompt_left(&self) -> Cow<'_, str> {
        Cow::Borrowed(&self.0)
    }

    fn render_prompt_right(&self) -> Cow<'_, str> {
        Cow::Borrowed("")
    }

    fn render_prompt_indicator(&self, _mode: PromptEditMode) -> Cow<'_, str> {
        Cow::Borrowed("> ")
    }

    fn render_prompt_multiline_indicator(&self) -> Cow<'_, str> {
        Cow::Borrowed("-> ")
    }

    fn render_prompt_history_search_indicator(&self, search: PromptHistorySearch) -> Cow<'_, str> {
        let status = match search.status {
            PromptHistorySearchStatus::Passing => "",
            PromptHistorySearchStatus::Failing => "failing ",
        };
        Cow::Owned(format!("({status}search: {}) ", search.term))
    }
}
