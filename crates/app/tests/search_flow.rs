//! Full-app palette and in-chat search flows against `FakeTd` (docs/plan.md
//! T48, spec §11): `ctrl+p` → fuzzy fragment → `⏎` opens the chat it ranked
//! first, and `/` → query → `⏎` → `n` walks onto a hit TDLib found outside
//! the loaded window, which the next scroll pages in.
//!
//! Same harness shape as `send_flow.rs` and `media_flow.rs`: the real
//! `runtime_loop::Core`, the real dispatcher and the real `App::update`, with
//! keys pushed in as `crossterm` events and TDLib replaced by a scripted
//! fixture.
//!
//! # Where the off-window hit's history page comes from
//!
//! `searchChatMessages` answers with message ids from anywhere in the chat's
//! history, so a hit is routinely older than the page or two the window
//! holds. Stepping onto one (`state::search`'s `n`) only moves the scroll
//! anchor — by design, T42 leaves paging to `state::conversation`, which
//! re-evaluates it whenever the anchor moves under a scroll key. Two things
//! follow, and this file asserts both:
//!
//! - Scroll keys are not routed while `Focus::ChatSearch` is on top (they
//!   belong to the composer and selection mode, `app.rs`'s step 5), so
//!   nothing is fetched until the user leaves the overlay. `esc` closes
//!   search and drops the hit list, but the anchor it left behind survives —
//!   that is what the following `page-up` acts on.
//! - That scroll finds an anchor pointing outside the window and asks for
//!   the page containing it rather than snapping back to the newest message
//!   (`conversation::trigger_paging_if_near_top`).
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

use tgt_core::app::{App, AppState, Boot};
use tgt_core::effect::TelemetryMode;
use tgt_core::model::chat::{ChatKind, ChatListId, ChatPositionEntry, ChatView};
use tgt_core::model::entity::FormattedText;
use tgt_core::model::ids::{ChatId, MessageId, UserId};
use tgt_core::model::key::KeyBindings;
use tgt_core::model::message::{MessageCaps, MessageContent, MessageView, SendState, Sender};
use tgt_core::state::chat_list::visible_rows;
use tgt_core::state::conversation::{ConversationState, Scroll};
use tgt_core::state::focus::Focus;
use tgt_core::state::palette::PaletteItem;
use tgt_core::td::fake::{FakeTd, RequestMatcher, RespondWith, ScriptStep};
use tgt_core::td::request::{TdRequest, TdResponse};
use tgt_core::td::runtime::TdRuntime;
use tgt_core::td::update::{AuthPhase, ConnectionPhase, TdUpdate};
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

/// Three chats, so "the palette found the right one" is a real claim rather
/// than the only possible answer. Ordered by TDLib position, most recent
/// first — the order the palette breaks score ties on.
const ADA: ChatId = ChatId(1);
const STANDUP: ChatId = ChatId(2);
const GRACE: ChatId = ChatId(3);

/// The window `ADA` opens with: ten recent messages.
const WINDOW: std::ops::RangeInclusive<i64> = 121..=130;
/// The page behind it, which only a `getChatHistory` reaches.
const OLDER_PAGE: std::ops::RangeInclusive<i64> = 111..=120;

/// The two search hits. One is in the opened window; the other is in
/// `OLDER_PAGE`, i.e. nothing the client has seen when `n` jumps to it.
const NEAR_HIT: MessageId = MessageId(128);
const OFFSCREEN_HIT: MessageId = MessageId(115);

/// `GRACE`'s history, so opening it from the palette lands on real content.
const GRACE_HISTORY: std::ops::RangeInclusive<i64> = 201..=203;

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
                database_directory: PathBuf::from("/tmp/tgt-search-flow-db"),
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
        self.send(code, KeyModifiers::NONE).await;
    }

    /// `ctrl+p` is the palette binding (`KeyBindings::default`), and it is
    /// the modifier that makes it one — `p` alone is a character.
    async fn press_ctrl(&self, code: KeyCode) {
        self.send(code, KeyModifiers::CONTROL).await;
    }

    async fn send(&self, code: KeyCode, modifiers: KeyModifiers) {
        let event = Event::Key(KeyEvent {
            code,
            modifiers,
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
            "timed out waiting for {what}\n  focus: {:?}\n  open chat: {:?}\n  scroll: {:?}\n  window: {:?}\n  requests: {:?}",
            self.state().focus.current(),
            self.state().open_chat,
            self.state()
                .open_chat
                .and_then(|id| self.state().conversations.get(&id))
                .map(|c| c.scroll),
            self.window_ids(ADA),
            self.requests(),
        );
    }

    fn state(&self) -> &AppState {
        self.core.app().state()
    }

    fn requests(&self) -> Vec<&'static str> {
        self.fake.received().iter().map(TdRequest::kind).collect()
    }

    fn request_count(&self, kind: &str) -> usize {
        self.requests().iter().filter(|k| **k == kind).count()
    }

    fn convo(&self, chat_id: ChatId) -> &ConversationState {
        self.state()
            .conversations
            .get(&chat_id)
            .expect("the chat is open")
    }

    fn window_ids(&self, chat_id: ChatId) -> Vec<i64> {
        self.state()
            .conversations
            .get(&chat_id)
            .map(|c| c.messages.iter().map(|m| m.id.0).collect())
            .unwrap_or_default()
    }

    /// Opens `ADA` — the top sidebar row — and waits for its first page, the
    /// state the search test starts from.
    async fn open_top_chat_with_history(&mut self) {
        self.advance_until("the sidebar to fill", |core, _| {
            visible_rows(&core.app().state().chat_list).len() == 3
        })
        .await;

        self.press(KeyCode::Down).await;
        self.advance_until("the top row to be selected", |core, _| {
            core.app().state().chat_list.selected == Some(ADA)
        })
        .await;

        self.press(KeyCode::Enter).await;
        self.advance_until("the first page to land", |core, _| {
            core.app()
                .state()
                .conversations
                .get(&ADA)
                .is_some_and(|c| c.messages.len() == WINDOW.count())
        })
        .await;

        assert_eq!(
            *self.state().focus.current(),
            Focus::Composer,
            "opening a chat leaves the conversation side focused (spec §6.2)"
        );
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

/// Spec §11, the palette end to end: it is a second door into a conversation.
/// A fuzzy fragment that is nobody's prefix ranks `Grace Hopper` first, `⏎`
/// opens it with the same two requests the sidebar's `⏎` would issue, and the
/// overlay gets out of the way — the focus stack ends one level deep, on the
/// chat that was just opened.
#[tokio::test]
async fn palette_opens_chat_by_fuzzy_match() {
    let mut app = Harness::new(&to_jsonl(&palette_script()));
    app.advance_until("the sidebar to fill", |core, _| {
        visible_rows(&core.app().state().chat_list).len() == 3
    })
    .await;
    assert!(app.state().open_chat.is_none());

    app.press_ctrl(KeyCode::Char('p')).await;
    app.advance_until("the palette to open", |core, _| {
        core.app().state().palette.is_some()
    })
    .await;
    assert_eq!(*app.state().focus.current(), Focus::Palette);

    // The empty query lists every chat, so the overlay is on screen and the
    // narrowing below is visible rather than inferred.
    let rendered = app.render(120, 40);
    assert!(
        rendered.contains("palette"),
        "the palette overlay never reached the frame:\n{rendered}"
    );
    assert!(rendered.contains("Grace Hopper"), "{rendered}");

    // `grho` is a subsequence of "Grace Hopper" and of neither other chat
    // title nor any command label.
    app.type_text("grho").await;
    app.advance_until("the query to rank", |core, _| {
        core.app()
            .state()
            .palette
            .as_ref()
            .is_some_and(|p| p.input.text == "grho")
    })
    .await;

    let results = &app.state().palette.as_ref().unwrap().results;
    assert!(
        matches!(results.first(), Some(PaletteItem::Chat { id, .. }) if *id == GRACE),
        "fuzzy fragment did not rank the chat first: {results:?}"
    );
    assert_eq!(
        results.len(),
        1,
        "the fragment should match nothing else: {results:?}"
    );

    app.press(KeyCode::Enter).await;
    app.advance_until("the chat to open", |core, _| {
        core.app().state().open_chat == Some(GRACE)
    })
    .await;

    app.advance_until("its history to land", |core, _| {
        core.app()
            .state()
            .conversations
            .get(&GRACE)
            .is_some_and(|c| c.messages.len() == GRACE_HISTORY.count())
    })
    .await;

    // Invoking a chat is `⏎` on a sidebar row by another route: the same two
    // requests, and the conversation side focused.
    assert!(
        app.requests().contains(&"OpenChat"),
        "requests: {:?}",
        app.requests()
    );

    assert!(app.state().palette.is_none(), "the palette must close");
    assert_eq!(*app.state().focus.current(), Focus::Composer);
    assert_eq!(
        app.state().focus.depth(),
        1,
        "the palette level must be popped, not left under the conversation"
    );

    let rendered = app.render(120, 40);
    assert!(
        !rendered.contains("› grho"),
        "the palette is still drawn after closing:\n{rendered}"
    );
    assert!(
        rendered.contains("compiler bug reproduced"),
        "the opened chat's history is not on screen:\n{rendered}"
    );
}

/// Spec §11, in-chat search end to end. The hits TDLib finds are ids, not
/// messages: one of them here is older than everything loaded, so stepping
/// onto it anchors the viewport at a message the client does not have. The
/// next scroll evaluation is what turns that into a `getChatHistory` — see
/// the module docs for why it takes a scroll and not the step itself.
#[tokio::test]
async fn search_step_to_offscreen_hit_pages_history() {
    let mut app = Harness::new(&read_fixture("search_flow.jsonl"));
    app.open_top_chat_with_history().await;
    assert_eq!(app.window_ids(ADA), WINDOW.collect::<Vec<_>>());

    // `/` is in-chat search only while the message list is focused, which is
    // selection mode (`app.rs`'s routing table); `↑` on the empty composer is
    // how the user gets there.
    app.press(KeyCode::Up).await;
    app.advance_until("selection mode", |core, _| {
        core.app().state().conversations[&ADA].selection.is_some()
    })
    .await;

    app.press(KeyCode::Char('/')).await;
    app.advance_until("the search overlay", |core, _| {
        core.app().state().chat_search.is_some()
    })
    .await;
    assert_eq!(*app.state().focus.current(), Focus::ChatSearch);

    app.type_text("budget").await;
    app.press(KeyCode::Enter).await;
    app.advance_until("the hits to arrive", |core, _| {
        !core.app().state().conversations[&ADA]
            .search_hits
            .is_empty()
    })
    .await;

    assert_eq!(
        app.convo(ADA).search_hits,
        vec![NEAR_HIT, OFFSCREEN_HIT],
        "hits are stored in TDLib's newest-first order, verbatim"
    );
    assert_eq!(
        app.convo(ADA).scroll,
        Scroll::At {
            message_id: NEAR_HIT,
            line_offset: 0,
        },
        "the answer anchors on the first hit"
    );
    let rendered = app.render(120, 40);
    assert!(
        rendered.contains("1/2"),
        "the header should count the hits:\n{rendered}"
    );

    // The step onto the off-window hit. The anchor moves to a message id the
    // window does not contain, and nothing is fetched yet: scroll keys are
    // not routed while the search overlay owns the focus.
    let pages_before = app.request_count("GetChatHistory");
    app.press(KeyCode::Char('n')).await;
    app.advance_until("the anchor to reach the second hit", |core, _| {
        core.app().state().conversations[&ADA].scroll
            == Scroll::At {
                message_id: OFFSCREEN_HIT,
                line_offset: 0,
            }
    })
    .await;
    assert!(
        !app.window_ids(ADA).contains(&OFFSCREEN_HIT.0),
        "the hit is supposed to be outside the loaded window: {:?}",
        app.window_ids(ADA)
    );
    assert_eq!(
        app.request_count("GetChatHistory"),
        pages_before,
        "stepping alone must not fetch — that is the scroll's job"
    );

    // Leaving search keeps the anchor it left behind (only the hit list goes
    // with it), and the first scroll from there asks for the page the anchor
    // points into instead of snapping back to the newest message.
    app.press(KeyCode::Esc).await;
    app.advance_until("search to close", |core, _| {
        core.app().state().chat_search.is_none()
    })
    .await;
    assert_eq!(*app.state().focus.current(), Focus::Composer);
    assert_eq!(
        app.convo(ADA).scroll,
        Scroll::At {
            message_id: OFFSCREEN_HIT,
            line_offset: 0,
        },
        "closing search must not move the viewport off the hit"
    );

    app.press(KeyCode::PageUp).await;
    app.advance_until("the older page to land", |core, _| {
        core.app().state().conversations[&ADA]
            .messages
            .front()
            .is_some_and(|m| m.id.0 == *OLDER_PAGE.start())
    })
    .await;

    assert_eq!(
        app.request_count("GetChatHistory"),
        pages_before + 1,
        "exactly one page was fetched for the hit: {:?}",
        app.requests()
    );
    assert!(
        app.window_ids(ADA).contains(&OFFSCREEN_HIT.0),
        "paging did not bring the hit into the window: {:?}",
        app.window_ids(ADA)
    );
    assert_eq!(
        app.convo(ADA).scroll,
        Scroll::At {
            message_id: OFFSCREEN_HIT,
            line_offset: 0,
        },
        "the anchor survives the page it asked for"
    );

    let rendered = app.render(120, 40);
    assert!(
        rendered.contains("budget review"),
        "the hit is still not on screen after paging:\n{rendered}"
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

fn chat(id: ChatId, title: &str, order: i64) -> TdUpdate {
    TdUpdate::NewChat(ChatView {
        id,
        kind: ChatKind::Private,
        title: title.to_string(),
        positions: vec![position(order)],
        unread_count: 0,
        unread_mention_count: 0,
        last_message: None,
        is_muted: false,
    })
}

fn incoming(chat_id: ChatId, id: i64, text: &str) -> MessageView {
    MessageView {
        id: MessageId(id),
        chat_id,
        sender: Sender::User(UserId(42)),
        sender_name: "Ada".to_string(),
        is_outgoing: false,
        date: 1_700_000_000 + id * 60,
        content: MessageContent::Text(FormattedText {
            text: text.to_string(),
            entities: Vec::new(),
        }),
        reply_to: None,
        send_state: SendState::Sent,
        reactions: Vec::new(),
        caps: MessageCaps::default(),
        is_edited: false,
    }
}

/// `ADA`'s history. The two ids the scripted search answers with carry the
/// word the query looks for, so the flow reads the way it would against a
/// real server — and so the rendered frame can be asked whether the hit is
/// actually on screen.
fn ada_message(id: i64) -> MessageView {
    let text = if MessageId(id) == NEAR_HIT || MessageId(id) == OFFSCREEN_HIT {
        format!("budget review, note {id}")
    } else {
        format!("standup notes {id}")
    };
    incoming(ADA, id, &text)
}

fn full_caps() -> MessageCaps {
    MessageCaps {
        can_be_edited: true,
        can_be_deleted_for_all_users: true,
        can_be_deleted_only_for_self: true,
        can_be_forwarded: true,
        can_be_saved: true,
    }
}

/// Logged in, three chats in the sidebar, nothing open: where both tests
/// start.
fn logged_in_with_chats() -> Vec<ScriptStep> {
    vec![
        ScriptStep::Emit(TdUpdate::Auth(AuthPhase::Ready)),
        // A connected client, so the header's right-hand slot is free for
        // what search puts there (`view::header::right_indicator` shows the
        // connection phase ahead of everything else).
        ScriptStep::Emit(TdUpdate::Connection(ConnectionPhase::Ready)),
        ScriptStep::Await {
            expect: expect("LoadChats"),
            respond: RespondWith::Ok(TdResponse::Ok),
        },
        ScriptStep::Emit(chat(ADA, "Ada Lovelace", 300)),
        ScriptStep::Emit(chat(STANDUP, "Berlin Standup", 200)),
        ScriptStep::Emit(chat(GRACE, "Grace Hopper", 100)),
    ]
}

/// The palette opens the *third* chat, so the only `getChatHistory` in this
/// script is the one the palette itself caused.
fn palette_script() -> Vec<ScriptStep> {
    let mut steps = logged_in_with_chats();
    steps.push(ScriptStep::Await {
        expect: expect("GetChatHistory"),
        respond: RespondWith::Ok(TdResponse::Messages {
            messages: GRACE_HISTORY
                .map(|id| incoming(GRACE, id, "compiler bug reproduced"))
                .collect(),
        }),
    });
    steps
}

fn search_script() -> Vec<ScriptStep> {
    let mut steps = logged_in_with_chats();
    steps.extend([
        // Opening `ADA` from the sidebar.
        ScriptStep::Await {
            expect: expect("GetChatHistory"),
            respond: RespondWith::Ok(TdResponse::Messages {
                messages: WINDOW.map(ada_message).collect(),
            }),
        },
        // `↑` into selection mode, on the way to `/`.
        ScriptStep::Await {
            expect: expect("GetMessageProperties"),
            respond: RespondWith::Ok(TdResponse::MessageProperties(full_caps())),
        },
        // The search itself: one hit inside the window, one behind it.
        ScriptStep::Await {
            expect: expect("SearchChatMessages"),
            respond: RespondWith::Ok(TdResponse::FoundMessages {
                message_ids: vec![NEAR_HIT, OFFSCREEN_HIT],
            }),
        },
        // The page the off-window hit lives in.
        ScriptStep::Await {
            expect: expect("GetChatHistory"),
            respond: RespondWith::Ok(TdResponse::Messages {
                messages: OLDER_PAGE.map(ada_message).collect(),
            }),
        },
    ]);
    steps
}

fn on_disk_fixtures() -> [(&'static str, Vec<ScriptStep>); 1] {
    [("search_flow.jsonl", search_script())]
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
            "{name} is stale — run: cargo test -p tgt-app --test search_flow \
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
