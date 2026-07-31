//! Modal lifecycle: confirm-delete, confirm-send-file. `ModalKind` — the
//! modal's identity and immutable parameters — lives on the focus stack
//! (`Focus::Modal(ModalKind)`, `state/focus.rs`, architecture §4.5) and is
//! not duplicated here.
//!
//! What this module owns is the modal's *transient UI state*: for
//! `ConfirmDelete`, a cursor over its two options ("Delete for me" /
//! "Delete for everyone", spec §6.3). It lives in `AppState::modal_ui`
//! (T28's placement decision), where the router creates it on push and drops
//! it on pop, but `handle_key` below still takes it as an explicit second
//! parameter rather than reaching through `app`: a `&mut` field of the same
//! struct cannot be borrowed alongside `&mut AppState`. This is a
//! deliberate, documented deviation from the canonical
//! `fn handle_key(app: &mut AppState, key: Key) -> Option<Vec<Effect>>`
//! shape, scoped to this file.
//!
//! See docs/architecture.md §4.5 (`ModalKind`), §4.4 (`Effect`), §4.7
//! (`TdRequest::DeleteMessages`); spec §6.3 (destructive confirmation UX).

use crate::app::AppState;
use crate::effect::Effect;
use crate::model::key::Key;
use crate::state::focus::{Focus, ModalKind};
use crate::td::request::{OutgoingFileKind, TdRequest};

/// Which of `ConfirmDelete`'s two options is selected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeleteChoice {
    ForMe,
    ForEveryone,
}

/// Transient per-modal UI state that outlives a single keystroke but not the
/// modal itself; stored in `AppState::modal_ui`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ModalState {
    /// 0 = "Delete for me", 1 = "Delete for everyone". Meaningless for any
    /// modal kind other than `ConfirmDelete`, and clamped back to `ForMe` by
    /// `delete_choice` whenever `can_revoke` is false.
    pub cursor: usize,
}

impl ModalState {
    /// The choice implied by `cursor`, clamped to `ForMe` when the chat
    /// doesn't allow revoking (mirrors `can_be_deleted_for_all_users`).
    pub fn delete_choice(&self, can_revoke: bool) -> DeleteChoice {
        if can_revoke && self.cursor != 0 {
            DeleteChoice::ForEveryone
        } else {
            DeleteChoice::ForMe
        }
    }
}

/// Handles a key while a modal has focus. The active `ModalKind` comes from
/// `app.focus.current()`; `modal_ui` is this modal's cursor (see module
/// doc comment for why it isn't reached through `app`).
///
/// Returns `None` for `Esc` — dismissal emits no effect; the router (T28)
/// pops the modal focus regardless of this return value, so "no effect" is
/// all `None` needs to mean here. Returns `Some(effects)` on confirm.
/// Any other key while a modal is focused is claimed (modals swallow
/// everything shown to them) and returns `Some(vec![])`. If no modal is
/// actually focused there is nothing for this handler to claim, so it
/// returns `None` in the ordinary "unclaimed" sense.
pub fn handle_key(app: &mut AppState, modal_ui: &mut ModalState, key: Key) -> Option<Vec<Effect>> {
    let Focus::Modal(kind) = app.focus.current().clone() else {
        return None;
    };

    match kind {
        ModalKind::ConfirmDelete {
            chat_id,
            message_id,
            can_revoke,
        } => match key {
            Key::Esc => None,
            Key::Enter => {
                let revoke = modal_ui.delete_choice(can_revoke) == DeleteChoice::ForEveryone;
                Some(vec![Effect::Td(TdRequest::DeleteMessages {
                    chat_id,
                    message_ids: vec![message_id],
                    revoke,
                })])
            }
            Key::Up | Key::Down | Key::Left | Key::Right => {
                if can_revoke {
                    modal_ui.cursor = if modal_ui.cursor == 0 { 1 } else { 0 };
                } else {
                    // Nothing to toggle to: the only option is "for me".
                    modal_ui.cursor = 0;
                }
                Some(Vec::new())
            }
            _ => Some(Vec::new()),
        },
        // `path` here came either from a parsed `/send <path>` (core does
        // not validate existence — no filesystem access, architecture §9.3)
        // or from a pasted bare path the app layer already confirmed exists
        // (`crates/app/src/media_kind.rs::existing_path`). Either way this
        // modal has no way to tell which, so both are treated the same: no
        // open chat is the only local no-op condition; anything else about
        // the path's validity surfaces later as a TDLib send failure.
        ModalKind::ConfirmSendFile { path } => match key {
            Key::Esc => None,
            Key::Enter => {
                let Some(chat_id) = app.open_chat else {
                    return Some(Vec::new());
                };
                // `kind` is always `Document` from core: core cannot sniff a
                // file's MIME type (no filesystem access). The dispatcher
                // (`crates/app/src/dispatch.rs`, wired in T40) upgrades
                // `kind` via `media_kind::kind_for(&path)` before the
                // request ever reaches TDLib.
                Some(vec![Effect::Td(TdRequest::SendMessageFile {
                    chat_id,
                    path,
                    kind: OutgoingFileKind::Document,
                    caption: None,
                })])
            }
            _ => Some(Vec::new()),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::Screen;
    use crate::effect::TelemetryMode;
    use crate::model::ids::{ChatId, MessageId};
    use crate::state::auth::{AuthField, AuthState, InputField};
    use crate::state::chat_list::ChatListState;
    use crate::state::composer::ComposerState;
    use crate::state::consent::{ConsentChoice, ConsentState};
    use crate::state::focus::FocusStack;
    use crate::state::media::MediaState;
    use crate::state::presence::PresenceState;
    use crate::state::toasts::ToastState;
    use crate::td::update::{AuthPhase, ConnectionPhase};
    use std::collections::HashMap;
    use std::path::PathBuf;

    const CHAT: ChatId = ChatId(1);
    const MSG: MessageId = MessageId(42);

    /// Mirrors `App::new`'s construction (`App::state()` is read-only, so
    /// tests build `AppState` directly; every field is `pub`).
    fn fixture_state(focus: Focus) -> AppState {
        AppState {
            screen: Screen::Main,
            focus: FocusStack::new(focus),
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
            conversations: HashMap::new(),
            open_chat: Some(CHAT),
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
            now: crate::model::time::Millis(0),
        }
    }

    fn confirm_delete(can_revoke: bool) -> Focus {
        Focus::Modal(ModalKind::ConfirmDelete {
            chat_id: CHAT,
            message_id: MSG,
            can_revoke,
        })
    }

    /// `Effect` derives no `PartialEq` (it isn't this module's to add), so
    /// assertions pattern-match instead of comparing whole `Vec<Effect>`s.
    fn assert_claimed_no_effects(effects: Option<Vec<Effect>>) {
        match effects {
            Some(v) => assert!(v.is_empty(), "expected no effects, got {v:?}"),
            None => panic!("expected the key to be claimed with no effects, got None"),
        }
    }

    fn assert_delete_request(effects: Option<Vec<Effect>>, expected_revoke: bool) {
        let effects = effects.expect("confirm must claim the key");
        assert_eq!(effects.len(), 1, "expected exactly one effect: {effects:?}");
        match &effects[0] {
            Effect::Td(TdRequest::DeleteMessages {
                chat_id,
                message_ids,
                revoke,
            }) => {
                assert_eq!(*chat_id, CHAT);
                assert_eq!(message_ids, &vec![MSG]);
                assert_eq!(*revoke, expected_revoke);
            }
            other => panic!("expected Effect::Td(DeleteMessages), got {other:?}"),
        }
    }

    #[test]
    fn revoke_option_present_only_when_capable() {
        let mut app = fixture_state(confirm_delete(false));
        let mut modal_ui = ModalState::default();

        // Toggling with the chat incapable of revoke never leaves "for me".
        for key in [Key::Down, Key::Right, Key::Up, Key::Left] {
            let effects = handle_key(&mut app, &mut modal_ui, key);
            assert_claimed_no_effects(effects);
            assert_eq!(modal_ui.cursor, 0);
            assert_eq!(modal_ui.delete_choice(false), DeleteChoice::ForMe);
        }

        // Even a stray non-zero cursor is clamped by `delete_choice` itself.
        modal_ui.cursor = 1;
        assert_eq!(modal_ui.delete_choice(false), DeleteChoice::ForMe);

        // Enter confirms with revoke = false regardless of cursor.
        let effects = handle_key(&mut app, &mut modal_ui, Key::Enter);
        assert_delete_request(effects, false);
    }

    #[test]
    fn confirm_emits_delete_with_revoke_flag() {
        let mut app = fixture_state(confirm_delete(true));
        let mut modal_ui = ModalState::default();

        // Cursor at "Delete for me" (default).
        let effects = handle_key(&mut app, &mut modal_ui, Key::Enter);
        assert_delete_request(effects, false);

        // Toggle to "Delete for everyone" and confirm again.
        let toggled = handle_key(&mut app, &mut modal_ui, Key::Down);
        assert_claimed_no_effects(toggled);
        assert_eq!(modal_ui.cursor, 1);

        let effects = handle_key(&mut app, &mut modal_ui, Key::Enter);
        assert_delete_request(effects, true);
    }

    #[test]
    fn esc_dismisses_without_effect() {
        let mut app = fixture_state(confirm_delete(true));
        let mut modal_ui = ModalState::default();
        assert!(handle_key(&mut app, &mut modal_ui, Key::Esc).is_none());

        let mut app = fixture_state(Focus::Modal(ModalKind::ConfirmSendFile {
            path: "/tmp/example.txt".into(),
        }));
        let mut modal_ui = ModalState::default();
        assert!(handle_key(&mut app, &mut modal_ui, Key::Esc).is_none());
    }

    #[test]
    fn confirm_send_file_emits_send_message_file() {
        let mut app = fixture_state(Focus::Modal(ModalKind::ConfirmSendFile {
            path: "/tmp/example.txt".into(),
        }));
        let mut modal_ui = ModalState::default();

        let effects = handle_key(&mut app, &mut modal_ui, Key::Enter);

        let effects = effects.expect("confirm must claim the key");
        assert_eq!(effects.len(), 1, "expected exactly one effect: {effects:?}");
        match &effects[0] {
            Effect::Td(TdRequest::SendMessageFile {
                chat_id,
                path,
                kind,
                caption,
            }) => {
                assert_eq!(*chat_id, CHAT);
                assert_eq!(path, &PathBuf::from("/tmp/example.txt"));
                // Core cannot sniff MIME; the dispatcher upgrades this later.
                assert_eq!(*kind, OutgoingFileKind::Document);
                assert!(caption.is_none());
            }
            other => panic!("expected Effect::Td(SendMessageFile), got {other:?}"),
        }
    }

    #[test]
    fn confirm_send_file_without_open_chat_is_a_noop() {
        let mut app = fixture_state(Focus::Modal(ModalKind::ConfirmSendFile {
            path: "/tmp/example.txt".into(),
        }));
        app.open_chat = None;
        let mut modal_ui = ModalState::default();

        let effects = handle_key(&mut app, &mut modal_ui, Key::Enter);

        assert_claimed_no_effects(effects);
    }

    #[test]
    fn no_modal_focused_is_unclaimed() {
        let mut app = fixture_state(Focus::ChatList);
        let mut modal_ui = ModalState::default();
        assert!(handle_key(&mut app, &mut modal_ui, Key::Enter).is_none());
    }
}
