//! The full-screen browser of `\browse` and `fsq browse`: the documents of a collection on the
//! left, the values of the selected one as a tree on the right, and the subcollections of each
//! document one level further down.

mod draw;
mod editor;
mod matching;
mod state;
mod tree;

use crate::connection::Connection;
use crate::describe::CollectionPath;
use crate::errors::{is_pipeline_refusal, FsqError, FsqResult};
use crate::render::ANIMATION_TICK;
use crate::walk::Walk;
use crossterm::clipboard::CopyToClipboard;
use crossterm::event::{
    DisableBracketedPaste, EnableBracketedPaste, Event, EventStream, KeyCode, KeyEvent,
    KeyEventKind, KeyModifiers,
};
use crossterm::execute;
use crossterm::terminal::{enable_raw_mode, EnterAlternateScreen};
use draw::Viewport;
pub use editor::FilterHistory;
use editor::{Edit, Recall};
use firestore::{FirestoreCollectionId, FirestoreDb, FirestoreDocument, FirestoreSqlOptions};
use futures::StreamExt;
use ratatui::backend::CrosstermBackend;
use ratatui::{DefaultTerminal, Terminal};
pub use state::FilterTail;
use state::{
    Browser, Copied, DocumentsQuery, Effect, Message, Mode, Movement, RequestId,
    BROWSE_DOCUMENT_LIMIT,
};
use std::future::Future;
use std::io::{IsTerminal, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Once;
use std::time::{Duration, Instant};
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

/// Browses `target` until the user quits, with the filters of `history` to recall, and adds the
/// filters that ran to it. Fails before it draws anything when stdin or stdout is not a terminal.
pub async fn run(
    connection: &Connection,
    target: BrowseTarget,
    history: &mut FilterHistory,
) -> FsqResult<()> {
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        return Err(FsqError::BrowseNeedsTerminal);
    }
    let db = &connection.db;
    let (mut browser, first_query) =
        Browser::open(target, db.get_documents_path(), std::mem::take(history));
    let browsed = BROWSER_TASK
        .scope((), browse(db, &mut browser, first_query))
        .await;
    *history = browser.into_history();
    browsed
}

/// Draws after every key, message and resize, and every [`ANIMATION_TICK`] while the browser waits
/// for Firestore. In between it sleeps.
async fn browse(db: &FirestoreDb, browser: &mut Browser, first_query: Effect) -> FsqResult<()> {
    let mut calls = Calls::default();
    let mut screen = Screen::enter()?;
    let mut viewport = Viewport::default();
    let mut effect = Some(first_query);
    loop {
        match effect {
            Some(Effect::Query(query)) => calls.query(db, query),
            Some(Effect::ListSubcollections { request, document }) => {
                calls.list_subcollections(db, request, document)
            }
            Some(Effect::Copy { text, .. }) => screen.copy(&text)?,
            Some(Effect::Cancel) => calls.cancel(),
            Some(Effect::Quit) => return Ok(()),
            None => {}
        }
        screen.draw(browser, &mut viewport)?;
        effect = loop {
            tokio::select! {
                answer = calls.answer() => break browser.update(answer),
                event = screen.next_event() => {
                    match event? {
                        Event::Key(key) if key.kind == KeyEventKind::Press => {
                            break pressed(browser, &mut viewport, key);
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

/// Takes `key`: while a popup is open, a move scrolls its text, and any other key goes to the
/// browser as the message it stands for. The note of a copy shows until the next key.
fn pressed(browser: &mut Browser, viewport: &mut Viewport, key: KeyEvent) -> Option<Effect> {
    viewport.copied = None;
    let mode = browser.mode();
    let page_rows = viewport.page_rows();
    if let (Mode::Popup, Some(movement)) = (mode, movement_for(key, page_rows)) {
        viewport.scroll_popup(movement);
        return None;
    }
    let effect = browser.update(message_for(key, mode, page_rows)?);
    if let Some(Effect::Copy { copied, .. }) = &effect {
        viewport.copied = Some(*copied);
    }
    effect
}

/// What `key` asks of the browser in `mode`: while the filter is being edited, a typed character
/// goes into it, and a popup takes no key but Esc, Ctrl-C, and `y` and `Y`, which copy. A letter
/// with Ctrl is a key of its own, which does nothing unless it is bound, as Ctrl-C is. Raw mode
/// delivers Ctrl-C as a key rather than a signal.
fn message_for(key: KeyEvent, mode: Mode, page_rows: usize) -> Option<Message> {
    let control = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::Char('c') if control => return Some(Message::Interrupt),
        // Ctrl-G rather than a letter, which the filter editor would take as text. less and vim
        // show what they know about the file on Ctrl-G.
        KeyCode::Char('g') if control && mode != Mode::Popup => return Some(Message::ShowError),
        _ => {}
    }
    match mode {
        Mode::Popup => match key.code {
            KeyCode::Esc => Some(Message::ClosePopup),
            KeyCode::Char('y') if !control => Some(Message::Copy(Copied::Value)),
            KeyCode::Char('Y') if !control => Some(Message::Copy(Copied::Path)),
            _ => None,
        },
        Mode::FilterEditor => filter_message_for(key, control),
        Mode::Panes => pane_message_for(key, control, page_rows),
    }
}

fn filter_message_for(key: KeyEvent, control: bool) -> Option<Message> {
    let edit = match key.code {
        KeyCode::Enter => return Some(Message::RunFilter),
        KeyCode::Esc => return Some(Message::CancelFilter),
        KeyCode::Up => return Some(Message::Recall(Recall::Older)),
        KeyCode::Down => return Some(Message::Recall(Recall::Newer)),
        KeyCode::Char('w') if control => Edit::DeleteWordBefore,
        KeyCode::Char('u') if control => Edit::DeleteToStart,
        KeyCode::Char(typed) if !control => Edit::Insert(typed),
        KeyCode::Backspace => Edit::DeleteBefore,
        KeyCode::Delete => Edit::DeleteAfter,
        KeyCode::Left => Edit::Left,
        KeyCode::Right => Edit::Right,
        KeyCode::Home => Edit::Start,
        KeyCode::End => Edit::End,
        _ => return None,
    };
    Some(Message::Edit(edit))
}

fn pane_message_for(key: KeyEvent, control: bool, page_rows: usize) -> Option<Message> {
    if let Some(movement) = movement_for(key, page_rows) {
        return Some(Message::Move(movement));
    }
    let message = match key.code {
        KeyCode::Char(_) if control => return None,
        KeyCode::Right | KeyCode::Char('l') => Message::Expand,
        KeyCode::Enter => Message::Open,
        KeyCode::Char(' ') => Message::Toggle,
        KeyCode::Left | KeyCode::Char('h') => Message::Collapse,
        KeyCode::Tab | KeyCode::BackTab => Message::SwitchPane,
        KeyCode::Char('/') => Message::EditFilter,
        KeyCode::Char('f') => Message::FilterOnValue,
        KeyCode::Char('y') => Message::Copy(Copied::Value),
        KeyCode::Char('Y') => Message::Copy(Copied::Path),
        KeyCode::Char('r') => Message::Reload,
        KeyCode::Char('?') => Message::ShowHelp,
        KeyCode::Esc | KeyCode::Backspace => Message::Back,
        KeyCode::Char('q') => Message::Quit,
        _ => return None,
    };
    Some(message)
}

/// The move of the cursor, or of the text of a popup, that `key` asks for, where a page is
/// `page_rows` rows.
fn movement_for(key: KeyEvent, page_rows: usize) -> Option<Movement> {
    let control = key.modifiers.contains(KeyModifiers::CONTROL);
    let half_page = (page_rows / 2).max(1);
    let movement = match key.code {
        KeyCode::Up => Movement::Up(1),
        KeyCode::Down => Movement::Down(1),
        KeyCode::PageUp => Movement::Up(page_rows),
        KeyCode::PageDown => Movement::Down(page_rows),
        KeyCode::Home => Movement::First,
        KeyCode::End => Movement::Last,
        KeyCode::Char('u') if control => Movement::Up(half_page),
        KeyCode::Char('d') if control => Movement::Down(half_page),
        _ if control => return None,
        KeyCode::Char('k') => Movement::Up(1),
        KeyCode::Char('j') => Movement::Down(1),
        KeyCode::Char('g') => Movement::First,
        KeyCode::Char('G') => Movement::Last,
        _ => return None,
    };
    Some(movement)
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

/// The terminal in raw mode on the alternate screen, with bracketed paste on, and its events,
/// until this is dropped. On a panic in the browser's own task, a panic hook restores the
/// terminal before the message prints.
struct Screen {
    terminal: DefaultTerminal,
    /// `None` only while the screen is dropped.
    events: Option<EventStream>,
    /// Whether colour is on, as for the rest of fsq: off with `NO_COLOR` set, for one.
    colour: bool,
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
            Ok(terminal) => Ok(Self {
                terminal,
                events: Some(EventStream::new()),
                colour: console::colors_enabled(),
            }),
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

    /// The next key, paste or resize. A terminal that ends its input is an error.
    async fn next_event(&mut self) -> std::io::Result<Event> {
        let events = self
            .events
            .as_mut()
            .expect("the events end only when the screen is dropped");
        events
            .next()
            .await
            .unwrap_or_else(|| Err(std::io::ErrorKind::UnexpectedEof.into()))
    }

    fn draw(&mut self, browser: &Browser, viewport: &mut Viewport) -> std::io::Result<()> {
        if SCREEN_OVERWRITTEN.swap(false, Ordering::SeqCst) {
            self.terminal.clear()?;
        }
        let now = Instant::now();
        self.terminal.draw(|frame| {
            browser.draw(frame, viewport, now);
            if !self.colour {
                draw::drop_hues(frame.buffer_mut());
            }
        })?;
        Ok(())
    }

    /// Puts `text` on the terminal's clipboard, between two frames.
    fn copy(&mut self, text: &str) -> std::io::Result<()> {
        write_clipboard(self.terminal.backend_mut(), text)
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
        // Dropping the stream wakes crossterm's reader, which reedline shares. When the browser
        // read a key last, nothing waits on the reader, so the wake stays queued, and reedline's
        // next prompt takes it for a timeout when it asks where the cursor is, and fails.
        // Reading until nothing is left takes the wake, and with it the keys pressed while the
        // browser was still on screen.
        self.events = None;
        while let Ok(true) = crossterm::event::poll(Duration::ZERO) {
            if crossterm::event::read().is_err() {
                break;
            }
        }
        if SCREEN_ACTIVE.swap(false, Ordering::SeqCst) {
            Self::restore();
        }
    }
}

/// Writes OSC 52, the escape sequence that asks the terminal to put `text` on the system
/// clipboard. It reaches the clipboard of the machine the terminal runs on, also over SSH, where
/// the terminal allows it.
fn write_clipboard(out: &mut impl Write, text: &str) -> std::io::Result<()> {
    execute!(out, CopyToClipboard::to_clipboard_from(text))
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
    use crate::browse::state::{Load, Pane};
    use crate::render::fixtures::{document, draft, dune, string, DOCUMENTS_PATH};
    use ratatui::backend::TestBackend;
    use std::time::Duration;

    /// A browser of `books` that has read `documents`, and the viewport of a frame of it at
    /// 80 by 24, which sizes its pages.
    fn browser_of(documents: Vec<FirestoreDocument>) -> (Browser, Viewport) {
        let target = BrowseTarget {
            collection: "books".parse().expect("a collection path"),
            filter: FilterTail::default(),
        };
        let (mut browser, Effect::Query(query)) =
            Browser::open(target, DOCUMENTS_PATH, FilterHistory::default())
        else {
            panic!("no query")
        };
        browser.update(Message::DocumentsLoaded {
            request: query.request,
            outcome: Ok(documents),
        });
        let mut viewport = Viewport::default();
        drawn(&browser, &mut viewport);
        (browser, viewport)
    }

    fn drawn(browser: &Browser, viewport: &mut Viewport) {
        Terminal::new(TestBackend::new(80, 24))
            .expect("a test terminal")
            .draw(|frame| browser.draw(frame, viewport, Instant::now()))
            .expect("a frame");
    }

    fn press(browser: &mut Browser, viewport: &mut Viewport, code: KeyCode) -> Option<Effect> {
        press_with(browser, viewport, code, KeyModifiers::NONE)
    }

    fn press_with(
        browser: &mut Browser,
        viewport: &mut Viewport,
        code: KeyCode,
        modifiers: KeyModifiers,
    ) -> Option<Effect> {
        let effect = pressed(browser, viewport, KeyEvent::new(code, modifiers));
        drawn(browser, viewport);
        effect
    }

    fn books(count: usize) -> Vec<FirestoreDocument> {
        (1..=count)
            .map(|number| document(&format!("book-{number:03}"), vec![("title", string("t"))]))
            .collect()
    }

    fn selected(browser: &Browser) -> usize {
        browser.level().selected()
    }

    /// How many rows the tree of the selected document shows.
    fn tree_rows(browser: &Browser) -> usize {
        let level = browser.level();
        let document = level.selected_document().expect("a document");
        level.tree().rows(document).len()
    }

    #[test]
    fn g_and_shift_g_go_to_the_first_and_the_last_row() {
        let (mut browser, mut viewport) = browser_of(books(30));

        press(&mut browser, &mut viewport, KeyCode::Char('G'));
        assert_eq!(selected(&browser), 29);
        press(&mut browser, &mut viewport, KeyCode::Char('g'));
        assert_eq!(selected(&browser), 0);
    }

    #[test]
    fn ctrl_d_and_ctrl_u_move_half_a_page() {
        let (mut browser, mut viewport) = browser_of(books(30));
        let half_page = viewport.page_rows() / 2;
        assert!(half_page > 1);

        press_with(
            &mut browser,
            &mut viewport,
            KeyCode::Char('d'),
            KeyModifiers::CONTROL,
        );
        assert_eq!(selected(&browser), half_page);
        press_with(
            &mut browser,
            &mut viewport,
            KeyCode::Char('d'),
            KeyModifiers::CONTROL,
        );
        press_with(
            &mut browser,
            &mut viewport,
            KeyCode::Char('u'),
            KeyModifiers::CONTROL,
        );
        assert_eq!(selected(&browser), half_page);
    }

    #[test]
    fn space_opens_and_closes_the_map_under_the_cursor() {
        let (mut browser, mut viewport) = browser_of(vec![dune()]);
        press(&mut browser, &mut viewport, KeyCode::Tab);
        let collapsed = tree_rows(&browser);

        press(&mut browser, &mut viewport, KeyCode::Char(' '));
        let expanded = tree_rows(&browser);
        press(&mut browser, &mut viewport, KeyCode::Char(' '));

        assert_eq!(expanded, collapsed + 2);
        assert_eq!(tree_rows(&browser), collapsed);
    }

    #[test]
    fn left_in_the_documents_goes_up_a_level() {
        let (mut browser, mut viewport) = browser_of(vec![draft(), dune()]);
        press(&mut browser, &mut viewport, KeyCode::Down);
        press(&mut browser, &mut viewport, KeyCode::Right);
        press(&mut browser, &mut viewport, KeyCode::Char('G'));
        let Some(Effect::ListSubcollections { request, .. }) =
            press(&mut browser, &mut viewport, KeyCode::Right)
        else {
            panic!("no listing")
        };
        browser.update(Message::SubcollectionsListed {
            request,
            outcome: Ok(vec!["reviews".parse().expect("a collection ID")]),
        });
        press(&mut browser, &mut viewport, KeyCode::Char('G'));
        press(&mut browser, &mut viewport, KeyCode::Right);
        assert!(browser.has_parent());
        assert_eq!(browser.focus(), Pane::Documents);

        let up = press(&mut browser, &mut viewport, KeyCode::Left);

        assert_eq!(up, Some(Effect::Cancel));
        assert!(!browser.has_parent());
        assert_eq!(selected(&browser), 1);
        assert_eq!(press(&mut browser, &mut viewport, KeyCode::Left), None);
    }

    #[test]
    fn r_reads_the_documents_again_with_their_filter_and_keeps_the_selected_one() {
        let (mut browser, mut viewport) = browser_of(vec![draft(), dune()]);
        press(&mut browser, &mut viewport, KeyCode::Down);
        press(&mut browser, &mut viewport, KeyCode::Right);
        press(&mut browser, &mut viewport, KeyCode::Right);
        let expanded = tree_rows(&browser);

        let Some(Effect::Query(query)) = press(&mut browser, &mut viewport, KeyCode::Char('r'))
        else {
            panic!("no query")
        };
        browser.update(Message::DocumentsLoaded {
            request: query.request,
            outcome: Ok(vec![document("prologue", Vec::new()), draft(), dune()]),
        });

        assert_eq!(query.filter, FilterTail::default());
        assert_eq!(selected(&browser), 2);
        assert_eq!(tree_rows(&browser), expanded);
    }

    #[test]
    fn the_keys_of_the_panes_type_into_the_filter_editor() {
        let (mut browser, mut viewport) = browser_of(vec![draft(), dune()]);
        press(&mut browser, &mut viewport, KeyCode::Char('/'));

        for typed in ['g', 'G', ' ', 'r', '?', 'q', 'j', 'l', 'y', 'Y', 'f'] {
            assert_eq!(
                press(&mut browser, &mut viewport, KeyCode::Char(typed)),
                None
            );
        }

        assert_eq!(
            browser.filter_editor().map(|editor| editor.text()),
            Some("gG r?qjlyYf")
        );
        assert_eq!(selected(&browser), 0);
        assert!(browser.popup().is_none());
    }

    #[test]
    fn a_letter_with_ctrl_is_not_the_letter_in_the_panes_and_the_popup() {
        let (mut browser, mut viewport) = browser_of(vec![dune()]);
        press(&mut browser, &mut viewport, KeyCode::Tab);

        for letter in ['q', 'r', 'y', 'f', 'l', '/'] {
            assert_eq!(
                press_with(
                    &mut browser,
                    &mut viewport,
                    KeyCode::Char(letter),
                    KeyModifiers::CONTROL
                ),
                None,
                "Ctrl-{letter}"
            );
        }
        assert!(browser.filter_editor().is_none());
        assert_eq!(tree_rows(&browser), dune().fields.len() + 1);
        press(&mut browser, &mut viewport, KeyCode::Enter);
        press(&mut browser, &mut viewport, KeyCode::Enter);
        assert!(browser.popup().is_some());
        assert_eq!(
            press_with(
                &mut browser,
                &mut viewport,
                KeyCode::Char('y'),
                KeyModifiers::CONTROL
            ),
            None
        );
    }

    /// What `effect` writes to the terminal to put its text on the clipboard.
    fn written_to_clipboard(effect: Option<Effect>) -> Vec<u8> {
        let Some(Effect::Copy { text, .. }) = effect else {
            panic!("no copy: {effect:?}")
        };
        let mut written = Vec::new();
        write_clipboard(&mut written, &text).expect("a write to memory");
        written
    }

    /// The OSC 52 sequence that puts the text encoded as `base64` on the clipboard.
    fn osc_52(base64: &str) -> Vec<u8> {
        format!("\x1b]52;c;{base64}\x1b\\").into_bytes()
    }

    #[test]
    fn y_copies_a_map_as_tagged_json_and_a_string_as_its_text_over_osc_52() {
        let (mut browser, mut viewport) = browser_of(vec![dune()]);
        assert_eq!(press(&mut browser, &mut viewport, KeyCode::Char('y')), None);
        press(&mut browser, &mut viewport, KeyCode::Tab);

        let address = press(&mut browser, &mut viewport, KeyCode::Char('y'));
        press(&mut browser, &mut viewport, KeyCode::Right);
        press(&mut browser, &mut viewport, KeyCode::Down);
        let city = press(&mut browser, &mut viewport, KeyCode::Char('y'));

        // {"city":"London","zip":"N1 9GU"} and London
        assert_eq!(
            written_to_clipboard(address),
            osc_52("eyJjaXR5IjoiTG9uZG9uIiwiemlwIjoiTjEgOUdVIn0=")
        );
        assert_eq!(written_to_clipboard(city), osc_52("TG9uZG9u"));
    }

    #[test]
    fn shift_y_copies_the_path_the_tree_border_shows_and_the_popup_copies_too() {
        let (mut browser, mut viewport) = browser_of(vec![dune()]);
        press(&mut browser, &mut viewport, KeyCode::Tab);
        press(&mut browser, &mut viewport, KeyCode::Right);
        press(&mut browser, &mut viewport, KeyCode::Down);
        let city = press(&mut browser, &mut viewport, KeyCode::Char('Y'));
        press(&mut browser, &mut viewport, KeyCode::Up);
        press(&mut browser, &mut viewport, KeyCode::Left);
        press(&mut browser, &mut viewport, KeyCode::Down);
        press(&mut browser, &mut viewport, KeyCode::Down);
        press(&mut browser, &mut viewport, KeyCode::Enter);
        let popup_path = press(&mut browser, &mut viewport, KeyCode::Char('Y'));
        let popup_value = press(&mut browser, &mut viewport, KeyCode::Char('y'));

        // address.city, blurb, and the blurb with its line break
        assert_eq!(written_to_clipboard(city), osc_52("YWRkcmVzcy5jaXR5"));
        assert_eq!(written_to_clipboard(popup_path), osc_52("Ymx1cmI="));
        assert_eq!(
            written_to_clipboard(popup_value),
            osc_52(
                "QSBkZXNlcnQgcGxhbmV0LCBhIHNwaWNlIHRoYXQgZXh0ZW5kcyBsaWZlLAphbmQgYSBkdWtlJ3Mgc29u"
            )
        );
    }

    #[test]
    fn f_opens_the_editor_with_a_where_on_a_leaf_and_does_nothing_on_a_map() {
        let (mut browser, mut viewport) = browser_of(vec![dune()]);
        press(&mut browser, &mut viewport, KeyCode::Tab);

        press(&mut browser, &mut viewport, KeyCode::Char('f'));
        assert!(browser.filter_editor().is_none());
        for _ in 0..6 {
            press(&mut browser, &mut viewport, KeyCode::Down);
        }
        press(&mut browser, &mut viewport, KeyCode::Char('f'));

        assert_eq!(
            browser.filter_editor().map(|editor| editor.text()),
            Some("WHERE pages = 412")
        );
    }

    async fn panicking_query() -> FsqResult<Vec<FirestoreDocument>> {
        panic!("a bug in the query")
    }

    #[tokio::test]
    async fn a_call_that_panics_fails_its_load() {
        let target = BrowseTarget {
            collection: "books".parse().expect("a collection path"),
            filter: FilterTail::default(),
        };
        let (mut browser, Effect::Query(query)) =
            Browser::open(target, DOCUMENTS_PATH, FilterHistory::default())
        else {
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
