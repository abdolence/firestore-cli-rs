//! The full-screen browser of `\browse` and `fsq browse`: the documents of a collection on the
//! left, the values of the selected one as a tree on the right, and the subcollections of each
//! document one level further down.

mod draw;
mod state;
mod tree;

use crate::connection::Connection;
use crate::describe::CollectionPath;
use crate::errors::{is_pipeline_refusal, FsqError, FsqResult};
use crate::walk::Walk;
use draw::Viewport;
use firestore::{FirestoreDb, FirestoreDocument};
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::DefaultTerminal;
pub use state::FilterTail;
use state::{Browser, DocumentsQuery, Effect, Message, Movement, BROWSE_DOCUMENT_LIMIT};
use std::io::IsTerminal;
use std::time::Duration;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio::task::AbortHandle;

#[derive(Debug, Clone, PartialEq, Eq, clap::Args)]
pub struct BrowseTarget {
    #[arg(help = "Collection path, such as `books` or `users/alice/orders`")]
    pub collection: CollectionPath,

    #[arg(
        long,
        default_value_t,
        hide_default_value = true,
        help = "The end of a SELECT after its FROM, such as \"WHERE genre = 'sf' ORDER BY title LIMIT 20\"; without a LIMIT, the first 100 documents"
    )]
    pub filter: FilterTail,
}

/// How long the browser waits for a key before it looks for answers from Firestore.
const BROWSE_KEY_WAIT: Duration = Duration::from_millis(50);

/// Browses `target` until the user quits. Fails before it draws anything when stdin or stdout is
/// not a terminal.
pub async fn run(connection: &Connection, target: BrowseTarget) -> FsqResult<()> {
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        return Err(FsqError::BrowseNeedsTerminal);
    }
    let (mut browser, first_query) = Browser::open(target, connection.db.get_documents_path());
    let mut calls = Calls::new(connection.db.clone());
    let mut screen = Screen::enter()?;
    let mut viewport = Viewport::default();
    let mut effects = vec![first_query];
    loop {
        for effect in effects.drain(..) {
            match effect {
                Effect::Query(query) => calls.query(query),
                Effect::ListSubcollections { request, document } => {
                    calls.list_subcollections(request, document)
                }
                Effect::Cancel => calls.cancel(),
                Effect::Quit => return Ok(()),
            }
        }
        screen
            .terminal
            .draw(|frame| browser.draw(frame, &mut viewport))?;
        let key = tokio::task::block_in_place(next_key)?;
        let typed = key.and_then(|key| {
            message_for(key, browser.filter_editor().is_some(), viewport.page_rows())
        });
        for message in calls.answers().chain(typed) {
            effects.extend(browser.update(message));
        }
    }
}

/// The next key pressed within [`BROWSE_KEY_WAIT`].
fn next_key() -> std::io::Result<Option<KeyEvent>> {
    if !event::poll(BROWSE_KEY_WAIT)? {
        return Ok(None);
    }
    Ok(match event::read()? {
        Event::Key(key) if key.kind == KeyEventKind::Press => Some(key),
        _ => None,
    })
}

/// What `key` asks of the browser: while the filter is being edited, a typed character goes into
/// it. Raw mode delivers Ctrl-C as a key rather than a signal.
fn message_for(key: KeyEvent, editing_filter: bool, page_rows: usize) -> Option<Message> {
    let control = key.modifiers.contains(KeyModifiers::CONTROL);
    if control && key.code == KeyCode::Char('c') {
        return Some(Message::Interrupt);
    }
    if editing_filter {
        return match key.code {
            KeyCode::Enter => Some(Message::RunFilter),
            KeyCode::Esc => Some(Message::CancelFilter),
            KeyCode::Backspace => Some(Message::FilterDelete),
            KeyCode::Char(typed) if !control => Some(Message::FilterInput(typed)),
            _ => None,
        };
    }
    let message = match key.code {
        KeyCode::Up | KeyCode::Char('k') => Message::Move(Movement::Up(1)),
        KeyCode::Down | KeyCode::Char('j') => Message::Move(Movement::Down(1)),
        KeyCode::PageUp => Message::Move(Movement::Up(page_rows)),
        KeyCode::PageDown => Message::Move(Movement::Down(page_rows)),
        KeyCode::Home => Message::Move(Movement::First),
        KeyCode::End => Message::Move(Movement::Last),
        KeyCode::Right | KeyCode::Char('l') | KeyCode::Enter => Message::Expand,
        KeyCode::Left | KeyCode::Char('h') => Message::Collapse,
        KeyCode::Tab | KeyCode::BackTab => Message::SwitchPane,
        KeyCode::Char('/') => Message::EditFilter,
        KeyCode::Esc | KeyCode::Backspace => Message::Back,
        KeyCode::Char('q') => Message::Quit,
        _ => return None,
    };
    Some(message)
}

/// The terminal in raw mode on the alternate screen, until this is dropped. On a panic, ratatui's
/// panic hook restores it before the message prints.
struct Screen {
    terminal: DefaultTerminal,
}

impl Screen {
    fn enter() -> FsqResult<Self> {
        match ratatui::try_init() {
            Ok(terminal) => Ok(Self { terminal }),
            Err(error) => {
                ratatui::restore();
                Err(error.into())
            }
        }
    }
}

impl Drop for Screen {
    fn drop(&mut self) {
        ratatui::restore();
    }
}

/// The Firestore calls the browser waits for, each of which sends its answer back as a message.
/// A query replaces the query before it, and a listing the listing before it, since the browser
/// only waits for the latest of each.
struct Calls {
    db: FirestoreDb,
    sender: UnboundedSender<Message>,
    receiver: UnboundedReceiver<Message>,
    query: Option<AbortHandle>,
    listing: Option<AbortHandle>,
}

impl Calls {
    fn new(db: FirestoreDb) -> Self {
        let (sender, receiver) = mpsc::unbounded_channel();
        Self {
            db,
            sender,
            receiver,
            query: None,
            listing: None,
        }
    }

    fn query(&mut self, query: DocumentsQuery) {
        let db = self.db.clone();
        let sender = self.sender.clone();
        let task = tokio::spawn(async move {
            let outcome = query.documents(&db).await;
            let _ = sender.send(Message::DocumentsLoaded {
                request: query.request,
                outcome,
            });
        });
        if let Some(replaced) = self.query.replace(task.abort_handle()) {
            replaced.abort();
        }
    }

    fn list_subcollections(&mut self, request: state::RequestId, document: String) {
        let db = self.db.clone();
        let sender = self.sender.clone();
        let task = tokio::spawn(async move {
            let outcome = Walk::new(&db).collection_ids(Some(&document)).await;
            let _ = sender.send(Message::SubcollectionsListed { request, outcome });
        });
        if let Some(replaced) = self.listing.replace(task.abort_handle()) {
            replaced.abort();
        }
    }

    fn cancel(&mut self) {
        for call in [self.query.take(), self.listing.take()]
            .into_iter()
            .flatten()
        {
            call.abort();
        }
    }

    /// The answers that have arrived since the last call.
    fn answers(&mut self) -> impl Iterator<Item = Message> + '_ {
        std::iter::from_fn(|| self.receiver.try_recv().ok())
    }
}

impl Drop for Calls {
    fn drop(&mut self) {
        self.cancel();
    }
}

impl DocumentsQuery {
    /// Runs the query. Without a filter, a database that refuses pipelines, as a Standard edition
    /// one does, gets the first documents by ID from a listing instead.
    async fn documents(&self, db: &FirestoreDb) -> FsqResult<Vec<FirestoreDocument>> {
        match db.fluent().sql(self.sql()).query().await {
            Err(error) if self.filter.is_empty() && is_pipeline_refusal(&error) => {
                Walk::new(db)
                    .first_documents(&self.collection, BROWSE_DOCUMENT_LIMIT)
                    .await
            }
            outcome => Ok(outcome?),
        }
    }
}
