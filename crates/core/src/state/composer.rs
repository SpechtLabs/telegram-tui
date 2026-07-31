//! Composer state. See docs/architecture.md §4.6 and §5.2.

use std::path::PathBuf;

use crate::action::TdResult;
use crate::app::{AppState, Screen};
use crate::effect::Effect;
use crate::model::entity::FormattedText;
use crate::model::ids::MessageId;
use crate::model::key::Key;
use crate::state::auth::InputField;
use crate::state::focus::{Focus, ModalKind};
use crate::td::request::TdRequest;
use crate::td::update::TdUpdate;

#[derive(Debug, Default)]
pub struct ComposerState {
    /// Multi-line buffer; `alt+enter` inserts '\n'.
    pub input: InputField,
    pub reply_to: Option<MessageId>,
    /// When set, Enter submits an edit instead of a send.
    pub editing: Option<MessageId>,
    /// Text held while a send is in flight. Restored to `input` on failure
    /// (spec §14: send failures never discard typed text).
    pub pending_send: Option<String>,
    /// A pasted bare path that exists on disk: offer to send as file.
    pub pending_path_offer: Option<PathBuf>,
}

/// Claims keys while `screen == Main`, a chat is open, and the composer is
/// on top of the focus stack. `T28` wires the real focus transitions into
/// (and out of) `Focus::Composer`; until then, callers/tests set
/// `app.focus` explicitly.
///
/// `Up`/`Down` split: `Up` has dedicated meaning (see below). `Down` is left
/// unclaimed here — the task list for this handler enumerates chars,
/// backspace/delete, left/right, home/end, enter and alt+enter explicitly,
/// and calls `Up` out on its own; `Down` isn't among them, so it falls
/// through to whatever handler `T28` ends up routing it to.
pub fn handle_key(app: &mut AppState, key: Key) -> Option<Vec<Effect>> {
    if app.screen != Screen::Main {
        return None;
    }
    let chat_id = app.open_chat?;
    if *app.focus.current() != Focus::Composer {
        return None;
    }

    match key {
        // A pending path offer (from a bare-path paste, see [`handle_paste`])
        // takes priority over both `/send` parsing and a normal text submit:
        // the offer already IS a fully-formed intent to send a file, so
        // `Enter` here means "yes, send it" rather than "submit whatever the
        // input buffer holds" (the buffer was left untouched when the offer
        // was made — see `handle_paste`'s doc comment).
        Key::Enter if app.composer.pending_path_offer.is_some() => {
            let path = app
                .composer
                .pending_path_offer
                .take()
                .expect("checked Some above");
            app.focus
                .push(Focus::Modal(ModalKind::ConfirmSendFile { path }));
            Some(Vec::new())
        }
        Key::Enter => match try_send_command(app) {
            Some(effects) => Some(effects),
            None => Some(submit(app, chat_id)),
        },
        Key::AltEnter => {
            insert_char(&mut app.composer.input, '\n');
            Some(Vec::new())
        }
        // Claimed only to discard a pending path offer; an ordinary `Esc`
        // with no offer pending is left unclaimed (falls through to `_`)
        // so the router's generic pop/back-to-chat-list handling still
        // applies. NOTE: `core/src/app.rs`'s `dispatch_key` currently
        // intercepts `Esc` globally (step 3) ahead of the focused-pane
        // dispatch (step 4), so this arm cannot fire through the live
        // router yet — reordering that is an `app.rs` change outside this
        // task's ownership. The logic lives here, ready for whenever that
        // reordering (or an offer-aware special case) lands; direct callers
        // of `handle_key` (this module's own tests) already exercise it.
        Key::Esc if app.composer.pending_path_offer.is_some() => {
            app.composer.pending_path_offer = None;
            Some(Vec::new())
        }
        Key::Up => {
            if app.composer.input.text.is_empty() {
                app.focus.push(Focus::Selection);
                // T26 initializes SelectionState via its own handler on entry
            } else {
                move_cursor_up(&mut app.composer.input);
            }
            Some(Vec::new())
        }
        Key::Char(c) => {
            insert_char(&mut app.composer.input, c);
            Some(Vec::new())
        }
        Key::Backspace => {
            let field = &mut app.composer.input;
            if field.cursor > 0 {
                let prev = prev_char_boundary(&field.text, field.cursor);
                field.text.replace_range(prev..field.cursor, "");
                field.cursor = prev;
            }
            Some(Vec::new())
        }
        Key::Delete => {
            let field = &mut app.composer.input;
            if field.cursor < field.text.len() {
                let next = next_char_boundary(&field.text, field.cursor);
                field.text.replace_range(field.cursor..next, "");
            }
            Some(Vec::new())
        }
        Key::Left => {
            let field = &mut app.composer.input;
            if field.cursor > 0 {
                field.cursor = prev_char_boundary(&field.text, field.cursor);
            }
            Some(Vec::new())
        }
        Key::Right => {
            let field = &mut app.composer.input;
            if field.cursor < field.text.len() {
                field.cursor = next_char_boundary(&field.text, field.cursor);
            }
            Some(Vec::new())
        }
        Key::Home => {
            app.composer.input.cursor = 0;
            Some(Vec::new())
        }
        Key::End => {
            app.composer.input.cursor = app.composer.input.text.len();
            Some(Vec::new())
        }
        _ => None,
    }
}

/// Completion of a dispatched `Effect::Td(SendMessageText)` /
/// `Effect::Td(EditMessageText)` request — the immediate RPC response, not
/// the later async push update.
///
/// `Ok` only drops the held text; the optimistic append into the message
/// window is `conversation.rs`'s job on this same `Action` (routed by
/// `T28`/`T32`). `Err` restores the held text to `input` — composer's half
/// of the "never discard typed text" contract (spec §14).
pub fn handle_td_result(app: &mut AppState, result: &TdResult) -> Vec<Effect> {
    if let TdResult::MessageSent { outcome, .. } = result {
        match outcome {
            Ok(_view) => app.composer.pending_send = None,
            Err(_e) => restore_pending(&mut app.composer),
        }
    }
    Vec::new()
}

/// Completion pushed asynchronously by TDLib after the RPC in
/// [`handle_td_result`] already returned `Ok` (see architecture §5.2's
/// "alt send fails" branch): the temporary message was created, then TDLib
/// later reports the send itself failed.
///
/// Dedupe with the `TdResult` path: [`restore_pending`] moves
/// `pending_send` out of the option (`Option::take`), so whichever handler
/// runs first performs the restore and the second finds `pending_send`
/// already `None` and is a no-op — the text is never duplicated into
/// `input`. Note this means if `MessageSent` already returned `Ok` (the
/// common case — pending_send was dropped there, not restored) by the time
/// this later failure arrives, there is nothing left here to restore; the
/// failed message itself stays visible (marked `SendState::Failed` by
/// `conversation.rs`) so the text is not lost, just no longer editable
/// in-place from this handler alone.
pub fn handle_td(app: &mut AppState, upd: &TdUpdate) -> Vec<Effect> {
    if let TdUpdate::MessageSendFailed { .. } = upd {
        restore_pending(&mut app.composer);
    }
    Vec::new()
}

fn submit(app: &mut AppState, chat_id: crate::model::ids::ChatId) -> Vec<Effect> {
    if app.composer.input.text.is_empty() {
        return Vec::new();
    }
    let text = std::mem::take(&mut app.composer.input.text);
    app.composer.input.cursor = 0;

    if let Some(message_id) = app.composer.editing.take() {
        vec![Effect::Td(TdRequest::EditMessageText {
            chat_id,
            message_id,
            text: FormattedText {
                text,
                entities: Vec::new(),
            },
        })]
    } else {
        let reply_to = app.composer.reply_to.take();
        app.composer.pending_send = Some(text.clone());
        vec![Effect::Td(TdRequest::SendMessageText {
            chat_id,
            reply_to,
            text: FormattedText {
                text,
                entities: Vec::new(),
            },
        })]
    }
}

const SEND_COMMAND: &str = "/send";

/// `/send <path>` (spec §10). Parsing only: the path is taken verbatim (no
/// tilde expansion, no existence check — both are I/O core cannot perform,
/// see [`looks_like_path`]'s doc comment for the full purity split). On a
/// successful parse the input buffer is cleared exactly as [`submit`] clears
/// it on an ordinary send, and `Focus::Modal(ConfirmSendFile)` is pushed;
/// existence is validated later, either by `crates/app/src/media_kind.rs`'s
/// `existing_path` before dispatch, or as an ordinary send failure surfaced
/// by TDLib itself if it slips through.
///
/// Returns `None` when `input` is not a `/send` command at all (so the
/// caller falls through to a normal text submit); `Some(vec![])` when it is
/// `/send` with a missing/blank argument (claimed, no-op — the input is left
/// alone so the user can fix it); `Some(vec![])` with the modal pushed as a
/// side effect on a successful parse (no `Effect` carries a focus change,
/// architecture §4.4).
fn try_send_command(app: &mut AppState) -> Option<Vec<Effect>> {
    let text = app.composer.input.text.as_str();
    if text != SEND_COMMAND && !text.starts_with("/send ") {
        return None;
    }
    let arg = text.strip_prefix(SEND_COMMAND).unwrap_or(text).trim();
    if arg.is_empty() {
        return Some(Vec::new());
    }

    let path = PathBuf::from(arg);
    app.composer.input.text.clear();
    app.composer.input.cursor = 0;
    app.focus
        .push(Focus::Modal(ModalKind::ConfirmSendFile { path }));
    Some(Vec::new())
}

/// Pure heuristic for "this pasted text is probably a filesystem path":
/// starts with `/`, `~/`, or `./`, has something after that prefix, and is a
/// single line. NOT an existence check — core has no filesystem access
/// (architecture §9.3's purity rules), so it cannot be one. This is one half
/// of a deliberate split documented on the plan (T39):
///
/// - Core ([`handle_paste`], this function): decides *whether to offer* a
///   send from pasted text, using only the string itself.
/// - `tgt-app`'s `crates/app/src/media_kind.rs::existing_path` (impure):
///   does the actual tilde-expansion (needs `$HOME`) and `fs` existence
///   check before a `/send` or offered path is ever handed to TDLib.
///
/// Consequently `pending_path_offer` can hold a path that turns out not to
/// exist — confirming it in that state surfaces as an ordinary send failure
/// once TDLib (or the app-layer pre-check) rejects it, per the plan's
/// resolution recorded in this task's final report.
pub fn looks_like_path(s: &str) -> bool {
    if s.contains('\n') || s.contains('\r') {
        return false;
    }
    let trimmed = s.trim();
    let after_prefix = trimmed
        .strip_prefix("~/")
        .or_else(|| trimmed.strip_prefix("./"))
        .or_else(|| trimmed.strip_prefix('/'));
    matches!(after_prefix, Some(rest) if !rest.is_empty())
}

/// Bracketed paste (spec §10: "terminals paste dropped files as plain text
/// paths, so the composer detects a bare existing path and offers to send
/// it"). When the pasted text [`looks_like_path`], it is held as
/// `pending_path_offer` instead of being inserted into the input buffer —
/// `Enter` in `handle_key` turns a pending offer into a
/// `Focus::Modal(ConfirmSendFile)` push, `Esc` discards it. Anything else
/// pastes as ordinary text at the cursor, one char at a time (multi-byte
/// paste content is not assumed to already sit at char boundaries).
///
/// Same claim rules as [`handle_key`] (`Screen::Main`, a chat open, composer
/// focused) — checked here rather than shared, since this isn't itself a
/// `handle_key` match arm (paste is a distinct `Action`, not a `Key`).
///
/// `core/src/app.rs`'s `dispatch` routes `Action::Paste` here (T40). The app
/// layer expands a pasted `~/…` to an absolute path first — see
/// `crates/app/src/runtime_loop.rs`'s paste arm, which also records why a
/// path-shaped string that doesn't exist is still allowed to raise an offer.
pub fn handle_paste(app: &mut AppState, text: String) {
    if app.screen != Screen::Main || app.open_chat.is_none() {
        return;
    }
    if *app.focus.current() != Focus::Composer {
        return;
    }

    if looks_like_path(&text) {
        app.composer.pending_path_offer = Some(PathBuf::from(text.trim()));
    } else {
        for c in text.chars() {
            insert_char(&mut app.composer.input, c);
        }
    }
}

/// Cancels an in-flight upload tracked under `message_id`
/// (`AppState::media.uploads`, architecture §4.6). TDLib has no dedicated
/// "cancel upload" request — `CancelDownloadFile` (`state/media.rs`) is for
/// the download side only — so v1 cancels by deleting the optimistic pending
/// message itself: `revoke: false`, since a still-uploading message was
/// never visible to the other side, so there is nothing to revoke there.
///
/// Drops the tracked [`crate::state::media::UploadProgress`] entry
/// immediately rather than waiting for TDLib's `MessagesDeleted` push — the
/// upload's own progress stops being meaningful the instant the user
/// cancels, and `state/media.rs`'s `progress_upload` is already a no-op
/// against an untracked id, so no dangling-write hazard is introduced by
/// removing it early. A `message_id` with no tracked upload (already
/// completed, or never started) is a no-op: no effects.
pub fn cancel_upload(app: &mut AppState, message_id: MessageId) -> Vec<Effect> {
    let Some(progress) = app.media.uploads.remove(&message_id) else {
        return Vec::new();
    };
    vec![Effect::Td(TdRequest::DeleteMessages {
        chat_id: progress.chat_id,
        message_ids: vec![message_id],
        revoke: false,
    })]
}

/// Moves `pending_send` (if any) back into `input`, cursor at the end.
/// A no-op if `pending_send` is already `None` — see the dedupe note on
/// [`handle_td`].
fn restore_pending(composer: &mut ComposerState) {
    if let Some(text) = composer.pending_send.take() {
        composer.input.text = text;
        composer.input.cursor = composer.input.text.len();
    }
}

fn insert_char(field: &mut InputField, c: char) {
    field.text.insert(field.cursor, c);
    field.cursor += c.len_utf8();
}

/// Moves the cursor to the start of the previous line, or to the buffer
/// start (`Home`) if already on the first line. No column-preservation —
/// `InputField` has no stored "target column" to preserve one against, so
/// this is deliberately the simplest rule that gets the cursor up a line.
fn move_cursor_up(field: &mut InputField) {
    let text = &field.text;
    let line_start = text[..field.cursor].rfind('\n').map_or(0, |i| i + 1);
    if line_start == 0 {
        field.cursor = 0;
        return;
    }
    let prev_line_end = line_start - 1; // the '\n' byte itself
    let prev_line_start = text[..prev_line_end].rfind('\n').map_or(0, |i| i + 1);
    field.cursor = prev_line_start;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::Screen;
    use crate::effect::TelemetryMode;
    use crate::model::ids::ChatId;
    use crate::state::auth::{AuthField, AuthState};
    use crate::state::chat_list::ChatListState;
    use crate::state::consent::{ConsentChoice, ConsentState};
    use crate::state::conversation;
    use crate::state::focus::FocusStack;
    use crate::state::media::MediaState;
    use crate::state::presence::PresenceState;
    use crate::state::toasts::ToastState;
    use crate::td::error::TdError;
    use crate::td::update::{AuthPhase, ConnectionPhase};
    use std::collections::HashMap;

    const CHAT: ChatId = ChatId(1);

    fn fixture_state() -> AppState {
        AppState {
            screen: Screen::Main,
            focus: FocusStack::new(Focus::Composer),
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
            crash_reports_available: false,
            telemetry_salt: [0u8; 32],
            now: crate::model::time::Millis(0),
        }
    }

    fn fixture_open(app: &mut AppState) {
        conversation::open(app, CHAT);
        app.focus = FocusStack::new(Focus::Composer);
    }

    // --- handle_key: claiming rules ------------------------------------

    #[test]
    fn unclaimed_outside_main_screen() {
        let mut app = fixture_state();
        app.screen = Screen::Auth;
        assert!(handle_key(&mut app, Key::Char('a')).is_none());
    }

    #[test]
    fn unclaimed_without_open_chat() {
        let mut app = fixture_state();
        app.open_chat = None;
        assert!(handle_key(&mut app, Key::Char('a')).is_none());
    }

    #[test]
    fn unclaimed_when_not_focused() {
        let mut app = fixture_state();
        app.focus = FocusStack::new(Focus::ChatList);
        assert!(handle_key(&mut app, Key::Char('a')).is_none());
    }

    #[test]
    fn typing_edits_input_at_cursor() {
        let mut app = fixture_state();
        handle_key(&mut app, Key::Char('h')).unwrap();
        handle_key(&mut app, Key::Char('i')).unwrap();
        assert_eq!(app.composer.input.text, "hi");
        assert_eq!(app.composer.input.cursor, 2);

        handle_key(&mut app, Key::Left).unwrap();
        handle_key(&mut app, Key::Char('!')).unwrap();
        assert_eq!(app.composer.input.text, "h!i");

        handle_key(&mut app, Key::Backspace).unwrap();
        assert_eq!(app.composer.input.text, "hi");
        assert_eq!(app.composer.input.cursor, 1);

        handle_key(&mut app, Key::End).unwrap();
        assert_eq!(app.composer.input.cursor, 2);
        handle_key(&mut app, Key::Home).unwrap();
        assert_eq!(app.composer.input.cursor, 0);
        handle_key(&mut app, Key::Delete).unwrap();
        assert_eq!(app.composer.input.text, "i");
    }

    // --- plan-named tests ------------------------------------------------

    #[test]
    fn enter_sends_and_holds_pending() {
        let mut app = fixture_state();
        app.composer.input.text = "hello".to_string();
        app.composer.input.cursor = 5;

        let effects = handle_key(&mut app, Key::Enter).expect("composer claims Enter");

        assert_eq!(app.composer.input.text, "");
        assert_eq!(app.composer.input.cursor, 0);
        assert_eq!(app.composer.pending_send.as_deref(), Some("hello"));
        assert_eq!(effects.len(), 1);
        match &effects[0] {
            Effect::Td(TdRequest::SendMessageText {
                chat_id,
                reply_to,
                text,
            }) => {
                assert_eq!(*chat_id, CHAT);
                assert_eq!(*reply_to, None);
                assert_eq!(text.text, "hello");
                assert!(text.entities.is_empty());
            }
            other => panic!("expected SendMessageText, got {other:?}"),
        }
    }

    #[test]
    fn enter_with_empty_input_is_claimed_but_a_noop() {
        let mut app = fixture_state();
        let effects = handle_key(&mut app, Key::Enter).expect("composer claims Enter");
        assert!(effects.is_empty());
        assert!(app.composer.pending_send.is_none());
    }

    #[test]
    fn enter_clears_reply_to_on_send() {
        let mut app = fixture_state();
        app.composer.input.text = "hi".to_string();
        app.composer.reply_to = Some(MessageId(9));

        let effects = handle_key(&mut app, Key::Enter).unwrap();

        assert!(app.composer.reply_to.is_none());
        match &effects[0] {
            Effect::Td(TdRequest::SendMessageText { reply_to, .. }) => {
                assert_eq!(*reply_to, Some(MessageId(9)));
            }
            other => panic!("expected SendMessageText, got {other:?}"),
        }
    }

    #[test]
    fn send_failure_restores_text_to_input() {
        let mut app = fixture_state();
        app.composer.input.text = "hello".to_string();
        handle_key(&mut app, Key::Enter).unwrap();
        assert_eq!(app.composer.pending_send.as_deref(), Some("hello"));

        let result = TdResult::MessageSent {
            chat_id: CHAT,
            outcome: Err(TdError::NetTimeout),
        };
        let effects = handle_td_result(&mut app, &result);

        assert!(effects.is_empty());
        assert!(app.composer.pending_send.is_none());
        assert_eq!(app.composer.input.text, "hello");
        assert_eq!(app.composer.input.cursor, 5);
    }

    #[test]
    fn send_success_drops_pending() {
        let mut app = fixture_state();
        fixture_open(&mut app);
        let sent = crate::model::message::MessageView {
            id: MessageId(-1),
            chat_id: CHAT,
            sender: crate::model::message::Sender::User(crate::model::ids::UserId(1)),
            sender_name: "Me".to_string(),
            is_outgoing: true,
            date: 0,
            content: crate::model::message::MessageContent::Text(FormattedText {
                text: "hello".to_string(),
                entities: Vec::new(),
            }),
            reply_to: None,
            send_state: crate::model::message::SendState::Sending,
            reactions: Vec::new(),
            caps: crate::model::message::MessageCaps::default(),
            is_edited: false,
        };
        app.composer.input.text = "hello".to_string();
        handle_key(&mut app, Key::Enter).unwrap();
        assert!(app.composer.pending_send.is_some());

        let result = TdResult::MessageSent {
            chat_id: CHAT,
            outcome: Ok(sent),
        };
        let effects = handle_td_result(&mut app, &result);

        assert!(effects.is_empty());
        assert!(app.composer.pending_send.is_none());
        assert_eq!(app.composer.input.text, "");
    }

    #[test]
    fn alt_enter_inserts_newline() {
        let mut app = fixture_state();
        app.composer.input.text = "ab".to_string();
        app.composer.input.cursor = 2;

        let effects = handle_key(&mut app, Key::AltEnter).expect("composer claims AltEnter");

        assert!(effects.is_empty());
        assert_eq!(app.composer.input.text, "ab\n");
        assert_eq!(app.composer.input.cursor, 3);
    }

    #[test]
    fn up_on_empty_enters_selection() {
        let mut app = fixture_state();
        assert!(app.composer.input.text.is_empty());

        let effects = handle_key(&mut app, Key::Up).expect("composer claims Up");

        assert!(effects.is_empty());
        assert_eq!(*app.focus.current(), Focus::Selection);
        assert_eq!(app.focus.depth(), 2);
    }

    #[test]
    fn up_on_nonempty_moves_cursor() {
        let mut app = fixture_state();
        app.composer.input.text = "abc".to_string();
        app.composer.input.cursor = 3;

        let effects = handle_key(&mut app, Key::Up).expect("composer claims Up");

        assert!(effects.is_empty());
        assert_eq!(*app.focus.current(), Focus::Composer);
        assert_eq!(app.composer.input.cursor, 0);
    }

    #[test]
    fn up_moves_to_start_of_previous_line_in_multiline_input() {
        let mut app = fixture_state();
        app.composer.input.text = "one\ntwo".to_string();
        app.composer.input.cursor = 7; // end, on the second line

        handle_key(&mut app, Key::Up).unwrap();

        assert_eq!(app.composer.input.cursor, 0); // start of "one"
    }

    #[test]
    fn edit_mode_submits_edit_message_text() {
        let mut app = fixture_state();
        app.composer.editing = Some(MessageId(42));
        app.composer.input.text = "edited text".to_string();
        app.composer.input.cursor = 11;

        let effects = handle_key(&mut app, Key::Enter).expect("composer claims Enter");

        assert!(app.composer.editing.is_none());
        assert!(app.composer.pending_send.is_none());
        assert_eq!(app.composer.input.text, "");
        assert_eq!(effects.len(), 1);
        match &effects[0] {
            Effect::Td(TdRequest::EditMessageText {
                chat_id,
                message_id,
                text,
            }) => {
                assert_eq!(*chat_id, CHAT);
                assert_eq!(*message_id, MessageId(42));
                assert_eq!(text.text, "edited text");
            }
            other => panic!("expected EditMessageText, got {other:?}"),
        }
    }

    // --- restore dedupe ---------------------------------------------------

    #[test]
    fn double_restore_does_not_duplicate_text() {
        let mut app = fixture_state();
        app.composer.input.text = "hello".to_string();
        handle_key(&mut app, Key::Enter).unwrap();
        assert_eq!(app.composer.pending_send.as_deref(), Some("hello"));

        // First: the async push update fires (as in the architecture §5.2
        // "alt send fails" branch) and restores the text.
        let upd = TdUpdate::MessageSendFailed {
            chat_id: CHAT,
            old_message_id: MessageId(-1),
            error: TdError::NetTimeout,
        };
        handle_td(&mut app, &upd);
        assert_eq!(app.composer.input.text, "hello");
        assert!(app.composer.pending_send.is_none());

        // Second: the immediate RPC result also comes back as an error
        // (belt-and-suspenders in this test — in practice only one path
        // fires per send). It must not append/duplicate the text.
        let result = TdResult::MessageSent {
            chat_id: CHAT,
            outcome: Err(TdError::NetTimeout),
        };
        handle_td_result(&mut app, &result);

        assert_eq!(app.composer.input.text, "hello");
    }

    #[test]
    fn message_send_failed_after_success_is_a_noop() {
        let mut app = fixture_state();
        app.composer.input.text = "hello".to_string();
        handle_key(&mut app, Key::Enter).unwrap();

        let sent = crate::model::message::MessageView {
            id: MessageId(-1),
            chat_id: CHAT,
            sender: crate::model::message::Sender::User(crate::model::ids::UserId(1)),
            sender_name: "Me".to_string(),
            is_outgoing: true,
            date: 0,
            content: crate::model::message::MessageContent::Text(FormattedText {
                text: "hello".to_string(),
                entities: Vec::new(),
            }),
            reply_to: None,
            send_state: crate::model::message::SendState::Sending,
            reactions: Vec::new(),
            caps: crate::model::message::MessageCaps::default(),
            is_edited: false,
        };
        handle_td_result(
            &mut app,
            &TdResult::MessageSent {
                chat_id: CHAT,
                outcome: Ok(sent),
            },
        );
        assert!(app.composer.pending_send.is_none());
        assert_eq!(app.composer.input.text, "");

        // Now type something new before the (belated) failure push arrives.
        app.composer.input.text = "new draft".to_string();
        app.composer.input.cursor = 9;

        handle_td(
            &mut app,
            &TdUpdate::MessageSendFailed {
                chat_id: CHAT,
                old_message_id: MessageId(-1),
                error: TdError::NetTimeout,
            },
        );

        // pending_send was already None: the new draft is left untouched.
        assert_eq!(app.composer.input.text, "new draft");
    }

    // --- T39: /send, pasted-path offers, upload cancel ---------------------

    /// The core half of the plan's `send_command_parses_path_and_validates_existence`.
    /// "Validates existence" is the app-layer half — see
    /// `crates/app/src/media_kind.rs`'s test of the same name — because
    /// existence checks are I/O and core is pure (module docs on
    /// [`looks_like_path`] record the full split). This half only asserts
    /// the parse: a well-formed `/send <path>` opens the confirm modal with
    /// exactly that path and clears the input; a missing argument is a
    /// claimed no-op; plain text is not treated as the command at all.
    #[test]
    fn send_command_parses_path_and_validates_existence() {
        let mut app = fixture_state();
        app.composer.input.text = "/send /tmp/photo.jpg".to_string();
        app.composer.input.cursor = 20;

        let effects = handle_key(&mut app, Key::Enter).expect("composer claims Enter");

        assert!(effects.is_empty());
        assert_eq!(app.composer.input.text, "");
        assert_eq!(app.composer.input.cursor, 0);
        assert_eq!(
            *app.focus.current(),
            Focus::Modal(ModalKind::ConfirmSendFile {
                path: PathBuf::from("/tmp/photo.jpg"),
            })
        );
    }

    #[test]
    fn send_command_with_missing_arg_is_a_claimed_noop() {
        let mut app = fixture_state();
        app.composer.input.text = "/send".to_string();
        app.composer.input.cursor = 5;

        let effects = handle_key(&mut app, Key::Enter).expect("composer claims Enter");

        assert!(effects.is_empty());
        assert_eq!(app.composer.input.text, "/send");
        assert_eq!(*app.focus.current(), Focus::Composer);

        app.composer.input.text = "/send   ".to_string();
        app.composer.input.cursor = 8;
        let effects = handle_key(&mut app, Key::Enter).expect("composer claims Enter");
        assert!(effects.is_empty());
        assert_eq!(*app.focus.current(), Focus::Composer);
    }

    #[test]
    fn plain_text_starting_with_slash_send_word_is_not_the_command() {
        let mut app = fixture_state();
        app.composer.input.text = "/sendoff tomorrow".to_string();

        let effects = handle_key(&mut app, Key::Enter).expect("composer claims Enter");

        // Falls through to an ordinary text submit, not the file command.
        assert_eq!(effects.len(), 1);
        assert!(matches!(
            &effects[0],
            Effect::Td(TdRequest::SendMessageText { .. })
        ));
        assert_eq!(*app.focus.current(), Focus::Composer);
    }

    #[test]
    fn looks_like_path_accepts_absolute_home_and_relative_prefixes() {
        assert!(looks_like_path("/tmp/dropped.png"));
        assert!(looks_like_path("~/Downloads/dropped.png"));
        assert!(looks_like_path("./dropped.png"));
        assert!(looks_like_path("  /tmp/dropped.png  "));
    }

    #[test]
    fn looks_like_path_rejects_ordinary_text() {
        assert!(!looks_like_path("hello there"));
        assert!(!looks_like_path("/"));
        assert!(!looks_like_path("~/"));
        assert!(!looks_like_path(""));
        assert!(!looks_like_path("/tmp/one\n/tmp/two"));
    }

    #[test]
    fn pasted_bare_path_offers_send() {
        let mut app = fixture_state();

        handle_paste(&mut app, "/tmp/dropped.png".to_string());

        assert_eq!(
            app.composer.pending_path_offer,
            Some(PathBuf::from("/tmp/dropped.png"))
        );
        // The offer replaces the ordinary paste-insert: nothing landed in
        // the input buffer for this pasted text.
        assert_eq!(app.composer.input.text, "");

        // Enter with an offer pending opens the confirm modal and clears it.
        let effects = handle_key(&mut app, Key::Enter).expect("composer claims Enter");
        assert!(effects.is_empty());
        assert!(app.composer.pending_path_offer.is_none());
        assert_eq!(
            *app.focus.current(),
            Focus::Modal(ModalKind::ConfirmSendFile {
                path: PathBuf::from("/tmp/dropped.png"),
            })
        );
    }

    #[test]
    fn pasted_ordinary_text_inserts_normally() {
        let mut app = fixture_state();

        handle_paste(&mut app, "just some words".to_string());

        assert!(app.composer.pending_path_offer.is_none());
        assert_eq!(app.composer.input.text, "just some words");
    }

    #[test]
    fn esc_with_pending_offer_discards_it() {
        let mut app = fixture_state();
        handle_paste(&mut app, "/tmp/dropped.png".to_string());
        assert!(app.composer.pending_path_offer.is_some());

        let effects = handle_key(&mut app, Key::Esc).expect("composer claims Esc for the offer");

        assert!(effects.is_empty());
        assert!(app.composer.pending_path_offer.is_none());
        assert_eq!(*app.focus.current(), Focus::Composer);
    }

    #[test]
    fn esc_without_pending_offer_is_unclaimed() {
        let mut app = fixture_state();
        assert!(app.composer.pending_path_offer.is_none());
        assert!(handle_key(&mut app, Key::Esc).is_none());
    }

    #[test]
    fn upload_cancellable_before_completion() {
        let mut app = fixture_state();
        let msg_id = MessageId(-7);
        crate::state::media::start_upload(&mut app, msg_id, CHAT, 2_000);
        crate::state::media::progress_upload(&mut app, msg_id, 500);
        assert!(app.media.uploads.contains_key(&msg_id));

        let effects = cancel_upload(&mut app, msg_id);

        assert!(!app.media.uploads.contains_key(&msg_id));
        assert_eq!(effects.len(), 1);
        match &effects[0] {
            Effect::Td(TdRequest::DeleteMessages {
                chat_id,
                message_ids,
                revoke,
            }) => {
                assert_eq!(*chat_id, CHAT);
                assert_eq!(message_ids, &vec![msg_id]);
                assert!(!revoke);
            }
            other => panic!("expected Effect::Td(DeleteMessages), got {other:?}"),
        }
    }

    #[test]
    fn cancel_upload_without_tracked_entry_is_a_noop() {
        let mut app = fixture_state();
        let effects = cancel_upload(&mut app, MessageId(-99));
        assert!(effects.is_empty());
    }
}
