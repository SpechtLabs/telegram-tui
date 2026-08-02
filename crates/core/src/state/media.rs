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
//! the file TDLib assigned to the upload. `app.rs` calls [`start_upload`]
//! when a file send is accepted and [`complete_upload`] when it resolves
//! (T40), so the table tracks which messages are in flight — but nothing
//! calls [`progress_upload`]: correlating an `updateFile` back to the
//! optimistic message id `SendMessageFile` minted needs a file id on
//! [`UploadProgress`] to match against, which this module does not record
//! yet. Until it does, a tracked upload's byte count stays at zero; see
//! `App::start_tracking_upload`'s "KNOWN GAP".
//!
//! ## Cancel
//!
//! `Chip` (T26's fixed set, `model/chips.rs`) has no `Cancel` variant, and
//! adding one is a chip-set change outside this task's ownership
//! (`state/media.rs` + `app.rs` only). [`cancel_effect`] gives T40 the
//! `CancelDownloadFile` request to fire once a cancel affordance exists;
//! wiring it to a key/chip is deferred.
//!
//! ## Auto-download (T66, design-language §6)
//!
//! [`auto_download_photos`] is the "auto-download all images and display
//! them inline when available" setting: photos only (never video/audio/
//! documents — a multi-hundred-MB video fetched because it scrolled past is
//! user-hostile), gated on [`MediaState::auto_download_photos`], scoped to
//! messages within `history::PAGE_TRIGGER_MESSAGES` of the scroll anchor —
//! the same proximity proxy `priority_for` already uses. It is called from
//! every place the visible window can change: `conversation::apply_history_page`,
//! a `NewMessage` arrival, `conversation::handle_key`'s scroll-anchor moves,
//! `App::scroll_conversation`'s mouse wheel, and `selection::select` landing
//! the anchor on a message.
//!
//! Every one of those triggers fires repeatedly for the same visible set, so
//! storm control is the point: [`MediaState::auto_download_requested`]
//! remembers every file id this function has ever emitted a `DownloadFile`
//! for, checked *before* consulting the file table — a request already
//! queued has no `FileSnapshot` yet to prove it, so the file table alone
//! cannot prevent a duplicate. A genuine failure
//! ([`handle_td_result`]'s `Err` arm) earns the file one more attempt (via
//! [`record_auto_download_failure`] clearing the id back out of
//! `auto_download_requested`) up to [`MAX_AUTO_DOWNLOAD_ATTEMPTS`]; past
//! that the id is left out of `auto_download_requested` for good and
//! [`should_auto_request`]'s failure-count check blocks it permanently,
//! rather than hammering a permanently broken file forever.
//! [`MAX_AUTO_DOWNLOAD_REQUESTS_PER_TRIGGER`] additionally caps how many
//! `DownloadFile` effects one call can emit, so a chat opening onto a long
//! run of undownloaded photos near the anchor cannot turn a single
//! `update()` call into a storm regardless of how large the loaded window
//! (up to `conversation::WINDOW_MAX_MESSAGES`) is.

use std::collections::{HashMap, HashSet};

use crate::app::AppState;
use crate::effect::Effect;
use crate::model::chips::chips_for;
use crate::model::ids::{ChatId, FileId, MessageId};
use crate::model::message::{FileSnapshot, MessageContent, SendState};
use crate::state::conversation::{self, Scroll};
use crate::state::history;
use crate::td::error::TdError;
use crate::td::request::TdRequest;
use crate::td::update::TdUpdate;

/// Auto-download only requests photos within this many messages of the
/// scroll anchor — the same window `history::PAGE_TRIGGER_MESSAGES` uses
/// elsewhere for "close enough to the anchor to matter" (module docs).
const AUTO_DOWNLOAD_WINDOW_MESSAGES: usize = history::PAGE_TRIGGER_MESSAGES;

/// Hard ceiling on `DownloadFile` effects one [`auto_download_photos`] call
/// may emit (module docs' storm-control section).
const MAX_AUTO_DOWNLOAD_REQUESTS_PER_TRIGGER: usize = 8;

/// How many auto-download attempts a single file id gets (the first request
/// plus retries after a genuine failure) before [`should_auto_request`]
/// blocks it for good.
const MAX_AUTO_DOWNLOAD_ATTEMPTS: u8 = 2;

#[derive(Debug)]
pub struct MediaState {
    pub files: HashMap<FileId, FileSnapshot>,
    /// Outgoing uploads keyed by the optimistic message id.
    pub uploads: HashMap<MessageId, UploadProgress>,
    /// Whether [`auto_download_photos`] requests anything at all (config
    /// `[app] auto_download_photos`, default on). `false` restores the
    /// pre-T66 behavior: nothing downloads until the user presses `⏎` on a
    /// message's `Download` chip.
    pub auto_download_photos: bool,
    /// Storm control: file ids [`auto_download_photos`] has already
    /// requested this session, so a trigger firing again for the same
    /// visible set never re-issues the same `DownloadFile` — including
    /// before any `updateFile`/`DownloadStarted` answer has landed to prove
    /// the first request is in flight (module docs).
    auto_download_requested: HashSet<FileId>,
    /// Per-file auto-download failure count (module docs).
    auto_download_failures: HashMap<FileId, u8>,
}

impl Default for MediaState {
    fn default() -> Self {
        MediaState {
            files: HashMap::new(),
            uploads: HashMap::new(),
            auto_download_photos: true,
            auto_download_requested: HashSet::new(),
            auto_download_failures: HashMap::new(),
        }
    }
}

impl MediaState {
    /// `App::new`'s constructor: everything at its default except the
    /// boot-configured auto-download setting. A free function rather than
    /// exposing the storm-control fields as `pub` — `App::new` has no
    /// business setting those, only the config-derived flag.
    pub fn new(auto_download_photos: bool) -> Self {
        MediaState {
            auto_download_photos,
            ..MediaState::default()
        }
    }
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
            record_auto_download_failure(app, file_id);
        }
    }
    Vec::new()
}

/// `CancelDownloadFile` for `file_id` — T40's wiring point once a cancel
/// affordance exists (module docs: `Chip` has no `Cancel` variant yet).
pub fn cancel_effect(file_id: FileId) -> Effect {
    Effect::Td(TdRequest::CancelDownloadFile { file_id })
}

/// Auto-downloads photos near `chat_id`'s scroll anchor. See the module
/// docs' "Auto-download" section for the trigger points, scope and storm
/// control. A no-op when the setting is off, the chat isn't tracked, or the
/// anchor doesn't name a message currently in the window (an empty
/// `Scroll::Bottom`, or a search jump whose page hasn't arrived yet — the
/// same "nothing loaded is near it yet" case `trigger_paging_if_near_top`
/// carves out).
pub fn auto_download_photos(app: &mut AppState, chat_id: ChatId) -> Vec<Effect> {
    if !app.media.auto_download_photos {
        return Vec::new();
    }
    let Some(convo) = app.conversations.get(&chat_id) else {
        return Vec::new();
    };
    let anchor_idx = match convo.scroll {
        Scroll::Bottom => convo.messages.len().checked_sub(1),
        // Which edge the anchor pins does not change which message it names,
        // and a download radius is measured in messages.
        Scroll::At { message_id, .. } | Scroll::AtTop { message_id } => {
            conversation::index_of(&convo.messages, message_id)
        }
    };
    let Some(anchor_idx) = anchor_idx else {
        return Vec::new();
    };

    // Collected up front so the borrow of `convo` ends before `app.media`
    // needs mutating below.
    let candidates: Vec<(FileId, usize)> = convo
        .messages
        .iter()
        .enumerate()
        .filter_map(|(idx, msg)| {
            // Outgoing photos are never a download target: the sender
            // already has the file on disk (it's what got uploaded), so
            // there is nothing for `DownloadFile` to fetch — the optimistic
            // append of a message the user just sent (this same trigger's
            // `NewMessage` call site) would otherwise ask for it right back.
            if msg.is_outgoing {
                return None;
            }
            let distance = idx.abs_diff(anchor_idx);
            if distance > AUTO_DOWNLOAD_WINDOW_MESSAGES {
                return None;
            }
            match &msg.content {
                MessageContent::Photo { file_id, .. } => Some((*file_id, distance)),
                _ => None,
            }
        })
        .collect();

    let mut effects = Vec::new();
    for (file_id, distance) in candidates {
        if effects.len() >= MAX_AUTO_DOWNLOAD_REQUESTS_PER_TRIGGER {
            break;
        }
        if !should_auto_request(app, file_id) {
            continue;
        }
        app.media.auto_download_requested.insert(file_id);
        effects.push(Effect::Td(TdRequest::DownloadFile {
            file_id,
            priority: priority_for(distance),
        }));
    }
    effects
}

/// Whether [`auto_download_photos`] should still ask for `file_id`: not
/// already requested this session, not permanently blocked by repeated
/// failure, and not already downloading or complete per the file table.
fn should_auto_request(app: &AppState, file_id: FileId) -> bool {
    if app.media.auto_download_requested.contains(&file_id) {
        return false;
    }
    if app
        .media
        .auto_download_failures
        .get(&file_id)
        .is_some_and(|attempts| *attempts >= MAX_AUTO_DOWNLOAD_ATTEMPTS)
    {
        return false;
    }
    match app.media.files.get(&file_id) {
        Some(snapshot) => !(snapshot.is_completed || snapshot.is_downloading),
        None => true,
    }
}

/// Bookkeeping for `auto_download_photos`'s storm control (module docs): a
/// failure earns the file one more auto-download attempt, by clearing it
/// out of `auto_download_requested`, unless it has already used up its
/// attempt budget — at which point it is left out of that set for good and
/// `should_auto_request`'s failure-count check takes over blocking it.
fn record_auto_download_failure(app: &mut AppState, file_id: FileId) {
    let attempts = app.media.auto_download_failures.entry(file_id).or_insert(0);
    *attempts += 1;
    if *attempts < MAX_AUTO_DOWNLOAD_ATTEMPTS {
        app.media.auto_download_requested.remove(&file_id);
    }
}

/// Starts tracking an outgoing upload under the optimistic message id
/// `SendMessageFile` mints. Called from `app.rs`'s `MessageSent` arm.
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
    let uploaded = snapshot.uploaded_size;
    app.media.files.insert(file_id, snapshot);
    advance_upload_for_file(app, file_id, uploaded);
    recompute_selection_chips_for_file(app, file_id);
}

/// Moves the progress bar of whichever in-flight upload owns `file_id`.
///
/// # Why the file id is derived rather than stored
///
/// TDLib reports upload progress as an ordinary `updateFile` keyed by the
/// *file* id it assigned, and [`UploadProgress`] is keyed by the optimistic
/// *message* id, so the two have to be joined somehow. The obvious fix — put
/// a `file_id` on `UploadProgress` at `start_upload` — is what
/// `App::start_tracking_upload`'s doc comment proposed, and it is the worse
/// one: `MessageContent` already carries the file id for every uploadable
/// kind, so storing a second copy is a denormalisation that can disagree
/// with the message it describes. Deriving it through [`file_id_of`] keeps
/// one fact in one place.
///
/// The scan costs a pass over tracked uploads, which is empty in any session
/// that is not currently sending and rarely more than one otherwise. It
/// mirrors [`recompute_selection_chips_for_file`] directly above, which
/// solves the same shape of problem the same way.
fn advance_upload_for_file(app: &mut AppState, file_id: FileId, uploaded: u64) {
    if uploaded == 0 || app.media.uploads.is_empty() {
        return;
    }
    let owner = app.media.uploads.iter().find_map(|(message_id, progress)| {
        let convo = app.conversations.get(&progress.chat_id)?;
        let idx = conversation::index_of(&convo.messages, *message_id)?;
        (file_id_of(&convo.messages[idx].content) == Some(file_id)).then_some(*message_id)
    });

    if let Some(message_id) = owner {
        progress_upload(app, message_id, uploaded);
    }
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
            crash_reports_available: false,
            telemetry_salt: [0u8; 32],
            now: Millis(0),
            visible_messages: None,
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
            uploaded_size: 0,
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

    // --- auto_download_photos (T66) ----------------------------------

    fn text_message(id: i64) -> MessageView {
        MessageView {
            id: MessageId(id),
            chat_id: CHAT,
            sender: Sender::User(UserId(1)),
            sender_name: "Ada".to_string(),
            is_outgoing: false,
            date: 1_700_000_000 + id,
            content: MessageContent::Text(FormattedText {
                text: format!("msg {id}"),
                entities: Vec::new(),
            }),
            reply_to: None,
            send_state: SendState::Sent,
            reactions: Vec::new(),
            caps: MessageCaps::default(),
            is_edited: false,
        }
    }

    fn video_message(id: i64, file_id: FileId) -> MessageView {
        let mut m = text_message(id);
        m.content = MessageContent::Video {
            file_id,
            file_name: "clip.mp4".to_string(),
            size: 100_000_000,
            duration_secs: 30,
            caption: FormattedText {
                text: String::new(),
                entities: Vec::new(),
            },
        };
        m
    }

    fn document_message(id: i64, file_id: FileId) -> MessageView {
        let mut m = text_message(id);
        m.content = MessageContent::Document {
            file_id,
            file_name: "report.pdf".to_string(),
            size: 500_000,
            caption: FormattedText {
                text: String::new(),
                entities: Vec::new(),
            },
        };
        m
    }

    /// A tracked, opened chat holding `messages` (pushed in order — callers
    /// keep them in ascending id order like every other window invariant in
    /// this codebase), anchored at `anchor_id`, paging exhausted so no
    /// `GetChatHistory` effect is mixed into what these tests assert on.
    fn app_with_messages(messages: Vec<MessageView>, anchor_id: i64) -> AppState {
        let mut app = fixture_state();
        convo_mod::open(&mut app, CHAT);
        let convo = app.conversations.get_mut(&CHAT).unwrap();
        for m in messages {
            convo.messages.push_back(m);
        }
        convo.paging = crate::state::history::PagingState::Exhausted;
        convo.scroll = Scroll::At {
            message_id: MessageId(anchor_id),
            line_offset: 0,
        };
        app
    }

    /// Pulls `(file_id, priority)` out of every `DownloadFile` effect,
    /// dropping anything else so assertions read as plainly as possible.
    fn download_requests(effects: &[Effect]) -> Vec<(FileId, i8)> {
        effects
            .iter()
            .filter_map(|e| match e {
                Effect::Td(TdRequest::DownloadFile { file_id, priority }) => {
                    Some((*file_id, *priority))
                }
                _ => None,
            })
            .collect()
    }

    #[test]
    fn auto_download_requests_photos_near_the_anchor_only() {
        const NEAR_AT_ANCHOR: FileId = FileId(101);
        const NEAR_WITHIN_WINDOW: FileId = FileId(102); // 15 messages away
        const FAR_OUTSIDE_WINDOW: FileId = FileId(103); // 25 messages away

        let messages: Vec<MessageView> = (1..=50)
            .map(|id| match id {
                25 => photo_message(id, NEAR_AT_ANCHOR),
                40 => photo_message(id, NEAR_WITHIN_WINDOW),
                50 => photo_message(id, FAR_OUTSIDE_WINDOW),
                _ => text_message(id),
            })
            .collect();
        let mut app = app_with_messages(messages, 25);

        let effects = auto_download_photos(&mut app, CHAT);

        assert_eq!(
            download_requests(&effects),
            vec![(NEAR_AT_ANCHOR, 32), (NEAR_WITHIN_WINDOW, 16)],
            "the photo 25 messages from the anchor must not be requested"
        );
    }

    #[test]
    fn never_requests_the_same_file_twice() {
        let mut app = app_with_messages(vec![photo_message(1, FILE)], 1);

        let first = auto_download_photos(&mut app, CHAT);
        assert_eq!(download_requests(&first), vec![(FILE, 32)]);

        // Every trigger this feature hangs off (history page, new message,
        // scroll) can fire repeatedly before TDLib ever answers the first
        // request, so the file table alone (still empty here) cannot be
        // what prevents a second `DownloadFile` for the same id.
        assert!(auto_download_photos(&mut app, CHAT).is_empty());
        assert!(auto_download_photos(&mut app, CHAT).is_empty());
    }

    #[test]
    fn failed_download_is_not_retried_forever() {
        let mut app = app_with_messages(vec![photo_message(1, FILE)], 1);

        let first = auto_download_photos(&mut app, CHAT);
        assert_eq!(download_requests(&first), vec![(FILE, 32)]);

        handle_td_result(&mut app, FILE, &Err(TdError::NetTimeout));
        let retry = auto_download_photos(&mut app, CHAT);
        assert_eq!(
            download_requests(&retry),
            vec![(FILE, 32)],
            "a genuine failure earns one retry"
        );

        handle_td_result(&mut app, FILE, &Err(TdError::NetTimeout));
        let after_second_failure = auto_download_photos(&mut app, CHAT);
        assert!(
            after_second_failure.is_empty(),
            "a permanently broken file must not be retried forever"
        );
    }

    #[test]
    fn disabled_setting_emits_nothing() {
        let mut app = app_with_messages(vec![photo_message(1, FILE)], 1);
        app.media.auto_download_photos = false;

        assert!(auto_download_photos(&mut app, CHAT).is_empty());
    }

    #[test]
    fn videos_and_documents_are_never_auto_downloaded() {
        let messages = vec![
            video_message(1, FileId(201)),
            document_message(2, FileId(202)),
        ];
        let mut app = app_with_messages(messages, 2);

        assert!(auto_download_photos(&mut app, CHAT).is_empty());
    }

    /// An outgoing photo (the local user's own send) already has its file on
    /// disk — nothing for `DownloadFile` to fetch. This is also what the
    /// `NewMessage` trigger's optimistic-append call site would otherwise
    /// hit the instant a photo send goes out (`conversation::handle_td`).
    #[test]
    fn outgoing_photos_are_never_auto_downloaded() {
        let mut outgoing = photo_message(1, FILE);
        outgoing.is_outgoing = true;
        let mut app = app_with_messages(vec![outgoing], 1);

        assert!(auto_download_photos(&mut app, CHAT).is_empty());
    }

    #[test]
    fn request_count_is_bounded_per_trigger() {
        // Every message near the anchor is an undownloaded photo with a
        // unique file id: the worst case for the per-trigger cap.
        let messages: Vec<MessageView> = (1..=25)
            .map(|id| photo_message(id, FileId(300 + id as i32)))
            .collect();
        let mut app = app_with_messages(messages, 13);

        let effects = auto_download_photos(&mut app, CHAT);

        assert_eq!(
            effects.len(),
            MAX_AUTO_DOWNLOAD_REQUESTS_PER_TRIGGER,
            "a long run of undownloaded photos must not turn one trigger into a storm"
        );
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
            uploaded_size: 0,
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
            uploaded_size: 0,
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

    /// The bug this closes: the bar rendered and never moved.
    ///
    /// TDLib reports upload progress as an ordinary `updateFile`, and until
    /// `uploaded_size` reached `FileSnapshot` there was nothing in the
    /// domain to move it with — `view/conversation.rs` drew
    /// `file_card_upload_line` from a `UploadProgress` frozen at whatever
    /// `start_upload` seeded. A bar stuck at 0% for the life of an upload
    /// reads as a stall, which is worse than showing nothing.
    #[test]
    fn an_update_file_push_advances_the_upload_it_belongs_to() {
        let msg_id = MessageId(-1);
        let mut app = app_with_messages(vec![document_message(-1, FILE)], -1);
        start_upload(&mut app, msg_id, CHAT, 1_000);
        assert_eq!(app.media.uploads[&msg_id].uploaded, 0);

        handle_td(
            &mut app,
            &TdUpdate::File(FileSnapshot {
                id: FILE,
                expected_size: 1_000,
                downloaded_size: 0,
                uploaded_size: 400,
                is_downloading: false,
                is_completed: false,
                local_path: None,
            }),
        );

        assert_eq!(
            app.media.uploads[&msg_id].uploaded, 400,
            "an updateFile carrying uploaded bytes must move the bar of the \
             message that owns that file"
        );
    }

    /// The join is by file id, not "whichever upload happens to be first".
    /// Two files in flight at once is the case a naive implementation gets
    /// wrong, and it is reachable — nothing serialises sends.
    #[test]
    fn an_update_file_push_moves_only_its_own_upload() {
        let other_file = FileId(99);
        let mut app = app_with_messages(
            vec![document_message(-2, other_file), document_message(-1, FILE)],
            -1,
        );
        start_upload(&mut app, MessageId(-2), CHAT, 1_000);
        start_upload(&mut app, MessageId(-1), CHAT, 1_000);

        handle_td(
            &mut app,
            &TdUpdate::File(FileSnapshot {
                id: FILE,
                expected_size: 1_000,
                downloaded_size: 0,
                uploaded_size: 700,
                is_downloading: false,
                is_completed: false,
                local_path: None,
            }),
        );

        assert_eq!(app.media.uploads[&MessageId(-1)].uploaded, 700);
        assert_eq!(
            app.media.uploads[&MessageId(-2)].uploaded,
            0,
            "the other upload must not move on a push that is not its file"
        );
    }

    /// A download push carries `uploaded_size: 0`, and must not reset an
    /// upload that happens to share the file id — which a forward does.
    #[test]
    fn a_download_push_does_not_disturb_an_upload() {
        let msg_id = MessageId(-1);
        let mut app = app_with_messages(vec![document_message(-1, FILE)], -1);
        start_upload(&mut app, msg_id, CHAT, 1_000);
        progress_upload(&mut app, msg_id, 600);

        handle_td(&mut app, &TdUpdate::File(undownloaded_snapshot()));

        assert_eq!(
            app.media.uploads[&msg_id].uploaded, 600,
            "a push with no uploaded bytes must leave the upload alone"
        );
    }

    #[test]
    fn progress_on_untracked_upload_is_a_no_op() {
        let mut app = fixture_state();
        progress_upload(&mut app, MessageId(-5), 100);
        assert!(app.media.uploads.is_empty());
    }
}
