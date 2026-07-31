//! Full-app authentication against `FakeTd` (docs/plan.md T14, spec §15.4).
//!
//! These tests drive the real `runtime_loop::Core` — the same action channel,
//! the same `App::update`, the same dispatcher and the same
//! `tgt_ui::input::map_event` a keystroke goes through in the binary. Only
//! the terminal is absent: keys are pushed as `crossterm::event::Event`s into
//! the channel the reader thread would feed, and nothing is drawn.
//!
//! `tgt-app` is a binary crate with no library target, so the modules under
//! test are included by path. The crate-level `allow(dead_code)` is for that:
//! an included module brings its whole surface along, and this binary only
//! exercises the auth slice of it.

#![allow(dead_code)]

#[path = "../src/config.rs"]
mod config;
#[path = "../src/dispatch.rs"]
mod dispatch;
#[path = "../src/runtime_loop.rs"]
mod runtime_loop;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};
use tokio::sync::mpsc;
use tokio::time::timeout;

use tgt_core::app::{App, Boot, Screen};
use tgt_core::effect::TelemetryMode;
use tgt_core::model::key::KeyBindings;
use tgt_core::model::time::Millis;
use tgt_core::state::auth::{AuthField, LoginMethod};
use tgt_core::td::error::TdError;
use tgt_core::td::fake::{FakeTd, RequestMatcher, RespondWith, ScriptStep};
use tgt_core::td::request::{TdRequest, TdResponse};
use tgt_core::td::runtime::TdRuntime;
use tgt_core::td::update::{AuthPhase, TdUpdate};

use config::Config;
use dispatch::TdBootParams;
use runtime_loop::Core;

/// Ceiling for any single "advance until" wait. Every step is driven by a
/// channel or the 250 ms tick, so a healthy flow settles in milliseconds;
/// this only bounds a hang.
const SETTLE_TIMEOUT: Duration = Duration::from_secs(5);

const QR_LINK: &str = "tg://login?token=AAEBAgMEBQYHCAkKCwwNDg8Q";

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

struct Harness {
    core: Core,
    fake: Arc<FakeTd>,
    keys: mpsc::Sender<Event>,
}

impl Harness {
    /// Boots an app with credentials already configured (the my.telegram.org
    /// wizard is a separate path, covered by `state::auth`'s unit tests)
    /// against a `FakeTd` replaying `fixture`.
    fn new(fixture: &str) -> Harness {
        let fake = Arc::new(FakeTd::from_jsonl(fixture).expect("fixture is valid JSONL"));
        let (keys, key_events) = mpsc::channel::<Event>(64);
        let core = Core::new(
            App::new(boot()),
            Arc::clone(&fake) as Arc<dyn TdRuntime>,
            Arc::new(Mutex::new(configured())),
            TdBootParams {
                database_directory: PathBuf::from("/tmp/tgt-auth-flow-db"),
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

    /// Steps the loop until `done` holds, failing with a readable state dump
    /// rather than hanging.
    async fn advance_until(&mut self, what: &str, mut done: impl FnMut(&Core, &FakeTd) -> bool) {
        let settled = timeout(SETTLE_TIMEOUT, async {
            while !done(&self.core, &self.fake) {
                self.core.step().await;
            }
        })
        .await;

        assert!(
            settled.is_ok(),
            "timed out waiting for {what}\n  screen: {:?}\n  phase: {:?}\n  active field: {:?}\n  requests: {:?}",
            self.core.app().state().screen,
            self.core.app().state().auth.phase,
            self.core.app().state().auth.active_field,
            self.fake
                .received()
                .iter()
                .map(TdRequest::kind)
                .collect::<Vec<_>>(),
        );
    }

    async fn advance_until_phase(&mut self, what: &str, phase: AuthPhase) {
        self.advance_until(what, |core, _| core.app().state().auth.phase == phase)
            .await;
    }

    fn requests(&self) -> Vec<&'static str> {
        self.fake.received().iter().map(TdRequest::kind).collect()
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

#[tokio::test]
async fn phone_login_reaches_ready() {
    let mut app = Harness::new(&read_fixture("auth_phone.jsonl"));

    // TDLib asks for its parameters; nothing in `update()` answers that —
    // the dispatcher does, from config + Keychain + database dir.
    app.advance_until("SetTdlibParameters to be sent", |_, fake| {
        fake.received()
            .iter()
            .any(|r| matches!(r, TdRequest::SetTdlibParameters(_)))
    })
    .await;

    app.advance_until_phase("the phone/QR method choice", AuthPhase::WaitPhoneNumber)
        .await;
    assert_eq!(app.core.app().state().screen, Screen::Auth);

    // Picking Phone is itself the confirmation (state::auth): from here the
    // keys go straight into the phone field and Enter submits it.
    app.press(KeyCode::Char('p')).await;
    app.advance_until("the phone field to take focus", |core, _| {
        core.app().state().auth.method == Some(LoginMethod::Phone)
            && core.app().state().auth.active_field == AuthField::Phone
    })
    .await;

    app.type_text("+4915112345678").await;
    app.press(KeyCode::Enter).await;
    app.advance_until_phase(
        "the code prompt",
        AuthPhase::WaitCode {
            delivery_hint: "SMS to +4***78".to_string(),
            length: 5,
        },
    )
    .await;
    assert_eq!(app.core.app().state().auth.active_field, AuthField::Code);

    app.type_text("54321").await;
    app.press(KeyCode::Enter).await;
    app.advance_until("the main screen", |core, _| {
        core.app().state().screen == Screen::Main
    })
    .await;

    // Ready produces the first real request of the session.
    app.advance_until("the chat list to be requested", |_, fake| {
        fake.received()
            .iter()
            .any(|r| matches!(r, TdRequest::LoadChats { .. }))
    })
    .await;

    assert_eq!(
        app.requests(),
        vec![
            "SetTdlibParameters",
            "SetAuthenticationPhoneNumber",
            "CheckAuthenticationCode",
            "LoadChats",
        ]
    );
    let received = app.fake.received();
    assert!(matches!(
        &received[1],
        TdRequest::SetAuthenticationPhoneNumber { phone } if phone == "+4915112345678"
    ));
    assert!(matches!(
        &received[2],
        TdRequest::CheckAuthenticationCode { code } if code == "54321"
    ));
    assert_eq!(app.core.app().state().auth.field_error, None);
}

#[tokio::test]
async fn qr_login_reaches_ready() {
    let mut app = Harness::new(&read_fixture("auth_qr.jsonl"));

    app.advance_until_phase("the phone/QR method choice", AuthPhase::WaitPhoneNumber)
        .await;

    app.press(KeyCode::Char('q')).await;
    app.press(KeyCode::Enter).await;

    // The link phase is a screen of its own: the user is looking at a QR
    // code while TDLib waits for the other device.
    app.advance_until_phase(
        "the QR link",
        AuthPhase::WaitOtherDeviceConfirmation {
            link: QR_LINK.to_string(),
        },
    )
    .await;

    app.advance_until("the main screen", |core, _| {
        core.app().state().screen == Screen::Main
    })
    .await;
    app.advance_until("the chat list to be requested", |_, fake| {
        fake.received()
            .iter()
            .any(|r| matches!(r, TdRequest::LoadChats { .. }))
    })
    .await;

    assert_eq!(
        app.requests(),
        vec![
            "SetTdlibParameters",
            "RequestQrCodeAuthentication",
            "LoadChats",
        ]
    );
    // No phone number was ever submitted on this path.
    assert!(
        !app.requests().contains(&"SetAuthenticationPhoneNumber"),
        "QR login must not send a phone number"
    );
}

#[tokio::test]
async fn flood_wait_surfaces_countdown_not_generic_error() {
    // Kept in memory rather than on disk: it is two steps long and exists
    // only to make one submission fail.
    let mut app = Harness::new(&to_jsonl(&flood_wait_script()));

    app.advance_until_phase("the phone/QR method choice", AuthPhase::WaitPhoneNumber)
        .await;
    app.press(KeyCode::Char('p')).await;
    app.advance_until("the phone field to take focus", |core, _| {
        core.app().state().auth.method == Some(LoginMethod::Phone)
    })
    .await;

    app.type_text("+4915112345678").await;
    app.press(KeyCode::Enter).await;

    app.advance_until("the flood-wait countdown", |core, _| {
        core.app().state().auth.flood_wait_until.is_some()
    })
    .await;

    let state = app.core.app().state();
    // A flood wait is a countdown, not a rejected field: the number the user
    // typed is fine and stays put, and no inline error is shown.
    assert_eq!(
        state.auth.field_error, None,
        "flood wait must not surface as a generic field error"
    );
    assert_eq!(state.auth.phone.text, "+4915112345678");
    assert!(!state.auth.in_flight);
    assert!(
        state.auth.flood_wait_until.unwrap() >= state.now.saturating_add(41_000),
        "countdown should run ~42s from now, got {:?} at now={:?}",
        state.auth.flood_wait_until,
        state.now
    );
    assert_eq!(state.screen, Screen::Auth);

    // Submission is blocked while the countdown runs: pressing Enter again
    // sends nothing.
    let before = app.fake.received().len();
    app.press(KeyCode::Enter).await;
    app.advance_until("a tick past the retry press", |core, _| {
        core.app().state().now > Millis(0)
    })
    .await;
    assert_eq!(app.fake.received().len(), before);
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

fn phone_script() -> Vec<ScriptStep> {
    vec![
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
        ScriptStep::Emit(TdUpdate::Auth(AuthPhase::Ready)),
    ]
}

fn qr_script() -> Vec<ScriptStep> {
    vec![
        ScriptStep::Emit(TdUpdate::Auth(AuthPhase::WaitTdlibParameters)),
        ScriptStep::Await {
            expect: expect("SetTdlibParameters"),
            respond: RespondWith::Ok(TdResponse::Ok),
        },
        ScriptStep::Emit(TdUpdate::Auth(AuthPhase::WaitPhoneNumber)),
        ScriptStep::Await {
            expect: expect("RequestQrCodeAuthentication"),
            respond: RespondWith::Ok(TdResponse::Ok),
        },
        ScriptStep::Emit(TdUpdate::Auth(AuthPhase::WaitOtherDeviceConfirmation {
            link: QR_LINK.to_string(),
        })),
        // Scanning the code on the other device is what advances TDLib; the
        // app sends nothing in between.
        ScriptStep::Emit(TdUpdate::Auth(AuthPhase::Ready)),
    ]
}

fn flood_wait_script() -> Vec<ScriptStep> {
    vec![
        ScriptStep::Emit(TdUpdate::Auth(AuthPhase::WaitTdlibParameters)),
        ScriptStep::Await {
            expect: expect("SetTdlibParameters"),
            respond: RespondWith::Ok(TdResponse::Ok),
        },
        ScriptStep::Emit(TdUpdate::Auth(AuthPhase::WaitPhoneNumber)),
        ScriptStep::Await {
            expect: expect("SetAuthenticationPhoneNumber"),
            respond: RespondWith::Err(TdError::FloodWait { seconds: 42 }),
        },
    ]
}

fn on_disk_fixtures() -> [(&'static str, Vec<ScriptStep>); 2] {
    [
        ("auth_phone.jsonl", phone_script()),
        ("auth_qr.jsonl", qr_script()),
    ]
}

/// The fixtures are generated, not hand-written: their encoding is whatever
/// serde derives on the boundary types, and a drift there would otherwise
/// show up as an unexplained parse failure. This test is the round-trip
/// check — it parses each file with `FakeTd` and compares it against the
/// script it should hold.
#[test]
fn fixtures_on_disk_match_their_scripts() {
    for (name, script) in on_disk_fixtures() {
        let text = read_fixture(name);
        FakeTd::from_jsonl(&text).unwrap_or_else(|e| panic!("{name} does not parse: {e}"));
        assert_eq!(
            text,
            to_jsonl(&script),
            "{name} is stale — run: cargo test -p tgt-app --test auth_flow \
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
