//! Full-app local-first history (T59, docs/plan.md; design spec §5.2): on
//! restart, opening a chat serves TDLib's on-disk cache instantly instead of
//! waiting behind TDLib's startup server sync, then reconciles with the
//! server exactly once in the background.
//!
//! Same harness shape as `read_only.rs` — the real `runtime_loop::Core`, the
//! real dispatcher and the real `App::update`, with keys pushed in as
//! `crossterm` events and TDLib replaced by a scripted fixture — except this
//! file deliberately never calls `tgt_ui::view`: T58 (mouse support) is
//! changing that signature in the main tree concurrently with this task, so
//! every assertion here reads `AppState` and `FakeTd::received()` directly.

#![allow(dead_code)]

#[path = "../src/config.rs"]
mod config;
#[path = "../src/dispatch.rs"]
mod dispatch;
#[path = "../src/media_kind.rs"]
mod media_kind;
#[path = "../src/notify.rs"]
mod notify;
#[path = "../src/runtime_loop.rs"]
mod runtime_loop;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};
use tokio::sync::mpsc;
use tokio::time::timeout;

use tgt_core::app::{App, AppState, Boot};
use tgt_core::effect::TelemetryMode;
use tgt_core::model::chat::{ChatKind, ChatListId, ChatPositionEntry, ChatView};
use tgt_core::model::entity::FormattedText;
use tgt_core::model::ids::{ChatId, MessageId, UserId};
use tgt_core::model::key::KeyBindings;
use tgt_core::model::message::{MessageCaps, MessageContent, MessageView, SendState, Sender};
use tgt_core::state::chat_list::visible_rows;
use tgt_core::td::fake::{FakeTd, RequestMatcher, RespondWith, ScriptStep};
use tgt_core::td::request::{TdRequest, TdResponse};
use tgt_core::td::runtime::TdRuntime;
use tgt_core::td::update::{AuthPhase, TdUpdate};

use config::Config;
use dispatch::TdBootParams;
use runtime_loop::Core;

/// Ceiling for any single "advance until" wait; every step is driven by a
/// channel or the 250 ms tick, so this only bounds a hang.
const SETTLE_TIMEOUT: Duration = Duration::from_secs(5);

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

struct Harness {
    core: Core,
    fake: Arc<FakeTd>,
    keys: mpsc::Sender<Event>,
}

impl Harness {
    fn new(fixture: &str) -> Harness {
        let fake = Arc::new(FakeTd::from_jsonl(fixture).expect("fixture is valid JSONL"));
        let (keys, key_events) = mpsc::channel::<Event>(64);
        let core = Core::new(
            App::new(boot()),
            Arc::clone(&fake) as Arc<dyn TdRuntime>,
            Arc::new(Mutex::new(configured())),
            TdBootParams {
                database_directory: PathBuf::from("/tmp/tgt-local-first-db"),
                database_encryption_key: vec![7u8; 32],
            },
            key_events,
            // No terminal graphics: these harnesses drive a TestBackend, and
            // the design-language §4 line is what a photo renders as there.
            None,
        );
        Harness { core, fake, keys }
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

    /// Steps the loop until `done` holds, failing with a readable dump rather
    /// than hanging.
    async fn advance_until(&mut self, what: &str, mut done: impl FnMut(&Core, &FakeTd) -> bool) {
        let settled = timeout(SETTLE_TIMEOUT, async {
            while !done(&self.core, &self.fake) {
                self.core.step().await;
            }
        })
        .await;

        assert!(
            settled.is_ok(),
            "timed out waiting for {what}\n  screen: {:?}\n  visible rows: {:?}\n  open chat: {:?}\n  window: {} messages\n  requests: {:?}",
            self.state().screen,
            visible_rows(&self.state().chat_list),
            self.state().open_chat,
            self.window_len(),
            self.requests(),
        );
    }

    fn state(&self) -> &AppState {
        self.core.app().state()
    }

    fn requests(&self) -> Vec<TdRequest> {
        self.fake.received()
    }

    /// Loaded messages in the open chat's window (0 when no chat is open).
    fn window_len(&self) -> usize {
        self.state()
            .open_chat
            .and_then(|id| self.state().conversations.get(&id))
            .map(|c| c.messages.len())
            .unwrap_or(0)
    }

    fn window_ids(&self) -> Vec<i64> {
        self.state()
            .open_chat
            .and_then(|id| self.state().conversations.get(&id))
            .map(|c| c.messages.iter().map(|m| m.id.0).collect())
            .unwrap_or_default()
    }

    /// Selects the top row of the sidebar and opens it, waiting for each step
    /// so the keys never race the updates that make them meaningful.
    async fn open_top_chat(&mut self) {
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
        self.advance_until("the chat to open", |core, _| {
            core.app().state().open_chat.is_some()
        })
        .await;
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

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// The bug report this task fixes: on restart, TDLib's on-disk database is
/// intact, but opening a chat used to wait behind TDLib's startup server
/// sync anyway (`only_local: false`). The fix — the request is
/// `only_local: true` — is asserted directly on what TDLib actually
/// received, not inferred from timing.
///
/// The local page renders instantly; a single remote reconcile then follows
/// up (still `from_message_id: MessageId(0)`, TDLib's "newest" sentinel) to
/// pick up whatever arrived on the server while the app was closed. The
/// reconcile's page here overlaps the local one and adds two genuinely new,
/// newer messages — the window ends up with exactly the union, no
/// duplicates.
#[tokio::test]
async fn local_page_renders_instantly_then_reconciles_with_remote() {
    let mut app = Harness::new(&to_jsonl(&local_first_script()));
    app.open_top_chat().await;

    app.advance_until("the local page to land", |core, _| {
        core.app()
            .state()
            .conversations
            .get(&ChatId(1))
            .is_some_and(|c| !c.messages.is_empty())
    })
    .await;

    assert_eq!(
        app.window_len(),
        50,
        "the cached page renders without waiting on the network"
    );

    // The reconcile fires automatically off the back of the local
    // completion — no key press needed. It overlaps ids 3..=50 with two new,
    // newer messages (51, 52); wait for the window to actually grow past 50
    // rather than racing the spawned request.
    app.advance_until("the remote reconcile to land", |core, _| {
        core.app()
            .state()
            .conversations
            .get(&ChatId(1))
            .is_some_and(|c| c.messages.len() > 50)
    })
    .await;

    let history_requests: Vec<TdRequest> = app
        .requests()
        .into_iter()
        .filter(|r| matches!(r, TdRequest::GetChatHistory { .. }))
        .collect();
    assert_eq!(
        history_requests.len(),
        2,
        "the local open and exactly one reconcile, no more: {history_requests:?}"
    );
    assert!(
        matches!(
            history_requests[0],
            TdRequest::GetChatHistory {
                chat_id: ChatId(1),
                from_message_id: MessageId(0),
                limit: 50,
                only_local: true,
            }
        ),
        "the opening request must be local-first: {:?}",
        history_requests[0]
    );
    assert!(
        matches!(
            history_requests[1],
            TdRequest::GetChatHistory {
                chat_id: ChatId(1),
                from_message_id: MessageId(0),
                limit: 50,
                only_local: false,
            }
        ),
        "the reconcile must go remote: {:?}",
        history_requests[1]
    );

    let ids = app.window_ids();
    assert_eq!(
        ids.len(),
        52,
        "the union of the local and reconciled pages, deduped: {ids:?}"
    );
    let mut deduped = ids.clone();
    deduped.dedup();
    assert_eq!(ids.len(), deduped.len(), "no duplicate ids: {ids:?}");
    assert!(ids.contains(&51), "the reconcile's new message is missing");
    assert!(ids.contains(&52), "the reconcile's new message is missing");
}

/// The other half of the local-first contract, and the scenario a cold start
/// (never-opened chat, or a freshly cleared cache) actually hits: TDLib's
/// local answer is empty. That is not proof of end-of-history (spec §5.2)
/// even on the very first page, where there is no previously-loaded message
/// id to retry from — the client must still fall back to a remote request,
/// this time from TDLib's "newest message" sentinel, and it must not spawn a
/// second reconcile once that remote page lands (it wasn't a local
/// completion).
#[tokio::test]
async fn empty_local_cache_falls_back_to_remote() {
    let mut app = Harness::new(&to_jsonl(&cold_cache_script()));
    app.open_top_chat().await;

    app.advance_until("the remote fallback page to land", |core, _| {
        core.app()
            .state()
            .conversations
            .get(&ChatId(1))
            .is_some_and(|c| !c.messages.is_empty())
    })
    .await;

    let history_requests: Vec<TdRequest> = app
        .requests()
        .into_iter()
        .filter(|r| matches!(r, TdRequest::GetChatHistory { .. }))
        .collect();
    assert_eq!(
        history_requests.len(),
        2,
        "the empty local attempt and its remote fallback, no reconcile after a \
         remote completion: {history_requests:?}"
    );
    assert!(
        matches!(
            history_requests[0],
            TdRequest::GetChatHistory {
                chat_id: ChatId(1),
                from_message_id: MessageId(0),
                only_local: true,
                ..
            }
        ),
        "{:?}",
        history_requests[0]
    );
    assert!(
        matches!(
            history_requests[1],
            TdRequest::GetChatHistory {
                chat_id: ChatId(1),
                from_message_id: MessageId(0),
                only_local: false,
                ..
            }
        ),
        "the empty-local retry must go remote, from the same newest-message \
         sentinel (nothing was ever loaded to anchor on): {:?}",
        history_requests[1]
    );

    assert_eq!(app.window_len(), 50);
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

fn chat(id: i64, title: &str, order: i64) -> ScriptStep {
    ScriptStep::Emit(TdUpdate::NewChat(ChatView {
        id: ChatId(id),
        kind: ChatKind::Private,
        title: title.to_string(),
        positions: vec![ChatPositionEntry {
            list: ChatListId::Main,
            order,
            is_pinned: false,
        }],
        unread_count: 0,
        unread_mention_count: 0,
        last_message: None,
        is_muted: false,
    }))
}

fn message(id: i64) -> MessageView {
    MessageView {
        id: MessageId(id),
        chat_id: ChatId(1),
        sender: Sender::User(UserId(if id % 2 == 0 { 42 } else { 7 })),
        sender_name: if id % 2 == 0 { "Ada" } else { "Bob" }.to_string(),
        is_outgoing: id % 2 == 0,
        date: 1_700_000_000 + id * 60,
        content: MessageContent::Text(FormattedText {
            text: format!("local-first line {id}"),
            entities: Vec::new(),
        }),
        reply_to: None,
        send_state: SendState::Sent,
        reactions: Vec::new(),
        caps: MessageCaps::default(),
        is_edited: false,
    }
}

fn page(ids: std::ops::RangeInclusive<i64>) -> RespondWith {
    RespondWith::Ok(TdResponse::Messages {
        messages: ids.map(message).collect(),
    })
}

fn ready_and_load_chats() -> Vec<ScriptStep> {
    vec![
        ScriptStep::Emit(TdUpdate::Auth(AuthPhase::Ready)),
        ScriptStep::Await {
            expect: expect("LoadChats"),
            respond: RespondWith::Ok(TdResponse::Ok),
        },
    ]
}

/// The restart scenario: TDLib's on-disk cache has the full 50-message page
/// (`only_local: true`, matched by `RequestMatcher::Kind` — the client's job
/// is to send that flag, not to make the fixture assert it; the assertions
/// in the test body read `FakeTd::received()` directly for that), then a
/// remote reconcile brings ids 3..=52: 48 already-seen ids plus two new,
/// newer ones.
fn local_first_script() -> Vec<ScriptStep> {
    let mut steps = ready_and_load_chats();
    steps.extend([
        chat(1, "Ada Lovelace", 100),
        ScriptStep::Await {
            expect: expect("GetChatHistory"),
            respond: page(1..=50),
        },
        ScriptStep::Await {
            expect: expect("GetChatHistory"),
            respond: page(3..=52),
        },
    ]);
    steps
}

/// The cold-cache scenario: a chat TDLib has never fetched history for (or a
/// freshly cleared local database) answers the local request with nothing —
/// the spec §5.2 trap, on the one request shape that has no prior message id
/// to retry from — and the remote fallback delivers the real page.
fn cold_cache_script() -> Vec<ScriptStep> {
    let mut steps = ready_and_load_chats();
    steps.extend([
        chat(1, "Ada Lovelace", 100),
        ScriptStep::Await {
            expect: expect("GetChatHistory"),
            respond: RespondWith::Ok(TdResponse::Messages {
                messages: Vec::new(),
            }),
        },
        ScriptStep::Await {
            expect: expect("GetChatHistory"),
            respond: page(1..=50),
        },
    ]);
    steps
}

fn on_disk_fixtures() -> [(&'static str, Vec<ScriptStep>); 1] {
    [("local_first.jsonl", local_first_script())]
}

/// The fixture is generated, not hand-written — same discipline as
/// `read_only.rs`'s `fixtures_on_disk_match_their_scripts`.
#[test]
fn fixtures_on_disk_match_their_scripts() {
    for (name, script) in on_disk_fixtures() {
        let text = read_fixture(name);
        FakeTd::from_jsonl(&text).unwrap_or_else(|e| panic!("{name} does not parse: {e}"));
        assert_eq!(
            text,
            to_jsonl(&script),
            "{name} is stale — run: cargo test -p tgt-app --test local_first \
             regenerate_fixtures -- --ignored"
        );
    }
}

/// Rewrites the fixture file from the script above. Manual, because it
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
