//! Selection mode: the message cursor and its capability chips.
//! See docs/architecture.md §4.2, §4.6, §7; spec §5.3.
//!
//! ## What selection mode is
//!
//! One message in the open conversation is "selected"; the chip row under it
//! offers exactly the actions TDLib will accept for that message. `←`/`→`
//! walk the chips, `⏎` invokes the focused one, and each chip also answers to
//! its leading letter. `↑`/`↓` walk the messages, dragging the scroll anchor
//! along so the viewport follows the cursor (and paging older history in when
//! the cursor reaches the top of the window, exactly as the scroll keys do).
//!
//! ## Capabilities are fetched, not guessed (architecture §7)
//!
//! TDLib ~1.8.61 does not put `can_be_edited` / `can_be_deleted_*` /
//! `can_be_forwarded` on `message`; they live on `messageProperties`. So
//! every time the cursor lands on a message this module fires
//! [`TdRequest::GetMessageProperties`] and folds the answer back into the
//! window via [`handle_td_result`], recomputing the chip row. Until that
//! answer arrives the row is the pessimistic one derivable from what the
//! message already carries — never a hardcoded menu. A failed lookup keeps
//! whatever caps the message already had rather than collapsing the row.
//!
//! Messages that failed to send are the one case with nothing to ask about:
//! they do not exist server-side, so no request goes out and the row is
//! `[Resend, Delete]`.
//!
//! ## Focus is the router's, not this module's
//!
//! T25 pushes `Focus::Selection` (`↑` on an empty composer) and T28 owns the
//! routing table. This module only mutates state and returns effects:
//! [`enter`] is called after the focus push, [`exit`] after the pop. `Esc`
//! deliberately returns `None` from [`handle_key`] so the router's generic
//! "pop one level" rule handles it. Chips that hand control back to the
//! composer (Reply, Edit) set the composer's context and leave the focus pop
//! to T28 — an effect list cannot express a focus change and shouldn't.

use crate::app::AppState;
use crate::effect::Effect;
use crate::model::chips::{Chip, chips_for};
use crate::model::ids::{ChatId, FileId, MessageId};
use crate::model::key::Key;
use crate::model::message::{MessageCaps, MessageContent, MessageView, SendState};
use crate::state::auth::InputField;
use crate::state::conversation::{self, ConversationState};
use crate::state::focus::{Focus, ModalKind};
use crate::state::media::{self, MediaState};
use crate::td::error::TdError;
use crate::td::request::TdRequest;

/// The reaction the `React` chip sends. A full emoji picker is out of scope
/// for v1 (spec §5.3: "default emoji set"), so one toggle covers the case
/// that matters: acknowledging a message without typing.
pub const DEFAULT_REACTION: &str = "👍";

/// How many chips the row is assumed to show at once for scroll bookkeeping.
/// Core cannot measure the terminal (that is `tgt-ui`'s business), so the
/// window is a fixed count: `chip_scroll` keeps the cursor inside it and T29
/// renders `‹ ›` affordances whenever `chip_scroll > 0` or chips remain to
/// the right.
pub const CHIP_VISIBLE_MAX: usize = 5;

/// Reply excerpts filled from the local window are cut to one line at this
/// many characters, matching what the runtime mapping layer does to the
/// excerpts TDLib does deliver.
pub const REPLY_EXCERPT_MAX_CHARS: usize = 96;

/// TDLib download priority for a user-initiated download: high, but below the
/// 1-31 band the media prefetcher (T36) uses for background work.
pub const DOWNLOAD_PRIORITY: i8 = 32;

#[derive(Debug, Clone, PartialEq)]
pub struct SelectionState {
    pub message_id: MessageId,
    /// Chips recomputed from caps whenever the selected message changes.
    pub chips: Vec<Chip>,
    pub chip_cursor: usize,
    /// First visible chip index (horizontal chip scrolling, ‹ › affordances).
    pub chip_scroll: usize,
}

impl SelectionState {
    /// The chip `⏎` would invoke, if the row is non-empty.
    pub fn focused_chip(&self) -> Option<Chip> {
        self.chips.get(self.chip_cursor).copied()
    }
}

/// Enters selection mode on the newest loaded message of the open chat.
/// Called by the router (T28) right after `Focus::Selection` is pushed.
///
/// Returns the capability refresh for the selected message, plus any history
/// page the anchor move triggered. A chat with no loaded messages selects
/// nothing and returns nothing — the caller's focus push then has an empty
/// selection under it, which every handler here treats as "unclaimed".
pub fn enter(app: &mut AppState) -> Vec<Effect> {
    let Some(chat_id) = app.open_chat else {
        return Vec::new();
    };
    let Some(newest) = app
        .conversations
        .get(&chat_id)
        .and_then(|convo| convo.messages.back())
        .map(|m| m.id)
    else {
        return Vec::new();
    };
    enter_at(app, newest)
}

/// Enters selection mode on `message_id` specifically, or moves an
/// already-active selection there. The mouse counterpart of [`enter`]
/// (architecture §7.5, right-click on a message): a click names the exact
/// message under the cursor rather than always starting at the newest one.
///
/// Same "nothing to select" contract as `enter`: no open chat, or
/// `message_id` not currently loaded in its window, returns nothing and
/// leaves the selection (if any) untouched — the router undoes a focus push
/// it made speculatively when this comes back empty, exactly as it does for
/// `enter`.
pub fn enter_at(app: &mut AppState, message_id: MessageId) -> Vec<Effect> {
    let Some(chat_id) = app.open_chat else {
        return Vec::new();
    };
    let Some(convo) = app.conversations.get(&chat_id) else {
        return Vec::new();
    };
    if conversation::index_of(&convo.messages, message_id).is_none() {
        return Vec::new();
    }
    select(app, chat_id, message_id)
}

/// Leaves selection mode: drops the selection of the open chat. Called by the
/// router (T28) after popping `Focus::Selection` — including the `Esc` path,
/// which [`handle_key`] deliberately does not claim.
pub fn exit(app: &mut AppState) {
    let Some(chat_id) = app.open_chat else {
        return;
    };
    if let Some(convo) = app.conversations.get_mut(&chat_id) {
        convo.selection = None;
    }
}

/// Selection-mode keys. `None` means unclaimed, so the router keeps walking
/// its table (`Esc` → pop focus; `?` → help; anything with no matching chip
/// shortcut → whatever claims it next).
pub fn handle_key(app: &mut AppState, key: Key) -> Option<Vec<Effect>> {
    if !matches!(app.focus.current(), Focus::Selection) {
        return None;
    }
    let chat_id = app.open_chat?;
    let selected = app
        .conversations
        .get(&chat_id)?
        .selection
        .as_ref()?
        .message_id;

    match key {
        Key::Up => Some(move_selection(app, chat_id, -1)),
        Key::Down => Some(move_selection(app, chat_id, 1)),
        Key::Left => {
            move_chip_cursor(app, chat_id, -1);
            Some(Vec::new())
        }
        Key::Right => {
            move_chip_cursor(app, chat_id, 1);
            Some(Vec::new())
        }
        Key::Enter => {
            let chip = app
                .conversations
                .get(&chat_id)?
                .selection
                .as_ref()?
                .focused_chip()?;
            Some(invoke(app, chat_id, selected, chip))
        }
        // A letter that no chip in THIS row answers to stays unclaimed: the
        // row is the truth about what is possible, and swallowing the key
        // would also swallow global bindings like `?`.
        Key::Char(c) => {
            let chip = app
                .conversations
                .get(&chat_id)?
                .selection
                .as_ref()?
                .chips
                .iter()
                .copied()
                .find(|chip| chip.shortcut() == c)?;
            Some(invoke(app, chat_id, selected, chip))
        }
        _ => None,
    }
}

/// Folds a `getMessageProperties` answer into the window: the message's caps
/// become the fetched ones and, if that message is still the selected one,
/// its chip row is recomputed. An `Err` is a no-op by design (architecture
/// §4.3): the row the user is looking at keeps working instead of losing
/// chips because one lookup timed out.
pub fn handle_td_result(
    app: &mut AppState,
    chat_id: ChatId,
    message_id: MessageId,
    outcome: &Result<MessageCaps, TdError>,
) -> Vec<Effect> {
    let Ok(caps) = outcome else {
        return Vec::new();
    };
    let Some(convo) = app.conversations.get_mut(&chat_id) else {
        return Vec::new();
    };
    let Some(idx) = conversation::index_of(&convo.messages, message_id) else {
        return Vec::new();
    };
    convo.messages[idx].caps = *caps;

    let still_selected = convo
        .selection
        .as_ref()
        .is_some_and(|sel| sel.message_id == message_id);
    if !still_selected {
        return Vec::new();
    }
    recompute_chips(app, chat_id, message_id);
    Vec::new()
}

// --- selection movement ------------------------------------------------

/// Points the selection at `message_id`: fills in a missing reply excerpt,
/// derives the chip row, drags the scroll anchor along and asks TDLib for the
/// capability flags it withholds from `message`.
fn select(app: &mut AppState, chat_id: ChatId, message_id: MessageId) -> Vec<Effect> {
    let Some(convo) = app.conversations.get(&chat_id) else {
        return Vec::new();
    };
    let Some(idx) = conversation::index_of(&convo.messages, message_id) else {
        return Vec::new();
    };
    let chips = chips_for_message(&convo.messages[idx], &app.media);
    let send_failed = matches!(convo.messages[idx].send_state, SendState::Failed(_));
    let now = app.now;

    let Some(convo) = app.conversations.get_mut(&chat_id) else {
        return Vec::new();
    };
    fill_reply_excerpt(convo, message_id);
    convo.selection = Some(SelectionState {
        message_id,
        chips,
        chip_cursor: 0,
        chip_scroll: 0,
    });

    // A message that never reached the server has no properties to fetch.
    let mut effects = if send_failed {
        Vec::new()
    } else {
        vec![Effect::Td(TdRequest::GetMessageProperties {
            chat_id,
            message_id,
        })]
    };
    effects.extend(conversation::anchor_to(convo, chat_id, message_id, now));
    // T66: selection landing on (or stepping to) a message drags the scroll
    // anchor with it, so it's a "the anchor moved" trigger like any other.
    effects.extend(media::auto_download_photos(app, chat_id));
    effects
}

/// `↑`/`↓`: one message older / newer, clamped at both ends of the window.
/// Landing on the same message (already at an end) changes nothing and fires
/// no request.
fn move_selection(app: &mut AppState, chat_id: ChatId, delta: isize) -> Vec<Effect> {
    let Some(convo) = app.conversations.get(&chat_id) else {
        return Vec::new();
    };
    let Some(sel) = convo.selection.as_ref() else {
        return Vec::new();
    };
    let Some(idx) = conversation::index_of(&convo.messages, sel.message_id) else {
        return Vec::new();
    };
    let last = convo.messages.len().saturating_sub(1) as isize;
    let target = (idx as isize + delta).clamp(0, last) as usize;
    if target == idx {
        return Vec::new();
    }
    let target_id = convo.messages[target].id;
    select(app, chat_id, target_id)
}

/// `←`/`→` over the chip row, with the visible window following the cursor.
fn move_chip_cursor(app: &mut AppState, chat_id: ChatId, delta: isize) {
    let Some(convo) = app.conversations.get_mut(&chat_id) else {
        return;
    };
    let Some(sel) = convo.selection.as_mut() else {
        return;
    };
    if sel.chips.is_empty() {
        return;
    }
    let last = (sel.chips.len() - 1) as isize;
    sel.chip_cursor = (sel.chip_cursor as isize + delta).clamp(0, last) as usize;
    clamp_chip_scroll(sel);
}

/// Keeps `chip_cursor` inside the `CHIP_VISIBLE_MAX`-wide window starting at
/// `chip_scroll`, scrolling by the minimum needed to bring it back in view.
fn clamp_chip_scroll(sel: &mut SelectionState) {
    if sel.chip_cursor < sel.chip_scroll {
        sel.chip_scroll = sel.chip_cursor;
    } else if sel.chip_cursor >= sel.chip_scroll + CHIP_VISIBLE_MAX {
        sel.chip_scroll = sel.chip_cursor + 1 - CHIP_VISIBLE_MAX;
    }
}

/// Re-derives the chip row for the (still selected) message, keeping the chip
/// cursor as close to where the user left it as the new row allows.
fn recompute_chips(app: &mut AppState, chat_id: ChatId, message_id: MessageId) {
    let Some(convo) = app.conversations.get(&chat_id) else {
        return;
    };
    let Some(idx) = conversation::index_of(&convo.messages, message_id) else {
        return;
    };
    let chips = chips_for_message(&convo.messages[idx], &app.media);

    let Some(convo) = app.conversations.get_mut(&chat_id) else {
        return;
    };
    let Some(sel) = convo.selection.as_mut() else {
        return;
    };
    sel.chip_cursor = sel.chip_cursor.min(chips.len().saturating_sub(1));
    sel.chips = chips;
    clamp_chip_scroll(sel);
}

/// The bridge between the message model and [`chips_for`]: TDLib's caps plus
/// the three local facts only the client knows.
fn chips_for_message(msg: &MessageView, media: &MediaState) -> Vec<Chip> {
    let file = file_of(&msg.content);
    let downloaded = file
        .and_then(|id| media.files.get(&id))
        .is_some_and(|f| f.is_completed);
    chips_for(
        &msg.caps,
        msg.is_outgoing,
        file.is_some(),
        downloaded,
        matches!(msg.send_state, SendState::Failed(_)),
    )
}

// --- chip invocation ----------------------------------------------------

/// Chip → effects. Two chips (Reply, Edit) are pure composer-context moves
/// and return nothing: T28 pops `Focus::Selection` for them so the user lands
/// back in the composer with the context set. Delete pushes its confirmation
/// modal instead of deleting (spec §5.3: destructive actions confirm first),
/// and T27's modal handler is what finally issues `DeleteMessages`.
fn invoke(app: &mut AppState, chat_id: ChatId, message_id: MessageId, chip: Chip) -> Vec<Effect> {
    let Some(msg) = app
        .conversations
        .get(&chat_id)
        .and_then(|convo| {
            conversation::index_of(&convo.messages, message_id).map(|i| &convo.messages[i])
        })
        .cloned()
    else {
        return Vec::new();
    };

    match chip {
        Chip::Reply => {
            app.composer.reply_to = Some(message_id);
            Vec::new()
        }
        Chip::Edit => {
            // Captions become editable with the media work (T36-T38). Until
            // then a non-text message enters no edit mode at all: arming the
            // composer with an empty buffer would destroy the caption on
            // submit.
            let MessageContent::Text(body) = &msg.content else {
                return Vec::new();
            };
            let text = body.text.clone();
            app.composer.editing = Some(message_id);
            app.composer.input = InputField {
                cursor: text.len(),
                text,
            };
            Vec::new()
        }
        // v1 forwards to the chat the chat list cursor is on; the palette
        // chat picker lands in T41 (plan T26 note). No selected chat means
        // no destination, so nothing is sent.
        Chip::Forward => match app.chat_list.selected {
            Some(to_chat_id) => vec![Effect::Td(TdRequest::ForwardMessages {
                to_chat_id,
                from_chat_id: chat_id,
                message_ids: vec![message_id],
            })],
            None => Vec::new(),
        },
        Chip::React => vec![Effect::Td(TdRequest::ToggleReaction {
            chat_id,
            message_id,
            emoji: DEFAULT_REACTION.to_string(),
        })],
        Chip::Copy => vec![Effect::CopyToClipboard {
            text: copy_text(&msg.content),
        }],
        Chip::Delete => {
            app.focus.push(Focus::Modal(ModalKind::ConfirmDelete {
                chat_id,
                message_id,
                can_revoke: msg.caps.can_be_deleted_for_all_users,
            }));
            Vec::new()
        }
        Chip::Download => match file_of(&msg.content) {
            Some(file_id) => vec![Effect::Td(TdRequest::DownloadFile {
                file_id,
                priority: DOWNLOAD_PRIORITY,
            })],
            None => Vec::new(),
        },
        Chip::Open => match file_of(&msg.content)
            .and_then(|id| app.media.files.get(&id))
            .and_then(|f| f.local_path.clone())
        {
            Some(path) => vec![Effect::OpenExternal { path }],
            None => Vec::new(),
        },
        Chip::Resend => resend(app, chat_id, &msg),
    }
}

/// Re-issues a failed send as a fresh `SendMessageText` and drops the failed
/// entry from the window: the retry arrives as its own optimistic message
/// (T25's send path), so keeping the corpse would show the text twice.
/// Selection is cleared along with it — there is nothing left to point at.
///
/// File sends are not resendable yet (uploads land in T36); their failed
/// entry is left alone rather than silently discarded.
fn resend(app: &mut AppState, chat_id: ChatId, msg: &MessageView) -> Vec<Effect> {
    let MessageContent::Text(text) = &msg.content else {
        return Vec::new();
    };
    let reply_to = msg.reply_to.as_ref().map(|r| r.message_id);
    let failed_id = msg.id;

    if let Some(convo) = app.conversations.get_mut(&chat_id) {
        convo.messages.retain(|m| m.id != failed_id);
        conversation::drop_selection_if_gone(convo);
    }
    vec![Effect::Td(TdRequest::SendMessageText {
        chat_id,
        reply_to,
        text: text.clone(),
    })]
}

/// What `Copy` puts on the clipboard: the message's text, or a file's name
/// when there is no text to copy. Never a rendered decoration.
fn copy_text(content: &MessageContent) -> String {
    match content {
        MessageContent::Text(f) => f.text.clone(),
        MessageContent::Video { file_name, .. }
        | MessageContent::Audio { file_name, .. }
        | MessageContent::Document { file_name, .. } => file_name.clone(),
        MessageContent::Photo { .. }
        | MessageContent::Sticker { .. }
        | MessageContent::Unsupported { .. } => String::new(),
    }
}

fn file_of(content: &MessageContent) -> Option<FileId> {
    match content {
        MessageContent::Photo { file_id, .. }
        | MessageContent::Video { file_id, .. }
        | MessageContent::Audio { file_id, .. }
        | MessageContent::Document { file_id, .. } => Some(*file_id),
        MessageContent::Text(_)
        | MessageContent::Sticker { .. }
        | MessageContent::Unsupported { .. } => None,
    }
}

// --- reply excerpts (architecture §7) ------------------------------------

/// TDLib inlines the quoted content only for cross-chat replies, so same-chat
/// replies arrive with an empty `excerpt` (and sometimes an empty
/// `sender_name`). Selecting a message is the moment to fill them from the
/// local window: the replied-to message is usually loaded, and a reply header
/// reading "Ada: …" beats a blank one. Nothing is invented — if the source
/// message is not in the window, the excerpt stays empty and the ui renders
/// the plain "reply to a message" form.
fn fill_reply_excerpt(convo: &mut ConversationState, message_id: MessageId) {
    let Some(idx) = conversation::index_of(&convo.messages, message_id) else {
        return;
    };
    let Some(reply) = convo.messages[idx].reply_to.as_ref() else {
        return;
    };
    if !reply.excerpt.is_empty() && !reply.sender_name.is_empty() {
        return;
    }
    let replied_id = reply.message_id;
    let Some(src_idx) = conversation::index_of(&convo.messages, replied_id) else {
        return;
    };
    let source = &convo.messages[src_idx];
    let excerpt = excerpt_of(&source.content);
    let sender_name = source.sender_name.clone();

    let Some(reply) = convo.messages[idx].reply_to.as_mut() else {
        return;
    };
    if reply.excerpt.is_empty() {
        reply.excerpt = excerpt;
    }
    if reply.sender_name.is_empty() {
        reply.sender_name = sender_name;
    }
}

/// One line, capped at [`REPLY_EXCERPT_MAX_CHARS`] characters (not bytes).
fn excerpt_of(content: &MessageContent) -> String {
    let raw = match content {
        MessageContent::Text(f) => f.text.clone(),
        MessageContent::Photo { caption, .. } if !caption.text.is_empty() => caption.text.clone(),
        MessageContent::Photo { .. } => "Photo".to_string(),
        MessageContent::Video {
            caption, file_name, ..
        } => {
            if caption.text.is_empty() {
                file_name.clone()
            } else {
                caption.text.clone()
            }
        }
        MessageContent::Audio { file_name, .. } => file_name.clone(),
        MessageContent::Document {
            caption, file_name, ..
        } => {
            if caption.text.is_empty() {
                file_name.clone()
            } else {
                caption.text.clone()
            }
        }
        MessageContent::Sticker { emoji } => emoji.clone(),
        MessageContent::Unsupported { description } => description.clone(),
    };
    let line = raw.lines().next().unwrap_or_default().trim();
    if line.chars().count() <= REPLY_EXCERPT_MAX_CHARS {
        return line.to_string();
    }
    let mut out: String = line.chars().take(REPLY_EXCERPT_MAX_CHARS - 1).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::Screen;
    use crate::effect::TelemetryMode;
    use crate::model::entity::FormattedText;
    use crate::model::ids::UserId;
    use crate::model::message::{FileSnapshot, ReplyPreview, Sender};
    use crate::model::time::Millis;
    use crate::state::auth::{AuthField, AuthState};
    use crate::state::chat_list::ChatListState;
    use crate::state::composer::ComposerState;
    use crate::state::consent::{ConsentChoice, ConsentState};
    use crate::state::conversation::{self, Scroll};
    use crate::state::focus::FocusStack;
    use crate::state::presence::PresenceState;
    use crate::state::toasts::ToastState;
    use crate::td::update::{AuthPhase, ConnectionPhase};
    use std::collections::HashMap;
    use std::path::PathBuf;

    const CHAT: ChatId = ChatId(1);
    const OTHER_CHAT: ChatId = ChatId(2);

    fn text(s: &str) -> FormattedText {
        FormattedText {
            text: s.to_string(),
            entities: Vec::new(),
        }
    }

    fn msg(id: i64) -> MessageView {
        MessageView {
            id: MessageId(id),
            chat_id: CHAT,
            sender: Sender::User(UserId(1)),
            sender_name: "Ada".to_string(),
            is_outgoing: false,
            date: 1_700_000_000 + id,
            content: MessageContent::Text(text(&format!("msg {id}"))),
            reply_to: None,
            send_state: SendState::Sent,
            reactions: Vec::new(),
            caps: MessageCaps::default(),
            is_edited: false,
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
            open_chat: None,
            composer: ComposerState::default(),
            modal_ui: None,
            palette: None,
            chat_search: None,
            toasts: ToastState::default(),
            media: crate::state::media::MediaState::default(),
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
            now: Millis(0),
        }
    }

    /// Opens CHAT with `messages`, focused in selection mode. Paging is
    /// parked in `Exhausted` so the near-top trigger does not add history
    /// requests to the effect lists under test (paging itself is T17's).
    fn with_messages(messages: Vec<MessageView>) -> AppState {
        let mut app = fixture_state();
        conversation::open(&mut app, CHAT);
        let convo = app.conversations.get_mut(&CHAT).unwrap();
        for m in messages {
            convo.messages.push_back(m);
        }
        convo.paging = crate::state::history::PagingState::Exhausted;
        // Selection sits ON TOP of the composer, the way T25's `↑`-on-empty
        // push leaves it — so `Esc` has something to pop back to.
        app.focus = FocusStack::new(Focus::Composer);
        app.focus.push(Focus::Selection);
        app
    }

    fn selection(app: &AppState) -> &SelectionState {
        app.conversations[&CHAT]
            .selection
            .as_ref()
            .expect("selection present")
    }

    // --- entering ---------------------------------------------------------

    #[test]
    fn selection_starts_at_newest() {
        let mut app = with_messages((1..=5).map(msg).collect());

        enter(&mut app);

        assert_eq!(selection(&app).message_id, MessageId(5));
        assert_eq!(selection(&app).chip_cursor, 0);
        assert_eq!(selection(&app).chip_scroll, 0);
        // The newest message is the bottom of the viewport, so the anchor
        // re-pins rather than freezing on an id.
        assert_eq!(app.conversations[&CHAT].scroll, Scroll::Bottom);
    }

    #[test]
    fn selection_entry_fires_get_message_properties() {
        let mut app = with_messages((1..=3).map(msg).collect());

        let effects = enter(&mut app);

        assert_eq!(effects.len(), 1);
        assert!(matches!(
            effects[0],
            Effect::Td(TdRequest::GetMessageProperties {
                chat_id: CHAT,
                message_id: MessageId(3),
            })
        ));
    }

    #[test]
    fn failed_send_selection_asks_for_no_properties_and_offers_resend_only() {
        let mut m = msg(1);
        m.send_state = SendState::Failed(TdError::NetTimeout);
        m.caps = full_caps();
        let mut app = with_messages(vec![m]);

        let effects = enter(&mut app);

        assert!(
            effects.is_empty(),
            "nothing to ask TDLib about: {effects:?}"
        );
        assert_eq!(selection(&app).chips, vec![Chip::Resend, Chip::Delete]);
    }

    #[test]
    fn entering_an_empty_conversation_selects_nothing() {
        let mut app = with_messages(Vec::new());
        assert!(enter(&mut app).is_empty());
        assert!(app.conversations[&CHAT].selection.is_none());
    }

    #[test]
    fn reply_excerpt_filled_from_window() {
        let mut source = msg(1);
        source.sender_name = "Grace".to_string();
        source.content = MessageContent::Text(text("the original message"));
        let mut reply = msg(2);
        reply.reply_to = Some(ReplyPreview {
            message_id: MessageId(1),
            sender_name: String::new(),
            excerpt: String::new(),
        });
        let mut app = with_messages(vec![source, reply]);

        enter(&mut app);

        let filled = app.conversations[&CHAT].messages[1]
            .reply_to
            .clone()
            .unwrap();
        assert_eq!(filled.excerpt, "the original message");
        assert_eq!(filled.sender_name, "Grace");
    }

    #[test]
    fn reply_excerpt_delivered_by_tdlib_is_left_alone() {
        let mut reply = msg(2);
        reply.reply_to = Some(ReplyPreview {
            message_id: MessageId(1),
            sender_name: "Someone".to_string(),
            excerpt: "as delivered".to_string(),
        });
        let mut app = with_messages(vec![msg(1), reply]);

        enter(&mut app);

        let kept = app.conversations[&CHAT].messages[1]
            .reply_to
            .clone()
            .unwrap();
        assert_eq!(kept.excerpt, "as delivered");
        assert_eq!(kept.sender_name, "Someone");
    }

    #[test]
    fn reply_excerpt_stays_empty_when_source_is_not_loaded() {
        let mut reply = msg(2);
        reply.reply_to = Some(ReplyPreview {
            message_id: MessageId(999),
            sender_name: String::new(),
            excerpt: String::new(),
        });
        let mut app = with_messages(vec![reply]);

        enter(&mut app);

        assert_eq!(
            app.conversations[&CHAT].messages[0]
                .reply_to
                .as_ref()
                .unwrap()
                .excerpt,
            ""
        );
    }

    #[test]
    fn long_excerpts_are_cut_to_one_line() {
        let long = "x".repeat(REPLY_EXCERPT_MAX_CHARS + 20);
        let mut source = msg(1);
        source.content = MessageContent::Text(text(&format!("{long}\nsecond line")));
        let mut reply = msg(2);
        reply.reply_to = Some(ReplyPreview {
            message_id: MessageId(1),
            sender_name: "Ada".to_string(),
            excerpt: String::new(),
        });
        let mut app = with_messages(vec![source, reply]);

        enter(&mut app);

        let excerpt = app.conversations[&CHAT].messages[1]
            .reply_to
            .as_ref()
            .unwrap()
            .excerpt
            .clone();
        assert_eq!(excerpt.chars().count(), REPLY_EXCERPT_MAX_CHARS);
        assert!(excerpt.ends_with('…'));
        assert!(!excerpt.contains("second line"));
    }

    // --- leaving ----------------------------------------------------------

    #[test]
    fn esc_returns_to_composer() {
        let mut app = with_messages(vec![msg(1)]);
        enter(&mut app);

        // Unclaimed: the router's generic pop handles Esc.
        assert!(handle_key(&mut app, Key::Esc).is_none());

        // T28 pops the focus and calls exit(), which drops the selection.
        assert!(app.focus.pop());
        exit(&mut app);
        assert_eq!(*app.focus.current(), Focus::Composer);
        assert!(app.conversations[&CHAT].selection.is_none());
    }

    #[test]
    fn keys_are_unclaimed_outside_selection_focus() {
        let mut app = with_messages(vec![msg(1)]);
        enter(&mut app);
        app.focus = FocusStack::new(Focus::Composer);
        assert!(handle_key(&mut app, Key::Up).is_none());
        assert!(handle_key(&mut app, Key::Enter).is_none());
    }

    // --- message movement --------------------------------------------------

    #[test]
    fn up_moves_to_the_older_message_and_refires_properties() {
        let mut app = with_messages((1..=5).map(msg).collect());
        enter(&mut app);

        let effects = handle_key(&mut app, Key::Up).expect("selection claims Up");

        assert_eq!(selection(&app).message_id, MessageId(4));
        assert!(matches!(
            effects[0],
            Effect::Td(TdRequest::GetMessageProperties {
                chat_id: CHAT,
                message_id: MessageId(4),
            })
        ));
        // The viewport follows the cursor.
        assert_eq!(
            app.conversations[&CHAT].scroll,
            Scroll::At {
                message_id: MessageId(4),
                line_offset: 0,
            }
        );
    }

    #[test]
    fn down_returns_to_the_newest_and_repins_to_bottom() {
        let mut app = with_messages((1..=3).map(msg).collect());
        enter(&mut app);
        handle_key(&mut app, Key::Up).unwrap();

        handle_key(&mut app, Key::Down).unwrap();

        assert_eq!(selection(&app).message_id, MessageId(3));
        assert_eq!(app.conversations[&CHAT].scroll, Scroll::Bottom);
    }

    #[test]
    fn movement_clamps_at_both_ends_without_refiring() {
        let mut app = with_messages(vec![msg(1)]);
        enter(&mut app);

        assert!(handle_key(&mut app, Key::Up).unwrap().is_empty());
        assert!(handle_key(&mut app, Key::Down).unwrap().is_empty());
        assert_eq!(selection(&app).message_id, MessageId(1));
    }

    #[test]
    fn moving_the_selection_near_the_top_pages_older_history() {
        let mut app = with_messages((1..=21).map(msg).collect());
        enter(&mut app);
        // Undo the fixture's paging park: this test is about the trigger.
        app.conversations.get_mut(&CHAT).unwrap().paging = crate::state::history::PagingState::Idle;
        app.conversations.get_mut(&CHAT).unwrap().selection = Some(SelectionState {
            message_id: MessageId(3),
            chips: Vec::new(),
            chip_cursor: 0,
            chip_scroll: 0,
        });

        let effects = handle_key(&mut app, Key::Up).unwrap();

        assert!(
            effects.iter().any(|e| matches!(
                e,
                Effect::Td(TdRequest::GetChatHistory {
                    chat_id: CHAT,
                    from_message_id: MessageId(1),
                    ..
                })
            )),
            "expected a history page request, got {effects:?}"
        );
    }

    // --- chip cursor -------------------------------------------------------

    #[test]
    fn left_right_walk_the_chip_row_and_clamp() {
        let mut m = msg(1);
        m.caps = full_caps();
        m.is_outgoing = true;
        let mut app = with_messages(vec![m]);
        enter(&mut app);
        let len = selection(&app).chips.len();
        assert!(len > 2, "fixture should produce a full row");

        assert!(handle_key(&mut app, Key::Left).unwrap().is_empty());
        assert_eq!(selection(&app).chip_cursor, 0, "clamps at the left edge");

        handle_key(&mut app, Key::Right).unwrap();
        assert_eq!(selection(&app).chip_cursor, 1);

        for _ in 0..len + 3 {
            handle_key(&mut app, Key::Right).unwrap();
        }
        assert_eq!(selection(&app).chip_cursor, len - 1);
    }

    #[test]
    fn chip_scroll_follows_the_cursor_out_of_the_visible_window() {
        let mut m = msg(1);
        m.caps = full_caps();
        m.is_outgoing = true;
        m.content = MessageContent::Document {
            file_id: FileId(9),
            file_name: "spec.pdf".to_string(),
            size: 10,
            caption: text(""),
        };
        let mut app = with_messages(vec![m]);
        enter(&mut app);
        // Reply, Forward, React, Copy, Edit, Download, Delete
        assert_eq!(selection(&app).chips.len(), 7);

        for _ in 0..4 {
            handle_key(&mut app, Key::Right).unwrap();
        }
        assert_eq!(selection(&app).chip_cursor, 4);
        assert_eq!(selection(&app).chip_scroll, 0, "still inside the window");

        handle_key(&mut app, Key::Right).unwrap();
        assert_eq!(selection(&app).chip_cursor, 5);
        assert_eq!(selection(&app).chip_scroll, 5 + 1 - CHIP_VISIBLE_MAX);

        for _ in 0..6 {
            handle_key(&mut app, Key::Left).unwrap();
        }
        assert_eq!(selection(&app).chip_cursor, 0);
        assert_eq!(selection(&app).chip_scroll, 0);
    }

    // --- capability refresh -------------------------------------------------

    #[test]
    fn properties_loaded_updates_chips() {
        let mut app = with_messages(vec![msg(1)]);
        enter(&mut app);
        assert_eq!(selection(&app).chips, vec![Chip::Reply, Chip::React]);

        handle_td_result(&mut app, CHAT, MessageId(1), &Ok(full_caps()));

        assert_eq!(app.conversations[&CHAT].messages[0].caps, full_caps());
        assert_eq!(
            selection(&app).chips,
            vec![
                Chip::Reply,
                Chip::Forward,
                Chip::React,
                Chip::Copy,
                Chip::Delete
            ],
            "incoming message: no Edit even with can_be_edited set"
        );
    }

    #[test]
    fn properties_error_keeps_existing_caps() {
        let mut m = msg(1);
        m.caps = full_caps();
        let mut app = with_messages(vec![m]);
        enter(&mut app);
        let before = selection(&app).chips.clone();

        handle_td_result(
            &mut app,
            CHAT,
            MessageId(1),
            &Err(TdError::FloodWait { seconds: 3 }),
        );

        assert_eq!(app.conversations[&CHAT].messages[0].caps, full_caps());
        assert_eq!(selection(&app).chips, before);
    }

    #[test]
    fn properties_for_a_no_longer_selected_message_still_update_caps() {
        let mut app = with_messages((1..=3).map(msg).collect());
        enter(&mut app);
        handle_key(&mut app, Key::Up).unwrap(); // now on message 2

        handle_td_result(&mut app, CHAT, MessageId(3), &Ok(full_caps()));

        assert_eq!(app.conversations[&CHAT].messages[2].caps, full_caps());
        // The row on screen belongs to message 2 and is untouched.
        assert_eq!(selection(&app).message_id, MessageId(2));
        assert_eq!(selection(&app).chips, vec![Chip::Reply, Chip::React]);
    }

    #[test]
    fn chip_cursor_survives_a_shrinking_row() {
        let mut m = msg(1);
        m.caps = full_caps();
        let mut app = with_messages(vec![m]);
        enter(&mut app);
        for _ in 0..4 {
            handle_key(&mut app, Key::Right).unwrap();
        }
        assert_eq!(selection(&app).chip_cursor, 4);

        handle_td_result(&mut app, CHAT, MessageId(1), &Ok(MessageCaps::default()));

        assert_eq!(selection(&app).chips, vec![Chip::Reply, Chip::React]);
        assert_eq!(selection(&app).chip_cursor, 1);
    }

    // --- chip invocation ----------------------------------------------------

    #[test]
    fn enter_invokes_the_focused_chip() {
        let mut app = with_messages(vec![msg(1)]);
        enter(&mut app);

        let effects = handle_key(&mut app, Key::Enter).expect("selection claims Enter");

        assert!(effects.is_empty(), "Reply is a composer-context move");
        assert_eq!(app.composer.reply_to, Some(MessageId(1)));
    }

    #[test]
    fn letter_shortcut_invokes_its_chip() {
        let mut app = with_messages(vec![msg(1)]);
        enter(&mut app);

        let effects = handle_key(&mut app, Key::Char('e')).expect("React answers to 'e'");

        assert!(matches!(
            effects.as_slice(),
            [Effect::Td(TdRequest::ToggleReaction { .. })]
        ));
        let Effect::Td(TdRequest::ToggleReaction { emoji, .. }) = &effects[0] else {
            unreachable!()
        };
        assert_eq!(emoji, DEFAULT_REACTION);
    }

    #[test]
    fn letters_without_a_chip_in_this_row_stay_unclaimed() {
        let mut app = with_messages(vec![msg(1)]);
        enter(&mut app);
        // Default caps: no Forward chip, so 'f' belongs to whoever is next.
        assert!(handle_key(&mut app, Key::Char('f')).is_none());
        assert!(handle_key(&mut app, Key::Char('?')).is_none());
    }

    #[test]
    fn delete_requires_modal_confirmation() {
        let mut m = msg(1);
        m.caps = full_caps();
        let mut app = with_messages(vec![m]);
        enter(&mut app);

        let effects = handle_key(&mut app, Key::Char('x')).expect("Delete answers to 'x'");

        assert!(
            effects.is_empty(),
            "nothing is deleted before the user confirms: {effects:?}"
        );
        assert_eq!(
            *app.focus.current(),
            Focus::Modal(ModalKind::ConfirmDelete {
                chat_id: CHAT,
                message_id: MessageId(1),
                can_revoke: true,
            })
        );
    }

    #[test]
    fn delete_modal_carries_revoke_capability_from_caps() {
        let mut m = msg(1);
        m.caps = MessageCaps {
            can_be_deleted_only_for_self: true,
            ..MessageCaps::default()
        };
        let mut app = with_messages(vec![m]);
        enter(&mut app);

        handle_key(&mut app, Key::Char('x')).unwrap();

        assert_eq!(
            *app.focus.current(),
            Focus::Modal(ModalKind::ConfirmDelete {
                chat_id: CHAT,
                message_id: MessageId(1),
                can_revoke: false,
            })
        );
    }

    #[test]
    fn forward_targets_selected_chat() {
        let mut m = msg(1);
        m.caps = full_caps();
        let mut app = with_messages(vec![m]);
        app.chat_list.selected = Some(OTHER_CHAT);
        enter(&mut app);

        let effects = handle_key(&mut app, Key::Char('f')).expect("Forward answers to 'f'");

        assert_eq!(effects.len(), 1);
        let Effect::Td(TdRequest::ForwardMessages {
            to_chat_id,
            from_chat_id,
            message_ids,
        }) = &effects[0]
        else {
            panic!("expected ForwardMessages, got {effects:?}");
        };
        assert_eq!(*to_chat_id, OTHER_CHAT);
        assert_eq!(*from_chat_id, CHAT);
        assert_eq!(message_ids, &vec![MessageId(1)]);
    }

    #[test]
    fn forward_without_a_destination_sends_nothing() {
        let mut m = msg(1);
        m.caps = full_caps();
        let mut app = with_messages(vec![m]);
        app.chat_list.selected = None;
        enter(&mut app);

        assert!(handle_key(&mut app, Key::Char('f')).unwrap().is_empty());
    }

    #[test]
    fn copy_puts_the_message_text_on_the_clipboard() {
        let mut m = msg(1);
        m.caps = full_caps();
        m.content = MessageContent::Text(text("copy me"));
        let mut app = with_messages(vec![m]);
        enter(&mut app);

        let effects = handle_key(&mut app, Key::Char('c')).unwrap();

        assert!(matches!(
            effects.as_slice(),
            [Effect::CopyToClipboard { text }] if text == "copy me"
        ));
    }

    #[test]
    fn copy_falls_back_to_the_file_name_for_documents() {
        let mut m = msg(1);
        m.caps = full_caps();
        m.content = MessageContent::Document {
            file_id: FileId(3),
            file_name: "invoice.pdf".to_string(),
            size: 10,
            caption: text(""),
        };
        let mut app = with_messages(vec![m]);
        enter(&mut app);

        let effects = handle_key(&mut app, Key::Char('c')).unwrap();

        assert!(matches!(
            effects.as_slice(),
            [Effect::CopyToClipboard { text }] if text == "invoice.pdf"
        ));
    }

    #[test]
    fn edit_loads_the_message_into_the_composer() {
        let mut m = msg(1);
        m.is_outgoing = true;
        m.caps = full_caps();
        m.content = MessageContent::Text(text("typo here"));
        let mut app = with_messages(vec![m]);
        enter(&mut app);

        let effects = handle_key(&mut app, Key::Char('d')).expect("Edit answers to 'd'");

        assert!(effects.is_empty(), "the edit is submitted by the composer");
        assert_eq!(app.composer.editing, Some(MessageId(1)));
        assert_eq!(app.composer.input.text, "typo here");
        assert_eq!(app.composer.input.cursor, "typo here".len());
    }

    #[test]
    fn download_requests_the_file_at_user_priority() {
        let mut m = msg(1);
        m.content = MessageContent::Photo {
            file_id: FileId(7),
            width: 10,
            height: 10,
            caption: text(""),
        };
        let mut app = with_messages(vec![m]);
        enter(&mut app);

        let effects = handle_key(&mut app, Key::Char('l')).expect("Download answers to 'l'");

        assert!(matches!(
            effects.as_slice(),
            [Effect::Td(TdRequest::DownloadFile {
                file_id: FileId(7),
                priority: DOWNLOAD_PRIORITY,
            })]
        ));
    }

    #[test]
    fn open_uses_the_downloaded_local_path() {
        let mut m = msg(1);
        m.content = MessageContent::Photo {
            file_id: FileId(7),
            width: 10,
            height: 10,
            caption: text(""),
        };
        let mut app = with_messages(vec![m]);
        app.media.files.insert(
            FileId(7),
            FileSnapshot {
                id: FileId(7),
                expected_size: 10,
                downloaded_size: 10,
                is_downloading: false,
                is_completed: true,
                local_path: Some(PathBuf::from("/tmp/photo.jpg")),
            },
        );
        enter(&mut app);
        assert!(selection(&app).chips.contains(&Chip::Open));

        let effects = handle_key(&mut app, Key::Char('o')).expect("Open answers to 'o'");

        assert!(matches!(
            effects.as_slice(),
            [Effect::OpenExternal { path }] if path == &PathBuf::from("/tmp/photo.jpg")
        ));
    }

    #[test]
    fn resend_reissues_text() {
        let mut failed = msg(-1);
        failed.send_state = SendState::Failed(TdError::NetTimeout);
        failed.content = MessageContent::Text(text("please arrive"));
        failed.reply_to = Some(ReplyPreview {
            message_id: MessageId(1),
            sender_name: "Ada".to_string(),
            excerpt: "earlier".to_string(),
        });
        let mut app = with_messages(vec![failed]);
        enter(&mut app);

        let effects = handle_key(&mut app, Key::Char('s')).expect("Resend answers to 's'");

        assert_eq!(effects.len(), 1);
        let Effect::Td(TdRequest::SendMessageText {
            chat_id,
            reply_to,
            text: body,
        }) = &effects[0]
        else {
            panic!("expected SendMessageText, got {effects:?}");
        };
        assert_eq!(*chat_id, CHAT);
        assert_eq!(*reply_to, Some(MessageId(1)));
        assert_eq!(body.text, "please arrive");
        // The failed corpse is gone, and so is the selection pointing at it.
        assert!(app.conversations[&CHAT].messages.is_empty());
        assert!(app.conversations[&CHAT].selection.is_none());
    }
}
