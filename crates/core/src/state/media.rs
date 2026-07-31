//! Download/upload tracking state. See docs/architecture.md §4.6.
//!
//! ## Download priority (architecture §4.6, plan T36)
//!
//! [`priority_for`] turns a message's distance from the scroll anchor into a
//! `DownloadFile` priority tier, using the same proximity proxy
//! `state/history.rs`'s paging trigger uses for "close to the anchor" — core
//! has no laid-out rows to measure real viewport distance from, only message
//! count.
//!
//! `selection.rs`'s user-initiated `Download` chip does not call this
//! function: it hardcodes `DOWNLOAD_PRIORITY = 32` directly, because T26
//! (selection) landed in M4, before this module had handlers to import from.
//! The two are equivalent by construction, not by accident — a selected
//! message is always at anchor distance 0, which `priority_for` also maps to
//! 32 (the top tier). That equivalence is the acceptable resolution the plan
//! calls for rather than a rewrite of `selection.rs`, which this task does
//! not own.
//!
//! ## Chip recompute on completion
//!
//! `selection.rs` recomputes a message's chip row from two triggers it owns:
//! landing the cursor on it ([`crate::state::selection::enter`]/movement) and
//! a `GetMessageProperties` answer arriving. Neither fires when a background
//! download finishes under a message that is *already* selected, so the
//! Download → Open affordance flip needs a third trigger, here.
//! `selection::recompute_chips` and `selection::chips_for_message` are
//! private to that module (this task owns `state/media.rs`, not
//! `state/selection.rs`), so [`handle_td`] rebuilds the equivalent
//! projection itself from the public [`crate::model::chips::chips_for`] —
//! same inputs, same derivation, just called from the file-table side of the
//! update instead of the selection side.
//!
//! ## Upload tracking
//!
//! TDLib has no dedicated "upload progress" push; an outgoing file's
//! progress surfaces as the same `updateFile` that downloads use, keyed by
//! the file TDLib assigned to the upload. Correlating that file id back to
//! the optimistic message id `SendMessageFile` mints is T39/T40's wiring
//! (the send-file flow does not exist yet). This module only owns the
//! table and the plain helpers ([`start_upload`], [`progress_upload`],
//! [`complete_upload`]) the future wiring will call.
//!
//! ## Cancel
//!
//! `Chip` (T26's fixed set, `model/chips.rs`) has no `Cancel` variant, and
//! adding one is a chip-set change outside this task's ownership
//! (`state/media.rs` + `app.rs` only). [`cancel_effect`] gives T40 the
//! `CancelDownloadFile` request to fire once a cancel affordance exists;
//! wiring it to a key/chip is deferred.

use std::collections::HashMap;

use crate::app::AppState;
use crate::effect::Effect;
use crate::model::chips::chips_for;
use crate::model::ids::{ChatId, FileId, MessageId};
use crate::model::message::{FileSnapshot, MessageContent, SendState};
use crate::state::conversation;
use crate::td::error::TdError;
use crate::td::request::TdRequest;
use crate::td::update::TdUpdate;

#[derive(Debug, Default)]
pub struct MediaState {
    pub files: HashMap<FileId, FileSnapshot>,
    /// Outgoing uploads keyed by the optimistic message id.
    pub uploads: HashMap<MessageId, UploadProgress>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct UploadProgress {
    pub chat_id: ChatId,
    pub uploaded: u64,
    pub total: u64,
}

/// `DownloadFile` priority tiers: ≤5 messages from the scroll anchor is the
/// user's immediate viewport (top tier, same value the user-initiated
/// Download chip uses); ≤20 is "about to scroll into view"; anything farther
/// is background prefetch, lowest tier.
pub fn priority_for(anchor_distance: usize) -> i8 {
    if anchor_distance <= 5 {
        32
    } else if anchor_distance <= 20 {
        16
    } else {
        4
    }
}

/// `updateFile` pushes: upsert the snapshot into the file table, then
/// recompute the chip row of any open selection pointing at a message that
/// references this file (see module docs — the completion-flip trigger
/// `selection.rs` cannot fire on its own).
pub fn handle_td(app: &mut AppState, upd: &TdUpdate) -> Vec<Effect> {
    let TdUpdate::File(snapshot) = upd else {
        return Vec::new();
    };
    upsert_file(app, snapshot.clone());
    Vec::new()
}

/// `TdResult::DownloadStarted`: TDLib's answer to a `DownloadFile` request.
/// `Ok` carries the file's state as of the call and is folded in exactly
/// like a push update. `Err` is log-worthy only (the runtime/dispatcher's
/// job, not this handler's) — the sole local optimism this table could hold
/// is `is_downloading`, and only a real `FileSnapshot` ever sets it, so
/// clearing it on a failed start is defensive rather than a normal-path
/// correction.
pub fn handle_td_result(
    app: &mut AppState,
    file_id: FileId,
    outcome: &Result<FileSnapshot, TdError>,
) -> Vec<Effect> {
    match outcome {
        Ok(snapshot) => upsert_file(app, snapshot.clone()),
        Err(_) => {
            if let Some(existing) = app.media.files.get_mut(&file_id) {
                existing.is_downloading = false;
            }
        }
    }
    Vec::new()
}

/// `CancelDownloadFile` for `file_id` — T40's wiring point once a cancel
/// affordance exists (module docs: `Chip` has no `Cancel` variant yet).
pub fn cancel_effect(file_id: FileId) -> Effect {
    Effect::Td(TdRequest::CancelDownloadFile { file_id })
}

/// Starts tracking an outgoing upload under the optimistic message id
/// `SendMessageFile` mints. Wiring (T39/T40) is not in place yet; this only
/// owns the table.
pub fn start_upload(app: &mut AppState, message_id: MessageId, chat_id: ChatId, total: u64) {
    app.media.uploads.insert(
        message_id,
        UploadProgress {
            chat_id,
            uploaded: 0,
            total,
        },
    );
}

/// Updates the uploaded-bytes count for a tracked upload. A `message_id`
/// with no tracked upload is a no-op — the caller may be racing a completion
/// that already dropped the entry.
pub fn progress_upload(app: &mut AppState, message_id: MessageId, uploaded: u64) {
    if let Some(progress) = app.media.uploads.get_mut(&message_id) {
        progress.uploaded = uploaded.min(progress.total);
    }
}

/// Drops the tracked upload. TDLib's own `updateFile`/`updateMessageContent`
/// for the now-sent message is the source of truth from here on, so nothing
/// else needs the progress entry once it completes.
pub fn complete_upload(app: &mut AppState, message_id: MessageId) {
    app.media.uploads.remove(&message_id);
}

fn upsert_file(app: &mut AppState, snapshot: FileSnapshot) {
    let file_id = snapshot.id;
    app.media.files.insert(file_id, snapshot);
    recompute_selection_chips_for_file(app, file_id);
}

/// Re-derives the chip row of every open selection (across every tracked
/// chat, not just the open one — a background download can complete under a
/// selection in a chat that is not currently on screen) whose selected
/// message references `file_id`.
fn recompute_selection_chips_for_file(app: &mut AppState, file_id: FileId) {
    let targets: Vec<(ChatId, MessageId)> = app
        .conversations
        .iter()
        .filter_map(|(chat_id, convo)| {
            let sel = convo.selection.as_ref()?;
            let idx = conversation::index_of(&convo.messages, sel.message_id)?;
            let references = file_id_of(&convo.messages[idx].content) == Some(file_id);
            references.then_some((*chat_id, sel.message_id))
        })
        .collect();

    for (chat_id, message_id) in targets {
        let Some(convo) = app.conversations.get(&chat_id) else {
            continue;
        };
        let Some(idx) = conversation::index_of(&convo.messages, message_id) else {
            continue;
        };
        let msg = &convo.messages[idx];
        let downloaded = app
            .media
            .files
            .get(&file_id)
            .is_some_and(|f| f.is_completed);
        let chips = chips_for(
            &msg.caps,
            msg.is_outgoing,
            true, // has_file: `targets` only contains messages that do.
            downloaded,
            matches!(msg.send_state, SendState::Failed(_)),
        );

        let Some(convo) = app.conversations.get_mut(&chat_id) else {
            continue;
        };
        let Some(sel) = convo.selection.as_mut() else {
            continue;
        };
        sel.chip_cursor = sel.chip_cursor.min(chips.len().saturating_sub(1));
        sel.chips = chips;
    }
}

/// Mirrors `selection.rs`'s private `file_of`: the file a message's content
/// carries, if any.
fn file_id_of(content: &MessageContent) -> Option<FileId> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::Screen;
    use crate::effect::TelemetryMode;
    use crate::model::chips::Chip;
    use crate::model::entity::FormattedText;
    use crate::model::ids::UserId;
    use crate::model::message::{MessageCaps, MessageView, Sender};
    use crate::model::time::Millis;
    use crate::state::auth::{AuthField, AuthState, InputField};
    use crate::state::chat_list::ChatListState;
    use crate::state::composer::ComposerState;
    use crate::state::consent::{ConsentChoice, ConsentState};
    use crate::state::conversation::{self as convo_mod, Scroll};
    use crate::state::focus::{Focus, FocusStack};
    use crate::state::presence::PresenceState;
    use crate::state::selection;
    use crate::state::toasts::ToastState;
    use crate::td::update::{AuthPhase, ConnectionPhase};
    use std::path::PathBuf;

    const CHAT: ChatId = ChatId(1);
    const FILE: FileId = FileId(7);

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

    fn photo_message(id: i64, file_id: FileId) -> MessageView {
        MessageView {
            id: MessageId(id),
            chat_id: CHAT,
            sender: Sender::User(UserId(1)),
            sender_name: "Ada".to_string(),
            is_outgoing: false,
            date: 1_700_000_000 + id,
            content: MessageContent::Photo {
                file_id,
                width: 10,
                height: 10,
                caption: FormattedText {
                    text: String::new(),
                    entities: Vec::new(),
                },
            },
            reply_to: None,
            send_state: SendState::Sent,
            reactions: Vec::new(),
            caps: MessageCaps::default(),
            is_edited: false,
        }
    }

    fn undownloaded_snapshot() -> FileSnapshot {
        FileSnapshot {
            id: FILE,
            expected_size: 1_000,
            downloaded_size: 0,
            is_downloading: true,
            is_completed: false,
            local_path: None,
        }
    }

    /// A conversation with one photo message, selected — the way T26's
    /// `enter` leaves it after landing on the newest message.
    fn app_with_selected_photo() -> AppState {
        let mut app = fixture_state();
        convo_mod::open(&mut app, CHAT);
        let convo = app.conversations.get_mut(&CHAT).unwrap();
        convo.messages.push_back(photo_message(1, FILE));
        convo.paging = crate::state::history::PagingState::Exhausted;
        convo.scroll = Scroll::Bottom;
        app.focus = FocusStack::new(Focus::Composer);
        app.focus.push(Focus::Selection);
        selection::enter(&mut app);
        app
    }

    #[test]
    fn progress_updates_downloaded_size() {
        let mut app = fixture_state();
        handle_td(&mut app, &TdUpdate::File(undownloaded_snapshot()));
        assert_eq!(app.media.files[&FILE].downloaded_size, 0);

        let mut halfway = undownloaded_snapshot();
        halfway.downloaded_size = 500;
        handle_td(&mut app, &TdUpdate::File(halfway));

        assert_eq!(app.media.files[&FILE].downloaded_size, 500);
        assert!(app.media.files[&FILE].is_downloading);
        assert!(!app.media.files[&FILE].is_completed);
    }

    #[test]
    fn completion_sets_local_path_and_completed() {
        let mut app = fixture_state();
        handle_td(&mut app, &TdUpdate::File(undownloaded_snapshot()));

        let completed = FileSnapshot {
            id: FILE,
            expected_size: 1_000,
            downloaded_size: 1_000,
            is_downloading: false,
            is_completed: true,
            local_path: Some(PathBuf::from("/tmp/photo.jpg")),
        };
        handle_td(&mut app, &TdUpdate::File(completed));

        let snapshot = &app.media.files[&FILE];
        assert!(snapshot.is_completed);
        assert!(!snapshot.is_downloading);
        assert_eq!(snapshot.local_path, Some(PathBuf::from("/tmp/photo.jpg")));
    }

    #[test]
    fn completion_flips_selected_chip_from_download_to_open() {
        let mut app = app_with_selected_photo();
        handle_td(&mut app, &TdUpdate::File(undownloaded_snapshot()));
        let sel = app.conversations[&CHAT].selection.as_ref().unwrap();
        assert!(sel.chips.contains(&Chip::Download));
        assert!(!sel.chips.contains(&Chip::Open));

        let completed = FileSnapshot {
            id: FILE,
            expected_size: 1_000,
            downloaded_size: 1_000,
            is_downloading: false,
            is_completed: true,
            local_path: Some(PathBuf::from("/tmp/photo.jpg")),
        };
        handle_td(&mut app, &TdUpdate::File(completed));

        let sel = app.conversations[&CHAT].selection.as_ref().unwrap();
        assert!(!sel.chips.contains(&Chip::Download));
        assert!(sel.chips.contains(&Chip::Open));
    }

    #[test]
    fn priority_tiers_by_viewport_proximity() {
        // ≤5: top tier.
        assert_eq!(priority_for(0), 32);
        assert_eq!(priority_for(5), 32);
        // 6..=20: middle tier.
        assert_eq!(priority_for(6), 16);
        assert_eq!(priority_for(20), 16);
        // >20: background tier.
        assert_eq!(priority_for(21), 4);
        assert_eq!(priority_for(1_000), 4);
    }

    #[test]
    fn cancel_emits_cancel_download() {
        // `Chip` has no Cancel variant (module docs): this only checks the
        // request `cancel_effect` builds for T40's future wiring.
        assert!(matches!(
            cancel_effect(FILE),
            Effect::Td(TdRequest::CancelDownloadFile { file_id: FILE })
        ));
    }

    #[test]
    fn download_started_ok_upserts_file() {
        let mut app = fixture_state();
        handle_td_result(&mut app, FILE, &Ok(undownloaded_snapshot()));
        assert_eq!(app.media.files[&FILE].expected_size, 1_000);
    }

    #[test]
    fn download_started_err_clears_downloading_optimism() {
        let mut app = fixture_state();
        handle_td(&mut app, &TdUpdate::File(undownloaded_snapshot()));
        assert!(app.media.files[&FILE].is_downloading);

        handle_td_result(&mut app, FILE, &Err(TdError::NetTimeout));

        assert!(!app.media.files[&FILE].is_downloading);
    }

    #[test]
    fn upload_lifecycle_tracks_progress() {
        let mut app = fixture_state();
        let msg_id = MessageId(-1);
        start_upload(&mut app, msg_id, CHAT, 1_000);
        assert_eq!(
            app.media.uploads[&msg_id],
            UploadProgress {
                chat_id: CHAT,
                uploaded: 0,
                total: 1_000,
            }
        );

        progress_upload(&mut app, msg_id, 400);
        assert_eq!(app.media.uploads[&msg_id].uploaded, 400);

        // Never exceeds the declared total, even if the caller overshoots.
        progress_upload(&mut app, msg_id, 5_000);
        assert_eq!(app.media.uploads[&msg_id].uploaded, 1_000);

        complete_upload(&mut app, msg_id);
        assert!(!app.media.uploads.contains_key(&msg_id));
    }

    #[test]
    fn progress_on_untracked_upload_is_a_no_op() {
        let mut app = fixture_state();
        progress_upload(&mut app, MessageId(-5), 100);
        assert!(app.media.uploads.is_empty());
    }
}
