//! **The allowlist proof** (docs/plan.md T52, spec §13.8): the Milestone 8
//! gate, and the one test the privacy promise in `docs/spec.md` §13 rests on.
//!
//! A whole session is driven against `FakeTd` — login, opening a chat,
//! downloading a file, reacting, deleting, replying, sending, editing, the
//! palette, in-chat search — through the real `App::update`, the real
//! dispatcher, the exporter `otel::init` builds and a real OTLP/HTTP round
//! trip into `support/otlp_stub.rs`. Every attribute key that arrives is
//! then checked against `schema::ALLOWED_KEYS`, and any key outside it fails
//! the test by name.
//!
//! # Why this is a subset assertion and not a coverage one
//!
//! The claim being proved is "nothing leaves that is not on the list", which
//! is about the keys that *did* arrive. It is not "every action in
//! `schema::actions` was exercised" — several of those constants have no
//! emitter yet (see "What this session emits" below), and a coverage
//! assertion would either fail today or have to carry a list of exemptions
//! that quietly rots. What keeps the subset assertion from passing
//! vacuously is the second half of `exported_attribute_keys_are_subset_of_allowlist`,
//! which names the keys and the actions that *must* be present: an exporter
//! that silently stopped working would fail there rather than pass with an
//! empty set.
//!
//! # What this session emits
//!
//! Nine of the seventeen `schema::actions` constants, which is every one
//! that has a producer in the merged tree:
//!
//! | action | what drives it |
//! |---|---|
//! | `phone_login` | the login script reaching `AuthPhase::Ready` |
//! | `chat.open` | `⏎` on the sidebar row |
//! | `message.react` | the React chip (`e`) |
//! | `message.delete` | the Delete chip (`x`) and its confirmation modal |
//! | `message.reply` | the Reply chip (`r`), then `⏎` in the composer |
//! | `message.send` | `⏎` on a typed line |
//! | `message.edit` | the Edit chip (`d`), then `⏎` in the composer |
//! | `palette.open` | `ctrl+p` |
//! | `search.run` | `/`, a query, `⏎` |
//!
//! The other eight are unreachable rather than skipped, and it is worth
//! recording which and why, because "the proof did not cover it" is a
//! different statement from "there is nothing to cover":
//!
//! - `app.start`, `app.quit`, `history.page`, `file.download`, `file.upload`
//!   and `theme.change` have **no emitter anywhere in the tree**. One of them
//!   the session performs for real: it downloads a file, and
//!   `App::telemetry_for` has no arm for `TdRequest::DownloadFile`, so
//!   `file.download`'s absence from the exported records is a fact this test
//!   observes rather than an omission in the script. The last assertion in
//!   `exported_attribute_keys_are_subset_of_allowlist` pins all six, so the
//!   day any of them grows an emitter this table gets updated with it.
//! - `qr_login` is the other branch of the login the script takes.
//! - `message.forward` needs a second chat selected in the sidebar as the
//!   destination; it shares `chat_event`'s shape with the five message
//!   actions that *are* driven, so it adds no attribute key of its own.
//!
//! # `service.name`
//!
//! One key arrives that is not in `ALLOWED_KEYS`: `service.name`, which OTLP
//! requires to route a signal at all. It is a constant (`"telegram-tui"`),
//! it says nothing about the user, and `otel::resource` is the only place it
//! is set. The assertion allows exactly that one key and nothing else — see
//! [`PROTOCOL_KEYS`].
//!
//! `tgt-app` is a binary crate with no library target, so the modules under
//! test are included by path; the crate-level `allow(dead_code)` is for the
//! surface that comes along with them.

#![allow(dead_code)]
// Every test here holds `env_guard` across the whole scripted session, awaits
// included. That is the point of it: the guard serializes this file against
// the other tests in the binary that mutate the process environment, and the
// window it has to cover is the session, not one statement of it. Nothing
// deadlocks — the runtime is single-threaded and the lock is taken once, at
// the top, by a test that never yields to another holder.
#![allow(clippy::await_holding_lock)]

#[path = "../src/config.rs"]
mod config;
#[path = "../src/dispatch.rs"]
mod dispatch;
#[path = "../src/logging.rs"]
mod logging;
#[path = "../src/media_kind.rs"]
mod media_kind;
#[path = "../src/notify.rs"]
mod notify;
#[path = "../src/otel.rs"]
mod otel;
#[path = "support/otlp_stub.rs"]
mod otlp_stub;
#[path = "../src/runtime_loop.rs"]
mod runtime_loop;

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};
use tokio::sync::mpsc;
use tokio::time::timeout;

use tgt_core::app::{App, AppState, Boot, Screen};
use tgt_core::effect::TelemetryMode;
use tgt_core::model::chat::{ChatKind, ChatListId, ChatPositionEntry, ChatView};
use tgt_core::model::chips::Chip;
use tgt_core::model::entity::FormattedText;
use tgt_core::model::ids::{ChatId, FileId, MessageId, UserId};
use tgt_core::model::key::KeyBindings;
use tgt_core::model::message::{
    FileSnapshot, MessageCaps, MessageContent, MessageView, SendState, Sender,
};
use tgt_core::state::auth::AuthField;
use tgt_core::state::chat_list::visible_rows;
use tgt_core::state::focus::Focus;
use tgt_core::td::fake::{FakeTd, RequestMatcher, RespondWith, ScriptStep};
use tgt_core::td::request::{TdRequest, TdResponse};
use tgt_core::td::runtime::TdRuntime;
use tgt_core::td::update::{AuthPhase, ConnectionPhase, TdUpdate};
use tgt_core::telemetry::schema::{ALLOWED_KEYS, actions, keys};

use config::Config;
use dispatch::TdBootParams;
use otel::{CustomEndpoint, SessionContext};
use otlp_stub::OtlpStub;
use runtime_loop::Core;

/// Ceiling for any single "advance until" wait; every step is driven by a
/// channel or the 250 ms tick, so this only bounds a hang.
/// A guard against a genuinely stuck loop, not a performance assertion.
/// `cargo test --workspace` runs every integration binary concurrently, so a
/// tight wall-clock bound here fails under load rather than on a real bug.
const SETTLE_TIMEOUT: Duration = Duration::from_secs(30);

/// Keys the OTLP *envelope* needs, which are therefore allowed on top of
/// `ALLOWED_KEYS`. Spelled out as a list of exactly one so that a second
/// exception has to be added here, in a diff, with a reason — the same
/// discipline spec §13.8 asks of `ALLOWED_KEYS` itself.
const PROTOCOL_KEYS: &[&str] = &["service.name"];

// ---------------------------------------------------------------------------
// The scripted session's cast
//
// Chosen to be searchable: the ids are large and distinctive so that
// `install_id_present_chat_ids_absent` can look for them as substrings of the
// raw exported bytes and not just as whole attribute values. A chat id of
// `1` would match the digit inside a hex hash and prove nothing.
// ---------------------------------------------------------------------------

const ADA: ChatId = ChatId(4_242_424_242);
const STANDUP: ChatId = ChatId(-1_001_234_567_890);
const ADA_TITLE: &str = "Ada Lovelace";
const STANDUP_TITLE: &str = "Berlin Standup";

/// The chat's three messages. The newest is the document, so entering
/// selection mode lands on it and the Download chip is one keystroke away.
const OLDEST: MessageId = MessageId(101);
const MIDDLE: MessageId = MessageId(102);
const DOCUMENT: MessageId = MessageId(103);
const OLDEST_TEXT: &str = "quarterly budget review";
const MIDDLE_TEXT: &str = "ship it on friday";
const FILE_NAME: &str = "secret-plans.pdf";
const FILE: FileId = FileId(77);
const FILE_BYTES: u64 = 3_500_000;

/// The temporary ids TDLib mints for messages it has accepted but not sent.
const REPLY_ID: MessageId = MessageId(9001);
const SENT_ID: MessageId = MessageId(9002);

const PHONE: &str = "+4915112345678";
const CODE: &str = "54321";
const REPLY_TEXT: &str = "shipping tonight";
const SENT_TEXT: &str = "acknowledged-42";
const QUERY: &str = "budget";

/// The session identity, fixed rather than random so that the "no user data
/// on the wire" search cannot collide with it by chance: a random hex
/// `install.id` would contain `54321` about once in forty thousand runs, and
/// a flaky privacy test is worse than none. [`the_forbidden_strings_cannot_collide_with_the_session_identity`]
/// keeps that promise as these constants are edited — the first draft of
/// this file picked a session id ending `…9876543210`, which contains the
/// login code and failed exactly as it should have.
const INSTALL_ID: &str = "00112233445566778899aabbccddeeff";
const SESSION_ID: &str = "beadfacecafebead";
const TERM_PROGRAM: &str = "iTerm.app";

// ---------------------------------------------------------------------------
// The process's subscriber
// ---------------------------------------------------------------------------

/// Held for the length of every test here, because `otel::init` *reads* the
/// process environment (`OTEL_EXPORTER_OTLP_{ENDPOINT,PROTOCOL}`, spec
/// §13.5) and other tests in this binary *write* it — `logging`'s and
/// `otel`'s own unit tests come along with the `#[path]` includes above,
/// since an integration test is compiled with `--cfg test` and its
/// dependencies' `#[cfg(test)]` modules come with it.
///
/// Nothing in this file writes the environment. That is a deliberate
/// constraint rather than an accident: `config`'s tests serialize on a
/// *different* mutex of their own, so a writer here could not be made safe
/// against them, and the scripted session is therefore kept to actions that
/// never reach `Config::save` — see `no_export_before_consent`, which stops
/// short of pressing `⏎` on the consent screen for exactly this reason.
fn env_guard() -> MutexGuard<'static, ()> {
    logging::tests::env_lock()
}

/// Builds the real exporter against `stub` and makes it this thread's
/// subscriber for as long as the returned guard lives.
///
/// # Why a thread-local subscriber and not `logging::init`'s global one
///
/// The faithful thing would be `logging::init` plus
/// `logging::install_export_layer`, exactly as `main::run_tui` does. It is
/// not available here: `logging`'s own `logging_writes_under_state_dir`
/// calls `init` in this same binary, only one `try_init` can win, and which
/// test gets there first is up to the harness. So the exporter layer is
/// installed with `tracing::subscriber::set_default` instead — the same
/// route `otel.rs`'s own tests take.
///
/// What that costs: this test does not exercise `logging`'s reload slot or
/// its layer composition. What it does not cost: everything downstream of
/// the layer — `PublicOnly`'s filter, the tracing→OTLP bridge, the batch
/// processor and the wire format — is the production article, and that is
/// where an allowlist violation would live.
///
/// Every telemetry event travels on the thread that calls `Core::step`
/// (`runtime_loop` dispatches `Effect::Telemetry` inline, not on a spawned
/// task), which is the thread holding this guard.
fn install_exporter(
    stub: &OtlpStub,
    protocol: Option<&str>,
) -> (otel::OtelGuard, tracing::subscriber::DefaultGuard) {
    use tracing_subscriber::prelude::*;

    let exporter = otel::init(
        TelemetryMode::Custom,
        &session_context(),
        Some(CustomEndpoint {
            endpoint: stub.endpoint(),
            protocol: protocol.map(str::to_string),
            headers: Vec::new(),
        }),
    )
    .expect("the exporter builds against the stub")
    .expect("custom mode with an endpoint yields an exporter");

    // `tracing`'s own `set_default` rather than `SubscriberInitExt`'s: the
    // latter also installs the `log` bridge process-wide, which would make
    // `logging::init`'s `try_init` fail for whichever test runs next.
    let subscriber =
        tracing::subscriber::set_default(tracing_subscriber::registry().with(exporter.layer));
    (exporter.guard, subscriber)
}

fn session_context() -> SessionContext {
    SessionContext {
        install_id: INSTALL_ID.to_string(),
        session_id: SESSION_ID.to_string(),
        term_program: Some(TERM_PROGRAM.to_string()),
        graphics_protocol: "kitty",
        width_bucket: "120-160",
    }
}

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

struct Harness {
    core: Core,
    fake: Arc<FakeTd>,
    keys: mpsc::Sender<Event>,
}

impl Harness {
    fn new(fixture: &str, consent_needed: bool) -> Harness {
        let fake = Arc::new(FakeTd::from_jsonl(fixture).expect("fixture is valid JSONL"));
        let (keys, key_events) = mpsc::channel::<Event>(64);
        let core = Core::new(
            App::new(boot(consent_needed)),
            Arc::clone(&fake) as Arc<dyn TdRuntime>,
            Arc::new(Mutex::new(configured())),
            TdBootParams {
                database_directory: PathBuf::from("/tmp/tgt-telemetry-allowlist-db"),
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

    async fn advance_until(&mut self, what: &str, mut done: impl FnMut(&Core, &FakeTd) -> bool) {
        let settled = timeout(SETTLE_TIMEOUT, async {
            while !done(&self.core, &self.fake) {
                self.core.step().await;
            }
        })
        .await;

        assert!(
            settled.is_ok(),
            "timed out waiting for {what}\n  screen: {:?}\n  focus: {:?}\n  composer: {:?}\n  chips: {:?}\n  requests: {:?}",
            self.state().screen,
            self.state().focus.current(),
            self.state().composer,
            self.chips(),
            self.requests(),
        );
    }

    fn state(&self) -> &AppState {
        self.core.app().state()
    }

    fn requests(&self) -> Vec<&'static str> {
        self.fake.received().iter().map(TdRequest::kind).collect()
    }

    fn chips(&self) -> Vec<Chip> {
        self.state()
            .conversations
            .get(&ADA)
            .and_then(|convo| convo.selection.as_ref())
            .map(|selection| selection.chips.clone())
            .unwrap_or_default()
    }

    /// The whole scripted session, from the login prompt to the last search
    /// hit. Every step here is a user action that either mints a telemetry
    /// event or is on the way to one; see the module docs for the mapping.
    ///
    /// Waiting on the *request* rather than the keystroke at each step is
    /// what keeps the fixture's `Await` cursor and the key stream in lockstep
    /// — `FakeTd` answers its steps strictly in order, so a key pressed
    /// before the previous request landed would leave the script behind.
    async fn drive_scripted_session(&mut self) {
        self.log_in().await;
        self.open_the_chat().await;
        self.act_on_the_document().await;
        self.reply_and_send().await;
        self.edit_the_sent_message().await;
        self.open_the_palette().await;
        self.run_a_search().await;
    }

    async fn log_in(&mut self) {
        self.advance_until("TDLib to be configured", |_, fake| {
            received(fake).contains(&"SetTdlibParameters")
        })
        .await;
        self.advance_until("the login method choice", |core, _| {
            core.app().state().auth.phase == AuthPhase::WaitPhoneNumber
        })
        .await;

        self.press(KeyCode::Char('p')).await;
        self.advance_until("the phone field to take focus", |core, _| {
            core.app().state().auth.active_field == AuthField::Phone
        })
        .await;
        self.type_text(PHONE).await;
        self.press(KeyCode::Enter).await;

        self.advance_until("the code prompt", |core, _| {
            core.app().state().auth.active_field == AuthField::Code
        })
        .await;
        self.type_text(CODE).await;
        self.press(KeyCode::Enter).await;

        // `AuthPhase::Ready` is what mints `phone_login`.
        self.advance_until("the main screen", |core, _| {
            core.app().state().screen == Screen::Main
        })
        .await;
    }

    async fn open_the_chat(&mut self) {
        self.advance_until("the sidebar to fill", |core, _| {
            visible_rows(&core.app().state().chat_list).len() == 2
        })
        .await;

        self.press(KeyCode::Down).await;
        self.advance_until("the top row to be selected", |core, _| {
            core.app().state().chat_list.selected == Some(ADA)
        })
        .await;

        // `chat.open`, plus the first page of history behind it.
        self.press(KeyCode::Enter).await;
        self.advance_until("the first page to land", |core, _| {
            core.app()
                .state()
                .conversations
                .get(&ADA)
                .is_some_and(|convo| convo.messages.len() == 3)
        })
        .await;
    }

    /// The document is the newest message, so `↑` selects it. Downloading it
    /// is the action that mints *nothing*: `App::telemetry_for` has no arm
    /// for `TdRequest::DownloadFile`, which is why `file.download` never
    /// shows up in the exported records — and why the file's name has to
    /// stay off the wire on its own merits, which
    /// `install_id_present_chat_ids_absent` checks.
    async fn act_on_the_document(&mut self) {
        self.press(KeyCode::Up).await;
        self.advance_until("the document's chip row", |core, _| {
            selection_chips(core).contains(&Chip::Download)
        })
        .await;

        self.press(KeyCode::Char('l')).await;
        self.advance_until("the download to reach TDLib", |_, fake| {
            received(fake).contains(&"DownloadFile")
        })
        .await;

        // `message.react`.
        self.press(KeyCode::Char('e')).await;
        self.advance_until("the reaction to reach TDLib", |_, fake| {
            received(fake).contains(&"ToggleReaction")
        })
        .await;

        // `message.delete`, which goes through the confirmation modal: `↓`
        // moves off "Delete for me", `⏎` confirms.
        self.press(KeyCode::Char('x')).await;
        self.advance_until("the confirmation modal", |core, _| {
            matches!(core.app().state().focus.current(), Focus::Modal(_))
        })
        .await;
        self.press(KeyCode::Down).await;
        self.press(KeyCode::Enter).await;
        self.advance_until("the delete to reach TDLib", |_, fake| {
            received(fake).contains(&"DeleteMessages")
        })
        .await;
    }

    /// `message.reply` then `message.send`: the same `SendMessageText`
    /// request, told apart by whether the composer was armed with a reply
    /// target (`App::telemetry_for`).
    async fn reply_and_send(&mut self) {
        self.back_to_selection_on_the_document().await;
        self.press(KeyCode::Char('r')).await;
        self.advance_until("the composer to be armed for a reply", |core, _| {
            core.app().state().composer.reply_to == Some(DOCUMENT)
        })
        .await;

        // No `Esc` here: arming the composer is itself the way out of
        // selection mode. `App::route_selection_key` sees `composer.reply_to`
        // change and pops the selection level for the chip, so an `Esc` at
        // this point would pop one level too many and land in the chat list.
        self.wait_for_the_composer().await;
        self.type_text(REPLY_TEXT).await;
        self.press(KeyCode::Enter).await;
        self.advance_until("the reply to reach TDLib", |_, fake| sends(fake) == 1)
            .await;

        self.type_text(SENT_TEXT).await;
        self.press(KeyCode::Enter).await;
        self.advance_until("the send to reach TDLib", |_, fake| sends(fake) == 2)
            .await;
    }

    /// `message.edit`. The Edit chip is only offered on an *outgoing*
    /// message (`chips::chips_for`), so it acts on the line just sent.
    async fn edit_the_sent_message(&mut self) {
        self.press(KeyCode::Up).await;
        self.advance_until("the Edit chip on the sent message", |core, _| {
            selection_chips(core).contains(&Chip::Edit)
        })
        .await;

        self.press(KeyCode::Char('d')).await;
        self.advance_until("the composer to be armed for an edit", |core, _| {
            core.app().state().composer.editing == Some(SENT_ID)
        })
        .await;

        // Same as the reply above: the Edit chip arms the composer, and the
        // router hands the focus over with it.
        self.wait_for_the_composer().await;
        self.press(KeyCode::Enter).await;
        self.advance_until("the edit to reach TDLib", |_, fake| {
            received(fake).contains(&"EditMessageText")
        })
        .await;
    }

    /// `palette.open` — the one event minted from a projection rather than
    /// from a request, so it is also the one that would be missed by a proof
    /// that only watched TDLib traffic.
    async fn open_the_palette(&mut self) {
        self.press_ctrl(KeyCode::Char('p')).await;
        self.advance_until("the palette to open", |core, _| {
            core.app().state().palette.is_some()
        })
        .await;
        self.press_ctrl(KeyCode::Char('p')).await;
        self.advance_until("the palette to close", |core, _| {
            core.app().state().palette.is_none()
        })
        .await;
    }

    /// `search.run`. The query itself is never part of the event — that is
    /// the claim `install_id_present_chat_ids_absent` checks by looking for
    /// [`QUERY`] in the exported bytes.
    async fn run_a_search(&mut self) {
        self.press(KeyCode::Up).await;
        self.advance_until("selection mode", |core, _| {
            core.app()
                .state()
                .conversations
                .get(&ADA)
                .is_some_and(|convo| convo.selection.is_some())
        })
        .await;

        self.press(KeyCode::Char('/')).await;
        self.advance_until("the search overlay", |core, _| {
            core.app().state().chat_search.is_some()
        })
        .await;

        self.type_text(QUERY).await;
        self.press(KeyCode::Enter).await;
        self.advance_until("the search to reach TDLib", |_, fake| {
            received(fake).contains(&"SearchChatMessages")
        })
        .await;

        self.press(KeyCode::Esc).await;
        self.advance_until("search to close", |core, _| {
            core.app().state().chat_search.is_none()
        })
        .await;
    }

    /// `↑` from the composer re-enters selection mode on the newest message.
    /// Used after the delete confirmation, which leaves the selection intact
    /// but is the point in the script where that is least obvious.
    async fn back_to_selection_on_the_document(&mut self) {
        self.advance_until("the document to be selected", |core, _| {
            core.app()
                .state()
                .conversations
                .get(&ADA)
                .and_then(|convo| convo.selection.as_ref())
                .is_some_and(|selection| selection.message_id == DOCUMENT)
        })
        .await;
    }

    /// Waits for the focus handover an armed composer causes
    /// (`App::route_selection_key`), which is where both the reply and the
    /// edit are actually submitted.
    async fn wait_for_the_composer(&mut self) {
        self.advance_until("the composer to take focus", |core, _| {
            *core.app().state().focus.current() == Focus::Composer
        })
        .await;
    }

    /// The first-run consent screen (spec §13.5): it is up before TDLib has
    /// even finished asking for a phone number, and it swallows every key
    /// except its own, so nothing behind it can act on input.
    ///
    /// `p` — the key that picks phone login — is pressed between two `↓`
    /// presses. `↓` toggles the screen's two-item choice, which is
    /// observable, so seeing the choice come back to where it started is
    /// proof that the `p` in between was delivered *and* processed. Waiting
    /// on the absence of an effect would otherwise be a race dressed up as
    /// an assertion.
    ///
    /// It deliberately stops short of `⏎`: see the caller's doc comment.
    async fn consent_screen_swallows_the_login_keys(&mut self) {
        self.advance_until("the consent screen", |core, _| {
            core.app().state().screen == Screen::Consent
        })
        .await;
        self.advance_until("TDLib to be waiting for a phone number", |core, _| {
            core.app().state().auth.phase == AuthPhase::WaitPhoneNumber
        })
        .await;

        let started_on = self.state().consent.selected;
        self.press(KeyCode::Down).await;
        self.advance_until("the choice to move", |core, _| {
            core.app().state().consent.selected != started_on
        })
        .await;
        self.press(KeyCode::Char('p')).await;
        self.press(KeyCode::Down).await;
        self.advance_until("the choice to move back", |core, _| {
            core.app().state().consent.selected == started_on
        })
        .await;

        assert_eq!(
            self.state().screen,
            Screen::Consent,
            "the consent screen must still be up"
        );
        assert_eq!(
            self.state().auth.method,
            None,
            "`p` reached the auth wizard through the consent screen"
        );
        assert!(
            !self.state().consent.acknowledged,
            "nothing but ⏎ may acknowledge consent"
        );
    }
}

fn selection_chips(core: &Core) -> Vec<Chip> {
    core.app()
        .state()
        .conversations
        .get(&ADA)
        .and_then(|convo| convo.selection.as_ref())
        .map(|selection| selection.chips.clone())
        .unwrap_or_default()
}

fn received(fake: &FakeTd) -> Vec<&'static str> {
    fake.received().iter().map(TdRequest::kind).collect()
}

fn sends(fake: &FakeTd) -> usize {
    fake.received()
        .iter()
        .filter(|request| matches!(request, TdRequest::SendMessageText { .. }))
        .count()
}

fn boot(consent_needed: bool) -> Boot {
    Boot {
        theme_name: "default".to_string(),
        bindings: KeyBindings::default(),
        layout_breakpoint_cols: 100,
        telemetry_mode: TelemetryMode::Custom,
        // Non-zero, so a `chat.hash` that accidentally shipped the raw id
        // could not be mistaken for a hash of it.
        telemetry_salt: [0x5au8; 32],
        consent_needed,
        has_credentials: true,
        width: 140,
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

/// **The gate.** Every attribute key that reached the collector, at every
/// level of the OTLP envelope, is in `schema::ALLOWED_KEYS` — and the keys
/// and actions that prove the exporter was alive are all there.
#[tokio::test]
async fn exported_attribute_keys_are_subset_of_allowlist() {
    let _env = env_guard();
    let stub = OtlpStub::start();
    let (guard, subscriber) = install_exporter(&stub, None);

    let mut app = Harness::new(&read_fixture("telemetry_session.jsonl"), false);
    app.drive_scripted_session().await;

    // Stop routing events here, then flush. `BatchLogProcessor` would
    // otherwise sit on its five-second scheduled delay; this is the same
    // `shutdown` `main` runs on quit.
    drop(subscriber);
    guard.shutdown();
    stub.wait_for("the export to arrive", |stub| stub.request_count() > 0);

    // --- the subset claim ---------------------------------------------
    let offenders: Vec<String> = stub
        .keys()
        .into_iter()
        .filter(|key| !ALLOWED_KEYS.contains(&key.as_str()))
        .filter(|key| !PROTOCOL_KEYS.contains(&key.as_str()))
        .collect();
    assert!(
        offenders.is_empty(),
        "these attribute keys are not in the allowlist: {offenders:?}\n\
         Either the key belongs on the wire — in which case add it to \
         schema::ALLOWED_KEYS in a reviewed diff (spec §13.8) — or it does \
         not, in which case stop exporting it.\n\
         everything that arrived: {:?}",
        stub.attributes(),
    );

    // --- and the claim that the subset is not empty --------------------
    let arrived = stub.keys();
    for key in [
        keys::ACTION,
        keys::OUTCOME,
        keys::PUBLIC_MARKER,
        keys::CHAT_KIND,
        keys::CHAT_HASH,
        keys::INSTALL_ID,
        keys::SESSION_ID,
        keys::APP_VERSION,
        keys::OS_VERSION,
        keys::TERM_PROGRAM,
        keys::TERM_GRAPHICS_PROTOCOL,
        keys::TERM_WIDTH_BUCKET,
    ] {
        assert!(
            arrived.contains(key),
            "{key:?} never arrived — an exporter that silently stopped \
             working would satisfy the subset assertion above without it.\n\
             arrived: {arrived:?}"
        );
    }

    let observed: BTreeSet<String> = stub.actions().into_iter().collect();
    for action in [
        actions::PHONE_LOGIN,
        actions::CHAT_OPEN,
        actions::MESSAGE_REACT,
        actions::MESSAGE_DELETE,
        actions::MESSAGE_REPLY,
        actions::MESSAGE_SEND,
        actions::MESSAGE_EDIT,
        actions::PALETTE_OPEN,
        actions::SEARCH_RUN,
    ] {
        assert!(
            observed.contains(action),
            "the session performed {action:?} but it was never exported\n\
             exported: {observed:?}"
        );
    }

    // --- and some things about the shape of what arrived ----------------
    assert!(
        stub.unexpected().is_empty(),
        "the collector saw traffic it did not expect: {:?}",
        stub.unexpected()
    );
    for record in stub.records() {
        assert_eq!(
            record.scope, "tgt_telemetry",
            "every exported record must come from the emit! target, got {record:?}"
        );
        assert_eq!(
            record.body, None,
            "a telemetry event with a formatted message is free-form text by \
             definition (otel::is_public_telemetry), got {record:?}"
        );
    }

    // The actions with no producer anywhere in the tree. Asserting their
    // absence keeps the module docs' table honest: the day one of them grows
    // an emitter, this line fails and the table gets updated with it.
    for action in [
        actions::APP_START,
        actions::APP_QUIT,
        actions::HISTORY_PAGE,
        actions::FILE_DOWNLOAD,
        actions::FILE_UPLOAD,
        actions::THEME_CHANGE,
    ] {
        assert!(
            !observed.contains(action),
            "{action:?} now has an emitter — update this file's \"What this \
             session emits\" table and move it to the expected list above"
        );
    }
}

/// Spec §13.5: an install that has not acknowledged the consent screen
/// exports nothing, *including for the run in which the user acknowledges
/// it* — `main` reads consent from disk before the screen can run, so the
/// choice takes effect on the next start.
///
/// # What this test does and does not prove
///
/// `main::run_tui` is a binary entry point: it installs a panic hook, enters
/// raw mode and builds a Tokio runtime, so a test cannot call it and watch
/// the gate execute. The guarantee is therefore proved in three parts, none
/// of which is the gate expression running:
///
/// 1. [`consent_gate`] restates `main.rs`'s condition, and
///    [`the_replicated_consent_gate_still_matches_main`] fails if the line it
///    restates has changed. A source-level canary, not an execution.
/// 2. A first run really does open on the consent screen, which really does
///    swallow every key — so nothing behind it can run before the user has
///    answered.
/// 3. With no exporter installed — the state part 1's gate leaves the
///    process in — a full session performs every action that mints an event
///    and the collector receives nothing at all.
///
/// What is *not* covered, and cannot be from outside a binary's entry point:
/// a change that moved exporter construction *earlier* than the consent
/// check would break part 1 and neither of the others. Part 2 also stops one
/// keystroke short of acknowledging: `⏎` there emits
/// `ConfigPatch::ConsentAcknowledged`, which the dispatcher persists with a
/// real file write to `$XDG_CONFIG_HOME`, and redirecting that would mean
/// racing `config`'s own tests on their private lock (see [`env_guard`]).
/// The screen's "takes effect next run" semantics are asserted in
/// `state::consent`'s unit tests instead.
#[tokio::test]
async fn no_export_before_consent() {
    let _env = env_guard();
    // No exporter is built and none is installed — the state `main` leaves
    // the process in when consent is unacknowledged.
    let stub = OtlpStub::start();

    // Part 1: the gate.
    assert!(
        !consent_gate(false, TelemetryMode::Custom),
        "an unacknowledged install must not construct an exporter, whatever the mode"
    );
    assert!(!consent_gate(false, TelemetryMode::Vendor));
    assert!(!consent_gate(true, TelemetryMode::Off));
    assert!(
        consent_gate(true, TelemetryMode::Custom),
        "and an acknowledged install with a mode must, or the gate would be a synonym for Off"
    );

    // Part 2: the screen in front of everything else.
    let mut first_run = Harness::new(&read_fixture("telemetry_session.jsonl"), true);
    first_run.consent_screen_swallows_the_login_keys().await;
    drop(first_run);

    // Part 3: a whole session with nowhere to export to.
    let mut app = Harness::new(&read_fixture("telemetry_session.jsonl"), false);
    app.drive_scripted_session().await;

    // The session really happened — otherwise "nothing was exported" would
    // be true of a session that never did anything.
    let requests = app.requests();
    for kind in [
        "OpenChat",
        "ToggleReaction",
        "DeleteMessages",
        "SendMessageText",
        "EditMessageText",
        "SearchChatMessages",
    ] {
        assert!(
            requests.contains(&kind),
            "the scripted session did not reach {kind}: {requests:?}"
        );
    }

    // Give an export that is merely slow a chance to be wrong here rather
    // than in the next test: the batch processor's scheduled delay is five
    // seconds, but a *rogue* exporter would connect long before that.
    std::thread::sleep(Duration::from_millis(250));
    assert_eq!(
        stub.request_count(),
        0,
        "an unacknowledged install exported {} request(s): {:?}",
        stub.request_count(),
        stub.keys()
    );
    assert!(stub.unexpected().is_empty(), "{:?}", stub.unexpected());
}

/// `main.rs`'s exporter gate, restated. Kept next to the test that depends
/// on it and checked against the original by
/// [`the_replicated_consent_gate_still_matches_main`].
fn consent_gate(consent_acknowledged: bool, telemetry_mode: TelemetryMode) -> bool {
    consent_acknowledged && telemetry_mode != TelemetryMode::Off
}

/// The canary for [`consent_gate`]. A source-text check is a blunt
/// instrument, but the alternative — trusting a comment that says "this
/// mirrors main.rs" — is worse: it cannot fail.
#[test]
fn the_replicated_consent_gate_still_matches_main() {
    let main = std::fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("src")
            .join("main.rs"),
    )
    .expect("main.rs is readable");

    assert!(
        main.contains("if config.consent_acknowledged && telemetry_mode != TelemetryMode::Off {"),
        "main.rs's exporter gate has changed. `no_export_before_consent` \
         replicates it in `consent_gate`; update both together, and re-read \
         that test's \"what this does not prove\" section before you do."
    );
}

/// Spec §13.4: the pseudonymous install id is exported, and nothing that
/// identifies a *chat* is. The strong half of this is the substring search
/// over the raw request bodies — it does not depend on this test's decoder
/// having understood the payload, only on the bytes that left the process.
#[tokio::test]
async fn install_id_present_chat_ids_absent() {
    let _env = env_guard();
    let stub = OtlpStub::start();
    let (guard, subscriber) = install_exporter(&stub, None);

    let mut app = Harness::new(&read_fixture("telemetry_session.jsonl"), false);
    app.drive_scripted_session().await;

    drop(subscriber);
    guard.shutdown();
    stub.wait_for("the export to arrive", |stub| stub.request_count() > 0);

    assert_eq!(
        stub.values_of(keys::INSTALL_ID),
        BTreeSet::from([INSTALL_ID.to_string()]),
        "the install id is the one thing that identifies the install, and it \
         must be exported verbatim (spec §13.4)"
    );
    assert_eq!(
        stub.values_of(keys::SESSION_ID),
        BTreeSet::from([SESSION_ID.to_string()])
    );

    // Chats reach the wire as an HMAC of their id under a salt that never
    // leaves the machine, never as the id itself.
    let hashes = stub.values_of(keys::CHAT_HASH);
    assert!(
        !hashes.is_empty(),
        "the session opened, searched and messaged a chat; some chat.hash must have shipped"
    );
    for hash in &hashes {
        assert_eq!(hash.len(), 16, "chat.hash is 8 bytes as hex, got {hash:?}");
        assert!(
            hash.chars().all(|c| c.is_ascii_hexdigit()),
            "chat.hash must be hex, got {hash:?}"
        );
    }

    // Nothing the user could recognise. The ids are checked as substrings
    // rather than whole values because a leak does not have to be neat: a
    // chat id concatenated into some other attribute's value would slip past
    // an equality check.
    for (what, needle) in forbidden_strings() {
        for (index, body) in stub.bodies().iter().enumerate() {
            assert!(
                !contains(body, needle.as_bytes()),
                "{what} ({needle:?}) is in exported request #{index}\n\
                 decoded attributes: {:?}",
                stub.attributes()
            );
        }
        // Belt and braces: also as an attribute value, in case a future
        // transport is not one this test can byte-search.
        for attribute in stub.attributes() {
            assert_ne!(
                attribute.value, needle,
                "{what} was exported as the value of {:?}",
                attribute.key
            );
        }
    }
}

/// Everything the scripted session touched that a user would recognise. None
/// of it may appear anywhere in an exported payload.
fn forbidden_strings() -> Vec<(&'static str, String)> {
    vec![
        ("the open chat's id", ADA.0.to_string()),
        ("the other chat's id", STANDUP.0.to_string()),
        // The `-100…` prefix TDLib puts on supergroups, in case only the
        // magnitude were exported.
        ("the other chat's id, unsigned", STANDUP.0.abs().to_string()),
        ("a chat title", ADA_TITLE.to_string()),
        ("a chat title", STANDUP_TITLE.to_string()),
        ("a message body", OLDEST_TEXT.to_string()),
        ("a message body", MIDDLE_TEXT.to_string()),
        ("the text that was sent", SENT_TEXT.to_string()),
        ("the text that was replied", REPLY_TEXT.to_string()),
        ("a downloaded file's name", FILE_NAME.to_string()),
        ("the phone number used to log in", PHONE.to_string()),
        ("the login code", CODE.to_string()),
        ("the search query", QUERY.to_string()),
        ("the sender's name", "Ada".to_string()),
    ]
}

/// `protocol = "http/json"` is a documented custom-destination setting (spec
/// §13.5), so it is a second wire format the allowlist has to hold on. It is
/// checked with bare `emit!` calls rather than the whole session: the
/// transport is what differs, not what the app decides to say.
#[tokio::test]
async fn http_json_transport_is_also_a_subset() {
    use tgt_core::telemetry::TelemetryEvent;

    let _env = env_guard();
    let stub = OtlpStub::start();
    let (guard, subscriber) = install_exporter(&stub, Some("http/json"));

    tgt_core::emit!(TelemetryEvent::ok(actions::CHAT_OPEN).with_chat_kind("supergroup"));
    tgt_core::emit!(
        TelemetryEvent::error(
            actions::MESSAGE_SEND,
            tgt_core::telemetry::schema::error_kinds::NET_TIMEOUT
        )
        .with_duration(12)
    );
    // The two optional keys the scripted session never produces, so that
    // this file has looked at every branch of `emit!` at least once.
    tgt_core::emit!(
        TelemetryEvent::ok(actions::HISTORY_PAGE)
            .with_page_depth(3)
            .with_download_bucket(tgt_core::telemetry::schema::buckets::download_size(
                FILE_BYTES
            ))
    );

    drop(subscriber);
    guard.shutdown();
    stub.wait_for("the JSON export to arrive", |stub| stub.request_count() > 0);

    let offenders: Vec<String> = stub
        .keys()
        .into_iter()
        .filter(|key| !ALLOWED_KEYS.contains(&key.as_str()))
        .filter(|key| !PROTOCOL_KEYS.contains(&key.as_str()))
        .collect();
    assert!(
        offenders.is_empty(),
        "OTLP/JSON carried keys outside the allowlist: {offenders:?}"
    );

    let arrived = stub.keys();
    for key in [
        keys::ACTION,
        keys::OUTCOME,
        keys::ERROR_KIND,
        keys::DURATION_MS,
        keys::HISTORY_PAGE_DEPTH,
        keys::DOWNLOAD_SIZE_BUCKET,
        keys::INSTALL_ID,
        keys::OS_VERSION,
    ] {
        assert!(
            arrived.contains(key),
            "{key:?} never arrived over OTLP/JSON: {arrived:?}"
        );
    }
    assert!(stub.unexpected().is_empty(), "{:?}", stub.unexpected());
}

/// The byte search in [`install_id_present_chat_ids_absent`] only means
/// something if the strings it hunts for cannot appear legitimately. Every
/// value this file *does* expect on the wire is a constant, so that is
/// checkable rather than hopeable.
#[test]
fn the_forbidden_strings_cannot_collide_with_the_session_identity() {
    let legitimate = [
        INSTALL_ID,
        SESSION_ID,
        TERM_PROGRAM,
        env!("CARGO_PKG_VERSION"),
        "telegram-tui",
    ];
    for (what, needle) in forbidden_strings() {
        for value in legitimate {
            assert!(
                !value.contains(&needle),
                "{what} ({needle:?}) occurs inside {value:?}, which is exported \
                 legitimately — the leak check would fail on a clean session. \
                 Pick a different constant."
            );
        }
    }
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

// ---------------------------------------------------------------------------
// Fixture
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

fn chat(id: ChatId, title: &str, kind: ChatKind, order: i64) -> TdUpdate {
    TdUpdate::NewChat(ChatView {
        id,
        kind,
        title: title.to_string(),
        positions: vec![position(order)],
        unread_count: 0,
        unread_mention_count: 0,
        last_message: None,
        is_muted: false,
    })
}

fn incoming(id: MessageId, content: MessageContent) -> MessageView {
    MessageView {
        id,
        chat_id: ADA,
        sender: Sender::User(UserId(42)),
        sender_name: "Ada".to_string(),
        is_outgoing: false,
        date: 1_700_000_000 + id.0 * 60,
        content,
        reply_to: None,
        send_state: SendState::Sent,
        reactions: Vec::new(),
        caps: MessageCaps::default(),
        is_edited: false,
    }
}

fn outgoing(id: MessageId, body: &str) -> MessageView {
    MessageView {
        id,
        chat_id: ADA,
        sender: Sender::User(UserId(7)),
        sender_name: "Me".to_string(),
        is_outgoing: true,
        date: 1_700_100_000,
        content: text(body),
        reply_to: None,
        send_state: SendState::Sending,
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

fn document() -> MessageContent {
    MessageContent::Document {
        file_id: FILE,
        file_name: FILE_NAME.to_string(),
        size: FILE_BYTES,
        caption: FormattedText {
            text: String::new(),
            entities: Vec::new(),
        },
    }
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

/// Selection mode's capability fetch, answered with everything enabled. The
/// script needs one per `↑` into selection mode: `selection::select` asks for
/// the properties of whatever it lands on.
fn properties_gate() -> ScriptStep {
    ScriptStep::Await {
        expect: expect("GetMessageProperties"),
        respond: RespondWith::Ok(TdResponse::MessageProperties(full_caps())),
    }
}

/// The session the three tests replay. Written as one straight line because
/// that is what it is: a user logging in and then doing every allowlisted
/// thing the app can currently do.
fn session_script() -> Vec<ScriptStep> {
    vec![
        // --- phone login ------------------------------------------------
        ScriptStep::Emit(TdUpdate::Auth(AuthPhase::WaitTdlibParameters)),
        ScriptStep::Await {
            expect: expect("SetTdlibParameters"),
            respond: RespondWith::Ok(TdResponse::Ok),
        },
        ScriptStep::Emit(TdUpdate::Auth(AuthPhase::WaitPhoneNumber)),
        ScriptStep::Await {
            expect: expect("SetAuthenticationPhoneNumber"),
            respond: RespondWith::Ok(TdResponse::Ok),
        },
        ScriptStep::Emit(TdUpdate::Auth(AuthPhase::WaitCode {
            delivery_hint: "SMS to +4***78".to_string(),
            length: 5,
        })),
        ScriptStep::Await {
            expect: expect("CheckAuthenticationCode"),
            respond: RespondWith::Ok(TdResponse::Ok),
        },
        // `phone_login`.
        ScriptStep::Emit(TdUpdate::Auth(AuthPhase::Ready)),
        ScriptStep::Emit(TdUpdate::Connection(ConnectionPhase::Ready)),
        // --- the sidebar ------------------------------------------------
        ScriptStep::Await {
            expect: expect("LoadChats"),
            respond: RespondWith::Ok(TdResponse::Ok),
        },
        // Two chats of different kinds, so `chat.kind` has more than one
        // possible value and the exported one is a real choice.
        ScriptStep::Emit(chat(ADA, ADA_TITLE, ChatKind::Private, 300)),
        ScriptStep::Emit(chat(STANDUP, STANDUP_TITLE, ChatKind::Supergroup, 200)),
        // --- opening the chat (`chat.open`) -----------------------------
        //
        // `OpenChat` goes out alongside this one and matches nothing, which
        // `FakeTd` answers with a plain `Ok` — the request is still recorded,
        // which is all `chat.open` and the consent test need from it.
        ScriptStep::Await {
            expect: expect("GetChatHistory"),
            respond: RespondWith::Ok(TdResponse::Messages {
                messages: vec![
                    incoming(OLDEST, text(OLDEST_TEXT)),
                    incoming(MIDDLE, text(MIDDLE_TEXT)),
                    incoming(DOCUMENT, document()),
                ],
            }),
        },
        // --- on the document: download, react, delete -------------------
        properties_gate(),
        ScriptStep::Await {
            expect: expect("DownloadFile"),
            respond: RespondWith::Ok(TdResponse::File(FileSnapshot {
                id: FILE,
                expected_size: FILE_BYTES,
                downloaded_size: 0,
                is_downloading: true,
                is_completed: false,
                local_path: None,
            })),
        },
        // `message.react`.
        ScriptStep::Await {
            expect: expect("ToggleReaction"),
            respond: RespondWith::Ok(TdResponse::Ok),
        },
        // `message.delete`. No `MessagesDeleted` push follows: leaving the
        // message in the window keeps the rest of the script's selection
        // arithmetic simple, and the event is minted from the request.
        ScriptStep::Await {
            expect: expect("DeleteMessages"),
            respond: RespondWith::Ok(TdResponse::Ok),
        },
        // --- `message.reply`, then `message.send` -----------------------
        ScriptStep::Await {
            expect: expect("SendMessageText"),
            respond: RespondWith::Ok(TdResponse::Message(outgoing(REPLY_ID, REPLY_TEXT))),
        },
        ScriptStep::Await {
            expect: expect("SendMessageText"),
            respond: RespondWith::Ok(TdResponse::Message(outgoing(SENT_ID, SENT_TEXT))),
        },
        // --- `message.edit`, on the message just sent -------------------
        properties_gate(),
        ScriptStep::Await {
            expect: expect("EditMessageText"),
            respond: RespondWith::Ok(TdResponse::Ok),
        },
        // --- `search.run` (the palette in between asks TDLib nothing) ---
        properties_gate(),
        ScriptStep::Await {
            expect: expect("SearchChatMessages"),
            respond: RespondWith::Ok(TdResponse::FoundMessages {
                message_ids: vec![OLDEST],
            }),
        },
    ]
}

fn on_disk_fixtures() -> [(&'static str, Vec<ScriptStep>); 1] {
    [("telemetry_session.jsonl", session_script())]
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
            "{name} is stale — run: cargo test -p tgt-app --test telemetry_allowlist \
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
