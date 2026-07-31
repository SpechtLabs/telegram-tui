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
use tgt_core::state::history::PagingState;
use tgt_core::td::fake::{FakeTd, RequestMatcher, RespondWith, ScriptStep};
use tgt_core::td::request::{TdRequest, TdResponse};
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

/// Title the position storm renames its last-touched chat to; see
/// [`position_storm_script`].
const STORM_SETTLED: &str = "Devon";

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
        let mut cache = LayoutCache::new();
        let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("test backend");
        terminal
            .draw(|f| {
                tgt_ui::view(self.state(), &theme, f, &mut cache);
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

/// The spec §5.2 trap, end to end: `getChatHistory` answers a scroll-triggered
/// page with zero messages even though older history exists. An empty response
/// is never proof of end-of-history — the client re-issues the request (up to
/// `MAX_EMPTY_ATTEMPTS`) and the next round delivers the page.
///
/// The chat is opened with a deliberately short first page: paging only
/// triggers once the scroll anchor is within `PAGE_TRIGGER_MESSAGES` of the
/// oldest loaded message, and five messages put the very first `↑` inside that
/// window.
#[tokio::test]
async fn empty_history_response_retries_then_succeeds() {
    let mut app = Harness::new(&to_jsonl(&empty_then_page_script()));
    app.open_top_chat().await;

    app.advance_until("the opening page to land", |core, _| {
        core.app()
            .state()
            .conversations
            .get(&ChatId(1))
            .is_some_and(|c| c.messages.len() == 5)
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
    assert_eq!(app.window_len(), 5, "the empty reconcile added nothing");

    // Scroll off the bottom: the anchor lands inside the paging window and
    // asks for the page before the oldest loaded message. `PageUp` rather
    // than `Up` since T28 wired the §6.2 routing table — with the composer
    // focused, `Up` on an empty input enters selection mode, and the page
    // keys are what reach the viewport from there.
    app.press(KeyCode::PageUp).await;

    app.advance_until("the retried page to land", |core, _| {
        core.app()
            .state()
            .conversations
            .get(&ChatId(1))
            .is_some_and(|c| c.messages.len() > 5)
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
                    from_message_id: MessageId(101),
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
        55,
        "the 50-message page was prepended"
    );
    assert_eq!(convo.messages.front().unwrap().id, MessageId(51));
    assert_eq!(convo.messages.back().unwrap().id, MessageId(105));

    let rendered = app.render(120, 40);
    assert!(
        rendered.contains("history line 100"),
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
        // The opening page (T59: local-first, `only_local: true`).
        ScriptStep::Await {
            expect: expect("GetChatHistory"),
            respond: page(101..=105),
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
            respond: page(51..=100),
        },
    ]);
    steps
}

fn on_disk_fixtures() -> [(&'static str, Vec<ScriptStep>); 1] {
    [("read_only.jsonl", read_only_script())]
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
