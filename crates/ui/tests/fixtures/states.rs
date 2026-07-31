//! Composable `AppState` fixture builders for the frame-snapshot suite
//! (spec §15.3, plan.md T55).
//!
//! Every builder takes a value and returns one, so a snapshot test composes
//! only the steps its scenario needs and reads as a short sequence of named
//! steps rather than a restatement of every field `AppState` happens to
//! have (mirroring the `fixture_state()` helpers already scattered across
//! `crates/ui/src/view/*.rs` and `crates/core/src/state/*.rs` — this module
//! exists so `tests/snapshots.rs` doesn't have to duplicate them). Broader
//! than any single test needs on purpose: this is a shared fixture library,
//! not a single call site.
#![allow(dead_code)]

use std::collections::{BTreeSet, HashMap};

use tgt_core::app::{AppState, Screen};
use tgt_core::effect::TelemetryMode;
use tgt_core::model::chat::{ChatKind, ChatListId, ChatOrderKey, ChatPositionEntry, ChatView};
use tgt_core::model::chips::Chip;
use tgt_core::model::entity::FormattedText;
use tgt_core::model::ids::{ChatId, FileId, MessageId, UserId};
use tgt_core::model::key::KeyBindings;
use tgt_core::model::message::{
    FileSnapshot, MessageCaps, MessageContent, MessageView, ReactionView, SendState, Sender,
};
use tgt_core::model::time::Millis;
use tgt_core::state::auth::{AuthField, AuthState, FieldError, InputField, LoginMethod};
use tgt_core::state::chat_list::{ChatListState, ChatLoadPhase};
use tgt_core::state::composer::ComposerState;
use tgt_core::state::consent::{ConsentChoice, ConsentState};
use tgt_core::state::conversation::{ConversationState, Scroll};
use tgt_core::state::focus::{Focus, FocusStack, ModalKind};
use tgt_core::state::history::PagingState;
use tgt_core::state::media::MediaState;
use tgt_core::state::modal::ModalState;
use tgt_core::state::palette::PaletteState;
use tgt_core::state::presence::PresenceState;
use tgt_core::state::search::ChatSearchState;
use tgt_core::state::selection::SelectionState;
use tgt_core::state::toasts::{Toast, ToastState};
use tgt_core::td::update::{AuthPhase, ConnectionPhase};

/// A fixed instant so no scenario ever depends on the wall clock (T55's
/// brief: "fixed deterministic content everywhere — no clocks").
pub const NOW: Millis = Millis(1_000_000);

/// Unix-seconds baseline `text_message`/`doc_message` offsets are added to;
/// arbitrary but fixed, matching the convention in
/// `crates/ui/src/view/conversation.rs`'s own tests.
const BASE_DATE: i64 = 1_700_000_000;

pub const MAIN_CHAT: ChatId = ChatId(1);

// --- top-level screens ----------------------------------------------------

/// `Screen::Main`, chat list populated with a few named chats, nothing
/// open, focus on the chat list, 120x40. Every chat-list/conversation
/// snapshot starts here and narrows with the `with_*` builders below.
pub fn base_main_state() -> AppState {
    AppState {
        screen: Screen::Main,
        focus: FocusStack::new(Focus::ChatList),
        connection: ConnectionPhase::Ready,
        consent: ConsentState {
            selected: ConsentChoice::Enable,
            acknowledged: true,
        },
        auth: default_auth_state(),
        chat_list: seeded_chat_list(),
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
        bindings: KeyBindings::default(),
        telemetry_mode: TelemetryMode::Off,
        // A build with a crash-reporting endpoint compiled in, which is what
        // a release is. The other branch of that copy — every build from
        // source — has its own snapshot in `view/consent.rs`.
        crash_reports_available: true,
        telemetry_salt: [0u8; 32],
        now: NOW,
    }
}

/// `Screen::Auth`, `WaitTdlibParameters` — the wizard's idle default.
/// Compose with [`with_auth_phase`] / [`with_auth_method`] /
/// [`with_auth_field`] for a specific wizard screen.
pub fn base_auth_state() -> AppState {
    AppState {
        screen: Screen::Auth,
        auth: AuthState {
            phase: AuthPhase::WaitTdlibParameters,
            ..default_auth_state()
        },
        ..base_main_state()
    }
}

/// `Screen::Consent`, unacknowledged, at the size its own view test uses
/// (100x30) — override with [`with_size`] if a scenario needs another.
pub fn base_consent_state(choice: ConsentChoice) -> AppState {
    AppState {
        screen: Screen::Consent,
        connection: ConnectionPhase::WaitingForNetwork,
        consent: ConsentState {
            selected: choice,
            acknowledged: false,
        },
        width: 100,
        height: 30,
        ..base_main_state()
    }
}

fn default_auth_state() -> AuthState {
    AuthState {
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
    }
}

// --- generic knobs ---------------------------------------------------------

pub fn with_size(mut state: AppState, width: u16, height: u16) -> AppState {
    state.width = width;
    state.height = height;
    state
}

pub fn with_focus(mut state: AppState, focus: FocusStack) -> AppState {
    state.focus = focus;
    state
}

pub fn with_chat_list(mut state: AppState, chat_list: ChatListState) -> AppState {
    state.chat_list = chat_list;
    state
}

// --- auth wizard ------------------------------------------------------------

pub fn with_auth_phase(mut state: AppState, phase: AuthPhase) -> AppState {
    state.auth.phase = phase;
    state
}

pub fn with_auth_method(mut state: AppState, method: Option<LoginMethod>) -> AppState {
    state.auth.method = method;
    state
}

/// Sets the active input field's text/cursor and, optionally, a validation
/// error pinned to it — the shape `crates/ui/src/view/auth.rs`'s own tests
/// use for the code-entry-with-error screen.
pub fn with_auth_field(
    mut state: AppState,
    active: AuthField,
    text: &str,
    cursor: usize,
    error: Option<FieldError>,
) -> AppState {
    let field = InputField {
        text: text.to_string(),
        cursor,
    };
    match active {
        AuthField::ApiId => state.auth.api_id = field,
        AuthField::ApiHash => state.auth.api_hash = field,
        AuthField::Phone => state.auth.phone = field,
        AuthField::Code => state.auth.code = field,
        AuthField::Password => state.auth.password = field,
    }
    state.auth.active_field = active;
    state.auth.field_error = error;
    state
}

// --- chat list ---------------------------------------------------------------

/// The general chat-row builder every seeded list is built from: one
/// position (`list`/`order`/`pinned`), the badge/mute inputs the sidebar
/// view actually renders.
#[allow(clippy::too_many_arguments)]
fn chat_view_in(
    id: i64,
    title: &str,
    list: ChatListId,
    order: i64,
    pinned: bool,
    unread: u32,
    mention: u32,
    muted: bool,
) -> ChatView {
    ChatView {
        id: ChatId(id),
        kind: ChatKind::Private,
        title: title.to_string(),
        positions: vec![ChatPositionEntry {
            list,
            order,
            is_pinned: pinned,
        }],
        unread_count: unread,
        unread_mention_count: mention,
        last_message: None,
        is_muted: muted,
    }
}

/// Unpinned Main-list chat, the common case.
fn chat_view(id: i64, title: &str, order: i64, unread: u32) -> ChatView {
    chat_view_in(id, title, ChatListId::Main, order, false, unread, 0, false)
}

fn chat_list_from(
    chats: Vec<ChatView>,
    active: ChatListId,
    selected: Option<ChatId>,
) -> ChatListState {
    let mut chat_map = HashMap::new();
    let mut orders: HashMap<ChatListId, BTreeSet<ChatOrderKey>> = HashMap::new();
    for chat in chats {
        for pos in &chat.positions {
            orders.entry(pos.list).or_default().insert(ChatOrderKey {
                order: pos.order,
                chat_id: chat.id,
            });
        }
        chat_map.insert(chat.id, chat);
    }
    ChatListState {
        chats: chat_map,
        orders,
        active_list: active,
        selected,
        filter: None,
        scroll_offset: 0,
        load: ChatLoadPhase::Complete,
        folder_titles: HashMap::new(),
    }
}

/// Three named chats, `MAIN_CHAT` ("Alice Müller") selected — the default
/// sidebar content for scenarios that aren't specifically about sidebar
/// organization.
pub fn seeded_chat_list() -> ChatListState {
    chat_list_from(
        vec![
            chat_view(1, "Alice Müller", 900, 2),
            chat_view(2, "Team Rust", 800, 9),
            chat_view(3, "Bob", 700, 0),
        ],
        ChatListId::Main,
        Some(MAIN_CHAT),
    )
}

/// Two pinned + two unpinned Main chats, one archived chat, and two
/// folders — everything the pinned/archive/folder-tab sidebar rendering
/// needs, all visible at once with `active_list == Main`. Flip
/// `active_list`/`selected` on the result to exercise the archive view.
pub fn sidebar_chat_list() -> ChatListState {
    chat_list_from(
        vec![
            chat_view_in(1, "Alice Müller", ChatListId::Main, 900, true, 2, 0, false),
            chat_view_in(2, "Boss", ChatListId::Main, 890, true, 0, 0, false),
            chat_view_in(3, "Team Rust", ChatListId::Main, 800, false, 9, 0, false),
            chat_view_in(4, "Mom", ChatListId::Main, 790, false, 0, 0, false),
            chat_view_in(5, "Old Chat", ChatListId::Archive, 500, false, 12, 0, false),
            chat_view_in(
                6,
                "Work Chat",
                ChatListId::Folder(1),
                700,
                false,
                0,
                0,
                false,
            ),
            chat_view_in(
                7,
                "News Chat",
                ChatListId::Folder(2),
                600,
                false,
                3,
                0,
                false,
            ),
        ],
        ChatListId::Main,
        Some(ChatId(1)),
    )
}

// --- conversation / messages --------------------------------------------

pub fn text_message(
    id: i64,
    chat_id: ChatId,
    sender: Sender,
    sender_name: &str,
    outgoing: bool,
    date_offset: i64,
    text: &str,
) -> MessageView {
    MessageView {
        id: MessageId(id),
        chat_id,
        sender,
        sender_name: sender_name.to_string(),
        is_outgoing: outgoing,
        date: BASE_DATE + date_offset,
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

#[allow(clippy::too_many_arguments)]
pub fn doc_message(
    id: i64,
    chat_id: ChatId,
    sender: Sender,
    sender_name: &str,
    date_offset: i64,
    file_id: FileId,
    file_name: &str,
    size: u64,
) -> MessageView {
    MessageView {
        id: MessageId(id),
        chat_id,
        sender,
        sender_name: sender_name.to_string(),
        is_outgoing: false,
        date: BASE_DATE + date_offset,
        content: MessageContent::Document {
            file_id,
            file_name: file_name.to_string(),
            size,
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

pub fn with_reactions(mut msg: MessageView, reactions: Vec<ReactionView>) -> MessageView {
    msg.reactions = reactions;
    msg
}

/// Nine messages spanning three senders and a file attachment — the
/// dialogue every "chat list + conversation" scenario opens, its text
/// deliberately mentioning "the PR" so the in-chat-search fixtures have
/// something thematically real to search for.
pub fn sample_history(chat_id: ChatId) -> Vec<MessageView> {
    let alice = Sender::User(UserId(1));
    let bob = Sender::User(UserId(2));
    let me = Sender::User(UserId(3));
    vec![
        text_message(
            1,
            chat_id,
            alice,
            "Alice",
            false,
            0,
            "hey, did you see the PR?",
        ),
        text_message(
            2,
            chat_id,
            alice,
            "Alice",
            false,
            60,
            "also CI is red on main",
        ),
        text_message(3, chat_id, me, "You", true, 120, "yeah, reviewing it now"),
        text_message(
            4,
            chat_id,
            alice,
            "Alice",
            false,
            300,
            "take your time, no rush",
        ),
        text_message(5, chat_id, bob, "Bob", false, 500, "hey team"),
        text_message(6, chat_id, me, "You", true, 560, "hi bob"),
        doc_message(
            7,
            chat_id,
            bob,
            "Bob",
            620,
            FileId(7),
            "architecture.pdf",
            2_516_582,
        ),
        text_message(8, chat_id, bob, "Bob", false, 630, "take a look"),
        text_message(9, chat_id, me, "You", true, 900, "will do"),
    ]
}

pub fn conversation_with(
    chat_id: ChatId,
    messages: Vec<MessageView>,
    scroll: Scroll,
) -> ConversationState {
    ConversationState {
        chat_id,
        messages: messages.into_iter().collect(),
        paging: PagingState::Idle,
        scroll,
        revealed_spoilers: BTreeSet::new(),
        last_read_inbox: MessageId(0),
        last_read_outbox: MessageId(0),
        pending_view: None,
        search_hits: Vec::new(),
        selection: None,
    }
}

pub fn with_selection(
    mut convo: ConversationState,
    message_id: MessageId,
    chips: Vec<Chip>,
) -> ConversationState {
    convo.selection = Some(SelectionState {
        message_id,
        chips,
        chip_cursor: 0,
        chip_scroll: 0,
    });
    convo
}

pub fn with_search_hits(mut convo: ConversationState, hits: Vec<MessageId>) -> ConversationState {
    convo.search_hits = hits;
    convo
}

pub fn with_last_read_outbox(
    mut convo: ConversationState,
    message_id: MessageId,
) -> ConversationState {
    convo.last_read_outbox = message_id;
    convo
}

/// Inserts `convo` under `chat_id` and opens it — the one step every
/// "a chat is open" scenario shares.
pub fn with_open_chat(mut state: AppState, chat_id: ChatId, convo: ConversationState) -> AppState {
    state.conversations.insert(chat_id, convo);
    state.open_chat = Some(chat_id);
    state
}

// --- modal ------------------------------------------------------------------

/// Pushes `Focus::Modal(ConfirmDelete)` and parks the matching
/// `ModalState` — the pairing `view::modal::draw` requires (module docs on
/// `AppState::modal_ui`).
pub fn with_delete_modal(
    mut state: AppState,
    chat_id: ChatId,
    message_id: MessageId,
    can_revoke: bool,
    cursor: usize,
) -> AppState {
    state.focus.push(Focus::Modal(ModalKind::ConfirmDelete {
        chat_id,
        message_id,
        can_revoke,
    }));
    state.modal_ui = Some(ModalState { cursor });
    state
}

// --- palette ------------------------------------------------------------------

/// Pushes `Focus::Palette` and parks a populated `PaletteState`. Result
/// titles are looked up from `state.chat_list` at render time, and match
/// highlighting is recomputed fresh from `query` — nothing about it lives
/// in `PaletteItem` itself (`crates/ui/src/view/palette.rs`).
pub fn with_palette(
    mut state: AppState,
    query: &str,
    cursor: usize,
    results: Vec<tgt_core::state::palette::PaletteItem>,
    selected: usize,
) -> AppState {
    state.focus.push(Focus::Palette);
    state.palette = Some(PaletteState {
        input: InputField {
            text: query.to_string(),
            cursor,
        },
        results,
        selected,
    });
    state
}

// --- in-chat search -----------------------------------------------------

/// Pushes `Focus::ChatSearch` and parks the query bar's state. Hits
/// themselves live on `ConversationState.search_hits` — set those with
/// [`with_search_hits`] on the conversation before opening it.
pub fn with_chat_search(
    mut state: AppState,
    query: &str,
    cursor: usize,
    current_hit: usize,
) -> AppState {
    state.focus.push(Focus::ChatSearch);
    state.chat_search = Some(ChatSearchState {
        input: InputField {
            text: query.to_string(),
            cursor,
        },
        current_hit,
        in_flight: false,
    });
    state
}

// --- toasts ------------------------------------------------------------------

pub fn toast(chat_id: ChatId, title: &str, body: &str, expires_at_ms: u64) -> Toast {
    Toast {
        chat_id: Some(chat_id),
        title: title.to_string(),
        body: body.to_string(),
        expires_at: Millis(expires_at_ms),
    }
}

pub fn with_toasts(mut state: AppState, toasts: Vec<Toast>) -> AppState {
    state.toasts = ToastState {
        toasts: toasts.into_iter().collect(),
    };
    state
}

// --- media / file cards --------------------------------------------------

pub fn file_snapshot(
    id: FileId,
    downloaded: u64,
    expected: u64,
    is_downloading: bool,
    is_completed: bool,
) -> FileSnapshot {
    FileSnapshot {
        id,
        expected_size: expected,
        downloaded_size: downloaded,
        uploaded_size: 0,
        is_downloading,
        is_completed,
        local_path: None,
    }
}

pub fn with_file(mut state: AppState, snapshot: FileSnapshot) -> AppState {
    state.media.files.insert(snapshot.id, snapshot);
    state
}
