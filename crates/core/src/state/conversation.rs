//! Per-chat message window. See docs/architecture.md §4.6, §5.3; design spec
//! §5.2.
//!
//! ## Scroll-anchor invariant (the point of this module)
//!
//! `Scroll::At { message_id, .. }` names a *message*, not an index or a row
//! offset. Prepending a history page in front of it, or evicting messages
//! from the window, never changes that id, so the anchor is inherently
//! stable across both operations — nothing here has to "fix up" an index
//! after a mutation. The only place the anchor id itself changes is
//! deliberate cursor movement (`handle_key`) or re-anchoring after the
//! anchored message was deleted out from under it.
//!
//! ## Eviction: one rule, two call sites
//!
//! `WINDOW_MAX_MESSAGES` is enforced by [`evict_excess`], which always drops
//! messages from whichever end of the window is farthest from the scroll
//! anchor:
//! - `Scroll::Bottom` is treated as anchored at the newest (back) end, so the
//!   far end is the front (oldest) — this is what fires while chatting live
//!   and a very long burst of `NewMessage`s arrives.
//! - `Scroll::At` looks up the anchor's real position in the window and
//!   evicts from whichever end is currently farther from it. Because history
//!   paging ([`apply_history_page`]) only ever fires while the anchor is
//!   within `history::PAGE_TRIGGER_MESSAGES` of the oldest loaded message —
//!   or older than the window entirely, see
//!   [`trigger_paging_if_near_top`] — "farthest from the anchor" reduces to
//!   the newest/back end in that case, i.e. the newly prepended page is
//!   always kept and older *already-read* context nearer the anchor is never
//!   what gets dropped. An anchor older than the whole window — a search hit
//!   whose page has not arrived yet — cannot be located, and [`evict_excess`]
//!   falls back to dropping from the front, which is the wrong end for it;
//!   that only becomes reachable at a full `WINDOW_MAX_MESSAGES` window, and
//!   ends the moment the page carrying the anchor lands.
//!
//! ## Marking messages read (T72)
//!
//! [`mark_visible_read`] is the only thing in the app that emits
//! `TdRequest::ViewMessages`. Without it TDLib is never told the user saw
//! anything: the sidebar badge never clears and the chat stays bold on the
//! user's phone and desktop. The badge itself is *not* zeroed locally —
//! `chat_list`'s `unread_count` only ever comes from TDLib's
//! `updateChatReadInbox` (spec §5.1), so a request that never lands leaves
//! the badge honestly showing unread instead of lying about it.
//!
//! Triggers, all of which can fire many times for the same set of messages:
//! opening a chat (`chat_list`'s Enter, the palette's open), a history page
//! landing, a `NewMessage` arriving in the open chat, a scroll that re-pins
//! to the bottom, and [`handle_tick`] as the retry safety net.
//!
//! ### Open versus actually looking
//!
//! Being *open* is not enough: the request is gated on `Scroll::Bottom`. See
//! [`mark_visible_read`] for why.
//!
//! ### Storm control
//!
//! `ViewMessages` is fire-and-forget (`dispatch.rs`'s `Completion`): there is
//! no completion action, so nothing can clear an "in flight" flag on the way
//! back. The only evidence the request worked is TDLib's own
//! `updateChatReadInbox` raising `last_read_inbox` — which is exactly what
//! stops the ids from being candidates again, so the common case needs no
//! extra bookkeeping at all. What does need it is the gap between sending and
//! that update landing, during which every trigger would otherwise re-send
//! the same ids on every keystroke.
//!
//! [`PendingView`] is that bookkeeping, and it lives on `ConversationState`
//! rather than in a side table like `media.rs`'s `auto_download_requested`:
//! read state is per-chat by nature, it is a watermark exactly like the
//! `last_read_inbox` it shadows, and it must die with the window it describes
//! — a `HashMap<ChatId, _>` on `AppState` would be a second lifetime to keep
//! in sync for no gain.
//!
//! It is a *watermark plus expiry*, not a plain "in flight" flag, because a
//! flag with no completion to clear it is precisely how a chat would wedge
//! permanently unread. A dropped or ignored request expires after
//! `VIEW_REQUEST_RETRY_AFTER_MS` and the next trigger (or tick) re-sends it,
//! up to [`MAX_VIEW_ATTEMPTS`] for the same watermark; a newer message raises
//! the watermark and starts a fresh budget. So the failure modes are bounded
//! in both directions: no storm while a request is outstanding, and no
//! silence if one goes missing.
//!
//! ## Reply excerpts (architecture §7 / T09 findings)
//!
//! TDLib leaves `ReplyPreview.excerpt` empty for same-chat replies; filling
//! it from the local window is T25/T26's job. Every handler in this module
//! only ever replaces a message wholesale with a `MessageView` TDLib itself
//! delivered, so there is nothing here that could clobber a filled-in
//! excerpt — this file simply never touches `reply_to` on an existing entry.

use std::collections::{BTreeSet, VecDeque};

use crate::app::{AppState, conversation_pane_visible};
use crate::effect::Effect;
use crate::model::entity::EntityKind;
use crate::model::ids::{ChatId, MessageId};
use crate::model::key::Key;
use crate::model::message::{MessageContent, MessageView};
use crate::model::time::Millis;
use crate::state::focus::Focus;
use crate::state::history::{self, PagingDirective, PagingState};
use crate::state::media;
use crate::state::selection::SelectionState;
use crate::state::toasts;
use crate::td::error::TdError;
use crate::td::request::TdRequest;
use crate::td::update::TdUpdate;

/// Bounded loaded window: memory stays flat in long-lived sessions.
pub const WINDOW_MAX_MESSAGES: usize = 500;

/// How many messages a conversation window must hold before the client stops
/// asking for older history on its own (T67 — see [`fill_viewport`]).
///
/// `history::PAGE_SIZE` worth of messages is the value because it is the only
/// number here that is already calibrated: it is what one `GetChatHistory`
/// asks for, so reaching it means one successful round trip's worth of
/// history is loaded. It is also comfortably more than any terminal viewport
/// can show — the conversation pane is at most a screen tall (tens of rows)
/// and spec §7.1 spends at least two rows on a message — so a window this
/// deep always fills the pane with something to scroll through. A larger
/// target would page in history nobody asked to see; a smaller one could
/// still leave the pane half empty on a tall terminal.
pub const VIEWPORT_FILL_TARGET_MESSAGES: usize = history::PAGE_SIZE as usize;

/// M3 approximation for `PageUp`/`PageDown`: the message-count equivalent of
/// "a page" while the UI has no viewport/layout info to consult (that lives
/// in `tgt-ui`, not here). `line_offset` therefore stays `0` for every
/// `handle_key`-driven scroll move in this milestone; T28/T38 may refine
/// PageUp/PageDown to a real row-based step once the ui crate can report
/// visible row counts back into core.
const PAGE_STEP_MESSAGES: usize = 10;

/// Ceiling on how many ids one `ViewMessages` request carries. `viewMessages`
/// moves a *watermark* (`last_read_inbox_message_id`), so viewing the newest
/// id implicitly marks everything older read — the list does not have to be
/// complete to be correct. That makes bounding it free: a
/// `WINDOW_MAX_MESSAGES` (500) window opened for the first time would
/// otherwise put five hundred ids in one request to say something the newest
/// one already says. `history::PAGE_SIZE` worth is the bound because it is
/// the same "one page of messages" unit everything else here is calibrated in.
pub const MAX_VIEW_MESSAGES_PER_REQUEST: usize = history::PAGE_SIZE as usize;

/// How long a sent `ViewMessages` is assumed to still be in flight. Long
/// enough that a normal round trip and its `updateChatReadInbox` land well
/// inside it (so the retry is never the reason a request goes out twice),
/// short enough that a user who is looking at a chat whose read receipt got
/// lost sees the badge clear in seconds rather than never.
const VIEW_REQUEST_RETRY_AFTER_MS: u64 = 5_000;

/// How many times the same watermark may be sent before this gives up on it —
/// the first request plus retries. Mirrors `media::MAX_AUTO_DOWNLOAD_ATTEMPTS`
/// and exists for the same reason: a request TDLib silently ignores must cost
/// a bounded number of retries, not one every
/// `VIEW_REQUEST_RETRY_AFTER_MS` for as long as the chat stays open. Any
/// newer message resets the budget by raising the watermark.
const MAX_VIEW_ATTEMPTS: u8 = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scroll {
    /// Pinned to newest; new messages keep the view at the bottom.
    Bottom,
    /// Anchored at a message (stable across prepends), offset in laid-out
    /// lines. The message's BOTTOM edge sits at the viewport's last row and
    /// the view fills backward from it.
    At {
        message_id: MessageId,
        line_offset: u16,
    },
    /// Anchored with this message's TOP edge at the viewport's first row —
    /// where a deliberate jump lands (the reply-quote chip, its backward
    /// hunt, and the mouse click on a quote line). The opposite fill
    /// direction to [`Scroll::At`], which pins a message's bottom edge to the
    /// last row.
    ///
    /// Carries no `line_offset`: the target's first line IS row 0.
    ///
    /// **Anchor movement preserves this flavour** ([`step_anchor`],
    /// [`move_anchor`]) — converting back to `At` on the next keypress would
    /// flip the target from the top of the screen to the bottom, which is a
    /// worse defect than the one this variant exists to fix and one no
    /// assertion on `convo.scroll` taken right after the jump can see.
    /// Everything that writes an anchor from a message id goes through
    /// [`anchored`] so the two flavours cannot drift apart.
    ///
    /// Core treats this identically to `At` everywhere else: both name the
    /// same message index, and only the view cares which edge is pinned.
    /// [`mark_visible_read`] is the deliberate exception — it gates on
    /// [`Scroll::Bottom`], and a user who jumped backward is not looking at
    /// the newest messages.
    ///
    /// The view falls back to a bottom-anchored fill when too few newer
    /// messages exist to fill the pane; see `view::conversation` and
    /// architecture §7.5.4.
    AtTop { message_id: MessageId },
}

/// Builds an anchor on `message_id` in `top`'s flavour. The one constructor
/// for both, so a caller that means to preserve the flavour it was handed
/// cannot half-do it. See [`Scroll::AtTop`].
///
/// `pub(crate)` for `app.rs`'s `scroll_conversation_move`, the mouse wheel's
/// own copy of [`move_anchor`], which has the same flavour to preserve.
pub(crate) fn anchored(message_id: MessageId, top: bool) -> Scroll {
    if top {
        Scroll::AtTop { message_id }
    } else {
        Scroll::At {
            message_id,
            line_offset: 0,
        }
    }
}

/// A `ViewMessages` request that has gone out but whose effect has not been
/// observed yet. See the module docs' storm-control section.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PendingView {
    /// Newest id the request asked TDLib to view. `viewMessages` moves a
    /// watermark, so this one id is what the whole request amounts to.
    pub up_to: MessageId,
    /// When the most recent attempt for `up_to` was sent.
    pub sent_at: Millis,
    /// Attempts spent on `up_to`, capped by [`MAX_VIEW_ATTEMPTS`].
    pub attempts: u8,
}

/// A search for a message older than the loaded window, started by the
/// jump-to-quote chip. Pages backward until the target arrives, the start of
/// history is reached, or the budget runs out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JumpHunt {
    pub target: MessageId,
    pub pages_spent: u8,
}

/// 20 × `history::PAGE_SIZE` = 1000 messages. A reply quote is nearly always
/// a message still in the window or a page behind it; this bounds the
/// pathological case (quoting something from a year ago) rather than sizing
/// the common one.
pub const MAX_HUNT_PAGES: u8 = 20;

#[derive(Debug)]
pub struct ConversationState {
    pub chat_id: ChatId,
    /// Ascending by message id; prepend on page, append on new message.
    pub messages: VecDeque<MessageView>,
    pub paging: PagingState,
    pub scroll: Scroll,
    pub revealed_spoilers: BTreeSet<MessageId>,
    pub last_read_inbox: MessageId,
    pub last_read_outbox: MessageId,
    /// Storm control for [`mark_visible_read`] (module docs). `None` means
    /// nothing is outstanding — either nothing was ever sent, or TDLib's
    /// `updateChatReadInbox` already confirmed what was.
    pub pending_view: Option<PendingView>,
    /// In-chat search hits (populated by state/search.rs).
    pub search_hits: Vec<MessageId>,
    /// Selection mode, per open chat and transient (architecture §4.6).
    /// `None` whenever the user is not in selection mode — and forced back to
    /// `None` by [`drop_selection_if_gone`] the moment the selected message
    /// leaves the window (deleted server-side, or evicted by the window
    /// bound), so no handler ever has to cope with a dangling selection.
    pub selection: Option<SelectionState>,
    /// An in-flight jump-to-quote search, or `None`. See [`JumpHunt`].
    pub hunt: Option<JumpHunt>,
}

/// Ensures a `ConversationState` exists for `chat_id` and makes it the open
/// chat. Idempotent: calling this on an already-tracked chat leaves its
/// window, paging state and scroll position untouched — only `open_chat` is
/// (re)set. Deliberately returns nothing: whoever drives the Enter-on-chat
/// flow (T15/T24) is responsible for the `Effect::Td(OpenChat)` and the
/// first `GetChatHistory` request; this function is pure bookkeeping.
pub fn open(app: &mut AppState, chat_id: ChatId) {
    app.conversations
        .entry(chat_id)
        .or_insert_with(|| ConversationState {
            chat_id,
            messages: VecDeque::new(),
            paging: PagingState::Idle,
            scroll: Scroll::Bottom,
            revealed_spoilers: BTreeSet::new(),
            last_read_inbox: MessageId(0),
            last_read_outbox: MessageId(0),
            pending_view: None,
            search_hits: Vec::new(),
            selection: None,
            hunt: None,
        });
    app.open_chat = Some(chat_id);
}

/// `CloseChat` for whichever chat is being left, if any and if it differs
/// from `new_chat_id` — TDLib's `openChat`/`closeChat` pair marks which chat
/// a client is actively viewing (governing that chat's per-chat update
/// subscription), and without a matching close, every chat a session has
/// ever opened stays marked open indefinitely.
///
/// Deliberately not folded into `open()`: that function is pure bookkeeping
/// by design (its own doc comment) and this needs to read `app.open_chat`'s
/// *outgoing* value, so callers must invoke it before `open()` overwrites
/// that field, not as part of the same call.
///
/// Re-opening the chat that is already open (`new_chat_id == app.open_chat`,
/// the common case of re-selecting the current row) returns nothing — this
/// is what keeps that path from emitting a close/open churn pair for the
/// same chat.
///
/// T9: also where a jump-to-quote hunt on the chat being left gets
/// cancelled — the one place both call sites (`chat_list::open_selected`,
/// `palette::open_chat`) already read `open_chat`'s outgoing value, so
/// cancelling it here rather than at each call site keeps it from being two
/// places that could drift, same as the `CloseChat` effect itself.
pub fn close_previous_chat(app: &mut AppState, new_chat_id: ChatId) -> Vec<Effect> {
    match app.open_chat {
        Some(previous) if previous != new_chat_id => {
            if let Some(convo) = app.conversations.get_mut(&previous) {
                cancel_hunt(convo);
            }
            vec![Effect::Td(TdRequest::CloseChat { chat_id: previous })]
        }
        _ => Vec::new(),
    }
}

/// `CloseChat` if a transition took the open chat from visible to
/// not-visible without the user ever picking a different chat (task #70):
/// `Esc` back to the single-pane chat list, or a resize that crosses the
/// breakpoint with focus still on the chat list. `was_visible` is the
/// caller's own snapshot of [`conversation_pane_visible`] from *before*
/// whatever it just did — this only reads the state *after*, so the two
/// together are the transition. Distinct from [`close_previous_chat`]: that
/// one fires when a different chat is about to become the one that is
/// open; this one fires when the same chat stops being visible without
/// `open_chat` itself ever changing, which `close_previous_chat`'s
/// before/after diff on `open_chat` cannot see at all.
pub fn close_if_now_hidden(app: &AppState, was_visible: bool) -> Vec<Effect> {
    let Some(chat_id) = app.open_chat else {
        return Vec::new();
    };
    if was_visible && !conversation_pane_visible(app) {
        vec![Effect::Td(TdRequest::CloseChat { chat_id })]
    } else {
        Vec::new()
    }
}

/// Whether `msg` carries a `Spoiler` entity not yet revealed for it — the
/// gate both the `Spoiler` click target and `Chip::Reveal` (architecture
/// §7.5.1, T77) use to decide whether reveal is even offered. Checks
/// whichever `FormattedText` the content variant carries (a caption counts
/// the same as body text; the render side masks both the same way).
pub fn has_unrevealed_spoiler(msg: &MessageView, revealed: &BTreeSet<MessageId>) -> bool {
    if revealed.contains(&msg.id) {
        return false;
    }
    let text = match &msg.content {
        MessageContent::Text(text)
        | MessageContent::Photo { caption: text, .. }
        | MessageContent::Video { caption: text, .. }
        | MessageContent::Document { caption: text, .. } => text,
        MessageContent::Audio { .. } | MessageContent::Sticker { .. } => return false,
        MessageContent::Unsupported { .. } => return false,
    };
    text.entities
        .iter()
        .any(|e| matches!(e.kind, EntityKind::Spoiler))
}

/// Reveals every spoiler run in `message_id` at once (architecture §7.5.1:
/// reveal is per-message, matching the single `spoilers_revealed` bit
/// already in the layout cache key — there is no per-run granularity to
/// preserve). A no-op if the chat or message is not tracked, which cannot
/// happen from either of this function's real call sites (a click or chip
/// invocation on a message currently on screen) but keeps this safe to call
/// speculatively.
pub fn reveal_spoilers(app: &mut AppState, chat_id: ChatId, message_id: MessageId) -> Vec<Effect> {
    if let Some(convo) = app.conversations.get_mut(&chat_id) {
        convo.revealed_spoilers.insert(message_id);
    }
    Vec::new()
}

/// Jumps the open chat's scroll anchor to `message_id` — the reply-quote
/// click target (architecture §7.5.1). Deliberately mirrors
/// `state::search`'s `step()`: sets the anchor and nothing else, whether
/// or not `message_id` is currently loaded. A quoted message is always
/// older than or equal to the message quoting it, so if it is not loaded it
/// is necessarily *older* than the window — exactly the case
/// `trigger_paging_if_near_top` already exists to notice and page toward
/// the next time the anchor is re-derived, the same path search hits
/// already rely on. No direct "load toward an arbitrary id" request is
/// issued here; see the architecture doc for why a second path into paging
/// was rejected.
///
/// Top-anchored (architecture §7.5.4): a click on a quote line is a
/// deliberate jump, and landing the target on the last visible row shows the
/// user a screenful of what they were already looking at. Set directly
/// rather than through [`anchor_to_top`] to preserve this function's "sets
/// the anchor and nothing else" contract — the target is routinely not
/// loaded, and the paging that fetches it is the ambient near-top trigger
/// §7.5.1 chose, not a request from here.
pub fn jump_to_message(app: &mut AppState, chat_id: ChatId, message_id: MessageId) -> Vec<Effect> {
    if let Some(convo) = app.conversations.get_mut(&chat_id) {
        convo.scroll = Scroll::AtTop { message_id };
    }
    Vec::new()
}

/// Begins a backward search for `target` (architecture §7.5.3): the
/// keyboard jump-to-quote chip's counterpart to [`jump_to_message`] for a
/// target that is not in the loaded window. Unlike that function, this does
/// issue a direct request — see §7.5.3 for why that is not a reopening of
/// the "no second path into paging" decision `jump_to_message`'s own doc
/// comment records.
///
/// Moves the anchor to the oldest loaded message first: `evict_excess` drops
/// from the FRONT while the anchor is at the bottom, which would evict each
/// page the hunt fetches as soon as the window hit `WINDOW_MAX_MESSAGES` —
/// the hunt would spend its whole budget and find nothing. With the anchor
/// at the front, eviction drops from the back and the window walks backward
/// instead. The moving anchor is also the progress indicator, which is why
/// there is no spinner.
pub fn start_hunt(app: &mut AppState, chat_id: ChatId, target: MessageId) -> Vec<Effect> {
    let now = app.now;
    let Some(convo) = app.conversations.get_mut(&chat_id) else {
        return Vec::new();
    };
    let Some(oldest) = convo.messages.front().map(|m| m.id) else {
        return Vec::new();
    };
    convo.hunt = Some(JumpHunt {
        target,
        pages_spent: 0,
    });
    let mut effects = anchor_to(convo, chat_id, oldest, now);
    if effects.is_empty() {
        // `anchor_to`'s paging trigger is a no-op when the machine is not
        // `Idle`; ask directly so the hunt always has a page in flight.
        effects.push(Effect::Td(TdRequest::GetChatHistory {
            chat_id,
            from_message_id: oldest,
            limit: history::PAGE_SIZE,
            only_local: false,
        }));
    }
    effects
}

/// Abandons an in-flight hunt. Called when the user takes over navigation:
/// `Esc`, closing the chat, or a manual scroll key.
pub fn cancel_hunt(convo: &mut ConversationState) {
    convo.hunt = None;
}

/// One step of an in-flight hunt, run from [`apply_history_page`] after a
/// page has been prepended (or a request failed). Returns the request that
/// continues it, a toast if it gave up, or nothing when there is no hunt to
/// advance.
///
/// Deliberately issues no `GetChatHistory` of its own when continuing:
/// [`anchor_to`]'s own near-top trigger already asks for the next page
/// (as does [`anchor_to_top`]'s — they share one body for exactly that
/// reason)
/// whenever `convo.paging` is `Idle`, which is exactly the state
/// `on_history_loaded` leaves it in for any non-empty page — the same
/// condition this function's own "keep going" branch requires. Adding a
/// second, unconditional request here (mirroring [`start_hunt`]'s fallback)
/// would double-dispatch: either against that trigger (a non-empty page,
/// where both fire) or against the empty-page retry `apply_history_page`'s
/// `directive` handling already issues on its own (where `convo.paging` is
/// `Loading`, not `Idle`, so the near-top trigger is silent but a request is
/// already outstanding regardless). `start_hunt`'s fallback exists because
/// it runs outside `apply_history_page` with no such directive alongside it;
/// this function always runs with one.
fn advance_hunt(app: &mut AppState, chat_id: ChatId) -> Vec<Effect> {
    let now = app.now;
    let Some(convo) = app.conversations.get_mut(&chat_id) else {
        return Vec::new();
    };
    let Some(hunt) = convo.hunt else {
        return Vec::new();
    };

    if index_of(&convo.messages, hunt.target).is_some() {
        // The landing: top-anchored, like every other quote jump
        // (architecture §7.5.4). The backward walk above stays bottom-
        // anchored — that anchor is the hunt's progress indicator, not
        // somewhere the user asked to be.
        convo.hunt = None;
        return anchor_to_top(convo, chat_id, hunt.target, now);
    }
    if matches!(convo.paging, PagingState::Exhausted) {
        convo.hunt = None;
        return toasts::on_action_failed(
            app,
            chat_id,
            "the quoted message is no longer available".to_string(),
        );
    }
    if hunt.pages_spent >= MAX_HUNT_PAGES {
        convo.hunt = None;
        return toasts::on_action_failed(
            app,
            chat_id,
            "could not find the quoted message".to_string(),
        );
    }
    let Some(oldest) = convo.messages.front().map(|m| m.id) else {
        convo.hunt = None;
        return Vec::new();
    };
    convo.hunt = Some(JumpHunt {
        target: hunt.target,
        pages_spent: hunt.pages_spent + 1,
    });
    anchor_to(convo, chat_id, oldest, now)
}

/// Tells TDLib which messages the user has actually seen in `chat_id`, so it
/// clears the unread badge and syncs the read state to the user's other
/// clients. See the module docs for the trigger points and the storm control;
/// this doc comment covers what "seen" is taken to mean.
///
/// ## Only while pinned to the bottom
///
/// A chat being open is not evidence the user read anything. The window can
/// be scrolled arbitrarily far back into history, and everything unread is by
/// definition *newer* than what is on screen there — below the fold, unseen.
/// Marking those read would be a claim the user never made, and unlike most
/// local state it is not private or recoverable: it clears the badge on their
/// phone and, in a private chat, shows the other side a read receipt for a
/// message that was never looked at. Getting it wrong in the other direction
/// costs a badge that clears a moment later, when the user scrolls down.
///
/// So the gate is `Scroll::Bottom` — core's only available proxy for "looking
/// at the newest messages", since the laid-out viewport lives in `tgt-ui` and
/// never comes back here. It is a faithful proxy in both directions: the
/// scroll keys re-pin to `Scroll::Bottom` the moment the anchor reaches the
/// newest loaded message ([`move_anchor`]), and every chat opens pinned there.
///
/// The bottom of the *loaded window* is also the bottom of the chat: v1 only
/// ever pages backwards, so the newest loaded message is always the newest
/// message that exists.
pub fn mark_visible_read(app: &mut AppState, chat_id: ChatId) -> Vec<Effect> {
    // A background chat keeps receiving messages and history pages
    // (`append_new_message`'s doc comment); none of that is on screen.
    if app.open_chat != Some(chat_id) {
        return Vec::new();
    }
    let now = app.now;
    let Some(convo) = app.conversations.get_mut(&chat_id) else {
        return Vec::new();
    };
    // THE decision of this module, and the one most likely to be "simplified"
    // away: a chat is marked read only while the user is looking at the
    // NEWEST messages, never merely because it is open. Do not relax this to
    // "the chat is open" — it reads plausible, every test about opening a
    // chat still passes (a chat opens pinned to the bottom), and it is
    // wrong: a window scrolled back into history has everything unread
    // *below* the fold, and reporting those read is not a local mistake that
    // the next frame corrects. It clears the badge on the user's phone and
    // shows the other side a read receipt for a message nobody looked at.
    // See this function's "Only while pinned to the bottom" for why
    // `Scroll::Bottom` is the honest proxy, and
    // `a_scrolled_back_window_marks_nothing_read` (plus the two arrival tests
    // beside it) for what holds it in place.
    if !matches!(convo.scroll, Scroll::Bottom) {
        return Vec::new();
    }

    // Newest first, so the bound keeps the ids that matter: outgoing messages
    // are never unread (they are the user's own), and anything at or below
    // the watermark TDLib already counts as read.
    let mut message_ids: Vec<MessageId> = convo
        .messages
        .iter()
        .rev()
        .filter(|m| !m.is_outgoing && m.id > convo.last_read_inbox)
        .take(MAX_VIEW_MESSAGES_PER_REQUEST)
        .map(|m| m.id)
        .collect();
    let Some(&newest) = message_ids.first() else {
        return Vec::new();
    };

    if let Some(pending) = convo.pending_view
        && pending.up_to >= newest
    {
        let elapsed = now.0.saturating_sub(pending.sent_at.0);
        if elapsed < VIEW_REQUEST_RETRY_AFTER_MS || pending.attempts >= MAX_VIEW_ATTEMPTS {
            return Vec::new();
        }
        convo.pending_view = Some(PendingView {
            up_to: newest,
            sent_at: now,
            attempts: pending.attempts + 1,
        });
    } else {
        convo.pending_view = Some(PendingView {
            up_to: newest,
            sent_at: now,
            attempts: 1,
        });
    }

    message_ids.reverse(); // ascending, like the window itself
    vec![Effect::Td(TdRequest::ViewMessages {
        chat_id,
        message_ids,
    })]
}

/// The retry half of [`mark_visible_read`]'s storm control (module docs).
/// Every other trigger is an event; this is what makes a `ViewMessages` that
/// TDLib dropped recoverable for a user who is sitting still with the chat
/// open, and it is why nothing else has to be wired up as a trigger to
/// guarantee eventual consistency. Costs nothing when there is nothing to
/// mark: with no unread messages below the watermark it returns before
/// touching any state, and while a request is outstanding the pending
/// watermark short-circuits it.
pub fn handle_tick(app: &mut AppState) -> Vec<Effect> {
    let Some(chat_id) = app.open_chat else {
        return Vec::new();
    };
    mark_visible_read(app, chat_id)
}

pub fn handle_td(app: &mut AppState, upd: &TdUpdate) -> Vec<Effect> {
    match upd {
        TdUpdate::NewMessage(msg) => {
            append_new_message(app, msg);
            // T66: the arrival changes what's visible near the anchor
            // (`Scroll::Bottom` especially — a new message is by
            // definition the newest thing loaded).
            let mut effects = media::auto_download_photos(app, msg.chat_id);
            // T72: and it arrived unread, in a chat the user may be looking
            // at right now.
            effects.extend(mark_visible_read(app, msg.chat_id));
            return effects;
        }
        TdUpdate::MessagesDeleted {
            chat_id,
            message_ids,
        } => {
            remove_deleted_messages(app, *chat_id, message_ids);
        }
        TdUpdate::MessageContentChanged {
            chat_id,
            message_id,
            content,
        } => {
            if let Some(convo) = app.conversations.get_mut(chat_id)
                && let Some(m) = convo.messages.iter_mut().find(|m| m.id == *message_id)
            {
                m.content = content.clone();
            }
        }
        TdUpdate::MessageSendSucceeded {
            chat_id,
            old_message_id,
            message,
        } => {
            if let Some(convo) = app.conversations.get_mut(chat_id)
                && let Some(idx) = convo.messages.iter().position(|m| m.id == *old_message_id)
            {
                convo.messages.remove(idx);
                let insert_idx = convo.messages.partition_point(|m| m.id < message.id);
                convo.messages.insert(insert_idx, message.clone());
            }
        }
        TdUpdate::MessageSendFailed {
            chat_id,
            old_message_id,
            error,
        } => {
            if let Some(convo) = app.conversations.get_mut(chat_id)
                && let Some(m) = convo.messages.iter_mut().find(|m| m.id == *old_message_id)
            {
                m.send_state = crate::model::message::SendState::Failed(error.clone());
            }
        }
        TdUpdate::ChatReadInbox {
            chat_id,
            last_read_inbox_message_id,
            ..
        } => {
            if let Some(convo) = app.conversations.get_mut(chat_id) {
                convo.last_read_inbox = *last_read_inbox_message_id;
                // T72: this is the answer `ViewMessages` never gets as a
                // completion — TDLib has moved the watermark past what was
                // asked for, so the outstanding request is done. Retiring it
                // is bookkeeping, not the thing that clears the badge: that
                // is `chat_list`'s arm of this same update writing TDLib's
                // own `unread_count`.
                if convo
                    .pending_view
                    .is_some_and(|p| p.up_to <= *last_read_inbox_message_id)
                {
                    convo.pending_view = None;
                }
            }
        }
        TdUpdate::ChatReadOutbox {
            chat_id,
            last_read_outbox_message_id,
        } => {
            if let Some(convo) = app.conversations.get_mut(chat_id) {
                convo.last_read_outbox = *last_read_outbox_message_id;
            }
        }
        // T34 addition: replaces a message's reactions wholesale in place,
        // same "TDLib is the source of truth" shape as every other arm here.
        TdUpdate::MessageInteractionInfo {
            chat_id,
            message_id,
            reactions,
        } => {
            if let Some(convo) = app.conversations.get_mut(chat_id)
                && let Some(m) = convo.messages.iter_mut().find(|m| m.id == *message_id)
            {
                m.reactions = reactions.clone();
            }
        }
        _ => {}
    }
    Vec::new()
}

/// A `NewMessage` only lands in a window that is already being tracked (a
/// chat that has been opened at least once via [`open`]) — not necessarily
/// the chat currently on screen, matching how the chat list keeps unopened
/// chats' state untouched while background chats a user has visited keep
/// updating. `Scroll::Bottom` needs no adjustment to "stay pinned" (it is not
/// tied to a message id); `Scroll::At` is left alone so the anchor never
/// jumps for an arrival at the opposite end of the window.
fn append_new_message(app: &mut AppState, msg: &MessageView) {
    let Some(convo) = app.conversations.get_mut(&msg.chat_id) else {
        return;
    };
    if convo.messages.iter().any(|m| m.id == msg.id) {
        return; // already present (e.g. duplicate delivery) — no-op.
    }
    match convo.messages.back() {
        Some(last) if msg.id > last.id => convo.messages.push_back(msg.clone()),
        None => convo.messages.push_back(msg.clone()),
        Some(_) => {
            // Out-of-order arrival: insert in sorted position rather than
            // assume it is always the newest.
            let idx = convo.messages.partition_point(|m| m.id < msg.id);
            convo.messages.insert(idx, msg.clone());
        }
    }
    evict_excess(&mut convo.messages, &convo.scroll);
    drop_selection_if_gone(convo);
}

/// Removes deleted ids from the window. If the scroll anchor itself was
/// deleted, re-anchors to the nearest surviving message. Direction choice:
/// prefer the nearest *newer* survivor first — it is the message that
/// visually slides into the deleted row's former position — falling back to
/// the nearest *older* survivor, and finally to `Scroll::Bottom` if the
/// window is now empty.
fn remove_deleted_messages(app: &mut AppState, chat_id: ChatId, ids: &[MessageId]) {
    let Some(convo) = app.conversations.get_mut(&chat_id) else {
        return;
    };
    let deleted: BTreeSet<MessageId> = ids.iter().copied().collect();
    if deleted.is_empty() {
        return;
    }
    convo.messages.retain(|m| !deleted.contains(&m.id));
    drop_selection_if_gone(convo);

    let anchor = match convo.scroll {
        Scroll::Bottom => None,
        Scroll::At { message_id, .. } => Some((message_id, false)),
        Scroll::AtTop { message_id } => Some((message_id, true)),
    };
    if let Some((message_id, top)) = anchor
        && deleted.contains(&message_id)
    {
        convo.scroll = reanchor_after_deletion(&convo.messages, message_id, top);
    }
}

fn reanchor_after_deletion(
    messages: &VecDeque<MessageView>,
    deleted_id: MessageId,
    top: bool,
) -> Scroll {
    if let Some(newer) = messages.iter().find(|m| m.id > deleted_id) {
        return anchored(newer.id, top);
    }
    if let Some(older) = messages.iter().rev().find(|m| m.id < deleted_id) {
        return anchored(older.id, top);
    }
    Scroll::Bottom
}

/// Routes a `GetChatHistory` completion through the T17 paging machine, then
/// prepends whatever came back and enforces the window bound. See the module
/// doc comment for the eviction rule.
///
/// ## T59 additions: local-first history and the remote reconcile
///
/// `only_local` here is the flag of the *request that just completed* (not a
/// property of `convo.paging`, which the machine already moved on from by
/// the time this reads it) — that is what both additions below key off:
///
/// - **The opening request has no `oldest_loaded` to retry from.** The
///   paging machine's empty-response trap (spec §5.2) needs a message id to
///   re-request from; `chat_list`/`palette` mark the opening request
///   `Loading` the same way scroll-triggered paging does (see their
///   `open_chat_requests_local_first` tests), so an empty completion drives
///   `on_history_loaded` into `Loading` wanting a remote retry — but with
///   nothing loaded yet, `oldest_loaded` is `None` and the machine (correctly
///   generic — it doesn't know about TDLib's id-0 sentinel) has nothing to
///   build a `Request` from. Detected here as "ended in `Loading` with no
///   directive and nothing loaded": retry from `MessageId(0)`, the same
///   "newest message" sentinel the opening request itself used. This applies
///   uniformly to every empty attempt while the window is still empty (local
///   or remote), not just the first.
/// - **A non-empty completion of a request that was itself `only_local:
///   true` only proves what TDLib's on-disk cache already had.** Messages
///   that arrived on another device (or on this one, from the server) while
///   the app was closed are not in that cache yet. Exactly one follow-up
///   `GetChatHistory { from_message_id: MessageId(0), only_local: false }`
///   reconciles them; [`prepend_messages`]'s dedupe-by-id absorbs whatever
///   overlaps. Keyed strictly off this call's `only_local` parameter — never
///   off `convo.paging`, which a scroll-up page racing the reconcile could
///   otherwise put back into `Loading` — so the reconcile's own completion
///   (always `only_local: false`) can never spawn another one: that is the
///   loop guard.
///
/// ## T67: filling the viewport
///
/// Whatever the page did, the window it landed in may still be too short to
/// fill the pane; [`fill_viewport`] decides whether to ask for more. See its
/// doc comment for why that is a caller-side policy rather than a paging
/// state, how it terminates, and why it cannot ping-pong with the reconcile
/// above.
pub fn apply_history_page(
    app: &mut AppState,
    chat_id: ChatId,
    only_local: bool,
    outcome: &Result<Vec<MessageView>, TdError>,
) -> Vec<Effect> {
    let Some(convo) = app.conversations.get_mut(&chat_id) else {
        return Vec::new();
    };

    let mut effects = match outcome {
        Ok(msgs) => {
            let oldest_loaded = convo.messages.front().map(|m| m.id);
            let received = msgs.len();
            let directive =
                history::on_history_loaded(&mut convo.paging, received, only_local, oldest_loaded);

            let added = prepend_messages(&mut convo.messages, msgs);
            evict_excess(&mut convo.messages, &convo.scroll);
            drop_selection_if_gone(convo);

            let mut effects = Vec::new();

            match directive {
                PagingDirective::Request {
                    from_message_id,
                    only_local,
                } => effects.push(Effect::Td(TdRequest::GetChatHistory {
                    chat_id,
                    from_message_id,
                    limit: history::PAGE_SIZE,
                    only_local,
                })),
                PagingDirective::None
                    if oldest_loaded.is_none()
                        && matches!(convo.paging, PagingState::Loading { .. }) =>
                {
                    // See the doc comment: the empty-response retry has
                    // nothing loaded to anchor on yet, so retry from
                    // TDLib's "newest message" sentinel instead.
                    effects.push(Effect::Td(TdRequest::GetChatHistory {
                        chat_id,
                        from_message_id: MessageId(0),
                        limit: history::PAGE_SIZE,
                        only_local: false,
                    }));
                }
                PagingDirective::None => {}
            }

            if only_local && received > 0 {
                effects.push(Effect::Td(TdRequest::GetChatHistory {
                    chat_id,
                    from_message_id: MessageId(0),
                    limit: history::PAGE_SIZE,
                    only_local: false,
                }));
            }

            // T9: advance an in-flight jump-to-quote hunt now that this page
            // has landed. Runs before `fill_viewport` deliberately: if the
            // hunt continues, its own `anchor_to` call may put `paging` back
            // into `Loading`, which is exactly what stops `fill_viewport`
            // from also asking for a page in the same call. Needs `app`
            // wholesale (the give-up toast does), so it runs after the
            // `convo` borrow above has gone out of use; `fill_viewport`
            // re-borrows fresh rather than reusing that binding.
            effects.extend(advance_hunt(app, chat_id));
            if let Some(convo) = app.conversations.get(&chat_id) {
                effects.extend(fill_viewport(convo, chat_id, added));
            }

            effects
        }
        Err(e) => {
            let retry_after = match e {
                TdError::FloodWait { seconds } => Some(*seconds),
                _ => None,
            };
            history::on_history_error(&mut convo.paging, retry_after, app.now);
            // T9: a hunt that silently waits out a cooldown looks stalled,
            // not stopped — end it and say so rather than leaving the user
            // wondering whether `j` did anything.
            if convo.hunt.take().is_some() {
                toasts::on_action_failed(
                    app,
                    chat_id,
                    "could not reach the quoted message".to_string(),
                )
            } else {
                Vec::new()
            }
        }
    };

    // T66: a prepended page (or even a failed/empty attempt, harmlessly —
    // storm control makes a redundant call a no-op) is exactly the kind of
    // change to the visible window auto-download exists to react to.
    effects.extend(media::auto_download_photos(app, chat_id));
    // T72: the unread messages arrive *with* this page — on a first open
    // there is nothing to mark read until it lands.
    effects.extend(mark_visible_read(app, chat_id));
    effects
}

/// Merges `new_msgs` into the front of `existing`, deduped by id (against
/// both the existing window and duplicates within `new_msgs` itself) and
/// kept in ascending order regardless of the order TDLib/the mapping layer
/// delivered them in. Returns how many messages were genuinely new, which is
/// what [`fill_viewport`] measures progress by — `new_msgs.len()` would count
/// a page of pure overlap as progress and is not the same number.
fn prepend_messages(existing: &mut VecDeque<MessageView>, new_msgs: &[MessageView]) -> usize {
    if new_msgs.is_empty() {
        return 0;
    }
    let mut seen: BTreeSet<MessageId> = existing.iter().map(|m| m.id).collect();
    let mut to_prepend: Vec<MessageView> = Vec::new();
    for m in new_msgs {
        if seen.insert(m.id) {
            to_prepend.push(m.clone());
        }
    }
    let added = to_prepend.len();
    to_prepend.sort_by_key(|m| m.id);
    for m in to_prepend.into_iter().rev() {
        existing.push_front(m);
    }
    added
}

/// T67: keeps asking for older history while the window is too short to fill
/// the pane. `added` is what the page that just landed contributed *after*
/// dedupe (see [`prepend_messages`]).
///
/// ## The bug this exists for
///
/// `getChatHistory` answers from TDLib's local database before it goes to the
/// server. For a chat this client has never opened, that database holds
/// exactly one message — the chat-list preview delivered by
/// `updateChatLastMessage`. The opening request (`only_local: true`, T59)
/// therefore comes back with a single message, which is a *short but
/// non-empty* page: the milder sibling of spec §5.2's empty-response trap.
/// The paging machine is right to call that progress and return to `Idle`
/// (`history::short_but_nonempty_response_is_not_exhausted` pins it — one
/// message is never proof of end-of-history), and it is right that nothing in
/// the machine asks again on its own. But the user is left looking at a pane
/// holding one message until they scroll, which is the reported bug. Asking
/// for the rest is a policy decision about what an *opened chat* should show,
/// so it lives here, in the caller, and adds no state to the machine.
///
/// ## Why this does not drive the paging machine
///
/// The request goes out without moving `convo.paging` out of `Idle` — the
/// same shape as T59's reconcile above, and deliberately not a
/// `Loading` transition. Driving the machine would hand an empty answer to
/// `history::on_history_loaded`, which correctly (spec §5.2) re-asks up to
/// `MAX_EMPTY_ATTEMPTS` times and then latches `Exhausted`. Spending that
/// ladder at *open* time, on a chat whose server sync merely hasn't caught up
/// yet, would leave `Exhausted` latched for the rest of the session and kill
/// scroll-up for that chat entirely — a worse bug than the one being fixed.
/// A fill that simply stops instead costs at most one unanswered round trip,
/// and the user's next scroll still goes through the machine and its retry
/// ladder in the normal way.
///
/// Its completion lands while `paging` is `Idle`, which
/// `history::on_history_loaded`'s stale-completion branch ignores outright
/// (state untouched, no directive) — while `apply_history_page` still
/// prepends the messages, since prepending is unconditional. That is exactly
/// the behaviour wanted: the page is kept, the machine is undisturbed, and
/// this function gets to decide whether to ask again.
///
/// ## Termination
///
/// Every fill round either ends the chain or grows the window by at least one
/// message:
///
/// - `added == 0` stops it. A server that answers every request with the same
///   message (or with pure overlap of what is already loaded) contributes
///   nothing new, so it cannot spin: the second identical answer ends the
///   chain. This is the rule that makes the fill safe against a
///   badly-behaved or stuck peer.
/// - `added > 0` means the window grew by `added`, and the window only ever
///   grows here: `evict_excess` cannot fire below `WINDOW_MAX_MESSAGES`
///   (500), and this function only runs at all below
///   `VIEWPORT_FILL_TARGET_MESSAGES` (50). So the length is strictly
///   increasing toward a fixed target it is tested against, and at most
///   `VIEWPORT_FILL_TARGET_MESSAGES` rounds can happen before
///   `messages.len() >= VIEWPORT_FILL_TARGET_MESSAGES` ends the chain.
///
/// A genuinely short chat therefore ends after one unanswerable round: TDLib
/// returns nothing older, `added` is 0, and the chain stops without ever
/// claiming the history is exhausted. No round counter is needed as a second
/// belt — the target itself is the bound, and every round it permits has paid
/// for itself with at least one message the user can now see.
///
/// ## Why this cannot ping-pong with T59's reconcile
///
/// The two requests ask opposite ends of the history and neither one's
/// completion can re-trigger the other:
///
/// - The reconcile asks from `MessageId(0)` (TDLib's *newest message*
///   sentinel) and is spawned only by a completion whose request was
///   `only_local: true`. Every fill request is `only_local: false`, so a
///   fill's completion never spawns a reconcile.
/// - The fill asks from the *oldest* loaded message and is spawned only while
///   the window is under target. The reconcile's completion can spawn a fill
///   — that is intended, it is another page landing in a short window — but
///   only if it brought something new, and then the fill asks in the older
///   direction, which is not where the reconcile was looking.
///
/// A cold open does put both in flight at once (the local page is short *and*
/// `only_local`). They are independent requests for different pages;
/// `prepend_messages` dedupes whatever overlaps, and because neither touches
/// `paging`, neither can consume or confuse the other's completion.
fn fill_viewport(convo: &ConversationState, chat_id: ChatId, added: usize) -> Option<Effect> {
    if added == 0 || convo.messages.len() >= VIEWPORT_FILL_TARGET_MESSAGES {
        return None;
    }
    // `Loading`: a request is already out and its page will run this check
    // again. `Cooldown`: TDLib asked us to back off, and a fill is the least
    // urgent reason to ignore that. `Exhausted`: there is genuinely no more
    // history to fetch, short window or not.
    if !matches!(convo.paging, PagingState::Idle) {
        return None;
    }
    let oldest = convo.messages.front()?.id;
    Some(Effect::Td(TdRequest::GetChatHistory {
        chat_id,
        from_message_id: oldest,
        limit: history::PAGE_SIZE,
        only_local: false,
    }))
}

/// Selection plumbing (T26): a selection that no longer names a message in
/// the window is dropped rather than left dangling. Called after every
/// mutation that can remove a message — deletion and both eviction paths —
/// so `selection.message_id` is an invariant, not a hope.
pub(crate) fn drop_selection_if_gone(convo: &mut ConversationState) {
    let gone = convo
        .selection
        .as_ref()
        .is_some_and(|sel| index_of(&convo.messages, sel.message_id).is_none());
    if gone {
        convo.selection = None;
    }
}

/// Selection plumbing (T26): points the scroll anchor at `message_id` so the
/// viewport follows the selection cursor, and pages older history in when the
/// selection walks near the top of the window (same trigger the scroll keys
/// use). Selecting the newest loaded message re-pins to [`Scroll::Bottom`],
/// which is what "selection starts at the newest message" must mean for a
/// live chat: new arrivals keep the view at the bottom.
pub(crate) fn anchor_to(
    convo: &mut ConversationState,
    chat_id: ChatId,
    message_id: MessageId,
    now: Millis,
) -> Vec<Effect> {
    anchor_to_flavoured(convo, chat_id, message_id, false, now)
}

/// Anchors so `message_id` is the FIRST visible row rather than the last —
/// what a deliberate jump to a quoted message wants (architecture §7.5.4).
/// [`anchor_to`] stays the default for everything else, selection stepping
/// and search hits included.
///
/// Re-pins to [`Scroll::Bottom`] when the target is the newest loaded
/// message, where "at the top" has no meaning: there is nothing newer to
/// fill the pane below it.
pub(crate) fn anchor_to_top(
    convo: &mut ConversationState,
    chat_id: ChatId,
    message_id: MessageId,
    now: Millis,
) -> Vec<Effect> {
    anchor_to_flavoured(convo, chat_id, message_id, true, now)
}

/// The one body behind [`anchor_to`] and [`anchor_to_top`], so the two
/// cannot drift on either of the things they must agree about: the re-pin to
/// [`Scroll::Bottom`] at the newest loaded message, and the near-top paging
/// trigger.
///
/// That trigger is why neither wrapper pushes a `GetChatHistory` of its own
/// — `trigger_paging_if_near_top` already issues one whenever the new anchor
/// needs it, and a second explicit push duplicates the request (see
/// [`advance_hunt`]'s doc comment, and the regression test pinning it to
/// exactly one).
fn anchor_to_flavoured(
    convo: &mut ConversationState,
    chat_id: ChatId,
    message_id: MessageId,
    top: bool,
    now: Millis,
) -> Vec<Effect> {
    let is_newest = convo.messages.back().is_some_and(|m| m.id == message_id);
    convo.scroll = if is_newest {
        Scroll::Bottom
    } else {
        anchored(message_id, top)
    };
    trigger_paging_if_near_top(convo, chat_id, now)
}

/// Moves the scroll anchor by one message in `delta`'s direction, clamped to
/// the loaded window, and re-triggers paging like any other anchor move.
///
/// This is the minimum-scroll counterpart to [`anchor_to`]: selection
/// movement uses it when the cursor walks off an edge, so the viewport
/// follows by one message instead of jumping to wherever the cursor went.
pub(crate) fn step_anchor(
    convo: &mut ConversationState,
    chat_id: ChatId,
    delta: isize,
    now: Millis,
) -> Vec<Effect> {
    if convo.messages.is_empty() {
        return Vec::new();
    }
    let last = convo.messages.len() - 1;
    // The flavour rides along: stepping off a top-anchored jump must land
    // top-anchored, or the message the user jumped to would drop from the
    // first row to the last on this very keypress ([`Scroll::AtTop`]).
    let (current, top) = match convo.scroll {
        Scroll::Bottom => (last, false),
        Scroll::At { message_id, .. } => {
            (index_of(&convo.messages, message_id).unwrap_or(last), false)
        }
        Scroll::AtTop { message_id } => {
            (index_of(&convo.messages, message_id).unwrap_or(last), true)
        }
    };
    let target = (current as isize + delta).clamp(0, last as isize) as usize;
    let id = convo.messages[target].id;
    anchor_to_flavoured(convo, chat_id, id, top, now)
}

/// Whether `id` names a message older than every message loaded, which is
/// the one way an anchor can point outside the window and still be reachable
/// by paging (older history is the only direction v1 ever fetches).
fn is_older_than_window(messages: &VecDeque<MessageView>, id: MessageId) -> bool {
    messages.front().is_some_and(|oldest| id < oldest.id)
}

/// Binary search for `id` in the ascending-by-id window.
pub(crate) fn index_of(messages: &VecDeque<MessageView>, id: MessageId) -> Option<usize> {
    let idx = messages.partition_point(|m| m.id < id);
    match messages.get(idx) {
        Some(m) if m.id == id => Some(idx),
        _ => None,
    }
}

/// See the module doc comment for the eviction rule this implements.
fn evict_excess(messages: &mut VecDeque<MessageView>, scroll: &Scroll) {
    while messages.len() > WINDOW_MAX_MESSAGES {
        let evict_back = match scroll {
            Scroll::Bottom => false,
            // Which edge the anchor pins is irrelevant here: both name the
            // same message, and eviction only asks how far that message sits
            // from each end of the window.
            Scroll::At { message_id, .. } | Scroll::AtTop { message_id } => {
                match index_of(messages, *message_id) {
                    Some(idx) => {
                        let dist_front = idx;
                        let dist_back = messages.len() - 1 - idx;
                        dist_front <= dist_back
                    }
                    None => false,
                }
            }
        };
        if evict_back {
            messages.pop_back();
        } else {
            messages.pop_front();
        }
    }
}

/// Minimal M3 key routing: claims `Up`/`Down`/`PageUp`/`PageDown` whenever a
/// chat is open and the focused pane is not the chat list. There is no pane
/// system yet (T28 builds the real modal → pane → global routing table over
/// `Focus`); this is the narrowest rule that lets the conversation pane
/// scroll today without guessing at panes that don't exist yet — anything
/// other than `Focus::ChatList` is treated as "the conversation may be
/// visible", including the base focus before any pane pushes onto the stack.
pub fn handle_key(app: &mut AppState, key: Key) -> Option<Vec<Effect>> {
    let chat_id = app.open_chat?;
    if matches!(app.focus.current(), Focus::ChatList) {
        return None;
    }
    let delta: isize = match key {
        Key::Up => -1,
        Key::Down => 1,
        Key::PageUp => -(PAGE_STEP_MESSAGES as isize),
        Key::PageDown => PAGE_STEP_MESSAGES as isize,
        _ => return None,
    };
    let now = app.now;
    let mut effects = {
        let convo = app.conversations.get_mut(&chat_id)?;
        // T9: a manual scroll is the user taking over navigation — an
        // in-flight jump-to-quote hunt no longer speaks for where the view
        // should go.
        cancel_hunt(convo);
        move_anchor(convo, chat_id, delta, now)
    };
    // T66: every anchor step changes what's near it.
    effects.extend(media::auto_download_photos(app, chat_id));
    // T72: scrolling back down to the newest message is the user saying they
    // are looking at it — the one anchor move `mark_visible_read`'s
    // `Scroll::Bottom` gate cares about (every other one bails immediately).
    effects.extend(mark_visible_read(app, chat_id));
    Some(effects)
}

/// Moves the scroll anchor by `delta` messages (negative = toward older,
/// positive = toward newer). `line_offset` is always reset to `0` (see the
/// `PAGE_STEP_MESSAGES` doc comment on why row-level offsets don't exist
/// yet). Firing from `Scroll::Bottom` treats "one past the newest loaded
/// index" as the starting point, so `Up`/`PageUp` uniformly land inside the
/// window; `Down`/`PageDown` from `Bottom` are a no-op (already pinned).
/// Overshooting past the newest loaded message from `Scroll::At` re-pins to
/// `Scroll::Bottom`; overshooting past the oldest simply clamps there.
fn move_anchor(
    convo: &mut ConversationState,
    chat_id: ChatId,
    delta: isize,
    now: Millis,
) -> Vec<Effect> {
    if convo.messages.is_empty() {
        return Vec::new();
    }
    let last_idx = (convo.messages.len() - 1) as isize;

    // As in [`step_anchor`], the flavour rides along: a scroll key pressed
    // after a jump keeps the target top-anchored ([`Scroll::AtTop`]).
    let (current_idx, top) = match convo.scroll {
        Scroll::Bottom => {
            if delta >= 0 {
                return Vec::new();
            }
            (last_idx + 1, false)
        }
        Scroll::At { message_id, .. } | Scroll::AtTop { message_id } => {
            let top = matches!(convo.scroll, Scroll::AtTop { .. });
            match index_of(&convo.messages, message_id) {
                Some(idx) => (idx as isize, top),
                // Anchor older than everything loaded: a deliberate jump past
                // the top of the window, waiting for the page that contains it
                // (see [`trigger_paging_if_near_top`]). Moving it by `delta`
                // is meaningless — there is nothing loaded around it to move
                // through — so it stays put and the scroll spends itself on
                // asking for that page instead.
                None if is_older_than_window(&convo.messages, message_id) => {
                    return trigger_paging_if_near_top(convo, chat_id, now);
                }
                None => {
                    // Evicted at the newest end, or otherwise gone: the safest
                    // recovery is to re-pin to the newest known state.
                    convo.scroll = Scroll::Bottom;
                    return Vec::new();
                }
            }
        }
    };

    let target_idx = current_idx + delta;
    if target_idx > last_idx {
        convo.scroll = Scroll::Bottom;
        return Vec::new();
    }
    let clamped_idx = target_idx.clamp(0, last_idx) as usize;
    let new_id = convo.messages[clamped_idx].id;
    convo.scroll = anchored(new_id, top);
    trigger_paging_if_near_top(convo, chat_id, now)
}

/// Anchor is "near the top" when fewer than `PAGE_TRIGGER_MESSAGES` older
/// messages remain loaded before it (index `< PAGE_TRIGGER_MESSAGES`,
/// counting from the oldest loaded message at index 0) — or when it names a
/// message older than the whole window, which is *past* the top rather than
/// near it.
///
/// That second case is not hypothetical: `state::search`'s `n` steps the
/// anchor onto a search hit that TDLib found anywhere in the chat's history,
/// which is routinely older than the page or two currently loaded. Bailing
/// out on an anchor that isn't in the window would mean the one anchor move
/// that most needs history fetched is the one that never asks for it.
pub(crate) fn trigger_paging_if_near_top(
    convo: &mut ConversationState,
    chat_id: ChatId,
    now: Millis,
) -> Vec<Effect> {
    // Both anchored flavours name a message and can therefore sit near (or
    // past) the top of the window; only `Scroll::Bottom` cannot.
    let (Scroll::At { message_id, .. } | Scroll::AtTop { message_id }) = convo.scroll else {
        return Vec::new();
    };
    let near_top = match index_of(&convo.messages, message_id) {
        Some(idx) => idx < history::PAGE_TRIGGER_MESSAGES,
        None => is_older_than_window(&convo.messages, message_id),
    };
    if !near_top {
        return Vec::new();
    }
    let oldest_loaded = convo.messages.front().map(|m| m.id);
    match history::on_scroll_near_top(&mut convo.paging, oldest_loaded, now) {
        PagingDirective::Request {
            from_message_id,
            only_local,
        } => vec![Effect::Td(TdRequest::GetChatHistory {
            chat_id,
            from_message_id,
            limit: history::PAGE_SIZE,
            only_local,
        })],
        PagingDirective::None => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::Screen;
    use crate::effect::TelemetryMode;
    use crate::model::entity::FormattedText;
    use crate::model::ids::UserId;
    use crate::model::message::{MessageCaps, MessageContent, ReplyPreview, SendState, Sender};
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

    const CHAT: ChatId = ChatId(1);

    fn msg(id: i64) -> MessageView {
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
            crash_reports_available: false,
            telemetry_salt: [0u8; 32],
            now: Millis(0),
            visible_messages: None,
        }
    }

    /// Drops the `ViewMessages` requests [`mark_visible_read`] adds to every
    /// window change, so the paging and scrolling tests below keep asserting
    /// on the effect they are actually about. Read marking has its own
    /// tests — see the "T72" section.
    fn without_view_messages(effects: Vec<Effect>) -> Vec<Effect> {
        effects
            .into_iter()
            .filter(|e| !matches!(e, Effect::Td(TdRequest::ViewMessages { .. })))
            .collect()
    }

    /// The `ViewMessages` requests in `effects`, as `(chat, ids)` pairs.
    fn view_requests(effects: &[Effect]) -> Vec<(ChatId, Vec<MessageId>)> {
        effects
            .iter()
            .filter_map(|e| match e {
                Effect::Td(TdRequest::ViewMessages {
                    chat_id,
                    message_ids,
                }) => Some((*chat_id, message_ids.clone())),
                _ => None,
            })
            .collect()
    }

    /// Opens the chat and focuses somewhere other than the chat list, so
    /// `handle_key` is willing to claim keys in tests that need it.
    fn fixture_open(app: &mut AppState) {
        open(app, CHAT);
        app.focus = FocusStack::new(Focus::Composer);
    }

    #[test]
    fn open_is_idempotent_and_preserves_existing_window() {
        let mut app = fixture_state();
        open(&mut app, CHAT);
        app.conversations
            .get_mut(&CHAT)
            .unwrap()
            .messages
            .push_back(msg(1));
        app.conversations.get_mut(&CHAT).unwrap().scroll = Scroll::At {
            message_id: MessageId(1),
            line_offset: 3,
        };

        open(&mut app, CHAT);
        let convo = &app.conversations[&CHAT];
        assert_eq!(convo.messages.len(), 1);
        assert_eq!(
            convo.scroll,
            Scroll::At {
                message_id: MessageId(1),
                line_offset: 3
            }
        );
        assert_eq!(app.open_chat, Some(CHAT));
    }

    #[test]
    fn close_previous_chat_targets_the_outgoing_chat_only_when_it_differs() {
        let mut app = fixture_state();
        const OTHER: ChatId = ChatId(2);

        // Nothing open yet: no chat to close.
        assert!(close_previous_chat(&mut app, CHAT).is_empty());

        open(&mut app, CHAT);
        // Same chat again: no close/open churn against itself.
        assert!(close_previous_chat(&mut app, CHAT).is_empty());

        // A different chat: close the one being left.
        let effects = close_previous_chat(&mut app, OTHER);
        assert_eq!(effects.len(), 1);
        assert!(matches!(
            effects[0],
            Effect::Td(TdRequest::CloseChat { chat_id: CHAT })
        ));
    }

    #[test]
    fn close_if_now_hidden_fires_only_on_a_visible_to_hidden_transition() {
        let mut app = fixture_state();
        // No chat open at all: nothing to close, whatever the snapshot says.
        assert!(close_if_now_hidden(&app, true).is_empty());

        open(&mut app, CHAT);
        // Single-pane, focus on the list: not visible right now.
        app.width = 80;
        app.layout_breakpoint_cols = 100;
        app.focus = FocusStack::new(Focus::ChatList);

        // Was visible before, hidden now: the transition this exists for.
        let effects = close_if_now_hidden(&app, true);
        assert_eq!(effects.len(), 1);
        assert!(matches!(
            effects[0],
            Effect::Td(TdRequest::CloseChat { chat_id: CHAT })
        ));

        // Was already hidden: no transition, nothing to close.
        assert!(close_if_now_hidden(&app, false).is_empty());

        // Visible both before and after (focus back on the composer): also
        // no transition.
        app.focus = FocusStack::new(Focus::Composer);
        assert!(close_if_now_hidden(&app, true).is_empty());
    }

    fn msg_with_spoiler(id: i64) -> MessageView {
        let mut m = msg(id);
        m.content = MessageContent::Text(FormattedText {
            text: "before secret after".to_string(),
            entities: vec![crate::model::entity::TextEntity {
                offset_utf16: 7,
                length_utf16: 6,
                kind: crate::model::entity::EntityKind::Spoiler,
            }],
        });
        m
    }

    #[test]
    fn has_unrevealed_spoiler_true_only_before_reveal() {
        let revealed = std::collections::BTreeSet::new();
        assert!(has_unrevealed_spoiler(&msg_with_spoiler(1), &revealed));
        // A message with no Spoiler entity at all: never true, however the
        // reveal set looks.
        assert!(!has_unrevealed_spoiler(&msg(2), &revealed));

        let mut revealed_1 = std::collections::BTreeSet::new();
        revealed_1.insert(MessageId(1));
        assert!(!has_unrevealed_spoiler(&msg_with_spoiler(1), &revealed_1));
    }

    #[test]
    fn reveal_spoilers_inserts_into_the_open_chats_set() {
        let mut app = fixture_state();
        open(&mut app, CHAT);
        assert!(
            !app.conversations[&CHAT]
                .revealed_spoilers
                .contains(&MessageId(1))
        );

        let effects = reveal_spoilers(&mut app, CHAT, MessageId(1));
        assert!(effects.is_empty());
        assert!(
            app.conversations[&CHAT]
                .revealed_spoilers
                .contains(&MessageId(1))
        );
    }

    #[test]
    fn jump_to_message_anchors_scroll_regardless_of_whether_its_loaded() {
        let mut app = fixture_state();
        open(&mut app, CHAT);
        app.conversations
            .get_mut(&CHAT)
            .unwrap()
            .messages
            .push_back(msg(5));

        // Loaded case. Top-anchored (architecture §7.5.4) — and note the
        // anchor is set even though msg 5 is the newest loaded message,
        // because this function deliberately does not go through
        // `anchor_to_top`'s re-pin: its contract is "set the anchor and
        // nothing else", for a target that is routinely not loaded at all.
        let effects = jump_to_message(&mut app, CHAT, MessageId(5));
        assert!(effects.is_empty());
        assert_eq!(
            app.conversations[&CHAT].scroll,
            Scroll::AtTop {
                message_id: MessageId(5),
            }
        );

        // Not loaded, and older than everything in the window (the only
        // relationship a quoted message can have to its window per the
        // architecture doc): still just sets the anchor, mirroring
        // `state::search`'s `step()` rather than paging directly.
        let effects = jump_to_message(&mut app, CHAT, MessageId(1));
        assert!(effects.is_empty());
        assert_eq!(
            app.conversations[&CHAT].scroll,
            Scroll::AtTop {
                message_id: MessageId(1),
            }
        );
    }

    // --- prepend / anchor stability -----------------------------------

    #[test]
    fn prepend_preserves_scroll_anchor() {
        let mut app = fixture_state();
        open(&mut app, CHAT);
        let convo = app.conversations.get_mut(&CHAT).unwrap();
        for id in 10..15 {
            convo.messages.push_back(msg(id));
        }
        convo.scroll = Scroll::At {
            message_id: MessageId(12),
            line_offset: 4,
        };
        convo.paging = PagingState::Loading {
            attempt: 1,
            only_local: false,
        };

        let page: Vec<MessageView> = (1..10).map(msg).collect();
        apply_history_page(&mut app, CHAT, false, &Ok(page));

        let convo = &app.conversations[&CHAT];
        assert_eq!(
            convo.scroll,
            Scroll::At {
                message_id: MessageId(12),
                line_offset: 4
            }
        );
        assert_eq!(convo.messages.len(), 14);
        let ids: Vec<i64> = convo.messages.iter().map(|m| m.id.0).collect();
        let mut sorted = ids.clone();
        sorted.sort_unstable();
        assert_eq!(ids, sorted, "window must stay ascending by id");
    }

    #[test]
    fn eviction_keeps_anchor_side() {
        let mut app = fixture_state();
        open(&mut app, CHAT);
        let convo = app.conversations.get_mut(&CHAT).unwrap();
        // Fill the window to the max, anchored near the front (as paging
        // near-top always leaves it).
        for id in 1..=(WINDOW_MAX_MESSAGES as i64) {
            convo.messages.push_back(msg(id));
        }
        convo.scroll = Scroll::At {
            message_id: MessageId(5),
            line_offset: 0,
        };
        convo.paging = PagingState::Loading {
            attempt: 1,
            only_local: false,
        };

        // Prepend 50 older messages; the window must evict 50 from the back
        // (newest) end, never touching the anchor or anything near it.
        let page: Vec<MessageView> = (-49..=0).map(msg).collect();
        apply_history_page(&mut app, CHAT, false, &Ok(page));

        let convo = &app.conversations[&CHAT];
        assert_eq!(convo.messages.len(), WINDOW_MAX_MESSAGES);
        assert_eq!(
            convo.scroll,
            Scroll::At {
                message_id: MessageId(5),
                line_offset: 0
            }
        );
        // The anchor and everything around it survived.
        assert!(convo.messages.iter().any(|m| m.id == MessageId(5)));
        assert!(convo.messages.iter().any(|m| m.id == MessageId(-49)));
        // 550 messages (500 existing + 50 prepended) over budget by 50: the
        // newest 50 pre-existing messages (451..=500) are the ones evicted;
        // 450 is the new newest survivor.
        assert!(!convo.messages.iter().any(|m| m.id == MessageId(500)));
        assert!(!convo.messages.iter().any(|m| m.id == MessageId(451)));
        assert!(convo.messages.iter().any(|m| m.id == MessageId(450)));
    }

    #[test]
    fn eviction_keeps_bottom_anchor_when_flooded_with_new_messages() {
        let mut app = fixture_state();
        open(&mut app, CHAT);
        let convo = app.conversations.get_mut(&CHAT).unwrap();
        for id in 1..=(WINDOW_MAX_MESSAGES as i64) {
            convo.messages.push_back(msg(id));
        }
        convo.scroll = Scroll::Bottom;

        handle_td(
            &mut app,
            &TdUpdate::NewMessage(msg(WINDOW_MAX_MESSAGES as i64 + 1)),
        );

        let convo = &app.conversations[&CHAT];
        assert_eq!(convo.messages.len(), WINDOW_MAX_MESSAGES);
        // Newest message survived; the oldest was evicted from the front.
        assert!(
            convo
                .messages
                .iter()
                .any(|m| m.id == MessageId(WINDOW_MAX_MESSAGES as i64 + 1))
        );
        assert!(!convo.messages.iter().any(|m| m.id == MessageId(1)));
    }

    // --- NewMessage ------------------------------------------------------

    #[test]
    fn new_message_at_bottom_stays_pinned_to_bottom() {
        let mut app = fixture_state();
        open(&mut app, CHAT);
        let convo = app.conversations.get_mut(&CHAT).unwrap();
        convo.messages.push_back(msg(1));
        convo.scroll = Scroll::Bottom;

        handle_td(&mut app, &TdUpdate::NewMessage(msg(2)));

        let convo = &app.conversations[&CHAT];
        assert_eq!(convo.scroll, Scroll::Bottom);
        assert_eq!(convo.messages.len(), 2);
        assert_eq!(convo.messages.back().unwrap().id, MessageId(2));
    }

    #[test]
    fn new_message_while_scrolled_up_does_not_jump() {
        let mut app = fixture_state();
        open(&mut app, CHAT);
        let convo = app.conversations.get_mut(&CHAT).unwrap();
        convo.messages.push_back(msg(1));
        convo.messages.push_back(msg(2));
        convo.scroll = Scroll::At {
            message_id: MessageId(1),
            line_offset: 0,
        };

        handle_td(&mut app, &TdUpdate::NewMessage(msg(3)));

        let convo = &app.conversations[&CHAT];
        assert_eq!(
            convo.scroll,
            Scroll::At {
                message_id: MessageId(1),
                line_offset: 0
            }
        );
        assert_eq!(convo.messages.len(), 3);
    }

    #[test]
    fn new_message_for_unopened_chat_is_ignored() {
        let mut app = fixture_state();
        // No `open` call: chat 99 has no ConversationState.
        let effects = handle_td(&mut app, &TdUpdate::NewMessage(msg(1)));
        assert!(effects.is_empty());
        assert!(app.conversations.is_empty());
    }

    #[test]
    fn duplicate_new_message_is_not_appended_twice() {
        let mut app = fixture_state();
        open(&mut app, CHAT);
        app.conversations
            .get_mut(&CHAT)
            .unwrap()
            .messages
            .push_back(msg(1));

        handle_td(&mut app, &TdUpdate::NewMessage(msg(1)));

        assert_eq!(app.conversations[&CHAT].messages.len(), 1);
    }

    // --- MessagesDeleted ---------------------------------------------------

    #[test]
    fn deleted_messages_removed_from_window() {
        let mut app = fixture_state();
        open(&mut app, CHAT);
        let convo = app.conversations.get_mut(&CHAT).unwrap();
        for id in 1..=5 {
            convo.messages.push_back(msg(id));
        }
        convo.scroll = Scroll::At {
            message_id: MessageId(3),
            line_offset: 0,
        };

        handle_td(
            &mut app,
            &TdUpdate::MessagesDeleted {
                chat_id: CHAT,
                message_ids: vec![MessageId(2), MessageId(4)],
            },
        );

        let convo = &app.conversations[&CHAT];
        let ids: Vec<i64> = convo.messages.iter().map(|m| m.id.0).collect();
        assert_eq!(ids, vec![1, 3, 5]);
        // Anchor (3) was not deleted, so it is untouched.
        assert_eq!(
            convo.scroll,
            Scroll::At {
                message_id: MessageId(3),
                line_offset: 0
            }
        );
    }

    #[test]
    fn deleting_the_anchor_reanchors_to_nearest_newer_survivor() {
        let mut app = fixture_state();
        open(&mut app, CHAT);
        let convo = app.conversations.get_mut(&CHAT).unwrap();
        for id in 1..=5 {
            convo.messages.push_back(msg(id));
        }
        convo.scroll = Scroll::At {
            message_id: MessageId(3),
            line_offset: 7,
        };

        handle_td(
            &mut app,
            &TdUpdate::MessagesDeleted {
                chat_id: CHAT,
                message_ids: vec![MessageId(3)],
            },
        );

        assert_eq!(
            app.conversations[&CHAT].scroll,
            Scroll::At {
                message_id: MessageId(4),
                line_offset: 0
            }
        );
    }

    #[test]
    fn deleting_the_anchor_falls_back_to_older_survivor_when_no_newer_one_exists() {
        let mut app = fixture_state();
        open(&mut app, CHAT);
        let convo = app.conversations.get_mut(&CHAT).unwrap();
        for id in 1..=3 {
            convo.messages.push_back(msg(id));
        }
        convo.scroll = Scroll::At {
            message_id: MessageId(3),
            line_offset: 0,
        };

        handle_td(
            &mut app,
            &TdUpdate::MessagesDeleted {
                chat_id: CHAT,
                message_ids: vec![MessageId(3)],
            },
        );

        assert_eq!(
            app.conversations[&CHAT].scroll,
            Scroll::At {
                message_id: MessageId(2),
                line_offset: 0
            }
        );
    }

    #[test]
    fn deleting_the_last_message_falls_back_to_bottom_when_window_empties() {
        let mut app = fixture_state();
        open(&mut app, CHAT);
        let convo = app.conversations.get_mut(&CHAT).unwrap();
        convo.messages.push_back(msg(1));
        convo.scroll = Scroll::At {
            message_id: MessageId(1),
            line_offset: 0,
        };

        handle_td(
            &mut app,
            &TdUpdate::MessagesDeleted {
                chat_id: CHAT,
                message_ids: vec![MessageId(1)],
            },
        );

        let convo = &app.conversations[&CHAT];
        assert!(convo.messages.is_empty());
        assert_eq!(convo.scroll, Scroll::Bottom);
    }

    // --- selection plumbing (T26) -------------------------------------------

    fn select(app: &mut AppState, id: MessageId) {
        app.conversations.get_mut(&CHAT).unwrap().selection = Some(SelectionState {
            message_id: id,
            chips: Vec::new(),
            chip_cursor: 0,
            chip_scroll: 0,
        });
    }

    #[test]
    fn deleting_the_selected_message_clears_selection() {
        let mut app = fixture_state();
        open(&mut app, CHAT);
        let convo = app.conversations.get_mut(&CHAT).unwrap();
        for id in 1..=3 {
            convo.messages.push_back(msg(id));
        }
        select(&mut app, MessageId(2));

        handle_td(
            &mut app,
            &TdUpdate::MessagesDeleted {
                chat_id: CHAT,
                message_ids: vec![MessageId(2)],
            },
        );

        assert!(app.conversations[&CHAT].selection.is_none());
    }

    #[test]
    fn deleting_an_unselected_message_keeps_the_selection() {
        let mut app = fixture_state();
        open(&mut app, CHAT);
        let convo = app.conversations.get_mut(&CHAT).unwrap();
        for id in 1..=3 {
            convo.messages.push_back(msg(id));
        }
        select(&mut app, MessageId(2));

        handle_td(
            &mut app,
            &TdUpdate::MessagesDeleted {
                chat_id: CHAT,
                message_ids: vec![MessageId(1)],
            },
        );

        assert_eq!(
            app.conversations[&CHAT]
                .selection
                .as_ref()
                .map(|s| s.message_id),
            Some(MessageId(2))
        );
    }

    #[test]
    fn evicting_the_selected_message_clears_selection() {
        let mut app = fixture_state();
        open(&mut app, CHAT);
        let convo = app.conversations.get_mut(&CHAT).unwrap();
        for id in 1..=(WINDOW_MAX_MESSAGES as i64) {
            convo.messages.push_back(msg(id));
        }
        convo.scroll = Scroll::Bottom;
        // Selected the oldest loaded message: the next arrival evicts it.
        select(&mut app, MessageId(1));

        handle_td(
            &mut app,
            &TdUpdate::NewMessage(msg(WINDOW_MAX_MESSAGES as i64 + 1)),
        );

        assert!(app.conversations[&CHAT].selection.is_none());
    }

    // --- MessageContentChanged / send succeeded / failed -------------------

    #[test]
    fn message_content_changed_replaces_content_in_place_and_keeps_reply() {
        let mut app = fixture_state();
        open(&mut app, CHAT);
        let mut m = msg(1);
        m.reply_to = Some(ReplyPreview {
            message_id: MessageId(0),
            sender_name: "Bob".to_string(),
            excerpt: "already filled in".to_string(),
        });
        app.conversations
            .get_mut(&CHAT)
            .unwrap()
            .messages
            .push_back(m);

        let new_content = MessageContent::Text(FormattedText {
            text: "edited".to_string(),
            entities: Vec::new(),
        });
        handle_td(
            &mut app,
            &TdUpdate::MessageContentChanged {
                chat_id: CHAT,
                message_id: MessageId(1),
                content: new_content.clone(),
            },
        );

        let updated = &app.conversations[&CHAT].messages[0];
        assert_eq!(updated.content, new_content);
        assert_eq!(
            updated.reply_to.as_ref().unwrap().excerpt,
            "already filled in"
        );
    }

    #[test]
    fn message_send_succeeded_swaps_temp_id_for_final_id() {
        let mut app = fixture_state();
        open(&mut app, CHAT);
        let mut pending = msg(-1);
        pending.send_state = SendState::Sending;
        app.conversations
            .get_mut(&CHAT)
            .unwrap()
            .messages
            .push_back(pending);

        let mut confirmed = msg(42);
        confirmed.send_state = SendState::Sent;
        handle_td(
            &mut app,
            &TdUpdate::MessageSendSucceeded {
                chat_id: CHAT,
                old_message_id: MessageId(-1),
                message: confirmed,
            },
        );

        let convo = &app.conversations[&CHAT];
        assert_eq!(convo.messages.len(), 1);
        assert_eq!(convo.messages[0].id, MessageId(42));
        assert_eq!(convo.messages[0].send_state, SendState::Sent);
    }

    #[test]
    fn message_send_failed_marks_the_window_entry_failed_without_touching_id() {
        let mut app = fixture_state();
        open(&mut app, CHAT);
        let mut pending = msg(-1);
        pending.send_state = SendState::Sending;
        app.conversations
            .get_mut(&CHAT)
            .unwrap()
            .messages
            .push_back(pending);

        handle_td(
            &mut app,
            &TdUpdate::MessageSendFailed {
                chat_id: CHAT,
                old_message_id: MessageId(-1),
                error: TdError::NetTimeout,
            },
        );

        let convo = &app.conversations[&CHAT];
        assert_eq!(convo.messages.len(), 1);
        assert_eq!(convo.messages[0].id, MessageId(-1));
        assert_eq!(
            convo.messages[0].send_state,
            SendState::Failed(TdError::NetTimeout)
        );
    }

    #[test]
    fn read_markers_update_from_td_updates() {
        let mut app = fixture_state();
        open(&mut app, CHAT);

        handle_td(
            &mut app,
            &TdUpdate::ChatReadInbox {
                chat_id: CHAT,
                last_read_inbox_message_id: MessageId(10),
                unread_count: 0,
            },
        );
        handle_td(
            &mut app,
            &TdUpdate::ChatReadOutbox {
                chat_id: CHAT,
                last_read_outbox_message_id: MessageId(7),
            },
        );

        let convo = &app.conversations[&CHAT];
        assert_eq!(convo.last_read_inbox, MessageId(10));
        assert_eq!(convo.last_read_outbox, MessageId(7));
    }

    // --- apply_history_page / paging machine routing ------------------------

    #[test]
    fn history_loaded_routes_through_paging_machine() {
        let mut app = fixture_state();
        open(&mut app, CHAT);
        let convo = app.conversations.get_mut(&CHAT).unwrap();
        convo.messages.push_back(msg(10));
        convo.paging = PagingState::Loading {
            attempt: 1,
            only_local: false,
        };

        let effects =
            without_view_messages(apply_history_page(&mut app, CHAT, false, &Ok(Vec::new())));

        assert_eq!(effects.len(), 1);
        assert!(matches!(
            effects[0],
            Effect::Td(TdRequest::GetChatHistory {
                chat_id: CHAT,
                from_message_id: MessageId(10),
                limit: history::PAGE_SIZE,
                only_local: false,
            })
        ));
        assert_eq!(
            app.conversations[&CHAT].paging,
            PagingState::Loading {
                attempt: 2,
                only_local: false,
            }
        );
    }

    #[test]
    fn history_error_enters_cooldown_via_the_real_machine() {
        let mut app = fixture_state();
        app.now = Millis(1_000);
        open(&mut app, CHAT);
        app.conversations.get_mut(&CHAT).unwrap().paging = PagingState::Loading {
            attempt: 1,
            only_local: false,
        };

        let effects = apply_history_page(
            &mut app,
            CHAT,
            false,
            &Err(TdError::FloodWait { seconds: 5 }),
        );

        assert!(effects.is_empty());
        assert_eq!(
            app.conversations[&CHAT].paging,
            PagingState::Cooldown {
                until: Millis(6_000)
            }
        );
    }

    #[test]
    fn apply_history_page_for_unopened_chat_is_a_noop() {
        let mut app = fixture_state();
        let effects = apply_history_page(&mut app, CHAT, false, &Ok(vec![msg(1)]));
        assert!(effects.is_empty());
        assert!(app.conversations.is_empty());
    }

    // --- T59: local-first history / remote reconcile ------------------

    /// The shape `chat_list::open_selected`/`palette::open_chat` produce: an
    /// opening request tracked as `Loading { only_local: true }` against an
    /// empty window (nothing loaded yet, so `oldest_loaded` is `None`).
    fn fixture_opening(app: &mut AppState) {
        open(app, CHAT);
        app.conversations.get_mut(&CHAT).unwrap().paging = PagingState::Loading {
            attempt: 1,
            only_local: true,
        };
    }

    #[test]
    fn local_page_applies_and_issues_one_remote_reconcile() {
        let mut app = fixture_state();
        fixture_opening(&mut app);

        let page: Vec<MessageView> = (1..=50).map(msg).collect();
        let effects = without_view_messages(apply_history_page(&mut app, CHAT, true, &Ok(page)));

        assert!(
            matches!(
                effects.as_slice(),
                [Effect::Td(TdRequest::GetChatHistory {
                    chat_id: CHAT,
                    from_message_id: MessageId(0),
                    limit: history::PAGE_SIZE,
                    only_local: false,
                })]
            ),
            "exactly one reconcile request, from the newest-message sentinel: {effects:?}"
        );
        let convo = &app.conversations[&CHAT];
        assert_eq!(
            convo.messages.len(),
            50,
            "the local page rendered instantly"
        );
        assert_eq!(convo.paging, PagingState::Idle);
    }

    /// The reconcile's own completion (`only_local: false`) must never spawn
    /// another one — the loop guard is keyed strictly off `only_local`, not
    /// off `paging`, which is already back to `Idle` by this point regardless.
    #[test]
    fn remote_completion_never_spawns_reconcile() {
        let mut app = fixture_state();
        fixture_opening(&mut app);
        let page: Vec<MessageView> = (1..=50).map(msg).collect();
        apply_history_page(&mut app, CHAT, true, &Ok(page));

        // The reconcile lands: two genuinely new, newer messages plus an
        // overlapping tail of what the local page already had.
        let mut reconcile_page: Vec<MessageView> = (30..=50).map(msg).collect();
        reconcile_page.push(msg(51));
        reconcile_page.push(msg(52));
        let effects = apply_history_page(&mut app, CHAT, false, &Ok(reconcile_page));

        assert!(
            effects.is_empty(),
            "a remote completion must never spawn a reconcile of its own: {effects:?}"
        );
        let convo = &app.conversations[&CHAT];
        assert_eq!(convo.messages.len(), 52, "no duplicates from the overlap");
        assert!(convo.messages.iter().any(|m| m.id == MessageId(51)));
        assert!(convo.messages.iter().any(|m| m.id == MessageId(52)));
        let ids: Vec<i64> = convo.messages.iter().map(|m| m.id.0).collect();
        let mut deduped = ids.clone();
        deduped.dedup();
        assert_eq!(ids.len(), deduped.len(), "no duplicate ids in the window");
    }

    /// The empty-response trap (spec §5.2), through the machine, for the one
    /// request shape it never had to handle before T59: the very first page,
    /// where nothing loaded yet means there is no `oldest_loaded` id to retry
    /// from. The retry must still happen, from TDLib's newest-message
    /// sentinel.
    #[test]
    fn empty_local_page_refetches_remote() {
        let mut app = fixture_state();
        fixture_opening(&mut app);

        let effects = apply_history_page(&mut app, CHAT, true, &Ok(Vec::new()));

        assert!(matches!(
            effects.as_slice(),
            [Effect::Td(TdRequest::GetChatHistory {
                chat_id: CHAT,
                from_message_id: MessageId(0),
                limit: history::PAGE_SIZE,
                only_local: false,
            })]
        ));
        assert_eq!(
            app.conversations[&CHAT].paging,
            PagingState::Loading {
                attempt: 1,
                only_local: false,
            }
        );
        assert!(app.conversations[&CHAT].messages.is_empty());
    }

    // --- T67: filling the viewport on a cold open ---------------------

    /// Every `GetChatHistory` in `effects`, as
    /// `(from_message_id, only_local)` pairs — the two parameters that say
    /// which end of the history a request is asking about.
    fn history_requests(effects: &[Effect]) -> Vec<(i64, bool)> {
        effects
            .iter()
            .filter_map(|e| match e {
                Effect::Td(TdRequest::GetChatHistory {
                    from_message_id,
                    only_local,
                    ..
                }) => Some((from_message_id.0, *only_local)),
                _ => None,
            })
            .collect()
    }

    /// The reported bug: a chat this client has never opened has exactly one
    /// message in TDLib's local database (the chat-list preview), so the
    /// local-first opening page comes back with one message and the paging
    /// machine — correctly — parks at `Idle`. The window must not be left
    /// like that.
    #[test]
    fn cold_open_of_one_message_asks_for_more() {
        let mut app = fixture_state();
        fixture_opening(&mut app);

        let effects = apply_history_page(&mut app, CHAT, true, &Ok(vec![msg(100)]));

        assert_eq!(
            history_requests(&effects),
            vec![(0, false), (100, false)],
            "the T59 reconcile asks from the newest end, the T67 fill from \
             the oldest loaded message: {effects:?}"
        );
        // The fill is a caller-side policy: it leaves the machine alone, so
        // the page it eventually gets back is ignored by the machine rather
        // than mistaken for a scroll-triggered page (and an empty answer
        // never spends the §5.2 retry ladder).
        assert_eq!(app.conversations[&CHAT].paging, PagingState::Idle);
    }

    /// The fill keeps going while the window is short, and stops the moment
    /// it is deep enough — without any scroll input.
    #[test]
    fn fill_repeats_until_the_target_is_reached() {
        let mut app = fixture_state();
        fixture_opening(&mut app);

        // The cold open: one message, and the fill asks for what precedes it.
        let effects = apply_history_page(&mut app, CHAT, true, &Ok(vec![msg(100)]));
        assert!(history_requests(&effects).contains(&(100, false)));

        // TDLib is still syncing and dribbles the history out in short
        // pages. Each one is progress, so each one is followed by another
        // ask, from the new oldest message.
        let mut oldest = 100;
        let mut rounds = 0;
        while app.conversations[&CHAT].messages.len() < VIEWPORT_FILL_TARGET_MESSAGES {
            rounds += 1;
            assert!(
                rounds <= VIEWPORT_FILL_TARGET_MESSAGES,
                "the fill must terminate"
            );
            let page: Vec<MessageView> = ((oldest - 10)..oldest).map(msg).collect();
            oldest -= 10;
            let effects = apply_history_page(&mut app, CHAT, false, &Ok(page));

            let requested = history_requests(&effects);
            if app.conversations[&CHAT].messages.len() < VIEWPORT_FILL_TARGET_MESSAGES {
                assert_eq!(
                    requested,
                    vec![(oldest, false)],
                    "a short window keeps asking, from its new oldest message"
                );
            } else {
                assert!(
                    requested.is_empty(),
                    "a full enough window asks for nothing: {effects:?}"
                );
            }
        }
        assert_eq!(rounds, 5, "10 messages a round from a window of 1");
        assert_eq!(app.conversations[&CHAT].messages.len(), 51);
    }

    /// The anti-spin rule, and the only one that has to hold against a
    /// misbehaving server: a page that contributes nothing new — the same
    /// message over and over, or pure overlap — ends the fill even though the
    /// window is still short.
    #[test]
    fn fill_stops_when_a_page_adds_nothing_new() {
        let mut app = fixture_state();
        fixture_opening(&mut app);
        apply_history_page(&mut app, CHAT, true, &Ok(vec![msg(100)]));

        // The server answers the fill with the message we already have.
        let effects = apply_history_page(&mut app, CHAT, false, &Ok(vec![msg(100)]));

        assert!(
            history_requests(&effects).is_empty(),
            "a page of pure overlap must not be answered with another ask: {effects:?}"
        );
        assert_eq!(app.conversations[&CHAT].messages.len(), 1);
    }

    /// A genuinely short chat: TDLib has nothing older to give. The fill ends
    /// on its own, and — because it never drove the machine — without
    /// latching `Exhausted` on a chat whose server sync may simply be behind.
    #[test]
    fn fill_stops_on_an_empty_answer_without_exhausting_the_machine() {
        let mut app = fixture_state();
        fixture_opening(&mut app);
        apply_history_page(&mut app, CHAT, true, &Ok(vec![msg(100)]));

        let effects = apply_history_page(&mut app, CHAT, false, &Ok(Vec::new()));

        assert!(history_requests(&effects).is_empty(), "{effects:?}");
        assert_eq!(app.conversations[&CHAT].paging, PagingState::Idle);
    }

    /// `Exhausted` is the machine's word for "there is genuinely no more
    /// history", and it outranks a short window.
    #[test]
    fn no_fill_while_exhausted() {
        let mut app = fixture_state();
        open(&mut app, CHAT);
        let convo = app.conversations.get_mut(&CHAT).unwrap();
        convo.messages.push_back(msg(100));
        convo.paging = PagingState::Exhausted;

        // A page still lands (a reconcile's, say) and is prepended, but the
        // window being short is no reason to ask a chat that has no more.
        let effects = apply_history_page(&mut app, CHAT, false, &Ok(vec![msg(99)]));

        assert!(history_requests(&effects).is_empty(), "{effects:?}");
        assert_eq!(app.conversations[&CHAT].messages.len(), 2);
    }

    /// `Cooldown` means TDLib asked for a backoff. A page can still land
    /// during one (a request that was already in flight), and it is still
    /// prepended — but a fill is the least urgent reason to ignore a backoff.
    #[test]
    fn no_fill_while_in_cooldown() {
        let mut app = fixture_state();
        app.now = Millis(1_000);
        open(&mut app, CHAT);
        let convo = app.conversations.get_mut(&CHAT).unwrap();
        convo.messages.push_back(msg(100));
        convo.paging = PagingState::Cooldown {
            until: Millis(9_000),
        };

        let effects = apply_history_page(&mut app, CHAT, false, &Ok(vec![msg(99)]));

        assert!(history_requests(&effects).is_empty(), "{effects:?}");
        assert_eq!(app.conversations[&CHAT].messages.len(), 2);
        assert_eq!(
            app.conversations[&CHAT].paging,
            PagingState::Cooldown {
                until: Millis(9_000)
            },
            "a fill must never move the paging state"
        );
    }

    /// The state table [`fill_viewport`] enforces, asserted on the function
    /// itself. `Loading` is checked here rather than through
    /// [`apply_history_page`] because the machine can only be left `Loading`
    /// by an *empty* page (a non-empty one returns it to `Idle`), and an
    /// empty page adds nothing, so that guard is unreachable from the outside
    /// — it is the belt to the `added == 0` braces, and the two must not be
    /// allowed to drift apart.
    #[test]
    fn fill_asks_only_from_idle_and_only_after_progress() {
        let mut app = fixture_state();
        open(&mut app, CHAT);
        let convo = app.conversations.get_mut(&CHAT).unwrap();
        convo.messages.push_back(msg(100));

        for paging in [
            PagingState::Loading {
                attempt: 1,
                only_local: false,
            },
            PagingState::Loading {
                attempt: 1,
                only_local: true,
            },
            PagingState::Cooldown {
                until: Millis(9_000),
            },
            PagingState::Exhausted,
        ] {
            let convo = app.conversations.get_mut(&CHAT).unwrap();
            convo.paging = paging;
            assert!(
                fill_viewport(convo, CHAT, 1).is_none(),
                "{paging:?} must not fill"
            );
        }

        let convo = app.conversations.get_mut(&CHAT).unwrap();
        convo.paging = PagingState::Idle;
        assert!(
            fill_viewport(convo, CHAT, 0).is_none(),
            "a page that added nothing new must not fill, even from Idle"
        );
        assert!(fill_viewport(convo, CHAT, 1).is_some());
    }

    /// A full first page needs no help — the common case, and the one that
    /// must stay free of extra traffic.
    #[test]
    fn full_first_page_triggers_no_fill() {
        let mut app = fixture_state();
        fixture_opening(&mut app);

        let page: Vec<MessageView> = (1..=(VIEWPORT_FILL_TARGET_MESSAGES as i64))
            .map(msg)
            .collect();
        let effects = apply_history_page(&mut app, CHAT, true, &Ok(page));

        assert_eq!(
            history_requests(&effects),
            vec![(0, false)],
            "only T59's reconcile, no fill: {effects:?}"
        );
    }

    /// The two follow-ups a cold open puts in flight ask opposite ends of the
    /// history, and neither one's completion re-triggers the other: the
    /// reconcile is keyed off `only_local` (a fill is never local), and the
    /// fill is keyed off a window that is still short *and* a page that
    /// brought something new.
    #[test]
    fn fill_and_reconcile_do_not_trigger_each_other() {
        let mut app = fixture_state();
        fixture_opening(&mut app);
        apply_history_page(&mut app, CHAT, true, &Ok(vec![msg(100)]));

        // The reconcile lands first with one genuinely newer message. It is
        // remote, so it spawns no reconcile of its own; the window is still
        // short and it did bring something new, so it does spawn a fill —
        // asking older, which is not where the reconcile was looking. (Which
        // id exactly is not this test's subject: the reconcile's own newer
        // message is what `prepend_messages` just put at the front.)
        let effects = apply_history_page(&mut app, CHAT, false, &Ok(vec![msg(100), msg(101)]));
        let requested = history_requests(&effects);
        assert_eq!(
            requested.len(),
            1,
            "one fill, no second reconcile: {effects:?}"
        );
        assert!(
            !requested[0].1,
            "every fill is remote, which is also why a fill's own completion \
             can never spawn a reconcile: {effects:?}"
        );

        // The original fill's page lands last and finishes the job; nothing
        // further is asked.
        let page: Vec<MessageView> = (50..100).map(msg).collect();
        let effects = apply_history_page(&mut app, CHAT, false, &Ok(page));
        assert!(history_requests(&effects).is_empty(), "{effects:?}");
        assert_eq!(app.conversations[&CHAT].messages.len(), 52);
    }

    // --- handle_key ----------------------------------------------------

    #[test]
    fn handle_key_unclaimed_without_open_chat() {
        let mut app = fixture_state();
        app.focus = FocusStack::new(Focus::Composer);
        assert!(handle_key(&mut app, Key::Up).is_none());
    }

    #[test]
    fn handle_key_unclaimed_while_chat_list_focused() {
        let mut app = fixture_state();
        open(&mut app, CHAT); // focus stays ChatList (fixture_state's default)
        assert!(handle_key(&mut app, Key::Up).is_none());
    }

    #[test]
    fn up_from_bottom_anchors_to_newest_loaded_message() {
        let mut app = fixture_state();
        fixture_open(&mut app);
        let convo = app.conversations.get_mut(&CHAT).unwrap();
        for id in 1..=5 {
            convo.messages.push_back(msg(id));
        }
        // Isolate the anchor transition from the near-top paging trigger
        // (covered separately by `scrolling_near_top_triggers_paging_request`).
        convo.paging = PagingState::Exhausted;

        let effects = handle_key(&mut app, Key::Up).expect("conversation claims Up");
        assert!(effects.is_empty());
        assert_eq!(
            app.conversations[&CHAT].scroll,
            Scroll::At {
                message_id: MessageId(5),
                line_offset: 0
            }
        );
    }

    /// The trap `Scroll::AtTop` exists to avoid, and the one no assertion
    /// taken right after a jump can catch: if anchor movement converted the
    /// flavour back, the message the user jumped to would drop from the
    /// first visible row to the last on their very next keypress. Both
    /// movers get their own case — `step_anchor` here, `move_anchor` (the
    /// scroll keys) below — because either can carry the suite alone.
    #[test]
    fn stepping_from_a_top_anchor_stays_top_anchored() {
        let mut app = fixture_state();
        fixture_open(&mut app);
        let convo = app.conversations.get_mut(&CHAT).unwrap();
        for id in 1..=20 {
            convo.messages.push_back(msg(id));
        }
        convo.paging = PagingState::Exhausted;
        convo.scroll = Scroll::AtTop {
            message_id: MessageId(10),
        };

        step_anchor(convo, CHAT, -1, Millis(0));

        assert_eq!(
            app.conversations[&CHAT].scroll,
            Scroll::AtTop {
                message_id: MessageId(9)
            },
            "NOT Scroll::At — see this test's doc comment"
        );
    }

    #[test]
    fn scrolling_from_a_top_anchor_stays_top_anchored() {
        let mut app = fixture_state();
        fixture_open(&mut app);
        let convo = app.conversations.get_mut(&CHAT).unwrap();
        for id in 1..=20 {
            convo.messages.push_back(msg(id));
        }
        convo.paging = PagingState::Exhausted;
        convo.scroll = Scroll::AtTop {
            message_id: MessageId(10),
        };

        handle_key(&mut app, Key::Down).expect("conversation claims Down");

        assert_eq!(
            app.conversations[&CHAT].scroll,
            Scroll::AtTop {
                message_id: MessageId(11)
            },
            "NOT Scroll::At — see `stepping_from_a_top_anchor_stays_top_anchored`"
        );
    }

    /// The one conversion that is correct: "at the top" means nothing for
    /// the newest loaded message, since there is nothing newer to fill the
    /// pane below it.
    #[test]
    fn stepping_a_top_anchor_onto_the_newest_message_repins_to_bottom() {
        let mut app = fixture_state();
        fixture_open(&mut app);
        let convo = app.conversations.get_mut(&CHAT).unwrap();
        for id in 1..=5 {
            convo.messages.push_back(msg(id));
        }
        convo.paging = PagingState::Exhausted;
        convo.scroll = Scroll::AtTop {
            message_id: MessageId(4),
        };

        step_anchor(convo, CHAT, 1, Millis(0));

        assert_eq!(app.conversations[&CHAT].scroll, Scroll::Bottom);
    }

    #[test]
    fn down_key_at_bottom_is_claimed_but_a_noop() {
        let mut app = fixture_state();
        fixture_open(&mut app);
        app.conversations
            .get_mut(&CHAT)
            .unwrap()
            .messages
            .push_back(msg(1));

        let effects = handle_key(&mut app, Key::Down).expect("conversation claims Down");
        assert!(without_view_messages(effects).is_empty());
        assert_eq!(app.conversations[&CHAT].scroll, Scroll::Bottom);
    }

    #[test]
    fn down_past_newest_returns_to_bottom() {
        let mut app = fixture_state();
        fixture_open(&mut app);
        let convo = app.conversations.get_mut(&CHAT).unwrap();
        convo.messages.push_back(msg(1));
        convo.messages.push_back(msg(2));
        convo.scroll = Scroll::At {
            message_id: MessageId(2),
            line_offset: 0,
        };

        handle_key(&mut app, Key::Down).unwrap();

        assert_eq!(app.conversations[&CHAT].scroll, Scroll::Bottom);
    }

    #[test]
    fn up_at_oldest_message_clamps_instead_of_underflowing() {
        let mut app = fixture_state();
        fixture_open(&mut app);
        let convo = app.conversations.get_mut(&CHAT).unwrap();
        convo.messages.push_back(msg(1));
        convo.messages.push_back(msg(2));
        convo.scroll = Scroll::At {
            message_id: MessageId(1),
            line_offset: 0,
        };
        // Long enough window that the near-top trigger doesn't also fire and
        // change paging state — this test only cares about clamping.
        convo.paging = PagingState::Exhausted;

        handle_key(&mut app, Key::Up).unwrap();

        assert_eq!(
            app.conversations[&CHAT].scroll,
            Scroll::At {
                message_id: MessageId(1),
                line_offset: 0
            }
        );
    }

    #[test]
    fn scrolling_near_top_triggers_paging_request() {
        let mut app = fixture_state();
        fixture_open(&mut app);
        let convo = app.conversations.get_mut(&CHAT).unwrap();
        // 21 messages: anchoring on the 2nd-oldest (index 1) is within
        // PAGE_TRIGGER_MESSAGES (20) of the oldest loaded message.
        for id in 1..=21 {
            convo.messages.push_back(msg(id));
        }
        convo.scroll = Scroll::At {
            message_id: MessageId(2),
            line_offset: 0,
        };

        let effects = handle_key(&mut app, Key::Up).unwrap();

        assert_eq!(effects.len(), 1);
        assert!(matches!(
            effects[0],
            Effect::Td(TdRequest::GetChatHistory {
                chat_id: CHAT,
                from_message_id: MessageId(1),
                limit: history::PAGE_SIZE,
                only_local: false,
            })
        ));
        assert_eq!(
            app.conversations[&CHAT].paging,
            PagingState::Loading {
                attempt: 1,
                only_local: false,
            }
        );
    }

    /// An anchor older than the whole window is where `state::search`'s `n`
    /// leaves the viewport when the hit TDLib found is off-window. Scrolling
    /// from there must fetch the page that contains it — re-pinning to the
    /// bottom instead would throw the jump away before it could ever be
    /// drawn.
    #[test]
    fn scrolling_with_an_anchor_older_than_the_window_pages_instead_of_repinning() {
        let mut app = fixture_state();
        fixture_open(&mut app);
        let convo = app.conversations.get_mut(&CHAT).unwrap();
        for id in 100..=130 {
            convo.messages.push_back(msg(id));
        }
        convo.scroll = Scroll::At {
            message_id: MessageId(42),
            line_offset: 0,
        };

        let effects = handle_key(&mut app, Key::PageUp).unwrap();

        assert!(
            matches!(
                effects.as_slice(),
                [Effect::Td(TdRequest::GetChatHistory {
                    chat_id: CHAT,
                    from_message_id: MessageId(100),
                    only_local: false,
                    ..
                })]
            ),
            "expected a page request from the oldest loaded message, got {effects:?}"
        );
        assert_eq!(
            app.conversations[&CHAT].scroll,
            Scroll::At {
                message_id: MessageId(42),
                line_offset: 0,
            },
            "the anchor must survive the scroll that pages it in"
        );
    }

    /// The other half of the rule: an anchor that is *newer* than everything
    /// loaded was evicted at the back, and no amount of older history will
    /// bring it back — that one still re-pins.
    #[test]
    fn scrolling_with_an_anchor_newer_than_the_window_repins_to_bottom() {
        let mut app = fixture_state();
        fixture_open(&mut app);
        let convo = app.conversations.get_mut(&CHAT).unwrap();
        for id in 100..=130 {
            convo.messages.push_back(msg(id));
        }
        convo.scroll = Scroll::At {
            message_id: MessageId(500),
            line_offset: 0,
        };

        let effects = without_view_messages(handle_key(&mut app, Key::PageUp).unwrap());

        assert!(effects.is_empty(), "{effects:?}");
        assert_eq!(app.conversations[&CHAT].scroll, Scroll::Bottom);
    }

    #[test]
    fn page_up_steps_by_page_step_messages() {
        let mut app = fixture_state();
        fixture_open(&mut app);
        let convo = app.conversations.get_mut(&CHAT).unwrap();
        for id in 1..=30 {
            convo.messages.push_back(msg(id));
        }
        convo.scroll = Scroll::At {
            message_id: MessageId(25),
            line_offset: 0,
        };

        handle_key(&mut app, Key::PageUp).unwrap();

        assert_eq!(
            app.conversations[&CHAT].scroll,
            Scroll::At {
                message_id: MessageId(15),
                line_offset: 0
            }
        );
    }

    #[test]
    fn handle_key_ignores_unrelated_keys() {
        let mut app = fixture_state();
        fixture_open(&mut app);
        app.conversations
            .get_mut(&CHAT)
            .unwrap()
            .messages
            .push_back(msg(1));
        assert!(handle_key(&mut app, Key::Char('a')).is_none());
    }

    // --- T72: marking messages read -----------------------------------

    fn own_msg(id: i64) -> MessageView {
        MessageView {
            is_outgoing: true,
            ..msg(id)
        }
    }

    /// An open, bottom-pinned chat holding `ids`, with everything at or below
    /// `last_read_inbox` already read.
    fn fixture_unread(app: &mut AppState, messages: Vec<MessageView>, last_read_inbox: i64) {
        fixture_open(app);
        let convo = app.conversations.get_mut(&CHAT).unwrap();
        convo.messages.extend(messages);
        convo.last_read_inbox = MessageId(last_read_inbox);
    }

    #[test]
    fn unread_messages_are_marked_read_from_the_watermark_up() {
        let mut app = fixture_state();
        fixture_unread(&mut app, (1..=5).map(msg).collect(), 2);

        let effects = mark_visible_read(&mut app, CHAT);

        assert_eq!(
            view_requests(&effects),
            vec![(CHAT, vec![MessageId(3), MessageId(4), MessageId(5)])],
            "only the messages newer than last_read_inbox, ascending"
        );
    }

    #[test]
    fn outgoing_messages_are_never_marked_read() {
        let mut app = fixture_state();
        fixture_unread(&mut app, vec![msg(1), own_msg(2), msg(3), own_msg(4)], 0);

        let effects = mark_visible_read(&mut app, CHAT);

        assert_eq!(
            view_requests(&effects),
            vec![(CHAT, vec![MessageId(1), MessageId(3)])],
            "the user's own messages are not theirs to read"
        );
    }

    #[test]
    fn a_chat_with_nothing_unread_asks_for_nothing() {
        let mut app = fixture_state();
        fixture_unread(&mut app, (1..=5).map(msg).collect(), 5);

        assert!(mark_visible_read(&mut app, CHAT).is_empty());
        assert!(app.conversations[&CHAT].pending_view.is_none());
    }

    /// Storm control: every trigger fires repeatedly, and `last_read_inbox`
    /// cannot advance until TDLib answers.
    #[test]
    fn the_same_ids_are_not_resent_while_a_request_is_in_flight() {
        let mut app = fixture_state();
        fixture_unread(&mut app, (1..=5).map(msg).collect(), 0);

        assert_eq!(view_requests(&mark_visible_read(&mut app, CHAT)).len(), 1);
        for _ in 0..10 {
            assert!(
                mark_visible_read(&mut app, CHAT).is_empty(),
                "a repeated trigger must not re-send the same ids"
            );
        }
    }

    /// A message arriving while the earlier request is still outstanding is
    /// new information, not a repeat: it raises the watermark and goes out.
    #[test]
    fn a_newer_arrival_is_marked_even_with_a_request_outstanding() {
        let mut app = fixture_state();
        fixture_unread(&mut app, (1..=5).map(msg).collect(), 0);
        mark_visible_read(&mut app, CHAT);

        let effects = handle_td(&mut app, &TdUpdate::NewMessage(msg(6)));

        let requests = view_requests(&effects);
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].1.last(), Some(&MessageId(6)));
    }

    /// The window can be scrolled arbitrarily far back, and everything unread
    /// is newer than what is on screen there. See `mark_visible_read`'s
    /// "Only while pinned to the bottom".
    #[test]
    fn a_scrolled_back_window_marks_nothing_read() {
        let mut app = fixture_state();
        fixture_unread(&mut app, (1..=50).map(msg).collect(), 0);
        app.conversations.get_mut(&CHAT).unwrap().scroll = Scroll::At {
            message_id: MessageId(5),
            line_offset: 0,
        };

        assert!(mark_visible_read(&mut app, CHAT).is_empty());

        // Scrolling back down to the newest message is the user saying they
        // are looking at it — and only the step that arrives there marks
        // anything read.
        let mut effects = Vec::new();
        while !matches!(app.conversations[&CHAT].scroll, Scroll::Bottom) {
            assert!(
                view_requests(&effects).is_empty(),
                "still short of the bottom"
            );
            effects = handle_key(&mut app, Key::PageDown).expect("conversation claims PageDown");
        }
        assert_eq!(view_requests(&effects).len(), 1);

        // Storm control applies to this trigger like every other: bouncing
        // off the bottom and back must not re-send ids already in flight.
        handle_key(&mut app, Key::Up).expect("conversation claims Up");
        let effects = handle_key(&mut app, Key::Down).expect("conversation claims Down");
        assert_eq!(app.conversations[&CHAT].scroll, Scroll::Bottom);
        assert!(
            view_requests(&effects).is_empty(),
            "returning to the bottom again re-sends nothing: {effects:?}"
        );
    }

    /// The scrolled-back rule holds for the *arrival* triggers too, not just
    /// a direct call: a message landing below the fold is exactly the one the
    /// user has not seen. The chat is open and the message is unread, so
    /// every condition but the anchor is met — which is what makes this the
    /// case a "mark it read, the chat is open" regression would slip past.
    #[test]
    fn a_message_arriving_below_the_fold_is_not_marked_read() {
        let mut app = fixture_state();
        fixture_unread(&mut app, (1..=50).map(msg).collect(), 0);
        app.conversations.get_mut(&CHAT).unwrap().scroll = Scroll::At {
            message_id: MessageId(5),
            line_offset: 0,
        };

        let effects = handle_td(&mut app, &TdUpdate::NewMessage(msg(51)));

        assert!(
            view_requests(&effects).is_empty(),
            "a message the user is not looking at is not read: {effects:?}"
        );
        assert!(app.conversations[&CHAT].pending_view.is_none());
    }

    /// Same rule for a history page: scrolling up pages older messages in
    /// while the unread ones sit below the fold, untouched.
    #[test]
    fn a_history_page_landing_below_the_fold_marks_nothing_read() {
        let mut app = fixture_state();
        fixture_unread(&mut app, (20..=50).map(msg).collect(), 0);
        let convo = app.conversations.get_mut(&CHAT).unwrap();
        convo.scroll = Scroll::At {
            message_id: MessageId(21),
            line_offset: 0,
        };
        convo.paging = PagingState::Loading {
            attempt: 1,
            only_local: false,
        };

        let effects = apply_history_page(&mut app, CHAT, false, &Ok((1..=19).map(msg).collect()));

        assert!(
            view_requests(&effects).is_empty(),
            "paging older history is not reading newer messages: {effects:?}"
        );
        assert!(app.conversations[&CHAT].pending_view.is_none());
    }

    /// A chat the user has visited keeps receiving messages in the
    /// background (`append_new_message`); none of it is on screen.
    #[test]
    fn a_background_chat_is_never_marked_read() {
        let mut app = fixture_state();
        fixture_unread(&mut app, (1..=5).map(msg).collect(), 0);
        app.open_chat = Some(ChatId(99));

        assert!(mark_visible_read(&mut app, CHAT).is_empty());
        assert!(handle_td(&mut app, &TdUpdate::NewMessage(msg(6))).is_empty());
    }

    #[test]
    fn one_request_carries_at_most_a_page_of_ids() {
        let mut app = fixture_state();
        fixture_unread(&mut app, (1..=200).map(msg).collect(), 0);

        let requests = view_requests(&mark_visible_read(&mut app, CHAT));

        let ids = &requests[0].1;
        assert_eq!(ids.len(), MAX_VIEW_MESSAGES_PER_REQUEST);
        assert_eq!(
            ids.last(),
            Some(&MessageId(200)),
            "the bound keeps the newest ids — `viewMessages` moves a watermark"
        );
    }

    /// TDLib's own update is the answer `ViewMessages` never gets as a
    /// completion, and it is what clears the badge. Nothing here zeroes an
    /// unread count locally.
    #[test]
    fn read_inbox_confirmation_retires_the_outstanding_request() {
        let mut app = fixture_state();
        fixture_unread(&mut app, (1..=5).map(msg).collect(), 0);
        mark_visible_read(&mut app, CHAT);
        assert!(app.conversations[&CHAT].pending_view.is_some());

        handle_td(
            &mut app,
            &TdUpdate::ChatReadInbox {
                chat_id: CHAT,
                last_read_inbox_message_id: MessageId(5),
                unread_count: 0,
            },
        );

        let convo = &app.conversations[&CHAT];
        assert_eq!(convo.last_read_inbox, MessageId(5));
        assert!(convo.pending_view.is_none());
        // And with the watermark caught up, there is nothing left to ask for.
        assert!(mark_visible_read(&mut app, CHAT).is_empty());
    }

    /// The anti-wedge rule: `ViewMessages` has no completion, so a request
    /// TDLib drops must expire rather than latch the chat unread forever.
    #[test]
    fn a_dropped_request_is_retried_after_the_in_flight_window_expires() {
        let mut app = fixture_state();
        fixture_unread(&mut app, (1..=5).map(msg).collect(), 0);
        assert_eq!(view_requests(&mark_visible_read(&mut app, CHAT)).len(), 1);

        app.now = Millis(VIEW_REQUEST_RETRY_AFTER_MS - 1);
        assert!(mark_visible_read(&mut app, CHAT).is_empty());

        app.now = Millis(VIEW_REQUEST_RETRY_AFTER_MS);
        assert_eq!(
            view_requests(&mark_visible_read(&mut app, CHAT)),
            vec![(
                CHAT,
                vec![
                    MessageId(1),
                    MessageId(2),
                    MessageId(3),
                    MessageId(4),
                    MessageId(5)
                ]
            )]
        );
    }

    /// ...but a TDLib that ignores the request forever must not be hammered:
    /// the retries are bounded per watermark.
    #[test]
    fn retries_for_one_watermark_are_bounded() {
        let mut app = fixture_state();
        fixture_unread(&mut app, (1..=5).map(msg).collect(), 0);

        let mut sent = 0;
        for round in 0..10u64 {
            app.now = Millis(round * VIEW_REQUEST_RETRY_AFTER_MS);
            sent += view_requests(&mark_visible_read(&mut app, CHAT)).len();
        }
        assert_eq!(sent, MAX_VIEW_ATTEMPTS as usize);

        // A newer message is new information and earns a fresh budget.
        app.conversations
            .get_mut(&CHAT)
            .unwrap()
            .messages
            .push_back(msg(6));
        assert_eq!(view_requests(&mark_visible_read(&mut app, CHAT)).len(), 1);
    }

    /// The tick is the retry trigger for a user sitting still with the chat
    /// open; it costs nothing when there is nothing to mark.
    #[test]
    fn the_tick_retries_and_is_otherwise_silent() {
        let mut app = fixture_state();
        assert!(handle_tick(&mut app).is_empty(), "no chat open");

        fixture_unread(&mut app, (1..=5).map(msg).collect(), 5);
        assert!(handle_tick(&mut app).is_empty(), "nothing unread");

        app.conversations.get_mut(&CHAT).unwrap().last_read_inbox = MessageId(0);
        assert_eq!(view_requests(&handle_tick(&mut app)).len(), 1);
        app.now = Millis(VIEW_REQUEST_RETRY_AFTER_MS);
        assert_eq!(
            view_requests(&handle_tick(&mut app)).len(),
            1,
            "the request TDLib never answered goes out again"
        );
    }

    /// The unread messages arrive *with* the opening page: a first open has
    /// nothing to mark until it lands.
    #[test]
    fn a_landing_history_page_marks_the_unread_it_brought() {
        let mut app = fixture_state();
        fixture_open(&mut app);
        app.conversations.get_mut(&CHAT).unwrap().paging = PagingState::Loading {
            attempt: 1,
            only_local: false,
        };

        let effects = apply_history_page(&mut app, CHAT, false, &Ok((1..=3).map(msg).collect()));

        assert_eq!(
            view_requests(&effects),
            vec![(CHAT, vec![MessageId(1), MessageId(2), MessageId(3)])]
        );
    }

    // --- T9: jump-to-quote hunt ----------------------------------------

    #[test]
    fn a_hunt_pages_backward_and_moves_the_anchor_so_pages_are_not_evicted() {
        let mut app = fixture_state();
        open(&mut app, CHAT);
        let convo = app.conversations.get_mut(&CHAT).unwrap();
        for id in 50..60 {
            convo.messages.push_back(msg(id));
        }
        convo.scroll = Scroll::Bottom;
        convo.paging = PagingState::Idle;

        let effects = start_hunt(&mut app, CHAT, MessageId(20));

        assert!(matches!(
            effects.as_slice(),
            [Effect::Td(TdRequest::GetChatHistory { from_message_id, .. })]
                if *from_message_id == MessageId(50)
        ));
        assert!(app.conversations[&CHAT].hunt.is_some());
        // The anchor left the bottom, or eviction would drop the pages the
        // hunt is about to fetch.
        assert_ne!(app.conversations[&CHAT].scroll, Scroll::Bottom);
    }

    #[test]
    fn a_hunt_lands_when_its_target_arrives() {
        let mut app = fixture_state();
        open(&mut app, CHAT);
        let convo = app.conversations.get_mut(&CHAT).unwrap();
        for id in 50..60 {
            convo.messages.push_back(msg(id));
        }
        convo.paging = PagingState::Loading {
            attempt: 1,
            only_local: false,
        };
        start_hunt(&mut app, CHAT, MessageId(45));

        let page: Vec<MessageView> = (40..50).map(msg).collect();
        apply_history_page(&mut app, CHAT, false, &Ok(page));

        assert!(app.conversations[&CHAT].hunt.is_none(), "hunt cleared");
        // The landing is top-anchored like every other quote jump
        // (architecture §7.5.4); only the hunt's backward walk, which is a
        // progress indicator rather than a destination, stays bottom-anchored.
        assert_eq!(
            app.conversations[&CHAT].scroll,
            Scroll::AtTop {
                message_id: MessageId(45),
            }
        );
    }

    #[test]
    fn a_hunt_gives_up_after_max_pages_and_says_so() {
        let mut app = fixture_state();
        open(&mut app, CHAT);
        let convo = app.conversations.get_mut(&CHAT).unwrap();
        for id in 900..1000 {
            convo.messages.push_back(msg(id));
        }
        convo.paging = PagingState::Loading {
            attempt: 1,
            only_local: false,
        };
        start_hunt(&mut app, CHAT, MessageId(1));
        app.conversations.get_mut(&CHAT).unwrap().hunt = Some(JumpHunt {
            target: MessageId(1),
            pages_spent: MAX_HUNT_PAGES,
        });

        let page: Vec<MessageView> = (800..900).map(msg).collect();
        apply_history_page(&mut app, CHAT, false, &Ok(page));

        assert!(app.conversations[&CHAT].hunt.is_none());
        assert!(
            !app.toasts.toasts.is_empty(),
            "giving up silently is the failure this bound exists to make visible"
        );
    }

    #[test]
    fn a_hunt_stops_at_the_start_of_history() {
        let mut app = fixture_state();
        open(&mut app, CHAT);
        let convo = app.conversations.get_mut(&CHAT).unwrap();
        convo.messages.push_back(msg(5));
        convo.paging = PagingState::Loading {
            attempt: history::MAX_EMPTY_ATTEMPTS,
            only_local: false,
        };
        start_hunt(&mut app, CHAT, MessageId(1));

        // An empty non-local page at max attempts latches `Exhausted`.
        apply_history_page(&mut app, CHAT, false, &Ok(Vec::new()));

        assert_eq!(app.conversations[&CHAT].paging, PagingState::Exhausted);
        assert!(app.conversations[&CHAT].hunt.is_none());
        assert!(!app.toasts.toasts.is_empty());
    }

    #[test]
    fn a_history_error_ends_the_hunt_rather_than_stalling_it() {
        let mut app = fixture_state();
        open(&mut app, CHAT);
        let convo = app.conversations.get_mut(&CHAT).unwrap();
        convo.messages.push_back(msg(50));
        convo.paging = PagingState::Loading {
            attempt: 1,
            only_local: false,
        };
        start_hunt(&mut app, CHAT, MessageId(1));

        apply_history_page(
            &mut app,
            CHAT,
            false,
            &Err(TdError::FloodWait { seconds: 30 }),
        );

        // Waiting out a 30-second cooldown with no sign of life is worse than
        // saying it stopped; the user can press `j` again.
        assert!(app.conversations[&CHAT].hunt.is_none());
        assert!(!app.toasts.toasts.is_empty());
    }

    /// None of the five termination-arm tests above ever drive a hunt past
    /// its first page: each hits `Landed`, `GaveUp` or the `Err` arm on the
    /// very first response. The continuing case — the page didn't have the
    /// target, the window isn't exhausted, and the budget isn't spent — is
    /// the hunt's actual steady state for anything more than one page away,
    /// and it is exactly the kind of arm that can go dead while the other
    /// five carry the suite (CLAUDE.md's own warning). This drives two
    /// pages and checks the second request is asked exactly once — a
    /// regression test for a real duplicate-dispatch bug: `anchor_to`'s own
    /// near-top trigger already re-requests once `convo.paging` goes back to
    /// `Idle` after a non-empty page, so a continuation that also pushes its
    /// own unconditional `GetChatHistory` fires the same request twice.
    #[test]
    fn a_hunt_continues_paging_and_asks_for_exactly_one_more_page() {
        let mut app = fixture_state();
        open(&mut app, CHAT);
        let convo = app.conversations.get_mut(&CHAT).unwrap();
        for id in 200..210 {
            convo.messages.push_back(msg(id));
        }
        convo.paging = PagingState::Idle;
        start_hunt(&mut app, CHAT, MessageId(1));

        // A page that neither contains the target nor is empty: the hunt
        // must continue rather than land, give up, or stop.
        let page: Vec<MessageView> = (190..200).map(msg).collect();
        let effects = apply_history_page(&mut app, CHAT, false, &Ok(page));

        let requests: Vec<_> = effects
            .iter()
            .filter(|e| matches!(e, Effect::Td(TdRequest::GetChatHistory { .. })))
            .collect();
        assert_eq!(
            requests.len(),
            1,
            "expected exactly one continuation request, got {effects:?}"
        );
        assert!(matches!(
            requests.as_slice(),
            [Effect::Td(TdRequest::GetChatHistory { from_message_id, .. })]
                if *from_message_id == MessageId(190)
        ));
        assert_eq!(
            app.conversations[&CHAT].hunt,
            Some(JumpHunt {
                target: MessageId(1),
                pages_spent: 1,
            })
        );
    }
}
