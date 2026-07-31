//! Full-app interaction flows against `FakeTd` (docs/plan.md T32, spec
//! §15.4): typing and sending with the optimistic append and its confirmation
//! swap (architecture §5.2), a send that fails at both the RPC and the later
//! push, and a delete-for-everyone round trip through the chip row and its
//! confirmation modal.
//!
//! Same harness shape as `read_only.rs`: the real `runtime_loop::Core`, the
//! real dispatcher and the real `App::update`, with keys pushed in as
//! `crossterm` events and TDLib replaced by a scripted fixture.
//!
//! # Why the scripts gate on `GetMessageProperties`
//!
//! `FakeTd` pushes every `Emit` step that follows an `Await` the moment that
//! `Await` is answered, and the loop's `select!` picks at random between a
//! ready action (the RPC completion) and a ready update (the push). A script
//! that emitted `MessageSendSucceeded` directly after answering
//! `SendMessageText` would therefore race: the swap could be applied before
//! the optimistic message it is supposed to replace exists.
//!
//! So the scripts put an `Await` between the two and let the test open the
//! gate deliberately — `↑` on an empty composer enters selection mode, which
//! fires `GetMessageProperties` for the newest message (T26). The push lands
//! only once the test has already asserted the intermediate state, and the
//! gate doubles as coverage of the capability round trip T32 wired.
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
use tgt_core::model::ids::{ChatId, MessageId, UserId};
use tgt_core::model::key::KeyBindings;
use tgt_core::model::message::{MessageCaps, MessageContent, MessageView, SendState, Sender};
use tgt_core::state::chat_list::visible_rows;
use tgt_core::state::conversation::ConversationState;
use tgt_core::state::focus::{Focus, ModalKind};
use tgt_core::td::error::TdError;
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

const CHAT: ChatId = ChatId(1);
/// The id TDLib mints for a message it has accepted but not yet sent, and the
/// real one it reports afterwards (architecture §5.2).
const TEMP_ID: MessageId = MessageId(9999);
const FINAL_ID: MessageId = MessageId(10001);
/// The three messages every script preloads as chat history.
const HISTORY: std::ops::RangeInclusive<i64> = 101..=103;

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
                database_directory: PathBuf::from("/tmp/tgt-send-flow-db"),
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
            "timed out waiting for {what}\n  focus: {:?}\n  composer: {:?}\n  window: {:?}\n  requests: {:?}",
            self.state().focus.current(),
            self.state().composer,
            self.window_ids(),
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

    fn message(&self, id: MessageId) -> Option<&MessageView> {
        self.convo().messages.iter().find(|m| m.id == id)
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
                .is_some_and(|c| c.messages.len() == HISTORY.count())
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

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// Architecture §5.2 end to end. `sendMessage` answers with a message carrying
/// a temporary id, which goes straight into the window as `Sending`; the
/// `MessageSendSucceeded` push then swaps it for the real id and `Sent`; a
/// `ChatReadOutbox` at or past that id advances the read marker the ✓✓ glyph
/// is derived from.
#[tokio::test]
async fn optimistic_message_confirmed_with_final_id() {
    let mut app = Harness::new(&read_fixture("send_flow.jsonl"));
    app.open_chat_with_history().await;

    app.type_text("hello").await;
    app.advance_until("the draft to reach the composer", |core, _| {
        core.app().state().composer.input.text == "hello"
    })
    .await;

    app.press(KeyCode::Enter).await;
    app.advance_until("the optimistic message to appear", |core, _| {
        core.app()
            .state()
            .conversations
            .get(&CHAT)
            .is_some_and(|c| c.messages.iter().any(|m| m.id == TEMP_ID))
    })
    .await;

    // Optimistic: the message is on screen under a temporary id, still
    // marked as in flight, and the composer has already been cleared.
    let optimistic = app.message(TEMP_ID).expect("just asserted it is there");
    assert_eq!(optimistic.send_state, SendState::Sending);
    assert!(optimistic.is_outgoing);
    assert_eq!(app.state().composer.input.text, "");
    assert!(
        app.state().composer.pending_send.is_none(),
        "an accepted send drops the held text (spec §14)"
    );
    let rendered = app.render(120, 40);
    assert!(
        rendered.contains("hello"),
        "the optimistic message is missing from the frame:\n{rendered}"
    );

    // Open the gate (see the module docs): `↑` on the now-empty composer
    // enters selection mode and asks TDLib for the newest message's caps.
    app.press(KeyCode::Up).await;
    app.advance_until("the confirmation swap", |core, _| {
        core.app()
            .state()
            .conversations
            .get(&CHAT)
            .is_some_and(|c| c.messages.iter().any(|m| m.id == FINAL_ID))
    })
    .await;

    assert!(
        app.message(TEMP_ID).is_none(),
        "the temporary id must be gone, not duplicated: {:?}",
        app.window_ids()
    );
    assert_eq!(app.message(FINAL_ID).unwrap().send_state, SendState::Sent);
    assert_eq!(
        app.window_ids(),
        vec![101, 102, 103, FINAL_ID.0],
        "the confirmed message keeps its place at the newest end"
    );

    // The read receipt. What the viewport draws from it (✓ vs ✓✓) is T35's;
    // what this flow owns is the marker it reads.
    app.advance_until("the read receipt", |core, _| {
        core.app()
            .state()
            .conversations
            .get(&CHAT)
            .is_some_and(|c| c.last_read_outbox >= FINAL_ID)
    })
    .await;
    assert_eq!(app.convo().last_read_outbox, FINAL_ID);

    assert!(
        app.requests().contains(&"SendMessageText"),
        "requests: {:?}",
        app.requests()
    );
}

/// Spec §14, both halves: typed text is never discarded by a failed send.
/// The RPC rejecting it restores the draft to the composer with nothing left
/// in the window; a send TDLib accepts and only later reports failed leaves
/// the optimistic message visible and marked `Failed` instead.
#[tokio::test]
async fn failed_send_restores_composer_and_marks_failed() {
    let mut app = Harness::new(&to_jsonl(&failed_send_script()));
    app.open_chat_with_history().await;

    // Phase 1: the RPC itself fails.
    app.type_text("first try").await;
    app.advance_until("the draft to be typed", |core, _| {
        core.app().state().composer.input.text == "first try"
    })
    .await;

    app.press(KeyCode::Enter).await;
    // Waiting on the text alone would be ambiguous — it reads the same
    // before the send as after the restore. The send having reached TDLib is
    // what distinguishes them: `submit` empties the input, so the draft can
    // only be back because the failure put it there.
    app.advance_until("the draft to come back", |core, fake| {
        sends(fake) == 1 && core.app().state().composer.input.text == "first try"
    })
    .await;

    let composer = &app.state().composer;
    assert!(
        composer.pending_send.is_none(),
        "the held text was moved back, not copied"
    );
    assert_eq!(
        composer.input.cursor,
        "first try".len(),
        "the cursor lands at the end of the restored draft"
    );
    assert_eq!(
        app.window_ids(),
        vec![101, 102, 103],
        "a send that never reached TDLib leaves nothing in the window"
    );

    // Phase 2: the user simply hits ⏎ again on the draft that came back.
    // TDLib accepts it this time and reports the failure afterwards.
    app.press(KeyCode::Enter).await;
    app.advance_until("the optimistic message", |core, _| {
        core.app()
            .state()
            .conversations
            .get(&CHAT)
            .is_some_and(|c| c.messages.iter().any(|m| m.id == TEMP_ID))
    })
    .await;

    // Open the gate; the failure push follows it.
    app.press(KeyCode::Up).await;
    app.advance_until("the failure to be recorded", |core, _| {
        core.app()
            .state()
            .conversations
            .get(&CHAT)
            .and_then(|c| c.messages.iter().find(|m| m.id == TEMP_ID))
            .is_some_and(|m| matches!(m.send_state, SendState::Failed(_)))
    })
    .await;

    // The message stays put under its temporary id: it is the only copy of
    // that text left, and selection mode offers Resend on it.
    let failed = app.message(TEMP_ID).expect("the failed message is kept");
    assert_eq!(failed.send_state, SendState::Failed(TdError::NetTimeout));
    assert!(matches!(
        &failed.content,
        MessageContent::Text(body) if body.text == "first try"
    ));
}

/// Spec §5.3 / §6.3: the Delete chip confirms before it deletes, the second
/// option is only offered when TDLib says the message can be revoked, and the
/// message leaves the window on the `MessagesDeleted` push rather than
/// optimistically.
#[tokio::test]
async fn delete_for_everyone_round_trip() {
    let mut app = Harness::new(&to_jsonl(&delete_script()));
    app.open_chat_with_history().await;

    // Selection mode starts on the newest message and fetches its caps —
    // without them the chip row cannot offer Delete at all (architecture §7).
    app.press(KeyCode::Up).await;
    app.advance_until("the capability flags", |core, _| {
        core.app()
            .state()
            .conversations
            .get(&CHAT)
            .and_then(|c| c.selection.as_ref())
            .is_some_and(|s| s.chips.contains(&Chip::Delete))
    })
    .await;

    assert_eq!(
        app.chips(),
        vec![
            Chip::Reply,
            Chip::Forward,
            Chip::React,
            Chip::Copy,
            Chip::Delete
        ],
        "the row is derived from the fetched caps, not a fixed menu"
    );

    // `x` is the Delete chip's shortcut; it raises the confirmation instead
    // of deleting anything.
    app.press(KeyCode::Char('x')).await;
    app.advance_until("the confirmation modal", |core, _| {
        matches!(core.app().state().focus.current(), Focus::Modal(_))
    })
    .await;

    assert!(
        matches!(
            app.state().focus.current(),
            Focus::Modal(ModalKind::ConfirmDelete {
                can_revoke: true,
                ..
            })
        ),
        "focus: {:?}",
        app.state().focus.current()
    );
    assert_eq!(
        app.window_ids(),
        vec![101, 102, 103],
        "raising the modal must not delete anything yet"
    );
    let rendered = app.render(120, 40);
    assert!(
        rendered.contains("Delete for everyone"),
        "the revoke option is missing from the modal:\n{rendered}"
    );

    // `↓` moves off the default "Delete for me", `⏎` confirms.
    app.press(KeyCode::Down).await;
    app.press(KeyCode::Enter).await;
    app.advance_until("the message to be gone", |core, _| {
        core.app()
            .state()
            .conversations
            .get(&CHAT)
            .is_some_and(|c| c.messages.len() == 2)
    })
    .await;

    assert_eq!(app.window_ids(), vec![101, 102]);
    assert!(
        !matches!(app.state().focus.current(), Focus::Modal(_)),
        "confirming closes the modal"
    );

    let deletes: Vec<TdRequest> = app
        .fake
        .received()
        .into_iter()
        .filter(|r| matches!(r, TdRequest::DeleteMessages { .. }))
        .collect();
    assert_eq!(deletes.len(), 1, "exactly one delete went out: {deletes:?}");
    assert!(
        matches!(
            &deletes[0],
            TdRequest::DeleteMessages {
                chat_id: CHAT,
                message_ids,
                revoke: true,
            } if message_ids == &vec![MessageId(103)]
        ),
        "the chosen option must reach TDLib as revoke: true, got {:?}",
        deletes[0]
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

/// How many sends have reached TDLib so far — the unambiguous marker for
/// "the composer has submitted", since its input reads the same before a
/// send as after a failure restores it.
fn sends(fake: &FakeTd) -> usize {
    fake.received()
        .iter()
        .filter(|r| matches!(r, TdRequest::SendMessageText { .. }))
        .count()
}

fn position(order: i64) -> ChatPositionEntry {
    ChatPositionEntry {
        list: ChatListId::Main,
        order,
        is_pinned: false,
    }
}

fn incoming(id: i64) -> MessageView {
    MessageView {
        id: MessageId(id),
        chat_id: CHAT,
        sender: Sender::User(UserId(42)),
        sender_name: "Ada".to_string(),
        is_outgoing: false,
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

/// An outgoing message as TDLib hands it back from `sendMessage` (temporary
/// id, `Sending`) or from the confirmation push (real id, `Sent`).
fn outgoing(id: MessageId, text: &str, send_state: SendState) -> MessageView {
    MessageView {
        id,
        chat_id: CHAT,
        sender: Sender::User(UserId(7)),
        sender_name: "Me".to_string(),
        is_outgoing: true,
        date: 1_700_100_000,
        content: MessageContent::Text(FormattedText {
            text: text.to_string(),
            entities: Vec::new(),
        }),
        reply_to: None,
        send_state,
        reactions: Vec::new(),
        caps: MessageCaps::default(),
        is_edited: false,
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

/// Logged in already, one chat in the sidebar, three messages of history:
/// the state every test in this file starts from.
fn opened_chat() -> Vec<ScriptStep> {
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
            respond: RespondWith::Ok(TdResponse::Messages {
                messages: HISTORY.map(incoming).collect(),
            }),
        },
    ]
}

/// The gate described in the module docs: selection mode's capability fetch,
/// answered with everything enabled.
fn properties_gate() -> ScriptStep {
    ScriptStep::Await {
        expect: expect("GetMessageProperties"),
        respond: RespondWith::Ok(TdResponse::MessageProperties(full_caps())),
    }
}

fn send_flow_script() -> Vec<ScriptStep> {
    let mut steps = opened_chat();
    steps.extend([
        ScriptStep::Await {
            expect: expect("SendMessageText"),
            respond: RespondWith::Ok(TdResponse::Message(outgoing(
                TEMP_ID,
                "hello",
                SendState::Sending,
            ))),
        },
        properties_gate(),
        ScriptStep::Emit(TdUpdate::MessageSendSucceeded {
            chat_id: CHAT,
            old_message_id: TEMP_ID,
            message: outgoing(FINAL_ID, "hello", SendState::Sent),
        }),
        ScriptStep::Emit(TdUpdate::ChatReadOutbox {
            chat_id: CHAT,
            last_read_outbox_message_id: FINAL_ID,
        }),
    ]);
    steps
}

fn failed_send_script() -> Vec<ScriptStep> {
    let mut steps = opened_chat();
    steps.extend([
        // The RPC refuses outright: nothing was ever created server-side.
        ScriptStep::Await {
            expect: expect("SendMessageText"),
            respond: RespondWith::Err(TdError::Other {
                code: 400,
                message: "CHAT_WRITE_FORBIDDEN".to_string(),
            }),
        },
        // The second attempt is accepted, then fails asynchronously.
        ScriptStep::Await {
            expect: expect("SendMessageText"),
            respond: RespondWith::Ok(TdResponse::Message(outgoing(
                TEMP_ID,
                "first try",
                SendState::Sending,
            ))),
        },
        properties_gate(),
        ScriptStep::Emit(TdUpdate::MessageSendFailed {
            chat_id: CHAT,
            old_message_id: TEMP_ID,
            error: TdError::NetTimeout,
        }),
    ]);
    steps
}

fn delete_script() -> Vec<ScriptStep> {
    let mut steps = opened_chat();
    steps.extend([
        properties_gate(),
        ScriptStep::Await {
            expect: expect("DeleteMessages"),
            respond: RespondWith::Ok(TdResponse::Ok),
        },
        ScriptStep::Emit(TdUpdate::MessagesDeleted {
            chat_id: CHAT,
            message_ids: vec![MessageId(103)],
        }),
    ]);
    steps
}

fn on_disk_fixtures() -> [(&'static str, Vec<ScriptStep>); 1] {
    [("send_flow.jsonl", send_flow_script())]
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
            "{name} is stale — run: cargo test -p tgt-app --test send_flow \
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
