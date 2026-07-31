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
//! ## Reply excerpts (architecture §7 / T09 findings)
//!
//! TDLib leaves `ReplyPreview.excerpt` empty for same-chat replies; filling
//! it from the local window is T25/T26's job. Every handler in this module
//! only ever replaces a message wholesale with a `MessageView` TDLib itself
//! delivered, so there is nothing here that could clobber a filled-in
//! excerpt — this file simply never touches `reply_to` on an existing entry.

use std::collections::{BTreeSet, VecDeque};

use crate::app::AppState;
use crate::effect::Effect;
use crate::model::ids::{ChatId, MessageId};
use crate::model::key::Key;
use crate::model::message::MessageView;
use crate::model::time::Millis;
use crate::state::focus::Focus;
use crate::state::history::{self, PagingDirective, PagingState};
use crate::state::media;
use crate::state::selection::SelectionState;
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scroll {
    /// Pinned to newest; new messages keep the view at the bottom.
    Bottom,
    /// Anchored at a message (stable across prepends), offset in laid-out lines.
    At {
        message_id: MessageId,
        line_offset: u16,
    },
}

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
    /// In-chat search hits (populated by state/search.rs).
    pub search_hits: Vec<MessageId>,
    /// Selection mode, per open chat and transient (architecture §4.6).
    /// `None` whenever the user is not in selection mode — and forced back to
    /// `None` by [`drop_selection_if_gone`] the moment the selected message
    /// leaves the window (deleted server-side, or evicted by the window
    /// bound), so no handler ever has to cope with a dangling selection.
    pub selection: Option<SelectionState>,
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
            search_hits: Vec::new(),
            selection: None,
        });
    app.open_chat = Some(chat_id);
}

pub fn handle_td(app: &mut AppState, upd: &TdUpdate) -> Vec<Effect> {
    match upd {
        TdUpdate::NewMessage(msg) => {
            append_new_message(app, msg);
            // T66: the arrival changes what's visible near the anchor
            // (`Scroll::Bottom` especially — a new message is by
            // definition the newest thing loaded).
            return media::auto_download_photos(app, msg.chat_id);
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

    if let Scroll::At { message_id, .. } = convo.scroll
        && deleted.contains(&message_id)
    {
        convo.scroll = reanchor_after_deletion(&convo.messages, message_id);
    }
}

fn reanchor_after_deletion(messages: &VecDeque<MessageView>, deleted_id: MessageId) -> Scroll {
    if let Some(newer) = messages.iter().find(|m| m.id > deleted_id) {
        return Scroll::At {
            message_id: newer.id,
            line_offset: 0,
        };
    }
    if let Some(older) = messages.iter().rev().find(|m| m.id < deleted_id) {
        return Scroll::At {
            message_id: older.id,
            line_offset: 0,
        };
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

            effects.extend(fill_viewport(convo, chat_id, added));

            effects
        }
        Err(e) => {
            let retry_after = match e {
                TdError::FloodWait { seconds } => Some(*seconds),
                _ => None,
            };
            history::on_history_error(&mut convo.paging, retry_after, app.now);
            Vec::new()
        }
    };

    // T66: a prepended page (or even a failed/empty attempt, harmlessly —
    // storm control makes a redundant call a no-op) is exactly the kind of
    // change to the visible window auto-download exists to react to.
    effects.extend(media::auto_download_photos(app, chat_id));
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
    let is_newest = convo.messages.back().is_some_and(|m| m.id == message_id);
    convo.scroll = if is_newest {
        Scroll::Bottom
    } else {
        Scroll::At {
            message_id,
            line_offset: 0,
        }
    };
    trigger_paging_if_near_top(convo, chat_id, now)
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
            Scroll::At { message_id, .. } => match index_of(messages, *message_id) {
                Some(idx) => {
                    let dist_front = idx;
                    let dist_back = messages.len() - 1 - idx;
                    dist_front <= dist_back
                }
                None => false,
            },
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
        move_anchor(convo, chat_id, delta, now)
    };
    // T66: every anchor step changes what's near it.
    effects.extend(media::auto_download_photos(app, chat_id));
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

    let current_idx = match convo.scroll {
        Scroll::Bottom => {
            if delta >= 0 {
                return Vec::new();
            }
            last_idx + 1
        }
        Scroll::At { message_id, .. } => match index_of(&convo.messages, message_id) {
            Some(idx) => idx as isize,
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
        },
    };

    let target_idx = current_idx + delta;
    if target_idx > last_idx {
        convo.scroll = Scroll::Bottom;
        return Vec::new();
    }
    let clamped_idx = target_idx.clamp(0, last_idx) as usize;
    let new_id = convo.messages[clamped_idx].id;
    convo.scroll = Scroll::At {
        message_id: new_id,
        line_offset: 0,
    };
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
    let Scroll::At { message_id, .. } = convo.scroll else {
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
            telemetry_salt: [0u8; 32],
            now: Millis(0),
        }
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

        let effects = apply_history_page(&mut app, CHAT, false, &Ok(Vec::new()));

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
        let effects = apply_history_page(&mut app, CHAT, true, &Ok(page));

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
        assert!(effects.is_empty());
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

        let effects = handle_key(&mut app, Key::PageUp).unwrap();

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
}
