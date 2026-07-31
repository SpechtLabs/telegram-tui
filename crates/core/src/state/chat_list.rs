//! Chat list state: mirrors TDLib ordering verbatim. See docs/architecture.md
//! §4.6; spec §5.1.
//!
//! ## Ordering
//!
//! `orders` holds one `BTreeSet<ChatOrderKey>` per chat list. TDLib is the
//! only source of `order: i64` per (chat, list); this module never computes
//! an order, it only adds/removes keys as `TdUpdate`s report them. Sorting
//! falls out of `ChatOrderKey`'s `Ord` impl (order DESC, chat_id DESC) for
//! free — walking the set IS the display order.
//!
//! ## Selection maintenance
//!
//! When the previously selected chat drops out of the visible set (removed
//! from its list, i.e. `order == 0`, or filtered out by `/`), selection
//! lands on the nearest still-visible row: `reconcile_selection` walks the
//! *unfiltered* order set outward from the selected chat's last known
//! position, forward first, then backward, and takes the first row that
//! survives filtering. This keeps the cursor close to where the user left
//! it instead of snapping to the top of the list.

use std::collections::{BTreeSet, HashMap, VecDeque};

use crate::app::AppState;
use crate::effect::Effect;
use crate::model::chat::{ChatListId, ChatOrderKey, ChatPositionEntry, ChatView};
use crate::model::ids::{ChatId, MessageId};
use crate::model::key::Key;
use crate::state::auth::InputField;
use crate::state::conversation::{ConversationState, Scroll};
use crate::state::focus::Focus;
use crate::state::history::PagingState;
use crate::td::request::TdRequest;
use crate::td::update::TdUpdate;

// `ChatListState` below derives `Default`, which requires every field to be
// `Default`. Architecture §4.6's listing gives `ChatLoadPhase` no `#[default]`
// variant; adding one here (Idle, the natural starting phase) is the minimal
// deviation that makes the derive on ChatListState compile.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ChatLoadPhase {
    #[default]
    Idle,
    Loading,
    Complete,
}

#[derive(Debug, Default)]
pub struct ChatListState {
    pub chats: HashMap<ChatId, ChatView>,
    /// One TDLib-mirrored order set per chat list. Never computed locally.
    pub orders: HashMap<ChatListId, BTreeSet<ChatOrderKey>>,
    pub active_list: ChatListId,
    pub selected: Option<ChatId>,
    pub filter: Option<InputField>,
    pub scroll_offset: usize,
    pub load: ChatLoadPhase,
}

// ChatListId is a foreign type (owned by T02's model/chat.rs, not touched
// here); its derive list can't gain `Default`/`#[default]`, so this stays a
// manual impl per architecture §4.6.
#[allow(clippy::derivable_impls)]
impl Default for ChatListId {
    fn default() -> Self {
        ChatListId::Main
    }
}

/// Projects a pre-digested TDLib update into the chat table and order sets.
/// Never computes an order: `ChatPosition`/`ChatLastMessage` carry it
/// verbatim from TDLib, and `order == 0` means "remove from that list".
pub fn handle_td(app: &mut AppState, upd: &TdUpdate) -> Vec<Effect> {
    match upd {
        TdUpdate::NewChat(chat) => {
            let chat_id = chat.id;
            let positions = chat.positions.clone();
            let mut chat = chat.clone();
            chat.positions = Vec::new();
            app.chat_list.chats.insert(chat_id, chat);
            for position in positions {
                apply_position(app, chat_id, position);
            }
        }
        TdUpdate::ChatPosition { chat_id, position } => {
            apply_position(app, *chat_id, *position);
        }
        TdUpdate::ChatLastMessage {
            chat_id,
            preview,
            positions,
        } => {
            if let Some(chat) = app.chat_list.chats.get_mut(chat_id) {
                chat.last_message = preview.clone();
            }
            for position in positions {
                apply_position(app, *chat_id, *position);
            }
        }
        TdUpdate::ChatReadInbox {
            chat_id,
            unread_count,
            ..
        } => {
            // `last_read_inbox_message_id` is conversation-window state
            // (T16's `ConversationState.last_read_inbox`); only the badge
            // count is a chat_list concern.
            if let Some(chat) = app.chat_list.chats.get_mut(chat_id) {
                chat.unread_count = *unread_count;
            }
        }
        TdUpdate::ChatTitle { chat_id, title } => {
            if let Some(chat) = app.chat_list.chats.get_mut(chat_id) {
                chat.title = title.clone();
            }
        }
        TdUpdate::ChatUnreadMentionCount { chat_id, count } => {
            if let Some(chat) = app.chat_list.chats.get_mut(chat_id) {
                chat.unread_mention_count = *count;
            }
        }
        TdUpdate::ChatNotificationSettings { chat_id, muted } => {
            if let Some(chat) = app.chat_list.chats.get_mut(chat_id) {
                chat.is_muted = *muted;
            }
        }
        // ChatReadOutbox is T16's (read-receipt marker on the conversation);
        // everything else (messages, files, presence, auth, connection) is
        // out of scope for this module. Unknown → empty, per contract.
        _ => {}
    }
    reconcile_selection(app);
    Vec::new()
}

/// Removes the (chat, list)'s old order key (if any) and inserts the new one
/// unless `position.order == 0`, which means "absent from this list".
/// Mirrors TDLib exactly: the order value itself is never computed here.
fn apply_position(app: &mut AppState, chat_id: ChatId, position: ChatPositionEntry) {
    let old = app
        .chat_list
        .chats
        .get(&chat_id)
        .and_then(|c| c.positions.iter().find(|p| p.list == position.list))
        .copied();

    if let Some(old) = old
        && let Some(set) = app.chat_list.orders.get_mut(&position.list)
    {
        set.remove(&ChatOrderKey {
            order: old.order,
            chat_id,
        });
    }

    if position.order != 0 {
        app.chat_list
            .orders
            .entry(position.list)
            .or_default()
            .insert(ChatOrderKey {
                order: position.order,
                chat_id,
            });
    }

    if let Some(chat) = app.chat_list.chats.get_mut(&chat_id) {
        chat.positions.retain(|p| p.list != position.list);
        if position.order != 0 {
            chat.positions.push(position);
        }
    }
}

/// Keeps `selected` on a visible row. No-op while the current selection is
/// still visible (or unset); see the module docs for the "nearest surviving
/// row" rule.
fn reconcile_selection(app: &mut AppState) {
    let Some(sel) = app.chat_list.selected else {
        return;
    };
    let visible = visible_rows(&app.chat_list);
    if visible.contains(&sel) {
        return;
    }
    if visible.is_empty() {
        app.chat_list.selected = None;
        return;
    }

    let full_order: Vec<ChatId> = app
        .chat_list
        .orders
        .get(&app.chat_list.active_list)
        .into_iter()
        .flatten()
        .map(|k| k.chat_id)
        .collect();
    let anchor = full_order.iter().position(|&id| id == sel).unwrap_or(0);

    let nearest = full_order[anchor..]
        .iter()
        .find(|id| visible.contains(id))
        .or_else(|| {
            full_order[..anchor]
                .iter()
                .rev()
                .find(|id| visible.contains(id))
        })
        .copied();

    app.chat_list.selected = nearest.or_else(|| visible.first().copied());
}

/// Walks the active list's order set (already sorted by TDLib order; never
/// reordered here) and keeps rows whose title matches the filter
/// case-insensitively.
pub fn visible_rows(list: &ChatListState) -> Vec<ChatId> {
    let filter_lower = list.filter.as_ref().map(|f| f.text.to_lowercase());
    list.orders
        .get(&list.active_list)
        .into_iter()
        .flatten()
        .filter_map(|key| {
            let chat = list.chats.get(&key.chat_id)?;
            let visible = match &filter_lower {
                Some(f) => chat.title.to_lowercase().contains(f.as_str()),
                None => true,
            };
            visible.then_some(key.chat_id)
        })
        .collect()
}

/// Active while focus is `ChatList` or `ChatFilter` (spec §6.2 routing:
/// this handler decides for itself, callers just check the return value).
pub fn handle_key(app: &mut AppState, key: Key) -> Option<Vec<Effect>> {
    match app.focus.current() {
        Focus::ChatList => handle_key_chat_list(app, key),
        Focus::ChatFilter => handle_key_chat_filter(app, key),
        _ => None,
    }
}

fn handle_key_chat_list(app: &mut AppState, key: Key) -> Option<Vec<Effect>> {
    match key {
        Key::Up => {
            move_selection(app, -1);
            Some(Vec::new())
        }
        Key::Down => {
            move_selection(app, 1);
            Some(Vec::new())
        }
        Key::Enter => Some(open_selected(app)),
        Key::Char('/') => {
            app.chat_list.filter = Some(InputField::default());
            app.focus.push(Focus::ChatFilter);
            Some(Vec::new())
        }
        _ => None,
    }
}

fn handle_key_chat_filter(app: &mut AppState, key: Key) -> Option<Vec<Effect>> {
    match key {
        // The focus stack pop is the router's job (architecture §4.5); this
        // handler only owns the filter text and list navigation.
        Key::Esc => None,
        Key::Enter => {
            app.focus.pop();
            Some(Vec::new())
        }
        Key::Up => {
            move_selection(app, -1);
            Some(Vec::new())
        }
        Key::Down => {
            move_selection(app, 1);
            Some(Vec::new())
        }
        Key::Char(c) => {
            edit_filter(app, |f| {
                f.text.insert(f.cursor, c);
                f.cursor += c.len_utf8();
            });
            reconcile_selection(app);
            Some(Vec::new())
        }
        Key::Backspace => {
            edit_filter(app, |f| {
                if f.cursor > 0 {
                    let mut idx = f.cursor - 1;
                    while idx > 0 && !f.text.is_char_boundary(idx) {
                        idx -= 1;
                    }
                    f.text.remove(idx);
                    f.cursor = idx;
                }
            });
            reconcile_selection(app);
            Some(Vec::new())
        }
        _ => None,
    }
}

fn edit_filter(app: &mut AppState, f: impl FnOnce(&mut InputField)) {
    if let Some(filter) = app.chat_list.filter.as_mut() {
        f(filter);
    }
}

fn move_selection(app: &mut AppState, delta: i32) {
    let rows = visible_rows(&app.chat_list);
    if rows.is_empty() {
        app.chat_list.selected = None;
        return;
    }
    let current = app
        .chat_list
        .selected
        .and_then(|id| rows.iter().position(|&r| r == id));
    let new_idx = match current {
        Some(idx) => (idx as i32 + delta).clamp(0, rows.len() as i32 - 1) as usize,
        None => 0,
    };
    app.chat_list.selected = Some(rows[new_idx]);
}

/// Enter on a selected row: opens the chat and requests the newest page of
/// history. `from_message_id: MessageId(0)` is TDLib's "from the latest
/// message" sentinel.
fn open_selected(app: &mut AppState) -> Vec<Effect> {
    let Some(chat_id) = app.chat_list.selected else {
        return Vec::new();
    };
    app.open_chat = Some(chat_id);
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
        });
    vec![
        Effect::Td(TdRequest::OpenChat { chat_id }),
        Effect::Td(TdRequest::GetChatHistory {
            chat_id,
            from_message_id: MessageId(0),
            limit: 50,
            only_local: false,
        }),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::Screen;
    use crate::effect::TelemetryMode;
    use crate::model::chat::ChatKind;
    use crate::model::time::Millis;
    use crate::state::auth::{AuthField, AuthState};
    use crate::state::composer::ComposerState;
    use crate::state::consent::{ConsentChoice, ConsentState};
    use crate::state::focus::FocusStack;
    use crate::state::media::MediaState;
    use crate::state::presence::PresenceState;
    use crate::state::toasts::ToastState;
    use crate::td::update::{AuthPhase, ConnectionPhase};

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

    fn chat(id: i64, title: &str) -> ChatView {
        ChatView {
            id: ChatId(id),
            kind: ChatKind::Private,
            title: title.to_string(),
            positions: Vec::new(),
            unread_count: 0,
            unread_mention_count: 0,
            last_message: None,
            is_muted: false,
        }
    }

    fn new_chat_with_order(id: i64, title: &str, order: i64) -> TdUpdate {
        let mut c = chat(id, title);
        c.positions = vec![ChatPositionEntry {
            list: ChatListId::Main,
            order,
            is_pinned: false,
        }];
        TdUpdate::NewChat(c)
    }

    #[test]
    fn position_update_reorders_without_local_computation() {
        let mut app = fixture_state();
        handle_td(&mut app, &new_chat_with_order(1, "Alice", 10));
        handle_td(&mut app, &new_chat_with_order(2, "Bob", -5));
        handle_td(&mut app, &new_chat_with_order(3, "Carol", 100));
        handle_td(&mut app, &new_chat_with_order(7, "Dave", 100));

        assert_eq!(
            visible_rows(&app.chat_list),
            vec![ChatId(7), ChatId(3), ChatId(1), ChatId(2)]
        );

        // Permute: move Alice above the tied pair, Bob to a positive order,
        // Carol/Dave stay tied (still broken by chat_id DESC).
        handle_td(
            &mut app,
            &TdUpdate::ChatPosition {
                chat_id: ChatId(1),
                position: ChatPositionEntry {
                    list: ChatListId::Main,
                    order: 200,
                    is_pinned: false,
                },
            },
        );
        handle_td(
            &mut app,
            &TdUpdate::ChatPosition {
                chat_id: ChatId(2),
                position: ChatPositionEntry {
                    list: ChatListId::Main,
                    order: 50,
                    is_pinned: false,
                },
            },
        );

        assert_eq!(
            visible_rows(&app.chat_list),
            vec![ChatId(1), ChatId(7), ChatId(3), ChatId(2)]
        );
    }

    #[test]
    fn order_zero_removes_from_list() {
        let mut app = fixture_state();
        handle_td(&mut app, &new_chat_with_order(1, "Alice", 10));
        handle_td(&mut app, &new_chat_with_order(2, "Bob", 5));
        assert_eq!(visible_rows(&app.chat_list), vec![ChatId(1), ChatId(2)]);

        handle_td(
            &mut app,
            &TdUpdate::ChatPosition {
                chat_id: ChatId(1),
                position: ChatPositionEntry {
                    list: ChatListId::Main,
                    order: 0,
                    is_pinned: false,
                },
            },
        );

        assert_eq!(visible_rows(&app.chat_list), vec![ChatId(2)]);
        // The chat itself stays known (title, unread count, ...); it just
        // has no position entry for Main any more.
        assert!(
            app.chat_list
                .chats
                .get(&ChatId(1))
                .unwrap()
                .positions
                .is_empty()
        );
    }

    #[test]
    fn enter_opens_selected_chat_and_emits_open_chat() {
        let mut app = fixture_state();
        handle_td(&mut app, &new_chat_with_order(1, "Alice", 10));
        app.chat_list.selected = Some(ChatId(1));

        let effects = handle_key(&mut app, Key::Enter).expect("chat list claims Enter");
        assert_eq!(app.open_chat, Some(ChatId(1)));
        assert!(app.conversations.contains_key(&ChatId(1)));
        assert_eq!(effects.len(), 2);
        assert!(matches!(
            effects[0],
            Effect::Td(TdRequest::OpenChat { chat_id: ChatId(1) })
        ));
        assert!(matches!(
            effects[1],
            Effect::Td(TdRequest::GetChatHistory {
                chat_id: ChatId(1),
                from_message_id: MessageId(0),
                limit: 50,
                only_local: false,
            })
        ));
    }

    #[test]
    fn filter_narrows_without_reordering() {
        let mut app = fixture_state();
        handle_td(&mut app, &new_chat_with_order(1, "Alice", 30));
        handle_td(&mut app, &new_chat_with_order(2, "Bob", 20));
        handle_td(&mut app, &new_chat_with_order(3, "Alicia", 10));
        assert_eq!(
            visible_rows(&app.chat_list),
            vec![ChatId(1), ChatId(2), ChatId(3)]
        );

        handle_key(&mut app, Key::Char('/'));
        assert_eq!(app.focus.current(), &Focus::ChatFilter);
        handle_key(&mut app, Key::Char('a'));
        handle_key(&mut app, Key::Char('l'));
        handle_key(&mut app, Key::Char('i'));

        // Case-insensitive substring match, TDLib order preserved (30 before 10).
        assert_eq!(visible_rows(&app.chat_list), vec![ChatId(1), ChatId(3)]);

        // The underlying order set is untouched by filtering.
        assert_eq!(
            app.chat_list
                .orders
                .get(&ChatListId::Main)
                .unwrap()
                .iter()
                .map(|k| k.chat_id)
                .collect::<Vec<_>>(),
            vec![ChatId(1), ChatId(2), ChatId(3)]
        );
    }

    #[test]
    fn read_inbox_clears_badge() {
        let mut app = fixture_state();
        let mut c = chat(1, "Alice");
        c.unread_count = 5;
        c.positions = vec![ChatPositionEntry {
            list: ChatListId::Main,
            order: 10,
            is_pinned: false,
        }];
        handle_td(&mut app, &TdUpdate::NewChat(c));

        handle_td(
            &mut app,
            &TdUpdate::ChatReadInbox {
                chat_id: ChatId(1),
                last_read_inbox_message_id: MessageId(42),
                unread_count: 0,
            },
        );

        assert_eq!(app.chat_list.chats.get(&ChatId(1)).unwrap().unread_count, 0);
    }

    #[test]
    fn up_down_move_selection_clamped_at_ends() {
        let mut app = fixture_state();
        handle_td(&mut app, &new_chat_with_order(1, "Alice", 30));
        handle_td(&mut app, &new_chat_with_order(2, "Bob", 20));
        app.chat_list.selected = Some(ChatId(1));

        handle_key(&mut app, Key::Down);
        assert_eq!(app.chat_list.selected, Some(ChatId(2)));
        // At the bottom already: Down does not fall off the end.
        handle_key(&mut app, Key::Down);
        assert_eq!(app.chat_list.selected, Some(ChatId(2)));

        handle_key(&mut app, Key::Up);
        assert_eq!(app.chat_list.selected, Some(ChatId(1)));
        // At the top already: Up does not fall off the end.
        handle_key(&mut app, Key::Up);
        assert_eq!(app.chat_list.selected, Some(ChatId(1)));
    }

    #[test]
    fn esc_is_unclaimed_in_chat_filter_router_owns_pop() {
        let mut app = fixture_state();
        app.chat_list.filter = Some(InputField::default());
        app.focus.push(Focus::ChatFilter);

        assert!(handle_key(&mut app, Key::Esc).is_none());
        assert_eq!(app.focus.current(), &Focus::ChatFilter);
    }

    #[test]
    fn enter_in_filter_returns_to_chat_list_keeping_filter() {
        let mut app = fixture_state();
        handle_td(&mut app, &new_chat_with_order(1, "Alice", 10));
        handle_key(&mut app, Key::Char('/'));
        handle_key(&mut app, Key::Char('a'));

        let effects = handle_key(&mut app, Key::Enter).expect("filter claims Enter");
        assert!(effects.is_empty());
        assert_eq!(app.focus.current(), &Focus::ChatList);
        assert_eq!(app.chat_list.filter.as_ref().unwrap().text, "a");
    }

    #[test]
    fn selection_moves_to_nearest_surviving_row_when_removed() {
        let mut app = fixture_state();
        handle_td(&mut app, &new_chat_with_order(1, "Alice", 30));
        handle_td(&mut app, &new_chat_with_order(2, "Bob", 20));
        handle_td(&mut app, &new_chat_with_order(3, "Carol", 10));
        app.chat_list.selected = Some(ChatId(2));

        // Remove the selected chat from the list entirely.
        handle_td(
            &mut app,
            &TdUpdate::ChatPosition {
                chat_id: ChatId(2),
                position: ChatPositionEntry {
                    list: ChatListId::Main,
                    order: 0,
                    is_pinned: false,
                },
            },
        );

        // Selection lands on a surviving row rather than becoming invalid.
        let selected = app.chat_list.selected.expect("a surviving row is picked");
        assert!(visible_rows(&app.chat_list).contains(&selected));
    }
}
