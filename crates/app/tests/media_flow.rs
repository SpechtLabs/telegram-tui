//! Full-app media flows against `FakeTd` (docs/plan.md T40, spec §10 and
//! §15.4): a download driven from the chip row through progress to
//! completion, the affordance flip that completion causes, and a `/send`
//! that reaches TDLib with a real file behind it.
//!
//! Same harness shape as `send_flow.rs`: the real `runtime_loop::Core`, the
//! real dispatcher and the real `App::update`, with keys pushed in as
//! `crossterm` events and TDLib replaced by a scripted fixture. Everything
//! `send_flow.rs`'s module docs say about gating applies here too, with one
//! media-specific twist recorded below.
//!
//! # Gating a progress sequence
//!
//! `FakeTd` pushes every `Emit` step following an `Await` the moment that
//! `Await` is answered, and the loop's `select!` picks freely between a
//! ready RPC completion and a ready push. A download script that emitted
//! `updateFile(50%)` and `updateFile(completed)` straight after answering
//! `downloadFile` would therefore have no defined order at all — the
//! completed snapshot could be overwritten by the "still downloading" answer
//! to the request that started it, and the assertion would flake.
//!
//! So each push sits behind its own `Await`, and the test opens those gates
//! deliberately: `esc` leaves selection mode and `↑` re-enters it on the
//! newest message, which fires a fresh `GetMessageProperties` (T26). That
//! gate was chosen over moving the selection cursor because it puts the
//! cursor back on the *same* message — the file's own — which is what
//! `completed_download_enables_open` needs standing when the completion
//! lands, so that the chip flip it observes is `state/media.rs`'s doing and
//! not a side effect of re-entering selection mode.
//!
//! # What is not driven here
//!
//! Pressing `⏎ open` on a completed download spawns a real child process
//! (`dispatch::open_external`, `TGT_OPENER`). Pointing that at a harmless
//! command means mutating the environment of a running test binary, which
//! `env::set_var` makes unsafe for good reason: sibling tests in this same
//! process read the environment concurrently. So these tests assert the
//! *input* to that handoff — the Open chip being offered and the local path
//! the completed snapshot carries — and `dispatch.rs`'s own
//! `open_external_reports_back_with_the_path_it_was_given` covers the spawn
//! itself.
//!
//! `tgt-app` is a binary crate with no library target, so the modules under
//! test are included by path; the crate-level `allow(dead_code)` is for the
//! surface that comes along with them.

#![allow(dead_code)]

#[path = "../src/config.rs"]
mod config;
#[path = "../src/dispatch.rs"]
mod dispatch;
#[path = "../src/media_kind.rs"]
mod media_kind;
#[path = "../src/runtime_loop.rs"]
mod runtime_loop;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use tokio::sync::mpsc;
use tokio::time::timeout;

use tgt_core::app::{App, AppState, Boot};
use tgt_core::effect::TelemetryMode;
use tgt_core::model::chat::{ChatKind, ChatListId, ChatPositionEntry, ChatView};
use tgt_core::model::chips::Chip;
use tgt_core::model::entity::FormattedText;
use tgt_core::model::ids::{ChatId, FileId, MessageId, UserId};
use tgt_core::model::key::KeyBindings;
use tgt_core::model::message::{
    FileSnapshot, MessageCaps, MessageContent, MessageView, SendState, Sender,
};
use tgt_core::state::chat_list::visible_rows;
use tgt_core::state::conversation::ConversationState;
use tgt_core::state::focus::{Focus, ModalKind};
use tgt_core::state::media::UploadProgress;
use tgt_core::td::fake::{FakeTd, RequestMatcher, RespondWith, ScriptStep};
use tgt_core::td::request::{OutgoingFileKind, TdRequest, TdResponse};
use tgt_core::td::runtime::TdRuntime;
use tgt_core::td::update::{AuthPhase, TdUpdate};
use tgt_ui::render::cache::LayoutCache;
use tgt_ui::theme::Theme;

use config::Config;
use dispatch::TdBootParams;
use runtime_loop::Core;

/// Ceiling for any single "advance until" wait; every step is driven by a
/// channel or the 250 ms tick, so this only bounds a hang.
const SETTLE_TIMEOUT: Duration = Duration::from_secs(5);

const CHAT: ChatId = ChatId(1);
/// The document message every download test selects, and the file behind it.
const DOC_ID: MessageId = MessageId(103);
const FILE: FileId = FileId(77);
const FILE_BYTES: u64 = 2_400;
/// The ids TDLib mints for a message it has accepted but not yet sent, and
/// the real one it reports afterwards (architecture §5.2).
const TEMP_ID: MessageId = MessageId(9999);
const FINAL_ID: MessageId = MessageId(10001);
/// The two text messages preceding the document in every script's history.
const HISTORY: std::ops::RangeInclusive<i64> = 101..=102;

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

struct Harness {
    core: Core,
    fake: Arc<FakeTd>,
    keys: mpsc::Sender<Event>,
    /// Every distinct `(downloaded_size, is_completed)` [`FILE`] has been
    /// seen in, in the order the loop applied them — see
    /// `download_progress_drives_snapshot_sequence`.
    file_trace: Vec<(u64, bool)>,
}

impl Harness {
    fn new(fixture: &str) -> Harness {
        let fake = Arc::new(FakeTd::from_jsonl(fixture).expect("fixture is valid JSONL"));
        // Generous on purpose: `press` is not a stepping call, so a whole
        // `/send <absolute path>` is queued before the loop runs once, and a
        // full key channel would deadlock the test against itself.
        let (keys, key_events) = mpsc::channel::<Event>(1024);
        let core = Core::new(
            App::new(boot()),
            Arc::clone(&fake) as Arc<dyn TdRuntime>,
            Arc::new(Mutex::new(configured())),
            TdBootParams {
                database_directory: PathBuf::from("/tmp/tgt-media-flow-db"),
                database_encryption_key: vec![7u8; 32],
            },
            key_events,
        );
        Harness {
            core,
            fake,
            keys,
            file_trace: Vec::new(),
        }
    }

    async fn press(&self, code: KeyCode) {
        let event = Event::Key(KeyEvent {
            code,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        });
        self.keys.send(event).await.expect("the loop is still up");
    }

    async fn type_text(&self, text: &str) {
        for c in text.chars() {
            self.press(KeyCode::Char(c)).await;
        }
    }

    /// Leaves selection mode and re-enters it on the newest message, firing
    /// the `GetMessageProperties` the scripts gate their pushes on (see the
    /// module docs). The selection ends up exactly where it started.
    async fn reenter_selection(&mut self) {
        self.press(KeyCode::Esc).await;
        self.press(KeyCode::Up).await;
    }

    /// Steps the loop until `done` holds, recording every change to the
    /// tracked file's progress along the way, and failing with a readable
    /// dump rather than hanging.
    async fn advance_until(&mut self, what: &str, mut done: impl FnMut(&Core, &FakeTd) -> bool) {
        let settled = timeout(SETTLE_TIMEOUT, async {
            while !done(&self.core, &self.fake) {
                self.core.step().await;
                if let Some(file) = self.core.app().state().media.files.get(&FILE) {
                    let point = (file.downloaded_size, file.is_completed);
                    if self.file_trace.last() != Some(&point) {
                        self.file_trace.push(point);
                    }
                }
            }
        })
        .await;

        assert!(
            settled.is_ok(),
            "timed out waiting for {what}\n  focus: {:?}\n  chips: {:?}\n  file: {:?}\n  uploads: {:?}\n  requests: {:?}",
            self.state().focus.current(),
            self.chips(),
            self.state().media.files.get(&FILE),
            self.state().media.uploads,
            self.requests(),
        );
    }

    fn state(&self) -> &AppState {
        self.core.app().state()
    }

    fn requests(&self) -> Vec<&'static str> {
        self.fake.received().iter().map(TdRequest::kind).collect()
    }

    fn convo(&self) -> &ConversationState {
        self.state()
            .conversations
            .get(&CHAT)
            .expect("the chat is open")
    }

    fn window_ids(&self) -> Vec<i64> {
        self.state()
            .conversations
            .get(&CHAT)
            .map(|c| c.messages.iter().map(|m| m.id.0).collect())
            .unwrap_or_default()
    }

    fn file(&self) -> Option<&FileSnapshot> {
        self.state().media.files.get(&FILE)
    }

    /// The chips selection mode is currently offering, if any.
    fn chips(&self) -> Vec<Chip> {
        self.state()
            .conversations
            .get(&CHAT)
            .and_then(|c| c.selection.as_ref())
            .map(|s| s.chips.clone())
            .unwrap_or_default()
    }

    /// Opens the only chat in the sidebar and waits for its first page, so
    /// every test starts from a loaded conversation with the composer focused.
    async fn open_chat_with_history(&mut self) {
        self.advance_until("the sidebar to fill", |core, _| {
            !visible_rows(&core.app().state().chat_list).is_empty()
        })
        .await;

        self.press(KeyCode::Down).await;
        self.advance_until("the top row to be selected", |core, _| {
            core.app().state().chat_list.selected.is_some()
        })
        .await;

        self.press(KeyCode::Enter).await;
        self.advance_until("the first page to land", |core, _| {
            core.app()
                .state()
                .conversations
                .get(&CHAT)
                .is_some_and(|c| c.messages.len() == HISTORY.count() + 1)
        })
        .await;

        assert_eq!(
            *self.state().focus.current(),
            Focus::Composer,
            "opening a chat leaves the conversation side focused (spec §6.2)"
        );
    }

    /// Enters selection mode on the newest message (the document) and waits
    /// for the capability fetch that gives it a full chip row.
    async fn select_the_document(&mut self) {
        self.press(KeyCode::Up).await;
        self.advance_until("the document's chip row", |core, _| {
            core.app()
                .state()
                .conversations
                .get(&CHAT)
                .and_then(|c| c.selection.as_ref())
                .is_some_and(|s| s.message_id == DOC_ID && s.chips.contains(&Chip::Download))
        })
        .await;
    }

    /// Draws one frame exactly as the binary does and flattens it into one
    /// string per row, so a plain `contains` can look for rendered text.
    fn render(&self, width: u16, height: u16) -> String {
        let theme = Theme::default_dark();
        let mut cache = LayoutCache::new();
        let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("test backend");
        terminal
            .draw(|f| tgt_ui::view(self.state(), &theme, f, &mut cache))
            .expect("draw");

        let buffer = terminal.backend().buffer();
        let mut out = String::with_capacity(buffer.content.len() + buffer.area.height as usize);
        for row in buffer.content.chunks(buffer.area.width as usize) {
            for cell in row {
                out.push_str(cell.symbol());
            }
            out.push('\n');
        }
        out
    }
}

fn boot() -> Boot {
    Boot {
        theme_name: "default".to_string(),
        bindings: KeyBindings::default(),
        layout_breakpoint_cols: 100,
        telemetry_mode: TelemetryMode::Off,
        telemetry_salt: [0u8; 32],
        consent_needed: false,
        has_credentials: true,
        width: 120,
        height: 40,
    }
}

fn configured() -> Config {
    Config {
        api_id: Some(12345),
        api_hash: Some("0123456789abcdef0123456789abcdef".to_string()),
        ..Config::default()
    }
}

/// A real file on disk for the send path to resolve, kept alive by the
/// returned `TempDir` (dropping it deletes the file).
fn temp_file(name: &str) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join(name);
    std::fs::write(&path, b"fake image bytes").expect("write temp file");
    (dir, path)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// Spec §10 end to end: the Download chip issues `downloadFile`, TDLib's
/// answer and its `updateFile` pushes land in the file table in order, and
/// the card under the message redraws from each of them — a progress bar
/// while it runs, the open affordance once it finishes.
#[tokio::test]
async fn download_progress_drives_snapshot_sequence() {
    let mut app = Harness::new(&read_fixture("media_flow.jsonl"));
    app.open_chat_with_history().await;
    app.select_the_document().await;

    // `l` is the Download chip's shortcut.
    app.press(KeyCode::Char('l')).await;
    app.advance_until("the download to start", |core, _| {
        core.app().state().media.files.contains_key(&FILE)
    })
    .await;

    let started = app.file().expect("just asserted it is there");
    assert!(started.is_downloading);
    assert_eq!(started.downloaded_size, 0);
    assert!(
        app.requests().contains(&"DownloadFile"),
        "requests: {:?}",
        app.requests()
    );

    // Gate the first progress push (see the module docs), then the second.
    app.reenter_selection().await;
    app.advance_until("the progress push", |core, _| {
        core.app()
            .state()
            .media
            .files
            .get(&FILE)
            .is_some_and(|f| f.downloaded_size == FILE_BYTES / 2)
    })
    .await;

    let rendered = app.render(120, 40);
    assert!(
        rendered.contains("50%"),
        "the half-finished download is missing its progress bar:\n{rendered}"
    );

    app.reenter_selection().await;
    app.advance_until("the download to finish", |core, _| {
        core.app()
            .state()
            .media
            .files
            .get(&FILE)
            .is_some_and(|f| f.is_completed)
    })
    .await;

    let done = app.file().expect("the file is tracked");
    assert!(!done.is_downloading);
    assert_eq!(done.downloaded_size, FILE_BYTES);
    assert_eq!(done.local_path, Some(PathBuf::from("/tmp/tgt-spec.pdf")));

    // Every state the table passed through, in order: the answer to the
    // request, then each push, with nothing skipped or applied twice.
    assert_eq!(
        app.file_trace,
        vec![(0, false), (FILE_BYTES / 2, false), (FILE_BYTES, true)],
    );

    let rendered = app.render(120, 40);
    assert!(
        rendered.contains("⏎ open"),
        "the finished download still isn't offering to open:\n{rendered}"
    );
}

/// A download that finishes under a message the cursor is already sitting on
/// has to flip that message's chip row from Download to Open by itself —
/// nothing else re-derives the row at that moment (`state/media.rs`'s "Chip
/// recompute on completion").
#[tokio::test]
async fn completed_download_enables_open() {
    let mut app = Harness::new(&read_fixture("media_flow.jsonl"));
    app.open_chat_with_history().await;
    app.select_the_document().await;

    app.press(KeyCode::Char('l')).await;
    app.advance_until("the download to start", |core, _| {
        core.app().state().media.files.contains_key(&FILE)
    })
    .await;

    assert!(app.chips().contains(&Chip::Download));
    assert!(
        !app.chips().contains(&Chip::Open),
        "nothing to open while the file is still coming down: {:?}",
        app.chips()
    );

    // Two gates, then the completion — the cursor never leaves the document.
    app.reenter_selection().await;
    app.advance_until("the progress push", |core, _| {
        core.app()
            .state()
            .media
            .files
            .get(&FILE)
            .is_some_and(|f| f.downloaded_size == FILE_BYTES / 2)
    })
    .await;
    app.reenter_selection().await;
    app.advance_until("the affordance to flip", |core, _| {
        core.app()
            .state()
            .conversations
            .get(&CHAT)
            .and_then(|c| c.selection.as_ref())
            .is_some_and(|s| s.chips.contains(&Chip::Open))
    })
    .await;

    let selection = app
        .convo()
        .selection
        .as_ref()
        .expect("selection mode is still up");
    assert_eq!(selection.message_id, DOC_ID);
    assert!(
        !selection.chips.contains(&Chip::Download),
        "a downloaded file is not downloadable again: {:?}",
        selection.chips
    );
    assert!(
        selection.chip_cursor < selection.chips.len(),
        "the cursor must stay inside the row it was clamped against"
    );
    // What pressing Open would hand to the platform viewer.
    assert_eq!(
        app.file().and_then(|f| f.local_path.clone()),
        Some(PathBuf::from("/tmp/tgt-spec.pdf")),
    );
}

/// Spec §10's send path, end to end: `/send <path>` confirms through a
/// modal, the dispatcher resolves the path and upgrades the kind core could
/// not derive (`.jpg` → `Photo`), and the optimistic message TDLib answers
/// with lands in the window with its upload tracked — until the send
/// completes and the tracking is dropped.
#[tokio::test]
async fn send_file_emits_upload_and_optimistic_message() {
    let (_dir, path) = temp_file("snapshot.jpg");
    let mut app = Harness::new(&to_jsonl(&send_file_script()));
    app.open_chat_with_history().await;

    app.type_text(&format!("/send {}", path.display())).await;
    app.press(KeyCode::Enter).await;
    app.advance_until("the confirmation modal", |core, _| {
        matches!(core.app().state().focus.current(), Focus::Modal(_))
    })
    .await;

    assert!(
        matches!(
            app.state().focus.current(),
            Focus::Modal(ModalKind::ConfirmSendFile { path: offered }) if offered == &path
        ),
        "focus: {:?}",
        app.state().focus.current()
    );
    assert_eq!(
        app.state().composer.input.text,
        "",
        "a parsed /send clears the input like an ordinary submit"
    );

    app.press(KeyCode::Enter).await;
    app.advance_until("the optimistic message", |core, _| {
        core.app()
            .state()
            .conversations
            .get(&CHAT)
            .is_some_and(|c| c.messages.iter().any(|m| m.id == TEMP_ID))
    })
    .await;

    // Core sends every file as `Document` because it may not look at the
    // file; the dispatcher replaced that with what the extension implies.
    let sends: Vec<TdRequest> = app
        .fake
        .received()
        .into_iter()
        .filter(|r| matches!(r, TdRequest::SendMessageFile { .. }))
        .collect();
    assert_eq!(sends.len(), 1, "exactly one send went out: {sends:?}");
    assert!(
        matches!(
            &sends[0],
            TdRequest::SendMessageFile {
                chat_id: CHAT,
                path: sent,
                kind: OutgoingFileKind::Photo,
                caption: None,
            } if sent == &path
        ),
        "got {:?}",
        sends[0]
    );

    let optimistic = app
        .convo()
        .messages
        .iter()
        .find(|m| m.id == TEMP_ID)
        .expect("just asserted it is there");
    assert_eq!(optimistic.send_state, SendState::Sending);
    assert!(optimistic.is_outgoing);
    // The upload is tracked under the temporary id. A photo carries no size
    // in the model, so its total is zero and the bar is indeterminate — see
    // `App::start_tracking_upload`.
    assert_eq!(
        app.state().media.uploads.get(&TEMP_ID),
        Some(&UploadProgress {
            chat_id: CHAT,
            uploaded: 0,
            total: 0,
        })
    );
    let rendered = app.render(120, 40);
    assert!(
        rendered.contains('↑'),
        "the in-flight upload is missing its card:\n{rendered}"
    );

    // Open the gate (see `send_flow.rs`'s module docs): the confirmation
    // push follows the capability fetch `↑` fires.
    app.press(KeyCode::Up).await;
    app.advance_until("the confirmation swap", |core, _| {
        core.app()
            .state()
            .conversations
            .get(&CHAT)
            .is_some_and(|c| c.messages.iter().any(|m| m.id == FINAL_ID))
    })
    .await;

    assert_eq!(app.window_ids(), vec![101, 102, 103, FINAL_ID.0]);
    assert!(
        app.state().media.uploads.is_empty(),
        "a sent message has nothing left to upload: {:?}",
        app.state().media.uploads
    );
}

/// The other half of the send path: a path that isn't there never reaches
/// TDLib, and the send still completes — as a failure, so nothing is left
/// waiting on it (`dispatch::resolve_outgoing_file`).
#[tokio::test]
async fn send_file_with_a_missing_path_never_reaches_tdlib() {
    let mut app = Harness::new(&to_jsonl(&opened_chat()));
    app.open_chat_with_history().await;

    app.type_text("/send /definitely/not/here.jpg").await;
    app.press(KeyCode::Enter).await;
    app.advance_until("the confirmation modal", |core, _| {
        matches!(core.app().state().focus.current(), Focus::Modal(_))
    })
    .await;

    app.press(KeyCode::Enter).await;
    app.advance_until("the modal to close", |core, _| {
        !matches!(core.app().state().focus.current(), Focus::Modal(_))
    })
    .await;
    // Nothing else is coming: give the (never sent) request every chance to
    // show up before concluding that it didn't.
    app.advance_until("a housekeeping tick", |core, _| {
        core.app().state().now.0 > 0
    })
    .await;

    assert!(
        !app.requests().contains(&"SendMessageFile"),
        "requests: {:?}",
        app.requests()
    );
    assert_eq!(
        app.window_ids(),
        vec![101, 102, 103],
        "a send that never happened leaves nothing in the window"
    );
}

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
}

fn read_fixture(name: &str) -> String {
    let path = fixture_dir().join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()))
}

fn to_jsonl(steps: &[ScriptStep]) -> String {
    let mut out = String::new();
    for step in steps {
        out.push_str(&serde_json::to_string(step).expect("ScriptStep serializes"));
        out.push('\n');
    }
    out
}

fn expect(kind: &str) -> RequestMatcher {
    RequestMatcher::Kind(kind.to_string())
}

fn position(order: i64) -> ChatPositionEntry {
    ChatPositionEntry {
        list: ChatListId::Main,
        order,
        is_pinned: false,
    }
}

fn incoming(id: i64, content: MessageContent) -> MessageView {
    MessageView {
        id: MessageId(id),
        chat_id: CHAT,
        sender: Sender::User(UserId(42)),
        sender_name: "Ada".to_string(),
        is_outgoing: false,
        date: 1_700_000_000 + id * 60,
        content,
        reply_to: None,
        send_state: SendState::Sent,
        reactions: Vec::new(),
        caps: MessageCaps::default(),
        is_edited: false,
    }
}

fn text(body: &str) -> MessageContent {
    MessageContent::Text(FormattedText {
        text: body.to_string(),
        entities: Vec::new(),
    })
}

/// The newest message in every script: a document nobody has downloaded yet.
fn document() -> MessageContent {
    MessageContent::Document {
        file_id: FILE,
        file_name: "spec.pdf".to_string(),
        size: FILE_BYTES,
        caption: FormattedText {
            text: String::new(),
            entities: Vec::new(),
        },
    }
}

/// An outgoing photo as TDLib hands it back from `sendMessageFile` (temporary
/// id, `Sending`) or from the confirmation push (real id, `Sent`).
fn outgoing_photo(id: MessageId, send_state: SendState) -> MessageView {
    MessageView {
        id,
        chat_id: CHAT,
        sender: Sender::User(UserId(7)),
        sender_name: "Me".to_string(),
        is_outgoing: true,
        date: 1_700_100_000,
        content: MessageContent::Photo {
            file_id: FileId(78),
            width: 800,
            height: 600,
            caption: FormattedText {
                text: String::new(),
                entities: Vec::new(),
            },
        },
        reply_to: None,
        send_state,
        reactions: Vec::new(),
        caps: MessageCaps::default(),
        is_edited: false,
    }
}

/// [`FILE`] as TDLib reports it partway through a download, and once it is
/// on disk.
fn downloading(bytes: u64) -> FileSnapshot {
    FileSnapshot {
        id: FILE,
        expected_size: FILE_BYTES,
        downloaded_size: bytes,
        is_downloading: true,
        is_completed: false,
        local_path: None,
    }
}

fn downloaded() -> FileSnapshot {
    FileSnapshot {
        id: FILE,
        expected_size: FILE_BYTES,
        downloaded_size: FILE_BYTES,
        is_downloading: false,
        is_completed: true,
        local_path: Some(PathBuf::from("/tmp/tgt-spec.pdf")),
    }
}

/// Everything TDLib withholds from `message` and only answers on
/// `getMessageProperties` (architecture §7).
fn full_caps() -> MessageCaps {
    MessageCaps {
        can_be_edited: true,
        can_be_deleted_for_all_users: true,
        can_be_deleted_only_for_self: true,
        can_be_forwarded: true,
        can_be_saved: true,
    }
}

/// Logged in already, one chat in the sidebar, two text messages and a
/// document: the state every test in this file starts from.
fn opened_chat() -> Vec<ScriptStep> {
    let mut messages: Vec<MessageView> = HISTORY
        .map(|id| incoming(id, text(&format!("history line {id}"))))
        .collect();
    messages.push(incoming(DOC_ID.0, document()));

    vec![
        ScriptStep::Emit(TdUpdate::Auth(AuthPhase::Ready)),
        ScriptStep::Await {
            expect: expect("LoadChats"),
            respond: RespondWith::Ok(TdResponse::Ok),
        },
        ScriptStep::Emit(TdUpdate::NewChat(ChatView {
            id: CHAT,
            kind: ChatKind::Private,
            title: "Ada Lovelace".to_string(),
            positions: vec![position(100)],
            unread_count: 0,
            unread_mention_count: 0,
            last_message: None,
            is_muted: false,
        })),
        ScriptStep::Await {
            expect: expect("GetChatHistory"),
            respond: RespondWith::Ok(TdResponse::Messages { messages }),
        },
    ]
}

/// Selection mode's capability fetch, answered with everything enabled. Used
/// both for its own sake and as the gate described in the module docs.
fn properties_gate() -> ScriptStep {
    ScriptStep::Await {
        expect: expect("GetMessageProperties"),
        respond: RespondWith::Ok(TdResponse::MessageProperties(full_caps())),
    }
}

fn download_script() -> Vec<ScriptStep> {
    let mut steps = opened_chat();
    steps.extend([
        // Entering selection mode on the document.
        properties_gate(),
        ScriptStep::Await {
            expect: expect("DownloadFile"),
            respond: RespondWith::Ok(TdResponse::File(downloading(0))),
        },
        properties_gate(),
        ScriptStep::Emit(TdUpdate::File(downloading(FILE_BYTES / 2))),
        properties_gate(),
        ScriptStep::Emit(TdUpdate::File(downloaded())),
    ]);
    steps
}

fn send_file_script() -> Vec<ScriptStep> {
    let mut steps = opened_chat();
    steps.extend([
        ScriptStep::Await {
            expect: expect("SendMessageFile"),
            respond: RespondWith::Ok(TdResponse::Message(outgoing_photo(
                TEMP_ID,
                SendState::Sending,
            ))),
        },
        properties_gate(),
        ScriptStep::Emit(TdUpdate::MessageSendSucceeded {
            chat_id: CHAT,
            old_message_id: TEMP_ID,
            message: outgoing_photo(FINAL_ID, SendState::Sent),
        }),
    ]);
    steps
}

fn on_disk_fixtures() -> [(&'static str, Vec<ScriptStep>); 1] {
    [("media_flow.jsonl", download_script())]
}

/// The fixtures are generated, not hand-written: their encoding is whatever
/// serde derives on the boundary types, and drift there would otherwise show
/// up as an unexplained parse failure.
#[test]
fn fixtures_on_disk_match_their_scripts() {
    for (name, script) in on_disk_fixtures() {
        let text = read_fixture(name);
        FakeTd::from_jsonl(&text).unwrap_or_else(|e| panic!("{name} does not parse: {e}"));
        assert_eq!(
            text,
            to_jsonl(&script),
            "{name} is stale — run: cargo test -p tgt-app --test media_flow \
             regenerate_fixtures -- --ignored"
        );
    }
}

/// Rewrites the fixture files from the scripts above. Manual, because it
/// writes into the source tree.
#[test]
#[ignore]
fn regenerate_fixtures() {
    for (name, script) in on_disk_fixtures() {
        let path = fixture_dir().join(name);
        std::fs::write(&path, to_jsonl(&script))
            .unwrap_or_else(|e| panic!("writing {}: {e}", path.display()));
    }
}
