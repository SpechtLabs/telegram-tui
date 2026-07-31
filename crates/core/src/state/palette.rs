//! Command palette state and handlers: `ctrl+p` fuzzy match over chats and
//! commands. See docs/architecture.md §4.6; spec §11.
//!
//! ## nucleo usage
//!
//! Matching runs synchronously inside `update()`, once per keystroke, over a
//! small item set — not the live, debounced, multi-frame filtering the
//! high-level `nucleo::Nucleo` worker (background thread pool, snapshot
//! polling via `tick`) is built for. That worker would be the wrong tool
//! here and would smuggle a background thread into a function that must stay
//! synchronous. Instead this module uses the pieces `nucleo` re-exports from
//! `nucleo_matcher` directly: a plain [`nucleo::Matcher`] plus
//! [`nucleo::pattern::Pattern`]. `Pattern::parse` builds a (possibly
//! multi-atom) pattern from the query text; `Pattern::score` runs it
//! against one haystack and returns `Option<u32>` (`None` = no match). A
//! fresh `Matcher` is constructed per rerank — matchers hold reusable scratch
//! memory meant to be kept across many matches in a tight loop, which is not
//! this module's access pattern, so the simplicity of "new matcher, one
//! pass over the item list" wins over reuse.
//!
//! An empty query parses to zero atoms, and `Pattern::score` special-cases
//! that to `Some(0)` for every haystack — so "list everything" falls out of
//! the exact same ranking code path as a real query, no special-casing
//! needed in this module.
//!
//! ## Ranking
//!
//! Results are ordered by:
//! 1. nucleo match score, descending.
//! 2. On a tied score: chats before commands. Chats are what the palette
//!    exists for; the five commands are a small fixed set the user already
//!    knows how to reach for, so they yield the tie.
//! 3. Within chats: TDLib recency — position in the `Main` list's order set,
//!    which already iterates most-recent-first (`ChatOrderKey`'s `Ord`).
//! 4. Within commands: declaration order (`ToggleTheme`, `TelemetrySettings`,
//!    `SendFile`, `LogOut`, `Quit`).
//!
//! At an empty query every item scores 0, so rules 2-4 alone decide the
//! order: exactly "chats by recency, then all commands" (plan T41).
//!
//! ## Command effects
//!
//! `ToggleTheme`, `TelemetrySettings` and `SendFile` are claimed (the
//! palette closes, an empty effect list is returned) but are no-ops for now:
//! real theme switching needs the theme file/generation machinery T53 adds,
//! a telemetry settings screen doesn't exist before T51, and offering to
//! send a file needs a file browser this milestone doesn't build. `LogOut`
//! and `Quit` are real: they emit `Effect::Td(TdRequest::LogOut)` and
//! `Effect::Quit` respectively.
//!
//! ## Focus-stack contract
//!
//! `handle_key` never touches `app.focus`. On `Esc` (and every other
//! unhandled key) it returns `None` — unclaimed — so the router pops
//! `Focus::Palette` and calls [`close`] itself, the same contract every
//! other pane's `handle_key` follows. On `Enter` it closes the palette
//! itself (`app.palette` goes from `Some` to `None`) and returns the
//! invoked item's effects for the router to dispatch; the router is still
//! the one that pops `Focus::Palette` off the focus stack (T45's job), the
//! same way a modal's focus entry is popped by whoever notices the modal's
//! transient state disappeared.

use nucleo::pattern::{CaseMatching, Normalization, Pattern};
use nucleo::{Config, Matcher, Utf32Str};

use crate::app::AppState;
use crate::effect::Effect;
use crate::model::chat::ChatListId;
use crate::model::ids::{ChatId, MessageId};
use crate::model::key::Key;
use crate::state::auth::InputField;
use crate::state::chat_list::ChatListState;
use crate::state::conversation;
use crate::td::request::TdRequest;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandId {
    ToggleTheme,
    TelemetrySettings,
    SendFile,
    LogOut,
    Quit,
}

#[derive(Debug, Clone, PartialEq)]
pub enum PaletteItem {
    Chat { id: ChatId, score: u32 },
    Command { id: CommandId, score: u32 },
}

#[derive(Debug)]
pub struct PaletteState {
    pub input: InputField,
    /// Ranked by nucleo match score, then chat recency (TDLib order).
    pub results: Vec<PaletteItem>,
    pub selected: usize,
}

/// The fixed command set, in declaration/tie-break order. The `&str` is the
/// label nucleo matches against (spec §11).
const COMMANDS: [(CommandId, &str); 5] = [
    (CommandId::ToggleTheme, "Toggle theme"),
    (CommandId::TelemetrySettings, "Telemetry settings"),
    (CommandId::SendFile, "Send file"),
    (CommandId::LogOut, "Log out"),
    (CommandId::Quit, "Quit"),
];

/// Opens the palette: a fresh empty query, results pre-populated for it (all
/// chats by recency, then every command — see the module docs).
pub fn open(app: &mut AppState) {
    app.palette = Some(PaletteState {
        input: InputField::default(),
        results: Vec::new(),
        selected: 0,
    });
    rerank(app);
}

/// Drops the palette state. Does not touch the focus stack — see the module
/// docs' focus-stack contract.
pub fn close(app: &mut AppState) {
    app.palette = None;
}

/// Active while `app.palette.is_some()` (the router only calls this while
/// `Focus::Palette` is current, which is exactly when the state exists).
pub fn handle_key(app: &mut AppState, key: Key) -> Option<Vec<Effect>> {
    match key {
        Key::Up => {
            move_selected(app, -1);
            Some(Vec::new())
        }
        Key::Down => {
            move_selected(app, 1);
            Some(Vec::new())
        }
        Key::Backspace => {
            edit_input(app, |field| {
                if field.cursor > 0 {
                    let mut idx = field.cursor - 1;
                    while idx > 0 && !field.text.is_char_boundary(idx) {
                        idx -= 1;
                    }
                    field.text.remove(idx);
                    field.cursor = idx;
                }
            });
            rerank(app);
            Some(Vec::new())
        }
        Key::Char(c) => {
            edit_input(app, |field| {
                field.text.insert(field.cursor, c);
                field.cursor += c.len_utf8();
            });
            rerank(app);
            Some(Vec::new())
        }
        Key::Enter => Some(invoke_selected(app)),
        // Esc, and anything else this module doesn't handle, is unclaimed:
        // the router pops the focus stack and calls `close` (module docs).
        _ => None,
    }
}

fn edit_input(app: &mut AppState, f: impl FnOnce(&mut InputField)) {
    if let Some(palette) = app.palette.as_mut() {
        f(&mut palette.input);
    }
}

fn move_selected(app: &mut AppState, delta: i32) {
    let Some(palette) = app.palette.as_mut() else {
        return;
    };
    if palette.results.is_empty() {
        palette.selected = 0;
        return;
    }
    let max = palette.results.len() as i32 - 1;
    palette.selected = (palette.selected as i32 + delta).clamp(0, max) as usize;
}

fn invoke_selected(app: &mut AppState) -> Vec<Effect> {
    let item = app
        .palette
        .as_ref()
        .and_then(|p| p.results.get(p.selected).cloned());
    close(app);
    match item {
        Some(PaletteItem::Chat { id, .. }) => open_chat(app, id),
        Some(PaletteItem::Command { id, .. }) => run_command(id),
        None => Vec::new(),
    }
}

/// Mirrors `chat_list`'s Enter-on-chat effects (architecture §4.6):
/// `conversation::open` does the bookkeeping (ensures a `ConversationState`
/// exists, sets `open_chat`), and the caller issues `OpenChat` plus the
/// first `GetChatHistory` page — the same two effects `chat_list::handle_key`
/// emits on Enter.
fn open_chat(app: &mut AppState, chat_id: ChatId) -> Vec<Effect> {
    conversation::open(app, chat_id);
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

fn run_command(id: CommandId) -> Vec<Effect> {
    match id {
        // T53 real theme switching.
        CommandId::ToggleTheme => Vec::new(),
        // No telemetry settings screen before T51.
        CommandId::TelemetrySettings => Vec::new(),
        // Needs a file browser; deferred (this milestone offers file sends
        // only via the composer's `/send <path>`, T39).
        CommandId::SendFile => Vec::new(),
        CommandId::LogOut => vec![Effect::Td(TdRequest::LogOut)],
        CommandId::Quit => vec![Effect::Quit],
    }
}

/// Recomputes `results` from the current query text and resets `selected`
/// to the top match. See the module docs for the ranking rule.
fn rerank(app: &mut AppState) {
    let query = app
        .palette
        .as_ref()
        .map(|p| p.input.text.clone())
        .unwrap_or_default();
    let results = ranked_results(&app.chat_list, &query);
    if let Some(palette) = app.palette.as_mut() {
        palette.results = results;
        palette.selected = 0;
    }
}

struct Ranked {
    item: PaletteItem,
    score: u32,
    /// 0 = chat, 1 = command: the tie-break applied when scores are equal.
    kind: u8,
    /// Recency rank for chats (0 = most recent); declaration index for
    /// commands.
    tie: usize,
}

fn ranked_results(chat_list: &ChatListState, query: &str) -> Vec<PaletteItem> {
    let mut matcher = Matcher::new(Config::DEFAULT);
    let pattern = Pattern::parse(query, CaseMatching::Ignore, Normalization::Smart);
    let mut buf: Vec<char> = Vec::new();
    let mut ranked = Vec::new();

    if let Some(orders) = chat_list.orders.get(&ChatListId::Main) {
        for (rank, key) in orders.iter().enumerate() {
            let Some(chat) = chat_list.chats.get(&key.chat_id) else {
                continue;
            };
            let haystack = Utf32Str::new(&chat.title, &mut buf);
            if let Some(score) = pattern.score(haystack, &mut matcher) {
                ranked.push(Ranked {
                    item: PaletteItem::Chat {
                        id: key.chat_id,
                        score,
                    },
                    score,
                    kind: 0,
                    tie: rank,
                });
            }
        }
    }

    for (tie, (id, label)) in COMMANDS.iter().enumerate() {
        let haystack = Utf32Str::new(label, &mut buf);
        if let Some(score) = pattern.score(haystack, &mut matcher) {
            ranked.push(Ranked {
                item: PaletteItem::Command { id: *id, score },
                score,
                kind: 1,
                tie,
            });
        }
    }

    ranked.sort_by(|a, b| {
        b.score
            .cmp(&a.score)
            .then(a.kind.cmp(&b.kind))
            .then(a.tie.cmp(&b.tie))
    });
    ranked.into_iter().map(|r| r.item).collect()
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::app::Screen;
    use crate::effect::TelemetryMode;
    use crate::model::chat::{ChatKind, ChatPositionEntry, ChatView};
    use crate::model::time::Millis;
    use crate::state::auth::{AuthField, AuthState};
    use crate::state::chat_list::ChatListState;
    use crate::state::composer::ComposerState;
    use crate::state::consent::{ConsentChoice, ConsentState};
    use crate::state::focus::{Focus, FocusStack};
    use crate::state::media::MediaState;
    use crate::state::presence::PresenceState;
    use crate::state::toasts::ToastState;
    use crate::td::update::{AuthPhase, ConnectionPhase};

    /// Mirrors `App::new`'s construction; every field is `pub` so tests can
    /// build `AppState` directly (same pattern as `state::chat_list`'s
    /// tests).
    fn fixture_state() -> AppState {
        AppState {
            screen: Screen::Main,
            focus: FocusStack::new(Focus::Palette),
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

    fn chat_with_order(id: i64, title: &str, order: i64) -> ChatView {
        ChatView {
            id: ChatId(id),
            kind: ChatKind::Private,
            title: title.to_string(),
            positions: vec![ChatPositionEntry {
                list: ChatListId::Main,
                order,
                is_pinned: false,
            }],
            unread_count: 0,
            unread_mention_count: 0,
            last_message: None,
            is_muted: false,
        }
    }

    fn insert_chat(app: &mut AppState, id: i64, title: &str, order: i64) {
        let chat = chat_with_order(id, title, order);
        app.chat_list.chats.insert(ChatId(id), chat);
        app.chat_list
            .orders
            .entry(ChatListId::Main)
            .or_default()
            .insert(crate::model::chat::ChatOrderKey {
                order,
                chat_id: ChatId(id),
            });
    }

    fn type_str(app: &mut AppState, s: &str) {
        for c in s.chars() {
            handle_key(app, Key::Char(c));
        }
    }

    #[test]
    fn empty_query_lists_chats_by_recency_then_all_commands() {
        let mut app = fixture_state();
        insert_chat(&mut app, 1, "Alice", 10);
        insert_chat(&mut app, 2, "Bob", 50);
        insert_chat(&mut app, 3, "Carol", 30);

        open(&mut app);

        let results = &app.palette.as_ref().unwrap().results;
        assert_eq!(results.len(), 3 + COMMANDS.len());
        // Chats first, most recent (highest order) first.
        assert_eq!(
            results[0],
            PaletteItem::Chat {
                id: ChatId(2),
                score: 0
            }
        );
        assert_eq!(
            results[1],
            PaletteItem::Chat {
                id: ChatId(3),
                score: 0
            }
        );
        assert_eq!(
            results[2],
            PaletteItem::Chat {
                id: ChatId(1),
                score: 0
            }
        );
        // Then every command, in declaration order.
        assert_eq!(
            results[3],
            PaletteItem::Command {
                id: CommandId::ToggleTheme,
                score: 0
            }
        );
        assert_eq!(
            results[7],
            PaletteItem::Command {
                id: CommandId::Quit,
                score: 0
            }
        );
    }

    #[test]
    fn fuzzy_ranks_score_then_recency() {
        let mut app = fixture_state();
        // Both titles match the query "abc" identically (same substring at
        // the same position), so score ties; only recency should separate
        // them.
        insert_chat(&mut app, 1, "abc older", 10);
        insert_chat(&mut app, 2, "abc newer", 50);

        open(&mut app);
        type_str(&mut app, "abc");

        let results = &app.palette.as_ref().unwrap().results;
        let chat_results: Vec<ChatId> = results
            .iter()
            .filter_map(|item| match item {
                PaletteItem::Chat { id, .. } => Some(*id),
                PaletteItem::Command { .. } => None,
            })
            .collect();
        assert_eq!(chat_results, vec![ChatId(2), ChatId(1)]);
    }

    #[test]
    fn commands_and_chats_interleave_by_score() {
        let mut app = fixture_state();
        // Against the query "sf": "SF Group" is a tight contiguous match
        // (higher nucleo score) than the "Send file" command's "S...f"
        // match, which in turn beats "Silly Far away chat"'s wider-gapped
        // match. Empirically verified: SF Group=62 > Send file=56 >
        // Silly Far away chat=55 (nucleo 0.5, `Config::DEFAULT`).
        insert_chat(&mut app, 1, "SF Group", 10);
        insert_chat(&mut app, 2, "Silly Far away chat", 20);

        open(&mut app);
        type_str(&mut app, "sf");

        let results = &app.palette.as_ref().unwrap().results;
        let positions: Vec<&str> = results
            .iter()
            .map(|item| match item {
                PaletteItem::Chat { id, .. } if *id == ChatId(1) => "chat:tight",
                PaletteItem::Chat { id, .. } if *id == ChatId(2) => "chat:loose",
                PaletteItem::Command {
                    id: CommandId::SendFile,
                    ..
                } => "cmd:send_file",
                _ => "other",
            })
            .collect();

        let tight_chat = positions.iter().position(|p| *p == "chat:tight").unwrap();
        let send_file = positions
            .iter()
            .position(|p| *p == "cmd:send_file")
            .unwrap();
        let loose_chat = positions.iter().position(|p| *p == "chat:loose").unwrap();

        // A chat outranks a command, and that same command outranks a
        // *different* chat: proof ranking follows score, not a fixed
        // "all chats, then all commands" (or vice versa) block order.
        assert!(tight_chat < send_file);
        assert!(send_file < loose_chat);
    }

    #[test]
    fn backspace_rematches() {
        let mut app = fixture_state();
        insert_chat(&mut app, 1, "Alice", 10);
        insert_chat(&mut app, 2, "Bob", 20);

        open(&mut app);
        type_str(&mut app, "alicex");
        assert!(
            app.palette
                .as_ref()
                .unwrap()
                .results
                .iter()
                .all(|item| !matches!(item, PaletteItem::Chat { id, .. } if *id == ChatId(1)))
        );

        handle_key(&mut app, Key::Backspace);
        let results = &app.palette.as_ref().unwrap().results;
        assert!(matches!(
            results[0],
            PaletteItem::Chat { id: ChatId(1), .. }
        ));
    }

    #[test]
    fn esc_is_unclaimed() {
        let mut app = fixture_state();
        open(&mut app);
        assert!(handle_key(&mut app, Key::Esc).is_none());
        // Router owns the pop + close; palette state is untouched here.
        assert!(app.palette.is_some());
    }

    #[test]
    fn enter_on_chat_opens_it() {
        let mut app = fixture_state();
        insert_chat(&mut app, 1, "Alice", 10);
        open(&mut app);

        let effects = handle_key(&mut app, Key::Enter).expect("palette claims Enter");
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
        // Enter closes the palette itself; the focus pop is the router's.
        assert!(app.palette.is_none());
    }

    #[test]
    fn enter_on_quit_emits_quit() {
        let mut app = fixture_state();
        open(&mut app);
        type_str(&mut app, "Quit");

        let results = &app.palette.as_ref().unwrap().results;
        assert!(matches!(
            results[0],
            PaletteItem::Command {
                id: CommandId::Quit,
                ..
            }
        ));

        let effects = handle_key(&mut app, Key::Enter).expect("palette claims Enter");
        assert_eq!(effects.len(), 1);
        assert!(matches!(effects[0], Effect::Quit));
        assert!(app.palette.is_none());
    }

    #[test]
    fn enter_on_log_out_emits_td_log_out() {
        let mut app = fixture_state();
        open(&mut app);
        type_str(&mut app, "Log out");

        let results = &app.palette.as_ref().unwrap().results;
        assert!(matches!(
            results[0],
            PaletteItem::Command {
                id: CommandId::LogOut,
                ..
            }
        ));

        let effects = handle_key(&mut app, Key::Enter).expect("palette claims Enter");
        assert_eq!(effects.len(), 1);
        assert!(matches!(effects[0], Effect::Td(TdRequest::LogOut)));
    }

    #[test]
    fn no_op_command_closes_palette_and_emits_nothing() {
        let mut app = fixture_state();
        open(&mut app);
        type_str(&mut app, "Toggle theme");

        let results = &app.palette.as_ref().unwrap().results;
        assert!(matches!(
            results[0],
            PaletteItem::Command {
                id: CommandId::ToggleTheme,
                ..
            }
        ));

        let effects = handle_key(&mut app, Key::Enter).expect("palette claims Enter");
        assert!(effects.is_empty());
        assert!(app.palette.is_none());
    }
}
