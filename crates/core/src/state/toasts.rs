//! Toast queue state and handlers. See docs/architecture.md §4.6, spec §6.4.
//!
//! `on_new_message` is called by `App::update`'s `TdUpdate::NewMessage` arm
//! (T45 wires the call site; this module only owns the decision + the
//! queue). Suppression rules, in order:
//!
//! 1. Outgoing messages (our own echo) never toast.
//! 2. The chat currently open in the conversation pane never toasts — the
//!    user is already looking at it.
//! 3. A chat muted via `updateChatNotificationSettings`
//!    (`ChatView.is_muted`) never toasts and never rings the terminal bell.
//!    Its unread/mention badge still updates, but that happens in
//!    `chat_list`, not here.
//!
//! Anything else pushes a `Toast` (title/body are in-app only, per the
//! doc-comment on `Toast`) and returns `Effect::Alert`, whose own payload is
//! structurally empty (see `core/src/effect.rs` and `tgt-app`'s `notify.rs`).

use std::collections::VecDeque;

use crate::app::AppState;
use crate::effect::Effect;
use crate::model::ids::ChatId;
use crate::model::message::{MessageContent, MessageView};
use crate::model::time::Millis;

pub const TOAST_MAX: usize = 3;
pub const TOAST_TTL_MS: u64 = 4_000;

/// In-app only: title/body may contain chat titles and message text because
/// they never leave the terminal cell grid. Effect::Alert (the escape-sequence
/// path) carries no payload at all.
#[derive(Debug, Clone, PartialEq)]
pub struct Toast {
    pub chat_id: ChatId,
    pub title: String,
    pub body: String,
    pub expires_at: Millis,
}

#[derive(Debug, Default)]
pub struct ToastState {
    pub toasts: VecDeque<Toast>,
}

/// Decides whether `msg` should raise a toast/alert, and if so, enqueues it.
///
/// Returns `vec![Effect::Alert]` when a toast was raised, `Vec::new()`
/// otherwise (suppressed, per the module doc's ordered rules).
pub fn on_new_message(app: &mut AppState, msg: &MessageView) -> Vec<Effect> {
    if msg.is_outgoing {
        return Vec::new();
    }
    if app.open_chat == Some(msg.chat_id) {
        return Vec::new();
    }
    let chat = app.chat_list.chats.get(&msg.chat_id);
    if chat.is_some_and(|c| c.is_muted) {
        return Vec::new();
    }

    let title = chat
        .map(|c| c.title.clone())
        .unwrap_or_else(|| "New message".to_string());
    let toast = Toast {
        chat_id: msg.chat_id,
        title,
        body: preview_text(&msg.content),
        expires_at: app.now.saturating_add(TOAST_TTL_MS),
    };
    app.toasts.toasts.push_back(toast);
    while app.toasts.toasts.len() > TOAST_MAX {
        app.toasts.toasts.pop_front();
    }
    vec![Effect::Alert]
}

/// Sweeps toasts whose TTL has elapsed. Mirrors `presence::handle_tick`'s
/// `expiry > now` convention: a toast expires the instant `now` reaches its
/// `expires_at`.
pub fn handle_tick(app: &mut AppState, now: Millis) -> Vec<Effect> {
    app.toasts.toasts.retain(|t| t.expires_at > now);
    Vec::new()
}

/// `esc` dismisses exactly one toast: the newest (the one at the bottom of
/// the on-screen stack, per `view/toast.rs`'s newest-at-bottom layout —
/// visually closest to the corner the eye lands on, so it is the one `esc`
/// removes first). Returns `false` (no-op) when the stack is empty, so the
/// router can tell whether it should keep looking for another `esc`
/// handler.
pub fn dismiss_newest(app: &mut AppState) -> bool {
    app.toasts.toasts.pop_back().is_some()
}

/// A short, human-readable summary of a message's content for the in-app
/// toast body. Unlike `Effect::Alert`'s generic terminal body, this text
/// never leaves the terminal cell grid, so it is fine for it to name the
/// content (spec §6.4's PII restriction is scoped to the escape sequence).
fn preview_text(content: &MessageContent) -> String {
    match content {
        MessageContent::Text(text) => text.text.clone(),
        MessageContent::Photo { caption, .. } => non_empty_or(&caption.text, "Photo"),
        MessageContent::Video { caption, .. } => non_empty_or(&caption.text, "Video"),
        MessageContent::Audio { file_name, .. } => non_empty_or(file_name, "Audio"),
        MessageContent::Document { file_name, .. } => non_empty_or(file_name, "Document"),
        MessageContent::Sticker { emoji } => {
            if emoji.is_empty() {
                "Sticker".to_string()
            } else {
                format!("{emoji} Sticker")
            }
        }
        MessageContent::Unsupported { description } => description.clone(),
    }
}

fn non_empty_or(text: &str, fallback: &str) -> String {
    if text.is_empty() {
        fallback.to_string()
    } else {
        text.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::Screen;
    use crate::effect::TelemetryMode;
    use crate::model::chat::{ChatKind, ChatView};
    use crate::model::entity::FormattedText;
    use crate::model::ids::{FileId, MessageId, UserId};
    use crate::model::message::{MessageCaps, SendState, Sender};
    use crate::state::auth::{AuthField, AuthState, InputField};
    use crate::state::chat_list::ChatListState;
    use crate::state::composer::ComposerState;
    use crate::state::consent::{ConsentChoice, ConsentState};
    use crate::state::focus::{Focus, FocusStack};
    use crate::state::media::MediaState;
    use crate::state::presence::PresenceState;
    use crate::td::update::{AuthPhase, ConnectionPhase};
    use std::collections::HashMap;

    const CHAT_A: ChatId = ChatId(1);
    const CHAT_B: ChatId = ChatId(2);
    const CHAT_C: ChatId = ChatId(3);

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

    fn chat(id: ChatId, title: &str, is_muted: bool) -> ChatView {
        ChatView {
            id,
            kind: ChatKind::Private,
            title: title.to_string(),
            positions: Vec::new(),
            unread_count: 0,
            unread_mention_count: 0,
            last_message: None,
            is_muted,
        }
    }

    fn message(chat_id: ChatId, text: &str, is_outgoing: bool) -> MessageView {
        MessageView {
            id: MessageId(1),
            chat_id,
            sender: Sender::User(UserId(9)),
            sender_name: "Ada".to_string(),
            is_outgoing,
            date: 1_700_000_000,
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

    #[test]
    fn toast_only_for_unfocused_unmuted_chats() {
        let mut app = fixture_state();
        app.chat_list
            .chats
            .insert(CHAT_A, chat(CHAT_A, "Alice", false));
        app.chat_list
            .chats
            .insert(CHAT_B, chat(CHAT_B, "Bob", false));
        app.open_chat = Some(CHAT_B);

        // Unfocused, unmuted: toasts and alerts.
        let effects = on_new_message(&mut app, &message(CHAT_A, "hi", false));
        assert_eq!(effects.len(), 1);
        assert!(matches!(effects[0], Effect::Alert));
        assert_eq!(app.toasts.toasts.len(), 1);
        assert_eq!(app.toasts.toasts[0].title, "Alice");
        assert_eq!(app.toasts.toasts[0].body, "hi");

        // Focused chat: suppressed entirely.
        let effects = on_new_message(&mut app, &message(CHAT_B, "hey", false));
        assert!(effects.is_empty());
        assert_eq!(app.toasts.toasts.len(), 1);

        // Outgoing echo: suppressed entirely, even for an unfocused chat.
        let effects = on_new_message(&mut app, &message(CHAT_A, "sent by me", true));
        assert!(effects.is_empty());
        assert_eq!(app.toasts.toasts.len(), 1);
    }

    #[test]
    fn muted_chat_updates_badge_but_no_toast_no_alert() {
        let mut app = fixture_state();
        app.chat_list
            .chats
            .insert(CHAT_A, chat(CHAT_A, "Muted Group", true));
        app.open_chat = None;

        let effects = on_new_message(&mut app, &message(CHAT_A, "still arrives", false));

        assert!(effects.is_empty());
        assert!(app.toasts.toasts.is_empty());
        // Badge maintenance (unread_count etc.) is chat_list's job, not
        // this module's; nothing here asserts on it.
    }

    #[test]
    fn stack_caps_at_three_dropping_oldest() {
        let mut app = fixture_state();
        for (id, title) in [
            (CHAT_A, "A"),
            (CHAT_B, "B"),
            (CHAT_C, "C"),
            (ChatId(4), "D"),
        ] {
            app.chat_list.chats.insert(id, chat(id, title, false));
        }

        on_new_message(&mut app, &message(CHAT_A, "1", false));
        on_new_message(&mut app, &message(CHAT_B, "2", false));
        on_new_message(&mut app, &message(CHAT_C, "3", false));
        on_new_message(&mut app, &message(ChatId(4), "4", false));

        assert_eq!(app.toasts.toasts.len(), TOAST_MAX);
        let titles: Vec<&str> = app.toasts.toasts.iter().map(|t| t.title.as_str()).collect();
        // Oldest (A) dropped; B, C, D remain, oldest-to-newest.
        assert_eq!(titles, vec!["B", "C", "D"]);
    }

    #[test]
    fn expires_on_tick() {
        let mut app = fixture_state();
        app.chat_list
            .chats
            .insert(CHAT_A, chat(CHAT_A, "Alice", false));
        app.now = Millis(1_000);
        on_new_message(&mut app, &message(CHAT_A, "hi", false));
        assert_eq!(app.toasts.toasts.len(), 1);
        let expires_at = app.toasts.toasts[0].expires_at;
        assert_eq!(expires_at, Millis(1_000 + TOAST_TTL_MS));

        // Not yet expired.
        handle_tick(&mut app, Millis(1_000 + TOAST_TTL_MS - 1));
        assert_eq!(app.toasts.toasts.len(), 1);

        // Past the TTL: swept.
        handle_tick(&mut app, Millis(1_000 + TOAST_TTL_MS + 1));
        assert!(app.toasts.toasts.is_empty());
    }

    #[test]
    fn dismiss_newest_pops_the_last_inserted_toast() {
        let mut app = fixture_state();
        app.chat_list.chats.insert(CHAT_A, chat(CHAT_A, "A", false));
        app.chat_list.chats.insert(CHAT_B, chat(CHAT_B, "B", false));
        on_new_message(&mut app, &message(CHAT_A, "1", false));
        on_new_message(&mut app, &message(CHAT_B, "2", false));

        assert!(dismiss_newest(&mut app));
        assert_eq!(app.toasts.toasts.len(), 1);
        assert_eq!(app.toasts.toasts[0].title, "A");

        assert!(dismiss_newest(&mut app));
        assert!(app.toasts.toasts.is_empty());

        // Nothing left to dismiss.
        assert!(!dismiss_newest(&mut app));
    }

    #[test]
    fn preview_text_falls_back_for_non_text_content() {
        assert_eq!(
            preview_text(&MessageContent::Photo {
                file_id: FileId(1),
                width: 100,
                height: 100,
                caption: FormattedText {
                    text: String::new(),
                    entities: Vec::new(),
                },
            }),
            "Photo"
        );
        assert_eq!(
            preview_text(&MessageContent::Sticker {
                emoji: "🎉".to_string(),
            }),
            "🎉 Sticker"
        );
    }
}
