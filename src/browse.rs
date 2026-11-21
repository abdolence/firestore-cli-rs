//! The full-screen browser of `\browse` and `fsq browse`: the documents of a collection on the
//! left, the values of the selected one as a tree on the right, and the subcollections of each
//! document one level further down.

mod draw;
mod state;
mod tree;

use crate::connection::Connection;
use crate::describe::CollectionPath;
use crate::errors::{is_pipeline_refusal, FsqError, FsqResult};
use crate::render::ANIMATION_TICK;
use crate::walk::Walk;
use crossterm::event::{
    DisableBracketedPaste, EnableBracketedPaste, Event, EventStream, KeyCode, KeyEvent,
    KeyEventKind, KeyModifiers,
};
use crossterm::execute;
use crossterm::terminal::{enable_raw_mode, EnterAlternateScreen};
use draw::Viewport;
use firestore::{FirestoreCollectionId, FirestoreDb, FirestoreDocument, FirestoreSqlOptions};
use futures::StreamExt;
use ratatui::backend::CrosstermBackend;
use ratatui::{DefaultTerminal, Terminal};
pub use state::FilterTail;
use state::{Browser, DocumentsQuery, Effect, Message, Movement, RequestId, BROWSE_DOCUMENT_LIMIT};
use std::future::Future;
use std::io::IsTerminal;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Once;
use tokio::task::JoinHandle;

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

/// Browses `target` until the user quits. Fails before it draws anything when stdin or stdout is
/// not a terminal.
pub async fn run(connection: &Connection, target: BrowseTarget) -> FsqResult<()> {
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        return Err(FsqError::BrowseNeedsTerminal);
    }
    BROWSER_TASK.scope((), browse(&connection.db, target)).await
}

/// Draws after every message and resize, and every [`ANIMATION_TICK`] while the browser waits for
/// Firestore. In between it sleeps.
async fn browse(db: &FirestoreDb, target: BrowseTarget) -> FsqResult<()> {
    let (mut browser, first_query) = Browser::open(target, db.get_documents_path());
    let mut calls = Calls::default();
    let mut screen = Screen::enter()?;
    let mut events = EventStream::new();
    let mut viewport = Viewport::default();
    let mut effect = Some(first_query);
    loop {
        match effect {
            Some(Effect::Query(query)) => calls.query(db, query),
            Some(Effect::ListSubcollections { request, document }) => {
                calls.list_subcollections(db, request, document)
            }
            Some(Effect::Cancel) => calls.cancel(),
            Some(Effect::Quit) => return Ok(()),
            None => {}
        }
        screen.draw(&browser, &mut viewport)?;
        effect = loop {
            tokio::select! {
                answer = calls.answer() => break browser.update(answer),
                event = events.next() => {
                    match event.unwrap_or_else(|| Err(std::io::ErrorKind::UnexpectedEof.into()))? {
                        Event::Key(key) if key.kind == KeyEventKind::Press => {
                            let editing_filter = browser.filter_editor().is_some();
                            if let Some(message) =
                                message_for(key, editing_filter, viewport.page_rows())
                            {
                                break browser.update(message);
                            }
                        }
                        Event::Paste(text) => break browser.update(Message::Paste(text)),
                        Event::Resize(..) => break None,
                        _ => {}
                    }
                }
                () = tokio::time::sleep(ANIMATION_TICK), if browser.is_waiting() => break None,
            }
        };
    }
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

tokio::task_local! {
    /// Set while the browser's own task runs. A panic there ends the browser, so the panic hook
    /// restores the terminal for it, and a panic in a call the browser waits for does not.
    static BROWSER_TASK: ();
}

/// Whether a [`Screen`] holds the terminal, which is what tells the panic hook that there is
/// something to restore.
static SCREEN_ACTIVE: AtomicBool = AtomicBool::new(false);

/// Whether a panic outside the browser's task printed its message over the screen, which the
/// next frame then draws whole.
static SCREEN_OVERWRITTEN: AtomicBool = AtomicBool::new(false);

/// The terminal in raw mode on the alternate screen, with bracketed paste on, until this is
/// dropped. On a panic in the browser's own task, a panic hook restores the terminal before the
/// message prints.
struct Screen {
    terminal: DefaultTerminal,
}

impl Screen {
    fn enter() -> FsqResult<Self> {
        static PANIC_HOOK: Once = Once::new();
        // One hook for the life of the process: a hook added per `\browse` would stack up, and
        // every later panic would restore the terminal once for each of them.
        PANIC_HOOK.call_once(|| {
            let previous = std::panic::take_hook();
            std::panic::set_hook(Box::new(move |info| {
                let in_browser_task = BROWSER_TASK.try_with(|_| ()).is_ok();
                if in_browser_task && SCREEN_ACTIVE.swap(false, Ordering::SeqCst) {
                    Self::restore();
                }
                previous(info);
                // Only once the message is out, so that a frame drawn while it prints does not
                // take the flag.
                if !in_browser_task && SCREEN_ACTIVE.load(Ordering::SeqCst) {
                    SCREEN_OVERWRITTEN.store(true, Ordering::SeqCst);
                }
            }));
        });
        SCREEN_ACTIVE.store(true, Ordering::SeqCst);
        match Self::init_terminal() {
            Ok(terminal) => Ok(Self { terminal }),
            Err(error) => {
                SCREEN_ACTIVE.store(false, Ordering::SeqCst);
                Self::restore();
                Err(error.into())
            }
        }
    }

    fn init_terminal() -> std::io::Result<DefaultTerminal> {
        enable_raw_mode()?;
        execute!(
            std::io::stdout(),
            EnterAlternateScreen,
            EnableBracketedPaste
        )?;
        Terminal::new(CrosstermBackend::new(std::io::stdout()))
    }

    fn draw(&mut self, browser: &Browser, viewport: &mut Viewport) -> std::io::Result<()> {
        if SCREEN_OVERWRITTEN.swap(false, Ordering::SeqCst) {
            self.terminal.clear()?;
        }
        self.terminal.draw(|frame| browser.draw(frame, viewport))?;
        Ok(())
    }

    fn restore() {
        // ratatui's restore leaves bracketed paste on. A terminal that cannot take this fails
        // that restore too, which reports the error.
        let _ = execute!(std::io::stdout(), DisableBracketedPaste);
        ratatui::restore();
    }
}

impl Drop for Screen {
    fn drop(&mut self) {
        if SCREEN_ACTIVE.swap(false, Ordering::SeqCst) {
            Self::restore();
        }
    }
}

/// The Firestore calls the browser waits for. A query replaces the query before it, and a
/// listing the listing before it, since the browser only waits for the latest of each.
#[derive(Default)]
struct Calls {
    query: Option<Call<Vec<FirestoreDocument>>>,
    listing: Option<Call<Vec<FirestoreCollectionId>>>,
}

impl Calls {
    fn query(&mut self, db: &FirestoreDb, query: DocumentsQuery) {
        let db = db.clone();
        self.query = Some(Call::spawn(query.request, async move {
            query.documents(&db).await
        }));
    }

    fn list_subcollections(&mut self, db: &FirestoreDb, request: RequestId, document: String) {
        let db = db.clone();
        self.listing = Some(Call::spawn(request, async move {
            Walk::new(&db).collection_ids(Some(&document)).await
        }));
    }

    fn cancel(&mut self) {
        self.query = None;
        self.listing = None;
    }

    /// The answer of the first pending call to end, which never comes while none is pending.
    async fn answer(&mut self) -> Message {
        tokio::select! {
            (request, outcome) = Call::ended(&mut self.query) => {
                Message::DocumentsLoaded { request, outcome }
            }
            (request, outcome) = Call::ended(&mut self.listing) => {
                Message::SubcollectionsListed { request, outcome }
            }
        }
    }
}

/// A Firestore call the browser waits for, stopped when it is dropped.
struct Call<T> {
    request: RequestId,
    task: JoinHandle<FsqResult<T>>,
}

impl<T: Send + 'static> Call<T> {
    fn spawn(
        request: RequestId,
        call: impl Future<Output = FsqResult<T>> + Send + 'static,
    ) -> Self {
        Self {
            request,
            task: tokio::spawn(call),
        }
    }
}

impl<T> Call<T> {
    /// Waits for the call in `slot` to end and empties the slot. A call that panicked fails with
    /// [`FsqError::CallPanicked`]. Never ends while the slot is empty.
    async fn ended(slot: &mut Option<Self>) -> (RequestId, FsqResult<T>) {
        let Some(call) = slot else {
            return std::future::pending().await;
        };
        let joined = (&mut call.task).await;
        let request = call.request;
        *slot = None;
        (
            request,
            joined.unwrap_or_else(|error| Err(FsqError::CallPanicked(error))),
        )
    }
}

impl<T> Drop for Call<T> {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl DocumentsQuery {
    /// Runs the query. Without a filter, a database that refuses pipelines, as a Standard edition
    /// one does, gets the first documents by ID from a listing instead.
    async fn documents(&self, db: &FirestoreDb) -> FsqResult<Vec<FirestoreDocument>> {
        let outcome = db
            .fluent()
            .sql(self.sql())
            .options(FirestoreSqlOptions::read_only())
            .query()
            .await;
        match outcome {
            Err(error) if self.filter.is_empty() && is_pipeline_refusal(&error) => {
                Walk::new(db)
                    .first_documents(&self.collection, BROWSE_DOCUMENT_LIMIT)
                    .await
            }
            outcome => Ok(outcome?),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::browse::state::Load;
    use crate::render::fixtures::DOCUMENTS_PATH;
    use std::time::Duration;

    async fn panicking_query() -> FsqResult<Vec<FirestoreDocument>> {
        panic!("a bug in the query")
    }

    #[tokio::test]
    async fn a_call_that_panics_fails_its_load() {
        let target = BrowseTarget {
            collection: "books".parse().expect("a collection path"),
            filter: FilterTail::default(),
        };
        let (mut browser, Effect::Query(query)) = Browser::open(target, DOCUMENTS_PATH) else {
            panic!("no query")
        };
        let mut calls = Calls {
            query: Some(Call::spawn(query.request, panicking_query())),
            listing: None,
        };

        let answer = tokio::time::timeout(Duration::from_secs(5), calls.answer())
            .await
            .expect("an answer from the call");
        browser.update(answer);

        assert!(matches!(
            browser.level().load(),
            Load::Failed {
                error: FsqError::CallPanicked(_),
                ..
            }
        ));
    }
}
