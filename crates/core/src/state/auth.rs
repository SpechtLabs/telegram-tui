//! Auth wizard state: a projection of TDLib's authorizationState.
//! See docs/architecture.md §4.6, §5.1; spec §9.
//!
//! ## The credentials-wizard contract
//!
//! `AuthState` cannot hold a `has_credentials: bool` (that fact lives in
//! `Boot`, and adding a field here would break `app.rs`'s field-by-field
//! construction, which this task does not own). Instead the wizard is
//! *driven by `active_field`*:
//!
//! - When no credentials are configured, whoever constructs `AppState`
//!   (currently `App::new`, to be adjusted by T14 to read `Boot.has_credentials`)
//!   must set `auth.active_field = AuthField::ApiId`.
//! - `handle_key` treats `active_field` being `ApiId` or `ApiHash` as "the
//!   credentials wizard is active", independent of `auth.phase`. Tab/Down on
//!   `ApiId` moves to `ApiHash`; Enter on `ApiId` also advances to `ApiHash`
//!   (it never submits from the first field). Enter on `ApiHash`, with
//!   `api_id` parsing as `i32` and `api_hash` non-empty, emits
//!   `Effect::SaveConfig(ConfigPatch::Credentials { .. })` and moves
//!   `active_field` to `AuthField::Phone` — leaving the wizard.
//! - When credentials are already configured, callers must set
//!   `active_field = AuthField::Phone` (today's `App::new` default), so the
//!   wizard branch never triggers and phase-driven routing (method choice →
//!   phone/code/password) takes over immediately.
//!
//! T12 (auth views) and T14 (wiring) must uphold: `active_field` starts at
//! `ApiId` iff credentials are missing, and never take the user back to
//! `ApiId`/`ApiHash` once `handle_td` has projected a phase past
//! `WaitTdlibParameters`.
//!
//! ## `WaitTdlibParameters`
//!
//! `TdlibParams` embeds `database_directory` and `database_encryption_key`
//! (a Keychain secret) — both impure, boot-time facts `tgt-core` does not
//! hold. `handle_td` therefore emits **no effect** on `WaitTdlibParameters`;
//! it only projects the phase. T14's dispatcher/app wiring is responsible
//! for issuing `Effect::Td(SetTdlibParameters(..))` once real credentials
//! and boot facts are available.

use crate::app::{AppState, Screen};
use crate::effect::{ConfigPatch, Effect};
use crate::model::chat::ChatListId;
use crate::model::key::Key;
use crate::model::time::Millis;
use crate::td::error::TdError;
use crate::td::request::TdRequest;
use crate::td::update::{AuthPhase, TdUpdate};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoginMethod {
    Phone,
    Qr,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InputField {
    pub text: String,
    /// Byte offset into `text`, always on a char boundary.
    pub cursor: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthField {
    ApiId,
    ApiHash,
    Phone,
    Code,
    Password,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FieldError {
    pub field: AuthField,
    pub error: TdError,
}

/// A PROJECTION of TDLib's authorizationState — never a parallel state machine.
#[derive(Debug, Clone, PartialEq)]
pub struct AuthState {
    pub phase: AuthPhase,
    pub method: Option<LoginMethod>,
    pub api_id: InputField,
    pub api_hash: InputField,
    pub phone: InputField,
    pub code: InputField,
    pub password: InputField,
    pub active_field: AuthField,
    pub field_error: Option<FieldError>,
    /// FLOOD_WAIT rendered as a live countdown against `AppState.now`.
    pub flood_wait_until: Option<Millis>,
    pub in_flight: bool,
}

/// Projects a pre-digested TDLib auth update into wizard state.
///
/// See the module-level docs for why `WaitTdlibParameters` yields no effect.
pub fn handle_td(app: &mut AppState, upd: &TdUpdate) -> Vec<Effect> {
    let TdUpdate::Auth(phase) = upd else {
        return Vec::new();
    };

    // Any TdUpdate::Auth is TDLib reporting the state it is actually in: a
    // genuine advance. A submission that got us here is, by definition, no
    // longer in flight, and any inline error belonged to the phase we just
    // left.
    app.auth.in_flight = false;
    app.auth.field_error = None;
    app.auth.phase = phase.clone();

    match phase {
        AuthPhase::WaitTdlibParameters => Vec::new(),
        AuthPhase::WaitPhoneNumber => Vec::new(),
        AuthPhase::WaitCode { .. } => {
            app.auth.active_field = AuthField::Code;
            Vec::new()
        }
        AuthPhase::WaitPassword { .. } => {
            app.auth.active_field = AuthField::Password;
            Vec::new()
        }
        // The projection IS the storage: a refreshed link simply replaces
        // the stored phase value above; nothing else to do.
        AuthPhase::WaitOtherDeviceConfirmation { .. } => Vec::new(),
        AuthPhase::Ready => {
            app.screen = Screen::Main;
            vec![Effect::Td(TdRequest::LoadChats {
                list: ChatListId::Main,
                limit: 200,
            })]
        }
        AuthPhase::LoggingOut
        | AuthPhase::Closing
        | AuthPhase::Closed
        | AuthPhase::Unsupported { .. } => Vec::new(),
    }
}

/// Routes a key press while the auth screen is showing. `None` means the key
/// was not claimed (only relevant when `app.screen != Screen::Auth`).
pub fn handle_key(app: &mut AppState, key: Key) -> Option<Vec<Effect>> {
    if app.screen != Screen::Auth {
        return None;
    }
    Some(route_auth_key(app, key))
}

fn route_auth_key(app: &mut AppState, key: Key) -> Vec<Effect> {
    // The credentials wizard takes priority: it is driven by `active_field`,
    // not by phase (see module docs).
    if matches!(app.auth.active_field, AuthField::ApiId | AuthField::ApiHash) {
        return handle_credentials_key(app, key);
    }

    match &app.auth.phase {
        AuthPhase::WaitPhoneNumber if app.auth.method.is_none() => {
            handle_method_choice_key(app, key)
        }
        // Picking Qr does not itself fire the request (that would make an
        // arrow-key press cause network I/O): it stays "armed" until Enter,
        // and Up/Down may still flip the choice back to Phone.
        AuthPhase::WaitPhoneNumber if app.auth.method == Some(LoginMethod::Qr) => {
            handle_qr_armed_key(app, key)
        }
        // method == Some(Phone): picking Phone *is* confirming — the next
        // keys type the phone number directly, and Enter submits it.
        AuthPhase::WaitPhoneNumber => handle_submit_field_key(app, key, AuthField::Phone),
        AuthPhase::WaitCode { .. } => handle_submit_field_key(app, key, AuthField::Code),
        AuthPhase::WaitPassword { .. } => handle_submit_field_key(app, key, AuthField::Password),
        // No editable field on these screens (QR, tdlib bootstrap, terminal
        // phases): the key is still claimed (this is the only screen up),
        // just a no-op.
        _ => Vec::new(),
    }
}

fn handle_credentials_key(app: &mut AppState, key: Key) -> Vec<Effect> {
    match key {
        Key::Tab | Key::Down if app.auth.active_field == AuthField::ApiId => {
            app.auth.active_field = AuthField::ApiHash;
            Vec::new()
        }
        Key::BackTab | Key::Up if app.auth.active_field == AuthField::ApiHash => {
            app.auth.active_field = AuthField::ApiId;
            Vec::new()
        }
        Key::Enter if app.auth.active_field == AuthField::ApiId => {
            app.auth.active_field = AuthField::ApiHash;
            Vec::new()
        }
        Key::Enter => submit_credentials(app),
        _ => {
            edit_field(field_mut(app, app.auth.active_field), key);
            Vec::new()
        }
    }
}

fn submit_credentials(app: &mut AppState) -> Vec<Effect> {
    if is_submission_blocked(app) {
        return Vec::new();
    }
    let api_hash = app.auth.api_hash.text.clone();
    match (app.auth.api_id.text.parse::<i32>(), api_hash.is_empty()) {
        (Ok(api_id), false) => {
            app.auth.field_error = None;
            app.auth.active_field = AuthField::Phone;
            vec![Effect::SaveConfig(ConfigPatch::Credentials {
                api_id,
                api_hash,
            })]
        }
        _ => {
            app.auth.field_error = Some(FieldError {
                field: AuthField::ApiId,
                error: TdError::Other {
                    code: 0,
                    message: "api_id must be a number and api_hash must not be empty".to_string(),
                },
            });
            Vec::new()
        }
    }
}

fn handle_method_choice_key(app: &mut AppState, key: Key) -> Vec<Effect> {
    match key {
        Key::Up | Key::Down => {
            app.auth.method = Some(match app.auth.method {
                Some(LoginMethod::Phone) => LoginMethod::Qr,
                Some(LoginMethod::Qr) | None => LoginMethod::Phone,
            });
            Vec::new()
        }
        Key::Char('p') => {
            app.auth.method = Some(LoginMethod::Phone);
            Vec::new()
        }
        Key::Char('q') => {
            app.auth.method = Some(LoginMethod::Qr);
            Vec::new()
        }
        Key::Enter => confirm_method(app),
        _ => Vec::new(),
    }
}

fn confirm_method(app: &mut AppState) -> Vec<Effect> {
    match app.auth.method {
        Some(LoginMethod::Phone) => {
            app.auth.active_field = AuthField::Phone;
            Vec::new()
        }
        Some(LoginMethod::Qr) => request_qr_auth(app),
        None => Vec::new(),
    }
}

/// Handles keys while Qr is the picked-but-not-yet-confirmed method:
/// Enter fires the request, Up/Down flips the choice back to Phone.
fn handle_qr_armed_key(app: &mut AppState, key: Key) -> Vec<Effect> {
    match key {
        Key::Up | Key::Down | Key::Char('p') => {
            app.auth.method = Some(LoginMethod::Phone);
            Vec::new()
        }
        Key::Enter => request_qr_auth(app),
        _ => Vec::new(),
    }
}

fn request_qr_auth(app: &mut AppState) -> Vec<Effect> {
    if is_submission_blocked(app) {
        return Vec::new();
    }
    app.auth.in_flight = true;
    vec![Effect::Td(TdRequest::RequestQrCodeAuthentication)]
}

fn handle_submit_field_key(app: &mut AppState, key: Key, field: AuthField) -> Vec<Effect> {
    match key {
        Key::Enter => submit_field(app, field),
        _ => {
            edit_field(field_mut(app, field), key);
            Vec::new()
        }
    }
}

fn submit_field(app: &mut AppState, field: AuthField) -> Vec<Effect> {
    if is_submission_blocked(app) {
        return Vec::new();
    }
    let request = match field {
        AuthField::Phone => TdRequest::SetAuthenticationPhoneNumber {
            phone: app.auth.phone.text.clone(),
        },
        AuthField::Code => TdRequest::CheckAuthenticationCode {
            code: app.auth.code.text.clone(),
        },
        AuthField::Password => TdRequest::CheckAuthenticationPassword {
            password: app.auth.password.text.clone(),
        },
        AuthField::ApiId | AuthField::ApiHash => {
            debug_assert!(false, "credentials wizard handled separately");
            return Vec::new();
        }
    };
    app.auth.in_flight = true;
    app.auth.field_error = None;
    vec![Effect::Td(request)]
}

fn is_submission_blocked(app: &AppState) -> bool {
    app.auth.in_flight
        || app
            .auth
            .flood_wait_until
            .is_some_and(|until| until > app.now)
}

fn field_mut(app: &mut AppState, field: AuthField) -> &mut InputField {
    match field {
        AuthField::ApiId => &mut app.auth.api_id,
        AuthField::ApiHash => &mut app.auth.api_hash,
        AuthField::Phone => &mut app.auth.phone,
        AuthField::Code => &mut app.auth.code,
        AuthField::Password => &mut app.auth.password,
    }
}

/// Char-boundary-safe single-line text editing, shared by every auth field.
fn edit_field(field: &mut InputField, key: Key) -> bool {
    match key {
        Key::Char(c) => {
            field.text.insert(field.cursor, c);
            field.cursor += c.len_utf8();
            true
        }
        Key::Backspace => {
            if field.cursor > 0 {
                let prev = prev_char_boundary(&field.text, field.cursor);
                field.text.replace_range(prev..field.cursor, "");
                field.cursor = prev;
            }
            true
        }
        Key::Delete => {
            if field.cursor < field.text.len() {
                let next = next_char_boundary(&field.text, field.cursor);
                field.text.replace_range(field.cursor..next, "");
            }
            true
        }
        Key::Left => {
            if field.cursor > 0 {
                field.cursor = prev_char_boundary(&field.text, field.cursor);
            }
            true
        }
        Key::Right => {
            if field.cursor < field.text.len() {
                field.cursor = next_char_boundary(&field.text, field.cursor);
            }
            true
        }
        Key::Home => {
            field.cursor = 0;
            true
        }
        Key::End => {
            field.cursor = field.text.len();
            true
        }
        _ => false,
    }
}

fn prev_char_boundary(s: &str, idx: usize) -> usize {
    if idx == 0 {
        return 0;
    }
    let mut i = idx - 1;
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

fn next_char_boundary(s: &str, idx: usize) -> usize {
    if idx >= s.len() {
        return s.len();
    }
    let mut i = idx + 1;
    while i < s.len() && !s.is_char_boundary(i) {
        i += 1;
    }
    i
}

/// Completion of a dispatched auth `Effect::Td` request
/// (`Action::TdResult(TdResult::AuthRequestDone { outcome })`).
pub fn handle_td_result(app: &mut AppState, outcome: &Result<(), TdError>) -> Vec<Effect> {
    match outcome {
        Ok(()) => {
            app.auth.in_flight = false;
            // Phase advance (and any associated active_field change) arrives
            // separately as a TdUpdate::Auth and is handled by `handle_td`.
        }
        Err(TdError::FloodWait { seconds }) => {
            app.auth.in_flight = false;
            app.auth.flood_wait_until = Some(app.now.saturating_add(u64::from(*seconds) * 1_000));
        }
        Err(e) => {
            app.auth.in_flight = false;
            app.auth.field_error = Some(FieldError {
                field: app.auth.active_field,
                error: e.clone(),
            });
        }
    }
    Vec::new()
}

/// Clears an expired flood-wait countdown against injected time.
pub fn handle_tick(app: &mut AppState, now: Millis) -> Vec<Effect> {
    if let Some(until) = app.auth.flood_wait_until
        && now >= until
    {
        app.auth.flood_wait_until = None;
    }
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effect::TelemetryMode;
    use crate::state::chat_list::ChatListState;
    use crate::state::composer::ComposerState;
    use crate::state::consent::{ConsentChoice, ConsentState};
    use crate::state::focus::{Focus, FocusStack};
    use crate::state::media::MediaState;
    use crate::state::presence::PresenceState;
    use crate::state::toasts::ToastState;
    use crate::td::update::ConnectionPhase;
    use std::collections::HashMap;

    /// Mirrors `App::new`'s construction (`App::state()` is read-only, so
    /// tests build `AppState` directly; every field is `pub`).
    fn fixture_state() -> AppState {
        AppState {
            screen: Screen::Auth,
            focus: FocusStack::new(Focus::ChatList),
            connection: ConnectionPhase::WaitingForNetwork,
            consent: ConsentState {
                selected: ConsentChoice::Enable,
                acknowledged: false,
            },
            auth: AuthState {
                phase: AuthPhase::WaitTdlibParameters,
                method: None,
                api_id: InputField::default(),
                api_hash: InputField::default(),
                phone: InputField::default(),
                code: InputField::default(),
                password: InputField::default(),
                active_field: AuthField::Phone,
                field_error: None,
                flood_wait_until: None,
                in_flight: false,
            },
            chat_list: ChatListState::default(),
            conversations: HashMap::new(),
            open_chat: None,
            composer: ComposerState::default(),
            modal_ui: None,
            palette: None,
            chat_search: None,
            toasts: ToastState::default(),
            media: MediaState::default(),
            presence: PresenceState::default(),
            width: 120,
            height: 40,
            layout_breakpoint_cols: 100,
            theme_name: "dark".to_string(),
            theme_generation: 0,
            bindings: crate::model::key::KeyBindings::default(),
            telemetry_mode: TelemetryMode::Off,
            telemetry_salt: [0u8; 32],
            now: Millis(0),
        }
    }

    fn type_text(app: &mut AppState, text: &str) {
        for c in text.chars() {
            handle_key(app, Key::Char(c));
        }
    }

    #[test]
    fn wait_code_renders_delivery_and_submits_check_code() {
        let mut app = fixture_state();
        app.auth.phase = AuthPhase::WaitPhoneNumber;
        app.auth.method = Some(LoginMethod::Phone);
        app.auth.active_field = AuthField::Phone;

        let effects = handle_td(
            &mut app,
            &TdUpdate::Auth(AuthPhase::WaitCode {
                delivery_hint: "SMS to +1***34".to_string(),
                length: 5,
            }),
        );
        assert!(effects.is_empty());
        assert_eq!(app.auth.active_field, AuthField::Code);
        assert!(matches!(app.auth.phase, AuthPhase::WaitCode { .. }));

        type_text(&mut app, "12345");
        let effects = handle_key(&mut app, Key::Enter).expect("auth screen claims Enter");
        assert_eq!(effects.len(), 1);
        assert!(matches!(
            effects[0],
            Effect::Td(TdRequest::CheckAuthenticationCode { ref code }) if code == "12345"
        ));
        assert!(app.auth.in_flight);
    }

    #[test]
    fn wrong_code_error_lands_on_code_field_and_preserves_phase() {
        let mut app = fixture_state();
        app.auth.phase = AuthPhase::WaitCode {
            delivery_hint: "SMS".to_string(),
            length: 5,
        };
        app.auth.active_field = AuthField::Code;
        app.auth.code.text = "00000".to_string();
        app.auth.code.cursor = 5;
        app.auth.in_flight = true;

        let effects = handle_td_result(&mut app, &Err(TdError::CodeInvalid));
        assert!(effects.is_empty());
        assert!(!app.auth.in_flight);
        assert_eq!(
            app.auth.field_error,
            Some(FieldError {
                field: AuthField::Code,
                error: TdError::CodeInvalid,
            })
        );
        // Phase is untouched: the error came back on the same phase, no
        // TdUpdate::Auth arrived.
        assert!(matches!(app.auth.phase, AuthPhase::WaitCode { .. }));
    }

    #[test]
    fn flood_wait_disables_submit_until_deadline() {
        let mut app = fixture_state();
        app.auth.phase = AuthPhase::WaitCode {
            delivery_hint: "SMS".to_string(),
            length: 5,
        };
        app.auth.active_field = AuthField::Code;
        app.auth.code.text = "12345".to_string();
        app.auth.code.cursor = 5;
        app.auth.in_flight = true;
        app.now = Millis(1_000);

        handle_td_result(&mut app, &Err(TdError::FloodWait { seconds: 2 }));
        assert!(!app.auth.in_flight);
        assert_eq!(app.auth.flood_wait_until, Some(Millis(3_000)));

        // Still within the window: Enter is claimed but produces no effect.
        app.now = Millis(2_500);
        let effects = handle_key(&mut app, Key::Enter).expect("auth screen claims Enter");
        assert!(effects.is_empty());
        assert!(app.auth.flood_wait_until.is_some());

        // Tick past the deadline clears the countdown.
        handle_tick(&mut app, Millis(3_000));
        assert_eq!(app.auth.flood_wait_until, None);

        // Submission now succeeds.
        app.now = Millis(3_000);
        let effects = handle_key(&mut app, Key::Enter).expect("auth screen claims Enter");
        assert_eq!(effects.len(), 1);
        assert!(matches!(
            effects[0],
            Effect::Td(TdRequest::CheckAuthenticationCode { .. })
        ));
    }

    #[test]
    fn qr_link_refresh_replaces_link() {
        let mut app = fixture_state();
        app.auth.phase = AuthPhase::WaitPhoneNumber;
        app.auth.method = Some(LoginMethod::Qr);

        handle_td(
            &mut app,
            &TdUpdate::Auth(AuthPhase::WaitOtherDeviceConfirmation {
                link: "tg://login?token=AAA".to_string(),
            }),
        );
        assert_eq!(
            app.auth.phase,
            AuthPhase::WaitOtherDeviceConfirmation {
                link: "tg://login?token=AAA".to_string(),
            }
        );

        handle_td(
            &mut app,
            &TdUpdate::Auth(AuthPhase::WaitOtherDeviceConfirmation {
                link: "tg://login?token=BBB".to_string(),
            }),
        );
        assert_eq!(
            app.auth.phase,
            AuthPhase::WaitOtherDeviceConfirmation {
                link: "tg://login?token=BBB".to_string(),
            }
        );
    }

    #[test]
    fn ready_switches_screen_to_main_and_loads_chats() {
        let mut app = fixture_state();
        app.auth.phase = AuthPhase::WaitPassword { hint: None };

        let effects = handle_td(&mut app, &TdUpdate::Auth(AuthPhase::Ready));
        assert_eq!(app.screen, Screen::Main);
        assert_eq!(effects.len(), 1);
        assert!(matches!(
            effects[0],
            Effect::Td(TdRequest::LoadChats {
                list: ChatListId::Main,
                limit: 200,
            })
        ));
    }

    #[test]
    fn no_credentials_shows_wizard_and_saves_config_patch() {
        let mut app = fixture_state();
        // Contract: whoever boots without credentials starts the wizard at
        // ApiId (T14's responsibility in App::new; simulated here).
        app.auth.active_field = AuthField::ApiId;

        type_text(&mut app, "12345");
        let effects = handle_key(&mut app, Key::Tab).expect("auth screen claims Tab");
        assert!(effects.is_empty());
        assert_eq!(app.auth.active_field, AuthField::ApiHash);

        type_text(&mut app, "deadbeefcafebabe");
        let effects = handle_key(&mut app, Key::Enter).expect("auth screen claims Enter");

        assert_eq!(effects.len(), 1);
        assert!(matches!(
            &effects[0],
            Effect::SaveConfig(ConfigPatch::Credentials { api_id, api_hash })
                if *api_id == 12345 && api_hash == "deadbeefcafebabe"
        ));
        assert_eq!(app.auth.active_field, AuthField::Phone);
        assert_eq!(app.auth.field_error, None);
    }

    #[test]
    fn invalid_api_id_blocks_submission_with_field_error() {
        let mut app = fixture_state();
        app.auth.active_field = AuthField::ApiId;
        type_text(&mut app, "not-a-number");
        handle_key(&mut app, Key::Tab);
        type_text(&mut app, "somehash");

        let effects = handle_key(&mut app, Key::Enter).expect("auth screen claims Enter");
        assert!(effects.is_empty());
        assert_eq!(app.auth.active_field, AuthField::ApiHash);
        assert!(app.auth.field_error.is_some());
    }

    #[test]
    fn backspace_is_char_boundary_safe_on_multibyte_input() {
        let mut app = fixture_state();
        app.auth.phase = AuthPhase::WaitCode {
            delivery_hint: "SMS".to_string(),
            length: 5,
        };
        app.auth.active_field = AuthField::Code;

        type_text(&mut app, "a🙂b");
        handle_key(&mut app, Key::Backspace);
        assert_eq!(app.auth.code.text, "a🙂");
        handle_key(&mut app, Key::Backspace);
        assert_eq!(app.auth.code.text, "a");
    }

    #[test]
    fn method_choice_picks_qr_and_requests_qr_auth() {
        let mut app = fixture_state();
        app.auth.phase = AuthPhase::WaitPhoneNumber;
        app.auth.method = None;

        handle_key(&mut app, Key::Char('q'));
        assert_eq!(app.auth.method, Some(LoginMethod::Qr));

        let effects = handle_key(&mut app, Key::Enter).expect("auth screen claims Enter");
        assert_eq!(effects.len(), 1);
        assert!(matches!(
            effects[0],
            Effect::Td(TdRequest::RequestQrCodeAuthentication)
        ));
        assert!(app.auth.in_flight);
    }

    #[test]
    fn handle_key_unclaimed_outside_auth_screen() {
        let mut app = fixture_state();
        app.screen = Screen::Main;
        assert!(handle_key(&mut app, Key::Char('x')).is_none());
    }
}
