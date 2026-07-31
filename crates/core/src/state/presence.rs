//! Presence/typing state. See docs/architecture.md §4.6. Handlers land in T34.

use std::collections::HashMap;

use crate::app::AppState;
use crate::effect::Effect;
use crate::model::ids::{ChatId, UserId};
use crate::model::time::Millis;
use crate::td::update::{PresenceStatus, TdUpdate};

pub const TYPING_TTL_MS: u64 = 6_000;

#[derive(Debug, Default)]
pub struct PresenceState {
    pub users: HashMap<UserId, PresenceStatus>,
    /// (chat, user) → expiry; swept on Tick.
    pub typing: HashMap<(ChatId, UserId), Millis>,
}

/// `UserStatus` updates the online/offline/recently projection; `ChatAction`
/// tracks per-(chat, user) typing with an expiry timestamp so a dropped
/// "stopped typing" push (network hiccup, client crash) can't leave a stale
/// indicator on screen forever — `handle_tick` sweeps anything that outlives
/// `TYPING_TTL_MS` regardless of whether the matching `is_typing: false` ever
/// arrives.
pub fn handle_td(app: &mut AppState, upd: &TdUpdate) -> Vec<Effect> {
    match upd {
        TdUpdate::UserStatus { user_id, status } => {
            app.presence.users.insert(*user_id, *status);
        }
        TdUpdate::ChatAction {
            chat_id,
            user_id,
            is_typing,
        } => {
            if *is_typing {
                app.presence
                    .typing
                    .insert((*chat_id, *user_id), app.now.saturating_add(TYPING_TTL_MS));
            } else {
                app.presence.typing.remove(&(*chat_id, *user_id));
            }
        }
        _ => {}
    }
    Vec::new()
}

/// Sweeps typing entries whose expiry has passed. Never touches `users` —
/// online/offline status has no TTL of its own, only `ChatAction` does.
pub fn handle_tick(app: &mut AppState, now: Millis) -> Vec<Effect> {
    app.presence.typing.retain(|_, expiry| *expiry > now);
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::Screen;
    use crate::effect::TelemetryMode;
    use crate::state::auth::{AuthField, AuthState, InputField};
    use crate::state::chat_list::ChatListState;
    use crate::state::composer::ComposerState;
    use crate::state::consent::{ConsentChoice, ConsentState};
    use crate::state::focus::{Focus, FocusStack};
    use crate::state::media::MediaState;
    use crate::state::toasts::ToastState;
    use crate::td::update::{AuthPhase, ConnectionPhase};
    use std::collections::HashMap as StdHashMap;

    const CHAT: ChatId = ChatId(1);
    const USER: UserId = UserId(7);

    /// Mirrors `App::new`'s construction (`App::state()` is read-only, so
    /// tests build `AppState` directly; every field is `pub`).
    fn fixture_state() -> AppState {
        AppState {
            screen: Screen::Main,
            focus: FocusStack::new(Focus::ChatList),
            connection: ConnectionPhase::Ready,
            consent: ConsentState {
                selected: ConsentChoice::Enable,
                acknowledged: true,
            },
            auth: AuthState {
                phase: AuthPhase::Ready,
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
            conversations: StdHashMap::new(),
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

    #[test]
    fn user_status_updates() {
        let mut app = fixture_state();
        handle_td(
            &mut app,
            &TdUpdate::UserStatus {
                user_id: USER,
                status: PresenceStatus::Online,
            },
        );
        assert_eq!(app.presence.users.get(&USER), Some(&PresenceStatus::Online));

        handle_td(
            &mut app,
            &TdUpdate::UserStatus {
                user_id: USER,
                status: PresenceStatus::Recently,
            },
        );
        assert_eq!(
            app.presence.users.get(&USER),
            Some(&PresenceStatus::Recently)
        );
    }

    #[test]
    fn typing_expires_after_ttl() {
        let mut app = fixture_state();
        app.now = Millis(1_000);
        handle_td(
            &mut app,
            &TdUpdate::ChatAction {
                chat_id: CHAT,
                user_id: USER,
                is_typing: true,
            },
        );
        assert!(app.presence.typing.contains_key(&(CHAT, USER)));

        // Not yet expired.
        handle_tick(&mut app, Millis(1_000 + TYPING_TTL_MS - 1));
        assert!(app.presence.typing.contains_key(&(CHAT, USER)));

        // Past the TTL: swept.
        handle_tick(&mut app, Millis(1_000 + TYPING_TTL_MS + 1));
        assert!(!app.presence.typing.contains_key(&(CHAT, USER)));
    }

    #[test]
    fn typing_cleared_when_not_typing() {
        let mut app = fixture_state();
        app.now = Millis(1_000);
        handle_td(
            &mut app,
            &TdUpdate::ChatAction {
                chat_id: CHAT,
                user_id: USER,
                is_typing: true,
            },
        );
        assert!(app.presence.typing.contains_key(&(CHAT, USER)));

        handle_td(
            &mut app,
            &TdUpdate::ChatAction {
                chat_id: CHAT,
                user_id: USER,
                is_typing: false,
            },
        );
        assert!(!app.presence.typing.contains_key(&(CHAT, USER)));
    }

    #[test]
    fn handle_tick_leaves_unexpired_entries_and_user_status_alone() {
        let mut app = fixture_state();
        app.now = Millis(0);
        handle_td(
            &mut app,
            &TdUpdate::UserStatus {
                user_id: USER,
                status: PresenceStatus::Online,
            },
        );
        handle_td(
            &mut app,
            &TdUpdate::ChatAction {
                chat_id: CHAT,
                user_id: USER,
                is_typing: true,
            },
        );

        handle_tick(&mut app, Millis(500));

        assert!(app.presence.typing.contains_key(&(CHAT, USER)));
        assert_eq!(app.presence.users.get(&USER), Some(&PresenceStatus::Online));
    }
}
