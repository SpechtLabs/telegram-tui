//! In-chat message search: `/` while the message list is focused opens a
//! query box, `Enter` fires `searchChatMessages`, `n`/`N` step through the
//! answer. See docs/architecture.md §4.6 (this struct), §4.3
//! (`TdResult::SearchDone`), §4.7 (`TdRequest::SearchChatMessages`); spec
//! §11.
//!
//! ## Focus is the router's, not this module's
//!
//! T45 pushes `Focus::ChatSearch` and calls [`open`] right after; it pops the
//! focus and calls [`close`] on the way out — including the `Esc` path,
//! which [`handle_key`] deliberately leaves unclaimed so the router's
//! generic pop handles it (mirrors `selection::enter`/`exit`, architecture
//! §4.6's routing contract).
//!
//! ## Hit order is TDLib's, verbatim
//!
//! `searchChatMessages` with `from_message_id: 0` answers newest-first (the
//! same convention as `getChatHistory`). [`handle_td_result`] stores
//! whatever order the response carries into `ConversationState.search_hits`
//! without re-sorting, so `n` walks toward older hits and `N`/shift-`n`
//! toward newer ones for a typical answer — this module makes no assumption
//! about the order beyond "index 0 is where stepping starts".
//!
//! ## Stepping only moves the anchor
//!
//! [`handle_key`]'s `n`/`N` arms set `ConversationState.scroll` to
//! `Scroll::At { message_id: hit, line_offset: 0 }` and nothing else. They do
//! not page in history themselves: if the hit is outside the currently
//! loaded window, `conversation.rs`'s own near-top/near-bottom paging logic
//! (architecture §4.6) is what notices and issues the follow-up
//! `GetChatHistory` the next time the anchor is re-derived — exactly as any
//! other deliberate anchor move (`selection` movement, scrollback) already
//! relies on.

use crate::app::AppState;
use crate::effect::Effect;
use crate::model::ids::{ChatId, MessageId};
use crate::model::key::Key;
use crate::state::auth::InputField;
use crate::state::conversation::Scroll;
use crate::state::focus::Focus;
use crate::td::error::TdError;
use crate::td::request::TdRequest;

/// `searchChatMessages` page size (architecture §4.7's `limit: u8`). One
/// page is all v1 fetches — paging further into search results is out of
/// scope (spec §11 only promises stepping between the hits already found).
const SEARCH_PAGE_LIMIT: u8 = 50;

#[derive(Debug, Default)]
pub struct ChatSearchState {
    pub input: InputField,
    /// Index into `ConversationState.search_hits` (`n`/`N` step).
    pub current_hit: usize,
    pub in_flight: bool,
}

/// Opens in-chat search: parks a fresh [`ChatSearchState`] in `AppState`.
/// Called by the router (T45) right after it pushes `Focus::ChatSearch`.
/// Pure bookkeeping — no request goes out until the user submits a query
/// ([`handle_key`]'s `Enter` arm).
pub fn open(app: &mut AppState) {
    app.chat_search = Some(ChatSearchState::default());
}

/// Leaves in-chat search: drops the overlay state and clears the open
/// chat's hit list (which also turns off the highlight, since nothing
/// downstream in `tgt-ui` has hits left to render). Called by the router
/// (T45) after popping `Focus::ChatSearch`.
pub fn close(app: &mut AppState) {
    app.chat_search = None;
    let Some(chat_id) = app.open_chat else {
        return;
    };
    if let Some(convo) = app.conversations.get_mut(&chat_id) {
        convo.search_hits.clear();
    }
}

/// Search-overlay keys, active while `Focus::ChatSearch` is on top of the
/// stack. `n`/`N` only claim the key once the open chat has hits to step
/// through — until then they are ordinary characters, so a query containing
/// the letter n still types correctly.
pub fn handle_key(app: &mut AppState, key: Key) -> Option<Vec<Effect>> {
    if !matches!(app.focus.current(), Focus::ChatSearch) {
        return None;
    }
    let chat_id = app.open_chat?;

    match key {
        Key::Char('n') if has_hits(app, chat_id) => Some(step(app, chat_id, 1)),
        Key::Char('N') if has_hits(app, chat_id) => Some(step(app, chat_id, -1)),
        Key::Char(c) => {
            let search = app.chat_search.as_mut()?;
            search.input.text.insert(search.input.cursor, c);
            search.input.cursor += c.len_utf8();
            Some(Vec::new())
        }
        Key::Backspace => {
            let search = app.chat_search.as_mut()?;
            if search.input.cursor > 0 {
                let prev = prev_char_boundary(&search.input.text, search.input.cursor);
                search
                    .input
                    .text
                    .replace_range(prev..search.input.cursor, "");
                search.input.cursor = prev;
            }
            Some(Vec::new())
        }
        Key::Enter => Some(submit(app, chat_id)),
        _ => None,
    }
}

/// Folds a `searchChatMessages` answer into the open chat's hit list. `Ok`
/// (even an empty one — no matches is a valid answer, not an error) stores
/// the hits verbatim (see module docs) and jumps the anchor to the first
/// one; `Err` just clears `in_flight` so the query box stops showing a
/// spinner. Per-field error display is deferred (architecture §4.3 carries
/// no error detail beyond `TdError` — logging it is the caller's job).
pub fn handle_td_result(
    app: &mut AppState,
    chat_id: ChatId,
    outcome: &Result<Vec<MessageId>, TdError>,
) -> Vec<Effect> {
    if let Some(search) = app.chat_search.as_mut() {
        search.in_flight = false;
    }
    let Ok(hits) = outcome else {
        return Vec::new();
    };
    let Some(convo) = app.conversations.get_mut(&chat_id) else {
        return Vec::new();
    };
    convo.search_hits = hits.clone();
    if let Some(search) = app.chat_search.as_mut() {
        search.current_hit = 0;
    }
    if let Some(&first) = convo.search_hits.first() {
        convo.scroll = Scroll::At {
            message_id: first,
            line_offset: 0,
        };
    }
    Vec::new()
}

fn has_hits(app: &AppState, chat_id: ChatId) -> bool {
    app.conversations
        .get(&chat_id)
        .is_some_and(|convo| !convo.search_hits.is_empty())
}

/// `Enter` with an empty query box is a no-op — nothing to search for, and
/// firing an empty-string `searchChatMessages` would just churn TDLib for a
/// result the user did not ask for.
fn submit(app: &mut AppState, chat_id: ChatId) -> Vec<Effect> {
    let Some(search) = app.chat_search.as_mut() else {
        return Vec::new();
    };
    if search.input.text.is_empty() {
        return Vec::new();
    }
    let query = search.input.text.clone();
    search.in_flight = true;
    vec![Effect::Td(TdRequest::SearchChatMessages {
        chat_id,
        query,
        from_message_id: MessageId(0),
        limit: SEARCH_PAGE_LIMIT,
    })]
}

/// Moves `current_hit` by `delta` with wraparound and drags the open chat's
/// scroll anchor to the newly-current hit. A chat with no hits (search not
/// yet submitted, or answered with none) is a no-op: [`handle_key`] already
/// guards `n`/`N` on [`has_hits`], but this stays defensive for direct
/// callers (this module's own tests).
fn step(app: &mut AppState, chat_id: ChatId, delta: i64) -> Vec<Effect> {
    let len = match app.conversations.get(&chat_id) {
        Some(convo) if !convo.search_hits.is_empty() => convo.search_hits.len() as i64,
        _ => return Vec::new(),
    };
    let Some(search) = app.chat_search.as_mut() else {
        return Vec::new();
    };
    let next_idx = (search.current_hit as i64 + delta).rem_euclid(len) as usize;
    search.current_hit = next_idx;

    let convo = app
        .conversations
        .get_mut(&chat_id)
        .expect("checked non-empty above");
    let hit = convo.search_hits[next_idx];
    convo.scroll = Scroll::At {
        message_id: hit,
        line_offset: 0,
    };
    Vec::new()
}

/// Char-boundary-safe backspace, matching the copy in `auth.rs`/
/// `composer.rs` (each editable-text module keeps its own tiny copy rather
/// than sharing one across disjoint-ownership files).
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::Screen;
    use crate::effect::TelemetryMode;
    use crate::model::ids::UserId;
    use crate::model::message::{MessageCaps, MessageContent, MessageView, SendState, Sender};
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

    const CHAT: ChatId = ChatId(1);

    fn msg(id: i64) -> MessageView {
        MessageView {
            id: MessageId(id),
            chat_id: CHAT,
            sender: Sender::User(UserId(1)),
            sender_name: "Ada".to_string(),
            is_outgoing: false,
            date: 1_700_000_000 + id,
            content: MessageContent::Text(crate::model::entity::FormattedText {
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
            telemetry_salt: [0u8; 32],
            now: Millis(0),
        }
    }

    /// Opens `CHAT` with `messages` loaded, `Focus::ChatSearch` pushed and
    /// [`open`] already called — the state the router (T45) hands every
    /// handler in this module.
    fn with_search_open(messages: Vec<MessageView>) -> AppState {
        let mut app = fixture_state();
        conversation::open(&mut app, CHAT);
        let convo = app.conversations.get_mut(&CHAT).unwrap();
        for m in messages {
            convo.messages.push_back(m);
        }
        convo.paging = crate::state::history::PagingState::Exhausted;
        app.focus = FocusStack::new(Focus::Composer);
        app.focus.push(Focus::ChatSearch);
        open(&mut app);
        app
    }

    fn type_text(app: &mut AppState, text: &str) {
        for c in text.chars() {
            handle_key(app, Key::Char(c));
        }
    }

    #[test]
    fn search_submits_request_and_stores_hits() {
        let mut app = with_search_open(vec![msg(1), msg(2), msg(3)]);
        type_text(&mut app, "hello");

        let effects = handle_key(&mut app, Key::Enter).expect("claimed");
        assert!(app.chat_search.as_ref().unwrap().in_flight);
        match effects.as_slice() {
            [
                Effect::Td(TdRequest::SearchChatMessages {
                    chat_id,
                    query,
                    from_message_id,
                    limit,
                }),
            ] => {
                assert_eq!(*chat_id, CHAT);
                assert_eq!(query, "hello");
                assert_eq!(*from_message_id, MessageId(0));
                assert_eq!(*limit, SEARCH_PAGE_LIMIT);
            }
            other => panic!("expected a single SearchChatMessages effect, got {other:?}"),
        }

        // TDLib answers with the newest-first order it actually uses.
        let hits = vec![MessageId(3), MessageId(2), MessageId(1)];
        let result_effects = handle_td_result(&mut app, CHAT, &Ok(hits.clone()));
        assert!(result_effects.is_empty());
        assert!(!app.chat_search.as_ref().unwrap().in_flight);
        assert_eq!(app.chat_search.as_ref().unwrap().current_hit, 0);
        assert_eq!(app.conversations[&CHAT].search_hits, hits);
        assert_eq!(
            app.conversations[&CHAT].scroll,
            Scroll::At {
                message_id: MessageId(3),
                line_offset: 0,
            }
        );
    }

    #[test]
    fn empty_query_enter_is_a_no_op() {
        let mut app = with_search_open(vec![msg(1)]);
        let effects = handle_key(&mut app, Key::Enter).expect("claimed");
        assert!(effects.is_empty());
        assert!(!app.chat_search.as_ref().unwrap().in_flight);
    }

    #[test]
    fn n_steps_forward_wraps() {
        let mut app = with_search_open(vec![msg(1), msg(2), msg(3)]);
        let hits = vec![MessageId(3), MessageId(2), MessageId(1)];
        handle_td_result(&mut app, CHAT, &Ok(hits));
        assert_eq!(app.chat_search.as_ref().unwrap().current_hit, 0);

        handle_key(&mut app, Key::Char('n'));
        assert_eq!(app.chat_search.as_ref().unwrap().current_hit, 1);
        assert_eq!(
            app.conversations[&CHAT].scroll,
            Scroll::At {
                message_id: MessageId(2),
                line_offset: 0,
            }
        );

        handle_key(&mut app, Key::Char('n'));
        assert_eq!(app.chat_search.as_ref().unwrap().current_hit, 2);

        // Third step wraps back to index 0.
        handle_key(&mut app, Key::Char('n'));
        assert_eq!(app.chat_search.as_ref().unwrap().current_hit, 0);
        assert_eq!(
            app.conversations[&CHAT].scroll,
            Scroll::At {
                message_id: MessageId(3),
                line_offset: 0,
            }
        );
    }

    #[test]
    fn shift_n_steps_back() {
        let mut app = with_search_open(vec![msg(1), msg(2), msg(3)]);
        let hits = vec![MessageId(3), MessageId(2), MessageId(1)];
        handle_td_result(&mut app, CHAT, &Ok(hits));
        assert_eq!(app.chat_search.as_ref().unwrap().current_hit, 0);

        // Stepping back from index 0 wraps to the last hit.
        handle_key(&mut app, Key::Char('N'));
        assert_eq!(app.chat_search.as_ref().unwrap().current_hit, 2);
        assert_eq!(
            app.conversations[&CHAT].scroll,
            Scroll::At {
                message_id: MessageId(1),
                line_offset: 0,
            }
        );

        handle_key(&mut app, Key::Char('N'));
        assert_eq!(app.chat_search.as_ref().unwrap().current_hit, 1);
    }

    #[test]
    fn esc_clears_search_state() {
        let mut app = with_search_open(vec![msg(1), msg(2)]);
        type_text(&mut app, "q");
        let hits = vec![MessageId(2), MessageId(1)];
        handle_td_result(&mut app, CHAT, &Ok(hits));
        assert!(!app.conversations[&CHAT].search_hits.is_empty());

        // Esc is left unclaimed by this module; the router pops the focus
        // and calls `close` (module docs). This test exercises the `close`
        // half of that contract directly.
        assert!(handle_key(&mut app, Key::Esc).is_none());
        close(&mut app);

        assert!(app.chat_search.is_none());
        assert!(app.conversations[&CHAT].search_hits.is_empty());
    }

    #[test]
    fn stepping_with_no_hits_is_a_no_op() {
        let mut app = with_search_open(vec![msg(1)]);
        assert!(app.conversations[&CHAT].search_hits.is_empty());
        let original_scroll = app.conversations[&CHAT].scroll;

        let effects = step(&mut app, CHAT, 1);
        assert!(effects.is_empty());
        assert_eq!(app.chat_search.as_ref().unwrap().current_hit, 0);
        assert_eq!(app.conversations[&CHAT].scroll, original_scroll);

        let effects = step(&mut app, CHAT, -1);
        assert!(effects.is_empty());
        assert_eq!(app.chat_search.as_ref().unwrap().current_hit, 0);
        assert_eq!(app.conversations[&CHAT].scroll, original_scroll);
    }

    #[test]
    fn char_n_types_into_query_before_results_exist() {
        let mut app = with_search_open(vec![msg(1)]);
        // No hits yet: 'n'/'N' are ordinary characters so a query
        // containing the letter n still types correctly.
        type_text(&mut app, "nNo");
        assert_eq!(app.chat_search.as_ref().unwrap().input.text, "nNo");
    }

    #[test]
    fn backspace_edits_the_query() {
        let mut app = with_search_open(vec![msg(1)]);
        type_text(&mut app, "abc");
        handle_key(&mut app, Key::Backspace);
        assert_eq!(app.chat_search.as_ref().unwrap().input.text, "ab");
        assert_eq!(app.chat_search.as_ref().unwrap().input.cursor, 2);
    }

    #[test]
    fn anchor_moves_on_step() {
        let mut app = with_search_open(vec![msg(1), msg(2), msg(3), msg(4)]);
        let hits = vec![MessageId(4), MessageId(1)];
        handle_td_result(&mut app, CHAT, &Ok(hits));
        assert_eq!(
            app.conversations[&CHAT].scroll,
            Scroll::At {
                message_id: MessageId(4),
                line_offset: 0,
            }
        );

        handle_key(&mut app, Key::Char('n'));
        assert_eq!(
            app.conversations[&CHAT].scroll,
            Scroll::At {
                message_id: MessageId(1),
                line_offset: 0,
            }
        );
    }

    #[test]
    fn handle_key_unclaimed_outside_chat_search_focus() {
        let mut app = with_search_open(vec![msg(1)]);
        app.focus = FocusStack::new(Focus::Composer);
        assert!(handle_key(&mut app, Key::Char('x')).is_none());
    }

    #[test]
    fn search_err_clears_in_flight_without_touching_hits() {
        let mut app = with_search_open(vec![msg(1)]);
        type_text(&mut app, "q");
        handle_key(&mut app, Key::Enter);
        assert!(app.chat_search.as_ref().unwrap().in_flight);

        let effects = handle_td_result(&mut app, CHAT, &Err(TdError::NetTimeout));
        assert!(effects.is_empty());
        assert!(!app.chat_search.as_ref().unwrap().in_flight);
        assert!(app.conversations[&CHAT].search_hits.is_empty());
    }
}
