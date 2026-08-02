//! Full-app read-only client against `FakeTd` (docs/plan.md T24, spec §15.4):
//! logged in → the sidebar mirrors TDLib's order → Enter opens a chat and
//! loads its history → scrolling up pages further back, through the empty
//! response that is not end-of-history (spec §5.2, architecture §5.3).
//!
//! Same harness shape as `auth_flow.rs`: the real `runtime_loop::Core`, the
//! real dispatcher and the real `App::update`, with keys pushed in as
//! `crossterm` events and TDLib replaced by a scripted fixture. Frames are
//! drawn through `tgt_ui::view` into a `TestBackend` so what the assertions
//! read is the same buffer a terminal would show.
//!
//! `tgt-app` is a binary crate with no library target, so the modules under
//! test are included by path; the crate-level `allow(dead_code)` is for the
//! surface that comes along with them.
//!
//! # T59: opening a chat is local-first
//!
//! The opening `GetChatHistory` request is `only_local: true` — TDLib's
//! on-disk cache renders instantly instead of waiting behind TDLib's startup
//! server sync (design spec §5.2). A non-empty local completion always
//! follows up with exactly one remote reconcile
//! (`from_message_id: MessageId(0), only_local: false`) to pick up whatever
//! arrived while the app was closed; see `state::conversation::apply_history_page`.
//! Both tests below that open a chat now script an extra `Await` step for
//! that reconcile. The reconcile/loop-guard behavior itself (dedupe, "a
//! remote completion never spawns another reconcile") is covered in depth by
//! `crates/app/tests/local_first.rs` — the assertions added here only check
//! that this suite's two existing narratives (instant render, and the
//! scroll-triggered empty-response trap) still hold with the extra request
//! spliced in.

#![allow(dead_code)]

#[path = "../src/config.rs"]
mod config;
#[path = "../src/crash.rs"]
mod crash;
#[path = "../src/dispatch.rs"]
mod dispatch;
#[path = "../src/logging.rs"]
mod logging;
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
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use tokio::sync::mpsc;
use tokio::time::timeout;

use tgt_core::app::{App, AppState, Boot, Screen};
use tgt_core::effect::TelemetryMode;
use tgt_core::model::chat::{ChatKind, ChatListId, ChatPositionEntry, ChatView};
use tgt_core::model::entity::FormattedText;
use tgt_core::model::ids::{ChatId, MessageId, UserId};
use tgt_core::model::key::KeyBindings;
use tgt_core::model::message::{MessageCaps, MessageContent, MessageView, SendState, Sender};
use tgt_core::state::chat_list::visible_rows;
use tgt_core::state::conversation::Scroll;
use tgt_core::state::history::PagingState;
use tgt_core::td::fake::{FakeTd, RequestMatcher, RespondWith, ScriptStep};
use tgt_core::td::request::{TdRequest, TdResponse};
use tgt_core::td::runtime::TdRuntime;
use tgt_core::td::update::{AuthPhase, TdUpdate};
use tgt_ui::render::state::RenderState;
use tgt_ui::theme::Theme;

use config::Config;
use dispatch::TdBootParams;
use runtime_loop::Core;

/// Ceiling for any single "advance until" wait; every step is driven by a
/// channel or the 250 ms tick, so this only bounds a hang.
/// A guard against a genuinely stuck loop, not a performance assertion.
/// `cargo test --workspace` runs every integration binary concurrently, so a
/// tight wall-clock bound here fails under load rather than on a real bug.
const SETTLE_TIMEOUT: Duration = Duration::from_secs(30);

/// Title the position storm renames its last-touched chat to; see
/// [`position_storm_script`].
const STORM_SETTLED: &str = "Devon";

/// Badge the T72 chat carries before anything is read (see
/// [`unread_chat_script`]).
const UNREAD_ON_OPEN: u32 = 3;

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
                database_directory: PathBuf::from("/tmp/tgt-read-only-db"),
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

    fn requests(&self) -> Vec<&'static str> {
        self.fake.received().iter().map(TdRequest::kind).collect()
    }

    /// Loaded messages in the open chat's window (0 when no chat is open).
    fn window_len(&self) -> usize {
        self.state()
            .open_chat
            .and_then(|id| self.state().conversations.get(&id))
            .map(|c| c.messages.len())
            .unwrap_or(0)
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

    /// Draws one frame exactly as the binary does and flattens it into one
    /// string per row, so a plain `contains` can look for rendered text.
    fn render(&self, width: u16, height: u16) -> String {
        let theme = Theme::default_dark();
        let mut rs = RenderState::new(None);
        let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("test backend");
        terminal
            .draw(|f| {
                tgt_ui::view(self.state(), &theme, f, &mut rs);
            })
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
        crash_reports_available: false,
        telemetry_salt: [0u8; 32],
        consent_needed: false,
        has_credentials: true,
        width: 120,
        height: 40,
        auto_download_photos: true,
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

/// Ordering is TDLib's alone (spec §5.1). The storm below reshuffles five
/// chats out of the order they arrived in, lands two of them on the *same*
/// order (which TDLib breaks by chat id, descending), and drops one out of
/// the list with `order: 0` — the update TDLib sends for "no longer in this
/// list", not a deletion.
#[tokio::test]
async fn chat_list_matches_tdlib_order_after_position_storm() {
    let mut app = Harness::new(&to_jsonl(&position_storm_script()));

    app.advance_until("the whole storm to be applied", |core, _| {
        core.app()
            .state()
            .chat_list
            .chats
            .get(&ChatId(4))
            .is_some_and(|c| c.title == STORM_SETTLED)
    })
    .await;

    assert_eq!(app.state().screen, Screen::Main);
    assert_eq!(
        visible_rows(&app.state().chat_list),
        vec![ChatId(4), ChatId(3), ChatId(1), ChatId(2)],
        "expected order 100 (ids 4, 3, 1 — descending on the tie) then 55, \
         with chat 5 gone at order 0"
    );

    // The order is what the sidebar draws, top to bottom.
    let rendered = app.render(120, 40);
    let sidebar: Vec<&str> = rendered
        .lines()
        .filter_map(|line| {
            [STORM_SETTLED, "Cid", "Ada", "Bob", "Eve"]
                .into_iter()
                .find(|title| line.contains(title))
        })
        .collect();
    assert_eq!(
        sidebar,
        vec![STORM_SETTLED, "Cid", "Ada", "Bob"],
        "sidebar rows disagree with visible_rows:\n{rendered}"
    );
}

/// T72: opening a chat with unread messages tells TDLib they were seen, and
/// the badge clears because TDLib says so.
///
/// The bug this pins: nothing ever emitted `ViewMessages`, so the unread
/// count stayed on the sidebar and the chat stayed bold on the user's other
/// devices no matter how long they read it here.
///
/// Both halves matter. The receipt has to reach TDLib (`FakeTd::received()`),
/// and the badge has to stay at whatever TDLib last said until TDLib says
/// otherwise — a client that zeroed `unread_count` locally would look fixed
/// on this screen while every other client kept showing the chat unread.
#[tokio::test]
async fn opening_an_unread_chat_marks_it_read_and_lets_tdlib_clear_the_badge() {
    let mut app = Harness::new(&to_jsonl(&unread_chat_script()));
    app.open_top_chat().await;

    // Opening it changes nothing about the badge on its own: the window is
    // still empty here, so there is not even anything to report as seen yet.
    assert_eq!(
        app.state().chat_list.chats[&ChatId(1)].unread_count,
        UNREAD_ON_OPEN,
        "the badge is TDLib's to clear"
    );

    app.advance_until("the read receipt to reach TDLib", |_, fake| {
        fake.received()
            .iter()
            .any(|r| matches!(r, TdRequest::ViewMessages { .. }))
    })
    .await;

    let viewed: Vec<MessageId> = app
        .fake
        .received()
        .iter()
        .find_map(|r| match r {
            TdRequest::ViewMessages {
                chat_id,
                message_ids,
            } if *chat_id == ChatId(1) => Some(message_ids.clone()),
            _ => None,
        })
        .expect("a ViewMessages for the open chat");
    assert!(
        viewed.iter().all(|id| id.0 % 2 == 1),
        "the user's own messages are never marked read: {viewed:?}"
    );
    assert_eq!(
        viewed.last(),
        Some(&MessageId(49)),
        "the newest loaded incoming message is the watermark: {viewed:?}"
    );

    // And now the half that is TDLib's: the update its answer produces is
    // what takes the badge to zero, in the sidebar and in the window's read
    // marker alike.
    app.advance_until("TDLib to report the chat read", |core, _| {
        core.app().state().chat_list.chats[&ChatId(1)].unread_count == 0
    })
    .await;
    assert_eq!(
        app.state().conversations[&ChatId(1)].last_read_inbox,
        MessageId(49)
    );
}

/// Enter on a selected row opens the chat in TDLib (so its updates start
/// flowing) and asks for the newest page of history; the page that comes back
/// is what the viewport renders.
#[tokio::test]
async fn open_chat_loads_history_and_renders() {
    let mut app = Harness::new(&read_fixture("read_only.jsonl"));
    app.open_top_chat().await;

    app.advance_until("the first page to land", |core, _| {
        core.app()
            .state()
            .conversations
            .get(&ChatId(1))
            .is_some_and(|c| !c.messages.is_empty())
    })
    .await;

    let received = app.fake.received();
    assert!(
        received
            .iter()
            .any(|r| matches!(r, TdRequest::OpenChat { chat_id } if *chat_id == ChatId(1))),
        "opening a chat must tell TDLib about it: {:?}",
        app.requests()
    );
    assert!(
        received.iter().any(|r| matches!(
            r,
            TdRequest::GetChatHistory {
                chat_id,
                from_message_id: MessageId(0),
                limit: 50,
                only_local: true,
            } if *chat_id == ChatId(1)
        )),
        "the first page is requested from the newest message (id 0 is TDLib's \
         sentinel), local-first (T59, design spec §5.2): {received:?}"
    );

    // T59: the non-empty local page must trigger exactly one remote
    // reconcile. Waited for via the request count rather than a state change
    // — this fixture's reconcile response is a no-op page (see
    // `read_only_script`), so nothing in `AppState` moves when it lands, but
    // `FakeTd::received()` records the request the instant its spawned task
    // is first polled, same as every other effect this harness waits on.
    app.advance_until("the T59 remote reconcile to be sent", |_, fake| {
        fake.received()
            .iter()
            .filter(|r| matches!(r, TdRequest::GetChatHistory { .. }))
            .count()
            >= 2
    })
    .await;
    let received = app.fake.received();
    assert!(
        received.iter().any(|r| matches!(
            r,
            TdRequest::GetChatHistory {
                chat_id,
                from_message_id: MessageId(0),
                limit: 50,
                only_local: false,
            } if *chat_id == ChatId(1)
        )),
        "a local-first open must reconcile with exactly one remote request \
         (T59): {received:?}"
    );

    assert_eq!(app.window_len(), 50);
    assert_eq!(app.state().open_chat, Some(ChatId(1)));

    // The newest message sits at the bottom of the viewport.
    let rendered = app.render(120, 40);
    assert!(
        rendered.contains("history line 50"),
        "the newest message is missing from the frame:\n{rendered}"
    );
    assert!(
        rendered.contains("Ada Lovelace"),
        "the opened chat is missing from the sidebar:\n{rendered}"
    );
}

/// The viewport feature's one wire into `update()`, end to end.
///
/// `HitMap::visible_messages` and `viewport_report` are each unit-tested
/// against hand-built maps, and `view::conversation` holds a rendered frame
/// against the range its own hit map reports. None of that touches the wire:
/// deleting `draw_if_due`'s `viewport_report` block left the entire
/// workspace green, so the feature could have been inert and nothing would
/// have said so. `Harness::render` cannot catch it either — it calls
/// `tgt_ui::view` into a scratch `HitMap` and throws it away, which is the
/// whole path under test.
///
/// So this drives the real `draw_if_due` (via [`Core::draw_once`]) against a
/// `TestBackend` and asserts `update()` was told what the frame drew. It
/// matters more than its size suggests: nothing on this branch has been run
/// against a live app, and this is the only thing distinguishing "the
/// viewport feature works" from "the viewport feature is inert".
#[tokio::test]
async fn a_drawn_frame_reports_its_viewport_into_update() {
    let mut app = Harness::new(&read_fixture("read_only.jsonl"));
    app.open_top_chat().await;
    app.advance_until("the first page to land", |core, _| {
        core.app()
            .state()
            .conversations
            .get(&ChatId(1))
            .is_some_and(|c| !c.messages.is_empty())
    })
    .await;

    assert_eq!(
        app.state().visible_messages,
        None,
        "nothing has been drawn through the loop yet"
    );

    let mut terminal = Terminal::new(TestBackend::new(120, 40)).expect("test backend");
    app.core.draw_once(&mut terminal).expect("draw");

    let (first, last) = app
        .state()
        .visible_messages
        .expect("a drawn frame owes `update()` the range it drew");
    assert!(first <= last);

    // The reported ids must be messages the window actually holds, and the
    // range must be a viewport rather than the whole loaded window — 50
    // messages do not fit in 40 rows.
    let window = &app.state().conversations[&ChatId(1)];
    for id in [first, last] {
        assert!(
            window.messages.iter().any(|m| m.id == id),
            "{id:?} is not a loaded message; window is {:?}..={:?}",
            window.messages.front().map(|m| m.id),
            window.messages.back().map(|m| m.id),
        );
    }
    assert_eq!(
        window.messages.back().map(|m| m.id),
        Some(last),
        "the chat opens pinned to the bottom, so the newest loaded message \
         is the last one drawn"
    );
    assert!(
        first > window.messages.front().unwrap().id,
        "a 40-row frame cannot be showing all {} loaded messages; the \
         reported range is the viewport, not the window",
        window.messages.len()
    );

    // Redrawing the same frame reports nothing new, which is what keeps the
    // loop from driving itself round for ever.
    let before = app.state().visible_messages;
    app.core.draw_once(&mut terminal).expect("redraw");
    assert_eq!(app.state().visible_messages, before);
}

/// The spec §5.2 trap, end to end: `getChatHistory` answers a scroll-triggered
/// page with zero messages even though older history exists. An empty response
/// is never proof of end-of-history — the client re-issues the request (up to
/// `MAX_EMPTY_ATTEMPTS`) and the next round delivers the page.
///
/// The chat opens with a *full* page. It used to open with five messages,
/// which put the very first `PageUp` inside `PAGE_TRIGGER_MESSAGES` of the
/// oldest loaded one; T67 made that shape mean something else entirely — a
/// window that short is one the client now fills on its own, and its fill
/// request would be the one answered with the empty page below, so the
/// scroll-triggered round this test is about would never see the trap. A full
/// opening page is under no such policy (see
/// `conversation::VIEWPORT_FILL_TARGET_MESSAGES`), so every request here is
/// still one this test asked for; the walk down to the trigger window just
/// takes four `PageUp`s instead of one.
#[tokio::test]
async fn empty_history_response_retries_then_succeeds() {
    let mut app = Harness::new(&to_jsonl(&empty_then_page_script()));
    app.open_top_chat().await;

    app.advance_until("the opening page to land", |core, _| {
        core.app()
            .state()
            .conversations
            .get(&ChatId(1))
            .is_some_and(|c| c.messages.len() == 50)
    })
    .await;

    // T59: the non-empty local opening page triggers an automatic remote
    // reconcile. This fixture answers it with nothing (`empty_then_page_script`)
    // to prove the *other* half of the loop guard: an empty response to a
    // request that was itself remote (`only_local: false`) must not be
    // mistaken for the scroll-triggered empty-response trap this test is
    // about. `apply_history_page` only ever acts on a completion while
    // `paging` is `Loading` — and `paging` is back to `Idle` by the time this
    // reconcile fires (the local completion already returned it there) — so
    // the machine's stale-completion branch ignores it outright: no retry,
    // no `Exhausted`, and nothing added to the window.
    app.advance_until("the T59 remote reconcile (empty) to land", |_, fake| {
        fake.received()
            .iter()
            .filter(|r| matches!(r, TdRequest::GetChatHistory { .. }))
            .count()
            >= 2
    })
    .await;
    assert_eq!(
        app.state().conversations[&ChatId(1)].paging,
        PagingState::Idle,
        "an empty *remote* reconcile completion must not disturb paging state"
    );
    assert_eq!(app.window_len(), 50, "the empty reconcile added nothing");

    // Scroll off the bottom until the anchor lands inside the paging window,
    // which asks for the page before the oldest loaded message. `PageUp`
    // rather than `Up` since T28 wired the §6.2 routing table — with the
    // composer focused, `Up` on an empty input enters selection mode, and the
    // page keys are what reach the viewport from there. Four of them: a press
    // steps `PAGE_STEP_MESSAGES` (10) through a window of 50, and the trigger
    // is the first press that lands within `PAGE_TRIGGER_MESSAGES` (20) of the
    // oldest — indices 40, 30, 20, then 10.
    for _ in 0..4 {
        app.press(KeyCode::PageUp).await;
    }

    app.advance_until("the retried page to land", |core, _| {
        core.app()
            .state()
            .conversations
            .get(&ChatId(1))
            .is_some_and(|c| c.messages.len() > 50)
    })
    .await;

    let history_requests: Vec<TdRequest> = app
        .fake
        .received()
        .into_iter()
        .filter(|r| matches!(r, TdRequest::GetChatHistory { .. }))
        .collect();
    assert_eq!(
        history_requests.len(),
        4,
        "expected the opening page, its T59 remote reconcile, the empty \
         scroll-triggered round and its retry: {history_requests:?}"
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
        "the opening request must be local-first (T59): {:?}",
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
        "the T59 reconcile must go remote: {:?}",
        history_requests[1]
    );
    // Both scroll-triggered paging requests start from the same
    // oldest-loaded message: the empty response moved nothing, so the retry
    // asks for the same page.
    for request in &history_requests[2..] {
        assert!(
            matches!(
                request,
                TdRequest::GetChatHistory {
                    chat_id: ChatId(1),
                    from_message_id: MessageId(51),
                    only_local: false,
                    ..
                }
            ),
            "a retry must re-ask remotely from the oldest loaded message, got {request:?}"
        );
    }

    let convo = &app.state().conversations[&ChatId(1)];
    assert_eq!(
        convo.messages.len(),
        100,
        "the 50-message page was prepended"
    );
    assert_eq!(convo.messages.front().unwrap().id, MessageId(1));
    assert_eq!(convo.messages.back().unwrap().id, MessageId(100));

    // One more press walks the anchor into the page that just arrived (the
    // viewport is drawn upward from the anchor, so what proves the page
    // rendered is a message just *above* it). It asks for nothing: at index
    // 50 of 100 the anchor is nowhere near the top any more.
    app.press(KeyCode::PageUp).await;
    app.advance_until("the anchor to step into the new page", |core, _| {
        core.app().state().conversations[&ChatId(1)].scroll
            == Scroll::At {
                message_id: MessageId(51),
                line_offset: 0,
            }
    })
    .await;

    let rendered = app.render(120, 40);
    assert!(
        rendered.contains("history line 50"),
        "the paged-in history is missing from the frame:\n{rendered}"
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

fn chat(id: i64, title: &str, order: i64) -> ScriptStep {
    ScriptStep::Emit(TdUpdate::NewChat(ChatView {
        id: ChatId(id),
        kind: ChatKind::Private,
        title: title.to_string(),
        positions: vec![position(order)],
        unread_count: 0,
        unread_mention_count: 0,
        last_message: None,
        is_muted: false,
    }))
}

fn position(order: i64) -> ChatPositionEntry {
    ChatPositionEntry {
        list: ChatListId::Main,
        order,
        is_pinned: false,
    }
}

fn reorder(id: i64, order: i64) -> ScriptStep {
    ScriptStep::Emit(TdUpdate::ChatPosition {
        chat_id: ChatId(id),
        position: position(order),
    })
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
            text: format!("history line {id}"),
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

/// Logged in already: TDLib restores an authorized session from its database,
/// so `Ready` is the first update of the run and no credentials round trip
/// happens. `LoadChats` is what `Ready` produces (that path is `auth_flow`'s).
fn ready_and_load_chats() -> Vec<ScriptStep> {
    vec![
        ScriptStep::Emit(TdUpdate::Auth(AuthPhase::Ready)),
        ScriptStep::Await {
            expect: expect("LoadChats"),
            respond: RespondWith::Ok(TdResponse::Ok),
        },
    ]
}

fn position_storm_script() -> Vec<ScriptStep> {
    let mut steps = ready_and_load_chats();
    steps.extend([
        chat(1, "Ada", 100),
        chat(2, "Bob", 90),
        chat(3, "Cid", 80),
        chat(4, "Dev", 70),
        chat(5, "Eve", 60),
        // Cid jumps to the top, tying Ada's order.
        reorder(3, 100),
        // Eve leaves the main list.
        reorder(5, 0),
        // Bob drops below everyone.
        reorder(2, 55),
        // Dev joins the tie at the top.
        reorder(4, 100),
        // Updates are applied in the order they arrive, so a rename of the
        // last chat the storm touched is a marker the test can wait for:
        // seeing it means every position above has already been applied.
        ScriptStep::Emit(TdUpdate::ChatTitle {
            chat_id: ChatId(4),
            title: STORM_SETTLED.to_string(),
        }),
    ]);
    steps
}

/// The on-disk read-only session: one chat with a full 50-message page, plus
/// (T59) the automatic remote reconcile that follows it — scripted here as a
/// no-op (the same 50 messages again) since this test's narrative is about
/// the opening render, not the reconcile's merge behavior (see
/// `local_first.rs` for that).
fn read_only_script() -> Vec<ScriptStep> {
    let mut steps = ready_and_load_chats();
    steps.extend([
        chat(1, "Ada Lovelace", 100),
        chat(2, "Bob", 90),
        ScriptStep::Await {
            expect: expect("GetChatHistory"),
            respond: page(1..=50),
        },
        ScriptStep::Await {
            expect: expect("GetChatHistory"),
            respond: page(1..=50),
        },
    ]);
    steps
}

fn empty_then_page_script() -> Vec<ScriptStep> {
    let mut steps = ready_and_load_chats();
    steps.extend([
        chat(1, "Ada Lovelace", 100),
        // The opening page (T59: local-first, `only_local: true`). A full
        // page, so T67's viewport fill has nothing to do — see the test's
        // doc comment.
        ScriptStep::Await {
            expect: expect("GetChatHistory"),
            respond: page(51..=100),
        },
        // T59: the automatic remote reconcile the non-empty opening page
        // triggers. Answered with nothing on purpose — an empty *remote*
        // reconcile must not be mistaken for the scroll-triggered trap below
        // (see the assertion in the test body).
        ScriptStep::Await {
            expect: expect("GetChatHistory"),
            respond: RespondWith::Ok(TdResponse::Messages {
                messages: Vec::new(),
            }),
        },
        // The trap: TDLib has more history but answers the scroll-triggered
        // page with nothing (spec §5.2).
        ScriptStep::Await {
            expect: expect("GetChatHistory"),
            respond: RespondWith::Ok(TdResponse::Messages {
                messages: Vec::new(),
            }),
        },
        // The retry the client is obliged to make.
        ScriptStep::Await {
            expect: expect("GetChatHistory"),
            respond: page(1..=50),
        },
    ]);
    steps
}

fn on_disk_fixtures() -> [(&'static str, Vec<ScriptStep>); 1] {
    [("read_only.jsonl", read_only_script())]
}

/// T72: a chat that opens with unread messages. The `ViewMessages` step is
/// the gate the read-inbox update hangs off — TDLib clears the badge
/// *because* the client said the messages were seen, so scripting it that way
/// is what makes the test able to tell the fix from a client that zeroes the
/// count on its own.
fn unread_chat_script() -> Vec<ScriptStep> {
    let mut steps = ready_and_load_chats();
    steps.extend([
        ScriptStep::Emit(TdUpdate::NewChat(ChatView {
            id: ChatId(1),
            kind: ChatKind::Private,
            title: "Ada Lovelace".to_string(),
            positions: vec![position(100)],
            unread_count: UNREAD_ON_OPEN,
            unread_mention_count: 0,
            last_message: None,
            is_muted: false,
        })),
        // The opening page. Half of it is the user's own messages
        // (`message()` makes even ids outgoing), which must never appear in
        // the read receipt.
        ScriptStep::Await {
            expect: expect("GetChatHistory"),
            respond: page(1..=50),
        },
        ScriptStep::Await {
            expect: expect("ViewMessages"),
            respond: RespondWith::Ok(TdResponse::Ok),
        },
        // TDLib's answer to having been told: the watermark moves and the
        // badge goes to zero. This is the only thing in the run that clears
        // it. (T59's remote reconcile is deliberately unscripted — it lands
        // on `FakeTd`'s default `Ok`, which the history completion reads as
        // an empty page and the paging machine ignores as stale.)
        ScriptStep::Emit(TdUpdate::ChatReadInbox {
            chat_id: ChatId(1),
            last_read_inbox_message_id: MessageId(49),
            unread_count: 0,
        }),
    ]);
    steps
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
            "{name} is stale — run: cargo test -p tgt-app --test read_only \
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
