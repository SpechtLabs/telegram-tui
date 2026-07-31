# telegram-tui — architecture

**Status:** Approved for implementation
**Derived from:** `docs/superpowers/specs/2026-07-30-telegram-tui-design.md` (authoritative for product behavior)
**Companion:** `docs/plan.md` (task decomposition for parallel subagent execution)

This document is the as-built-to-be architecture. Type definitions here are the
contract between tasks in the plan: a task implements exactly these signatures,
and a task that needs a neighbor's type reads it from here, not from the
neighbor's diff.

---

## 1. Crate graph

```
tgt-core  ──────────►  (tokio/sync, serde, thiserror, nucleo, hmac)
   ▲    ▲
   │    │
tgt-ui ─┘ ──────────►  (ratatui, crossterm, unicode-*, lru, qrcode, ratatui-image)
   ▲
   │
tgt-app ────────────►  (tdlib-rs, tracing-batteries, keyring, clap, color-eyre)
```

| Package | Lib/bin name | Role |
|---|---|---|
| `tgt-core` (`crates/core`) | `tgt_core` | Domain model, `AppState`, pure `update()`, TDLib boundary types, `TdRuntime` trait, `FakeTd`, telemetry schema and `emit!` |
| `tgt-ui` (`crates/ui`) | `tgt_ui` | ratatui views, message layout engine, layout cache, theme, crossterm→`Key` input mapping |
| `tgt-app` (`crates/app`) | binary `tgt` | Composition root: config, CLI, main loop, `Effect` dispatcher, `TdlibRuntime`, logging/OTLP wiring, Keychain, packaging glue |

Enforced boundaries (CI script `scripts/check-crate-boundaries.sh`, see §9.1):

- `tgt-core` must not depend on `ratatui` or `crossterm`.
- `tgt-ui` must not depend on `tdlib-rs`.

`tgt-ui` consumes only plain data re-exported from `tgt_core`.

## 2. Module map

Every module, its path, and its single responsibility. Files are deliberately
small; a module that outgrows ~300 lines is a candidate for splitting before it
is a candidate for editing.

### 2.1 `crates/core/src/`

| Path | Responsibility |
|---|---|
| `lib.rs` | Module tree and public re-exports; no logic |
| `action.rs` | The `Action` enum: every input to `update()` |
| `effect.rs` | The `Effect` enum: every side effect `update()` may request |
| `app.rs` | `App` root: owns `AppState`, routes actions (modal → focused pane → global), dirty flag |
| `model/ids.rs` | Newtype ids: `ChatId`, `MessageId`, `UserId`, `FileId` |
| `model/message.rs` | `MessageView`, `MessageContent`, `SendState`, `MessageCaps`, `ReactionView`, `ReplyPreview` |
| `model/chat.rs` | `ChatView`, `ChatKind`, `ChatListId`, `ChatPositionEntry`, `MessagePreview` |
| `model/entity.rs` | `FormattedText`, `TextEntity`, `EntityKind` (UTF-16 offsets, unconverted) |
| `model/chips.rs` | `Chip` enum and `chips_for(caps, is_outgoing) -> Vec<Chip>` derivation from TDLib flags |
| `model/key.rs` | Terminal-agnostic `Key` enum and `KeyBindings` |
| `model/time.rs` | `Millis` monotonic timestamp (injected via `Action::Tick`) |
| `state/focus.rs` | `Focus`, `ModalKind`, `FocusStack` (push/pop; `Esc` pops exactly one) |
| `state/auth.rs` | Projection of `AuthPhase` into wizard fields, inline errors, flood-wait countdown |
| `state/consent.rs` | First-run telemetry consent screen state |
| `state/chat_list.rs` | Chat map plus per-list `BTreeSet<ChatOrderKey>` mirroring TDLib order; selection, filter |
| `state/conversation.rs` | Per-chat message window, scroll anchor, window eviction, read markers |
| `state/history.rs` | `PagingState` machine (`Idle`/`Loading`/`Cooldown`/`Exhausted`) and its transitions |
| `state/selection.rs` | Selection mode: selected message, chip cursor, chip invocation → effects |
| `state/composer.rs` | Input buffer, reply/edit context, `pending_send` (text held until send confirmed), bare-path detection |
| `state/modal.rs` | Modal lifecycle: confirm-delete, send-file; confirmation → effects |
| `state/palette.rs` | `ctrl+p` palette: nucleo fuzzy match over chats and commands |
| `state/search.rs` | In-chat search: query, hit list, `n`/`N` stepping |
| `state/toasts.rs` | Toast queue (max 3, 4 s TTL), mute/focused-chat suppression |
| `state/media.rs` | `FileSnapshot` table, download/upload progress, viewport-priority derivation |
| `state/presence.rs` | Typing indicators (with expiry) and user online status |
| `td/runtime.rs` | `TdRuntime` trait |
| `td/request.rs` | `TdRequest`, `TdResponse`, `TdlibParams` |
| `td/update.rs` | `TdUpdate`, `AuthPhase`, `ConnectionPhase` (pre-digested, serde-serializable) |
| `td/error.rs` | `TdError` with named variants (`FloodWait` etc.) |
| `td/fake.rs` | `FakeTd`: JSONL fixture replay, request matching, received-request log |
| `telemetry/mod.rs` | `TelemetryEvent`, `Outcome`, builder constructors |
| `telemetry/schema.rs` | Allowlisted event/attribute constants; `ALLOWED_KEYS` |
| `telemetry/emit.rs` | The `emit!` macro: sole path to the exporter, sets `telemetry.public = true` |
| `telemetry/hashing.rs` | `hash_id(salt, id) -> String`: HMAC-SHA256 truncated to 8 bytes, hex |

### 2.2 `crates/ui/src/`

| Path | Responsibility |
|---|---|
| `lib.rs` | Module tree; `pub fn view(state: &AppState, theme: &Theme, f: &mut Frame, cache: &mut LayoutCache)` root |
| `theme/mod.rs` | `Theme` struct (semantic tokens only), default theme, sender-color derivation |
| `theme/loader.rs` | TOML theme file parsing, truecolor→256 degradation |
| `input/mod.rs` | Mechanical `crossterm::event::Event` → `Option<Action>` conversion (no routing logic) |
| `view/root.rs` | Responsive arrangement: two-pane ≥ breakpoint, single-pane stack below; breadcrumb header |
| `view/chat_list.rs` | Sidebar: rows, badges, pinned section, archive pseudo-row, folder tabs |
| `view/conversation.rs` | Message list viewport: consumes cached layouts, scroll, selection highlight, search-hit highlight |
| `view/header.rs` | Chat header: title, presence, typing, connection state indicator |
| `view/composer.rs` | Input box, reply/edit banner, upload progress |
| `view/chips.rs` | Action chip row with horizontal scroll affordances |
| `view/hint_bar.rs` | Bottom hint bar (context-dependent key hints) |
| `view/modal.rs` | Centered modal rendering (confirm delete, send file) |
| `view/toast.rs` | Lower-right toast stack |
| `view/palette.rs` | Centered command palette |
| `view/auth.rs` | Auth wizard screens: credentials, method choice, phone/code/password, QR (Unicode half-blocks) |
| `view/consent.rs` | Telemetry consent screen |
| `view/help.rs` | `?` help overlay |
| `render/offsets.rs` | **Isolated pure function**: UTF-16 code-unit span → byte range (constraint 5) |
| `render/wrap.rs` | Grapheme-aware, width-aware span wrapping (`unicode-segmentation` + `unicode-width`) |
| `render/message_layout.rs` | `layout_message()`: entities → styled spans → wrapped `Line`s; rails, grouping, quotes, spoilers, file cards |
| `render/cache.rs` | `LayoutCache`: LRU bounded by total line count, wholesale clear on width/theme change |
| `render/image.rs` | Inline image cells via `ratatui-image`; scroll invalidation; placeholder fallback |

### 2.3 `crates/app/src/`

| Path | Responsibility |
|---|---|
| `main.rs` | Entry: CLI parse, config load, panic hook, consent gating, terminal setup/teardown |
| `cli.rs` | `clap` definitions: `tgt`, `--no-telemetry`, `tgt telemetry show|reset-id` |
| `config.rs` | TOML config load/generate (`etcetera` paths), unknown-key warnings, `ConfigPatch` application |
| `keychain.rs` | 32-byte DB encryption key via `keyring` (macOS Keychain); generate-on-first-run |
| `runtime_loop.rs` | The `tokio::select!` main loop: action channel, terminal events, tick, coalesced draw |
| `dispatch.rs` | `Effect` → async execution; completion re-enters as `Action::TdResult`/`Action::Io` |
| `td_runtime.rs` | `TdlibRuntime`: tdlib-rs client, `spawn_blocking` receive loop, type mapping both directions |
| `graphics.rs` | Terminal graphics protocol probe at startup (kitty/iterm2/sixel/none) |
| `media_kind.rs` | Path/extension → `OutgoingFileKind` |
| `notify.rs` | `OSC 777` / `BEL` emission; generic body only, structurally no payload |
| `logging.rs` | Rolling file log under `~/.local/state/telegram-tui/`; nothing to stdout/stderr while TUI active |
| `otel.rs` | `tracing-batteries` session, public-marker filter layer, 2 s shutdown timeout |
| `telemetry_cli.rs` | `telemetry show` / `reset-id` implementations |
| `panic.rs` | Panic hook: leave alternate screen and raw mode before printing |
| `build.rs` | macOS rpath link args and dev-time dylib copy (§9.2) |

---

## 3. Runtime data flow

The Elm architecture over one `tokio::sync::mpsc` channel. One owner of state,
no locks, no `Arc<RwLock<_>>`.

```rust
// crates/app/src/runtime_loop.rs (shape; exact code lives there)
loop {
    tokio::select! {
        Some(action) = actions.recv() => effects.extend(app.update(action)),
        Some(ev) = term_events.next() => {
            if let Some(action) = tgt_ui::input::map_event(ev?) {
                effects.extend(app.update(action));
            }
        }
        _ = tick.tick() => effects.extend(app.update(Action::Tick { now: clock.now() })),
    }
    for eff in effects.drain(..) {
        dispatcher.dispatch(eff); // spawns; completions come back as Actions
    }
    if app.take_dirty() && draw_gate.ready() /* ≥16 ms since last draw */ {
        terminal.draw(|f| tgt_ui::view(app.state(), &theme, f, &mut cache))?;
    }
}
```

- `update()` is pure: no I/O, no spawning, no clock/RNG reads. Time arrives via
  `Action::Tick { now }` and is cached in `AppState.now`; randomness is never
  needed inside `update()` (install id and HMAC salt are generated in
  `tgt-app` at boot and passed in as plain data).
- TDLib updates are pre-digested by the runtime into `TdUpdate` (a serde-able
  domain enum) before entering the channel as `Action::Td(_)`.
- `Effect` dispatch completions re-enter the channel as domain-specific
  completion actions (`Action::TdResult`, `Action::Io`) carrying
  `Result<_, TdError>`; the dispatcher never handles errors itself beyond
  logging locally.
- Rendering is dirty-flag driven; a 16 ms gate coalesces draw bursts.
  Resize events invalidate the layout cache and force a full redraw.

---

## 4. Load-bearing types

All code below is real Rust and compiles as written given the listed imports.
Tasks must not rename fields or variants without updating this document first.

### 4.1 Ids and primitives — `core/src/model/ids.rs`, `model/time.rs`, `model/key.rs`

```rust
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ChatId(pub i64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct MessageId(pub i64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct UserId(pub i64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct FileId(pub i32);

/// Monotonic milliseconds since process start. Injected via `Action::Tick`;
/// `update()` never reads a clock.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Millis(pub u64);

impl Millis {
    pub fn saturating_add(self, ms: u64) -> Millis { Millis(self.0.saturating_add(ms)) }
}
```

```rust
// core/src/model/key.rs
use serde::{Deserialize, Serialize};

/// Terminal-agnostic key. `tgt-ui` converts crossterm events into this;
/// all routing happens inside `update()` against the focus stack, so focus
/// transitions are unit-testable without a terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Key {
    Char(char),
    Enter,
    AltEnter,
    Esc,
    Tab,
    BackTab,
    Up,
    Down,
    Left,
    Right,
    Backspace,
    Delete,
    Home,
    End,
    PageUp,
    PageDown,
    Ctrl(char), // Ctrl('c'), Ctrl('p'), …
}

/// Rebindable global keys, parsed from config ("ctrl+p" → Key::Ctrl('p')).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyBindings {
    pub palette: Key,
    pub help: Key,
    pub quit: Key,
}

impl Default for KeyBindings {
    fn default() -> Self {
        KeyBindings { palette: Key::Ctrl('p'), help: Key::Char('?'), quit: Key::Ctrl('c') }
    }
}
```

### 4.2 Message and chat model — `core/src/model/`

```rust
// core/src/model/entity.rs
use serde::{Deserialize, Serialize};

/// Offsets are UTF-16 code units exactly as Telegram delivers them.
/// Conversion to byte offsets happens in ONE place: `tgt_ui::render::offsets`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FormattedText {
    pub text: String,
    pub entities: Vec<TextEntity>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextEntity {
    pub offset_utf16: u32,
    pub length_utf16: u32,
    pub kind: EntityKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum EntityKind {
    Bold,
    Italic,
    Underline,
    Strikethrough,
    Spoiler,
    Code,
    Pre { language: Option<String> },
    Blockquote,
    TextUrl { url: String },
    Url,
    Mention,
    Hashtag,
}
```

```rust
// core/src/model/message.rs
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use crate::model::entity::FormattedText;
use crate::model::ids::{ChatId, FileId, MessageId, UserId};
use crate::td::error::TdError;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MessageView {
    pub id: MessageId,
    pub chat_id: ChatId,
    pub sender: Sender,
    pub sender_name: String,
    pub is_outgoing: bool,
    /// Unix seconds as delivered by TDLib. Formatting is a ui concern.
    pub date: i64,
    pub content: MessageContent,
    pub reply_to: Option<ReplyPreview>,
    pub send_state: SendState,
    pub reactions: Vec<ReactionView>,
    pub caps: MessageCaps,
    pub is_edited: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Sender {
    User(UserId),
    Chat(ChatId), // channel posts, anonymous admins
}

impl Sender {
    /// Stable value for deterministic per-sender accent color derivation.
    pub fn color_seed(&self) -> i64 {
        match self { Sender::User(u) => u.0, Sender::Chat(c) => c.0 }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum MessageContent {
    Text(FormattedText),
    Photo { file_id: FileId, width: u32, height: u32, caption: FormattedText },
    Video { file_id: FileId, file_name: String, size: u64, duration_secs: u32, caption: FormattedText },
    Audio { file_id: FileId, file_name: String, size: u64, duration_secs: u32 },
    Document { file_id: FileId, file_name: String, size: u64, caption: FormattedText },
    Sticker { emoji: String },
    Unsupported { description: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum SendState {
    /// Optimistic: appended from the sendMessage response (temporary id).
    Sending,
    /// Confirmed by updateMessageSendSucceeded (final id).
    Sent,
    Failed(TdError),
}
// "Read" (✓✓) is not a SendState: it is derived at render time from
// `ConversationState.last_read_outbox >= message.id`.

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReactionView {
    pub emoji: String,
    pub count: u32,
    pub chosen_by_me: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplyPreview {
    pub message_id: MessageId,
    pub sender_name: String,
    /// Single line, pre-truncated by the runtime mapping layer.
    pub excerpt: String,
}

/// Mirrors TDLib's per-message capability flags verbatim. Chips derive from
/// these and are never hardcoded (spec §5.3).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageCaps {
    pub can_be_edited: bool,
    pub can_be_deleted_for_all_users: bool,
    pub can_be_deleted_only_for_self: bool,
    pub can_be_forwarded: bool,
    pub can_be_saved: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FileSnapshot {
    pub id: FileId,
    pub expected_size: u64,
    pub downloaded_size: u64,
    pub is_downloading: bool,
    pub is_completed: bool,
    pub local_path: Option<PathBuf>,
}
```

```rust
// core/src/model/chat.rs
use serde::{Deserialize, Serialize};
use crate::model::ids::ChatId;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChatKind { Private, Group, Supergroup, Channel }

impl ChatKind {
    /// Allowlisted telemetry value (`chat.kind`).
    pub fn telemetry_str(self) -> &'static str {
        match self {
            ChatKind::Private => "private",
            ChatKind::Group => "group",
            ChatKind::Supergroup => "supergroup",
            ChatKind::Channel => "channel",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ChatListId {
    Main,
    Archive,
    Folder(i32),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatPositionEntry {
    pub list: ChatListId,
    /// TDLib's order. 0 means "remove from this list". NEVER computed locally.
    pub order: i64,
    pub is_pinned: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatView {
    pub id: ChatId,
    pub kind: ChatKind,
    pub title: String,
    pub positions: Vec<ChatPositionEntry>,
    pub unread_count: u32,
    pub unread_mention_count: u32,
    pub last_message: Option<MessagePreview>,
    pub is_muted: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MessagePreview {
    pub sender_name: String,
    pub text: String,
    pub date: i64,
    pub is_outgoing: bool,
}

/// Sort key mirroring TDLib: (order DESC, chat_id DESC).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChatOrderKey {
    pub order: i64,
    pub chat_id: ChatId,
}

impl Ord for ChatOrderKey {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        other.order.cmp(&self.order).then(other.chat_id.cmp(&self.chat_id))
    }
}
impl PartialOrd for ChatOrderKey {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> { Some(self.cmp(other)) }
}
```

```rust
// core/src/model/chips.rs
use crate::model::message::MessageCaps;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Chip {
    Reply,    // 'r'
    Forward,  // 'f'
    React,    // 'e'
    Copy,     // 'c'
    Edit,     // 'd'  (only own editable messages)
    Delete,   // 'x'
    Download, // 'l'  (file content, not yet downloaded)
    Open,     // 'o'  (file content, downloaded)
    Resend,   // 's'  (only SendState::Failed)
}

impl Chip {
    pub fn shortcut(self) -> char {
        match self {
            Chip::Reply => 'r', Chip::Forward => 'f', Chip::React => 'e',
            Chip::Copy => 'c', Chip::Edit => 'd', Chip::Delete => 'x',
            Chip::Download => 'l', Chip::Open => 'o', Chip::Resend => 's',
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Chip::Reply => "Reply", Chip::Forward => "Forward", Chip::React => "React",
            Chip::Copy => "Copy", Chip::Edit => "Edit", Chip::Delete => "Delete",
            Chip::Download => "Download", Chip::Open => "Open", Chip::Resend => "Resend",
        }
    }
}

/// Pure derivation from TDLib capability flags plus local message facts.
/// An action that would fail is never offered.
pub fn chips_for(
    caps: &MessageCaps,
    is_outgoing: bool,
    has_file: bool,
    file_downloaded: bool,
    send_failed: bool,
) -> Vec<Chip> {
    let mut chips = Vec::new();
    if send_failed {
        chips.push(Chip::Resend);
        chips.push(Chip::Delete);
        return chips;
    }
    chips.push(Chip::Reply);
    if caps.can_be_forwarded { chips.push(Chip::Forward); }
    chips.push(Chip::React);
    if caps.can_be_saved { chips.push(Chip::Copy); }
    if is_outgoing && caps.can_be_edited { chips.push(Chip::Edit); }
    if has_file && !file_downloaded { chips.push(Chip::Download); }
    if has_file && file_downloaded { chips.push(Chip::Open); }
    if caps.can_be_deleted_for_all_users || caps.can_be_deleted_only_for_self {
        chips.push(Chip::Delete);
    }
    chips
}
```

### 4.3 `Action` — `core/src/action.rs`

Decision: the top level is coarse (one variant per input source); each source
carries its own fine-grained enum. TDLib updates are pre-digested into
`TdUpdate` (not 1:1 raw TDLib types) so `core` never sees `tdlib-rs` types and
fixtures stay serde-serializable.

```rust
use std::path::PathBuf;
use crate::model::key::Key;
use crate::model::time::Millis;
use crate::td::update::TdUpdate;
use crate::td::error::TdError;
use crate::model::ids::{ChatId, FileId, MessageId};
use crate::model::message::{FileSnapshot, MessageCaps, MessageView};

#[derive(Debug, Clone)]
pub enum Action {
    /// A key press, unrouted. `update()` routes modal → focused pane → global.
    Key(Key),
    /// Bracketed paste (terminals paste dropped files as plain text paths).
    Paste(String),
    Resize { width: u16, height: u16 },
    /// Periodic housekeeping tick (250 ms). Carries injected time.
    Tick { now: Millis },
    /// Pre-digested TDLib push update.
    Td(TdUpdate),
    /// Completion of a dispatched `Effect::Td(_)` request.
    TdResult(TdResult),
    /// Completion of a dispatched non-TDLib effect.
    Io(IoResult),
}

/// Domain-specific completions: the dispatcher maps (request, response) pairs
/// into these mechanically. No correlation tokens; the domain context rides
/// along in the variant. (Judgment call, see architecture §8.)
#[derive(Debug, Clone)]
pub enum TdResult {
    /// Result of an auth submission (phone / code / password / QR request).
    AuthRequestDone { outcome: Result<(), TdError> },
    ChatsLoaded { outcome: Result<(), TdError> },
    HistoryLoaded {
        chat_id: ChatId,
        only_local: bool,
        outcome: Result<Vec<MessageView>, TdError>,
    },
    /// sendMessage returned: the optimistic message with its temporary id.
    MessageSent { chat_id: ChatId, outcome: Result<MessageView, TdError> },
    /// getMessageProperties completion: the selected message's capability
    /// flags (§7 — they do not ride on `message`). An `Err` leaves the
    /// message's existing caps in place.
    MessagePropertiesLoaded {
        chat_id: ChatId,
        message_id: MessageId,
        outcome: Result<MessageCaps, TdError>,
    },
    EditDone { chat_id: ChatId, message_id: MessageId, outcome: Result<(), TdError> },
    DeleteDone { chat_id: ChatId, outcome: Result<(), TdError> },
    ForwardDone { to_chat_id: ChatId, outcome: Result<(), TdError> },
    ReactionDone { chat_id: ChatId, message_id: MessageId, outcome: Result<(), TdError> },
    DownloadStarted { file_id: FileId, outcome: Result<FileSnapshot, TdError> },
    SearchDone {
        chat_id: ChatId,
        outcome: Result<Vec<MessageId>, TdError>,
    },
    LogOutDone { outcome: Result<(), TdError> },
}

#[derive(Debug, Clone)]
pub enum IoResult {
    ClipboardCopied { outcome: Result<(), IoErrorKind> },
    ExternalOpened { path: PathBuf, outcome: Result<(), IoErrorKind> },
    ConfigSaved { outcome: Result<(), IoErrorKind> },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IoErrorKind { Denied, NotFound, Other }
```

### 4.4 `Effect` — `core/src/effect.rs`

```rust
use std::path::PathBuf;
use crate::td::request::TdRequest;
use crate::telemetry::TelemetryEvent;

#[derive(Debug, Clone)]
pub enum Effect {
    /// Execute a TDLib request. Completion re-enters as `Action::TdResult`.
    Td(TdRequest),
    /// Emit an allowlisted telemetry event (dispatcher calls `emit!`).
    Telemetry(TelemetryEvent),
    /// Ring the terminal: OSC 777 with a GENERIC body, or BEL fallback.
    /// Deliberately carries no payload — PII cannot ride on it structurally.
    Alert,
    CopyToClipboard { text: String },
    OpenExternal { path: PathBuf },
    SaveConfig(ConfigPatch),
    Quit,
}

/// The only config mutations `update()` may request.
#[derive(Debug, Clone, PartialEq)]
pub enum ConfigPatch {
    Theme(String),
    TelemetryMode(TelemetryMode),
    Credentials { api_id: i32, api_hash: String },
    ConsentAcknowledged { enabled: bool },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TelemetryMode { Vendor, Custom, Off }
```

### 4.5 Focus — `core/src/state/focus.rs`

```rust
use std::path::PathBuf;
use crate::model::ids::{ChatId, MessageId};

#[derive(Debug, Clone, PartialEq)]
pub enum Focus {
    ChatList,
    ChatFilter,
    Composer,
    /// Message selection mode (chips visible).
    Selection,
    ChatSearch,
    Palette,
    Help,
    Modal(ModalKind),
}

#[derive(Debug, Clone, PartialEq)]
pub enum ModalKind {
    ConfirmDelete { chat_id: ChatId, message_id: MessageId, can_revoke: bool },
    ConfirmSendFile { path: PathBuf },
}

/// Invariant: never empty. `Esc` pops exactly one level and never pops the base.
#[derive(Debug, Clone, PartialEq)]
pub struct FocusStack {
    stack: Vec<Focus>,
}

impl FocusStack {
    pub fn new(base: Focus) -> Self { FocusStack { stack: vec![base] } }
    pub fn current(&self) -> &Focus { self.stack.last().expect("focus stack never empty") }
    pub fn push(&mut self, f: Focus) { self.stack.push(f); }
    /// Pops one level; returns false (and does nothing) at the base.
    pub fn pop(&mut self) -> bool {
        if self.stack.len() > 1 { self.stack.pop(); true } else { false }
    }
    pub fn replace_base(&mut self, f: Focus) { self.stack[0] = f; }
    pub fn depth(&self) -> usize { self.stack.len() }
}
```

### 4.6 `AppState` and sub-states — `core/src/app.rs`, `core/src/state/`

Decision: sub-states are separate structs in separate modules, each with its own
handler functions; `App::update` is a thin router. This keeps file ownership
disjoint for parallel subagents and keeps every handler unit-testable in
isolation.

```rust
// core/src/app.rs
use std::collections::HashMap;
use crate::action::Action;
use crate::effect::{Effect, TelemetryMode};
use crate::model::ids::ChatId;
use crate::model::key::KeyBindings;
use crate::model::time::Millis;
use crate::state::auth::AuthState;
use crate::state::chat_list::ChatListState;
use crate::state::composer::ComposerState;
use crate::state::consent::ConsentState;
use crate::state::conversation::ConversationState;
use crate::state::focus::FocusStack;
use crate::state::media::MediaState;
use crate::state::palette::PaletteState;
use crate::state::presence::PresenceState;
use crate::state::search::ChatSearchState;
use crate::state::toasts::ToastState;
use crate::td::update::ConnectionPhase;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen { Consent, Auth, Main }

#[derive(Debug)]
pub struct AppState {
    pub screen: Screen,
    pub focus: FocusStack,
    pub connection: ConnectionPhase,
    pub consent: ConsentState,
    pub auth: AuthState,
    pub chat_list: ChatListState,
    pub conversations: HashMap<ChatId, ConversationState>,
    pub open_chat: Option<ChatId>,
    pub composer: ComposerState,
    pub palette: Option<PaletteState>,
    pub chat_search: Option<ChatSearchState>,
    pub toasts: ToastState,
    pub media: MediaState,
    pub presence: PresenceState,
    pub width: u16,
    pub height: u16,
    pub layout_breakpoint_cols: u16,
    pub theme_name: String,
    pub theme_generation: u64,
    pub bindings: KeyBindings,
    pub telemetry_mode: TelemetryMode,
    /// HMAC salt for hashed-id telemetry attributes. Generated in tgt-app.
    pub telemetry_salt: [u8; 32],
    /// Last observed tick time; the only "clock" update logic may consult.
    pub now: Millis,
}

/// Boot-time data computed impurely in tgt-app and injected as plain values.
#[derive(Debug, Clone)]
pub struct Boot {
    pub theme_name: String,
    pub bindings: KeyBindings,
    pub layout_breakpoint_cols: u16,
    pub telemetry_mode: TelemetryMode,
    pub telemetry_salt: [u8; 32],
    pub consent_needed: bool,
    pub has_credentials: bool,
    pub width: u16,
    pub height: u16,
}

#[derive(Debug)]
pub struct App {
    state: AppState,
    dirty: bool,
}

impl App {
    pub fn new(boot: Boot) -> Self;
    /// THE pure transition function. No I/O, no spawning, no clock, no RNG.
    pub fn update(&mut self, action: Action) -> Vec<Effect>;
    /// True once per render-worthy change; cleared on read.
    pub fn take_dirty(&mut self) -> bool;
    pub fn state(&self) -> &AppState;
}
```

Routing contract inside `App::update` (spec §6.2): keys go modal → focused pane
→ global; the first handler that claims the key stops propagation. Non-key
actions route by payload (e.g. `TdUpdate::ChatPosition` → `chat_list`,
`HistoryLoaded` → `history`/`conversation`). Every sub-state module exposes
plain functions; the canonical handler shapes are:

```rust
// state/<name>.rs — handlers own their sub-struct, may read AppState context
pub fn handle_key(app: &mut AppState, key: Key) -> Option<Vec<Effect>>; // None = unclaimed
pub fn handle_td(app: &mut AppState, upd: &TdUpdate) -> Vec<Effect>;
pub fn handle_tick(app: &mut AppState, now: Millis) -> Vec<Effect>;
```

Sub-state definitions:

```rust
// core/src/state/consent.rs
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConsentChoice { Enable, Disable }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConsentState {
    pub selected: ConsentChoice, // Enable preselected (spec §13.5)
    pub acknowledged: bool,
}
```

```rust
// core/src/state/auth.rs
use crate::model::time::Millis;
use crate::td::error::TdError;
use crate::td::update::AuthPhase;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoginMethod { Phone, Qr }

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InputField {
    pub text: String,
    /// Byte offset into `text`, always on a char boundary.
    pub cursor: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthField { ApiId, ApiHash, Phone, Code, Password }

#[derive(Debug, Clone, PartialEq)]
pub struct FieldError {
    pub field: AuthField,
    pub error: TdError,
}

/// Credentials-wizard contract (T11, binding on T12/T14): the wizard adds NO
/// fields anywhere — it is driven entirely by `active_field`. The AppState
/// constructor seeds `active_field = AuthField::ApiId` iff credentials are
/// missing (else `Phone`); `handle_key` treats `active_field ∈ {ApiId,
/// ApiHash}` as wizard-active regardless of phase; Enter on ApiHash (api_id
/// parses as i32, api_hash non-empty) emits
/// `Effect::SaveConfig(ConfigPatch::Credentials)` and moves to `Phone`.
/// Nothing may route back into ApiId/ApiHash once a phase past
/// WaitTdlibParameters has been projected.
///
/// A PROJECTION of TDLib's authorizationState — never a parallel state machine.
#[derive(Debug, Clone, PartialEq)]
pub struct AuthState {
    pub phase: AuthPhase,
    pub method: Option<LoginMethod>,
    pub api_id: InputField,
    pub api_hash: InputField,
    pub phone: InputField,
    pub code: InputField,
    pub password: InputField,
    pub active_field: AuthField,
    pub field_error: Option<FieldError>,
    /// FLOOD_WAIT rendered as a live countdown against `AppState.now`.
    pub flood_wait_until: Option<Millis>,
    pub in_flight: bool,
}
```

```rust
// core/src/state/chat_list.rs
use std::collections::{BTreeSet, HashMap};
use crate::model::chat::{ChatListId, ChatOrderKey, ChatView};
use crate::model::ids::ChatId;
use crate::state::auth::InputField;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatLoadPhase { Idle, Loading, Complete }

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

impl Default for ChatListId {
    fn default() -> Self { ChatListId::Main }
}
```

```rust
// core/src/state/history.rs — the paging machine, freestanding and pure.
use crate::model::ids::MessageId;
use crate::model::time::Millis;

pub const PAGE_SIZE: u8 = 50;
/// Trigger paging when the scroll anchor is within this many MESSAGES of the
/// oldest loaded one (core counts messages, not rows: rows are a ui concept).
pub const PAGE_TRIGGER_MESSAGES: usize = 20;
/// An empty response is NOT end-of-history (spec §5.2): retry with
/// only_local = false up to this bound before believing TDLib.
pub const MAX_EMPTY_ATTEMPTS: u8 = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PagingState {
    Idle,
    Loading { attempt: u8, only_local: bool },
    /// FloodWait or transient error: no requests until `until`.
    Cooldown { until: Millis },
    /// Only entered when a non-local request came back empty at max attempts.
    Exhausted,
}

/// What the caller (conversation.rs) must do after feeding an event in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PagingDirective {
    None,
    /// Issue Effect::Td(GetChatHistory { from_message_id, limit: PAGE_SIZE, only_local }).
    Request { from_message_id: MessageId, only_local: bool },
}

pub fn on_scroll_near_top(
    paging: &mut PagingState,
    oldest_loaded: Option<MessageId>,
    now: Millis,
) -> PagingDirective;

pub fn on_history_loaded(
    paging: &mut PagingState,
    received: usize,
    was_only_local: bool,
    oldest_loaded: Option<MessageId>,
) -> PagingDirective;

pub fn on_history_error(paging: &mut PagingState, retry_after: Option<u32>, now: Millis);
```

```rust
// core/src/state/conversation.rs
use std::collections::{BTreeSet, VecDeque};
use crate::model::ids::{ChatId, MessageId};
use crate::model::message::MessageView;
use crate::state::history::PagingState;

/// Bounded loaded window: memory stays flat in long-lived sessions.
pub const WINDOW_MAX_MESSAGES: usize = 500;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scroll {
    /// Pinned to newest; new messages keep the view at the bottom.
    Bottom,
    /// Anchored at a message (stable across prepends), offset in laid-out lines.
    At { message_id: MessageId, line_offset: u16 },
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
}
```

```rust
// core/src/state/selection.rs
use crate::model::chips::Chip;
use crate::model::ids::MessageId;

#[derive(Debug, Clone, PartialEq)]
pub struct SelectionState {
    pub message_id: MessageId,
    /// Chips recomputed from caps whenever the selected message changes.
    pub chips: Vec<Chip>,
    pub chip_cursor: usize,
    /// First visible chip index (horizontal chip scrolling, ‹ › affordances).
    pub chip_scroll: usize,
}
```

Selection is per open chat and transient, so it lives on the conversation it
selects in: `ConversationState` gains
`pub selection: Option<SelectionState>` in milestone 4 (task T26). The field is
listed here so neighboring tasks can rely on the name.

```rust
// core/src/state/composer.rs
use std::path::PathBuf;
use crate::model::ids::MessageId;
use crate::state::auth::InputField;

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
```

```rust
// core/src/state/palette.rs
use crate::model::ids::ChatId;
use crate::state::auth::InputField;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandId { ToggleTheme, TelemetrySettings, SendFile, LogOut, Quit }

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
```

```rust
// core/src/state/search.rs
use crate::state::auth::InputField;

#[derive(Debug, Default)]
pub struct ChatSearchState {
    pub input: InputField,
    /// Index into ConversationState.search_hits ('n'/'N' step).
    pub current_hit: usize,
    pub in_flight: bool,
}
```

```rust
// core/src/state/toasts.rs
use std::collections::VecDeque;
use crate::model::ids::ChatId;
use crate::model::time::Millis;

pub const TOAST_MAX: usize = 3;
pub const TOAST_TTL_MS: u64 = 4_000;

/// In-app only: title/body may contain chat titles and message text because
/// they never leave the terminal cell grid. Effect::Alert (the escape-sequence
/// path) carries no payload at all.
#[derive(Debug, Clone, PartialEq)]
pub struct Toast {
    pub chat_id: ChatId,
    pub title: String,
    pub body: String,
    pub expires_at: Millis,
}

#[derive(Debug, Default)]
pub struct ToastState {
    pub toasts: VecDeque<Toast>,
}
```

```rust
// core/src/state/media.rs
use std::collections::HashMap;
use crate::model::ids::{ChatId, FileId, MessageId};
use crate::model::message::FileSnapshot;

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
```

```rust
// core/src/state/presence.rs
use std::collections::HashMap;
use crate::model::ids::{ChatId, UserId};
use crate::model::time::Millis;
use crate::td::update::PresenceStatus;

pub const TYPING_TTL_MS: u64 = 6_000;

#[derive(Debug, Default)]
pub struct PresenceState {
    pub users: HashMap<UserId, PresenceStatus>,
    /// (chat, user) → expiry; swept on Tick.
    pub typing: HashMap<(ChatId, UserId), Millis>,
}
```

### 4.7 TDLib boundary — `core/src/td/`

```rust
// core/src/td/runtime.rs
use async_trait::async_trait;
use tokio::sync::mpsc;
use crate::td::error::TdError;
use crate::td::request::{TdRequest, TdResponse};
use crate::td::update::TdUpdate;

#[async_trait]
pub trait TdRuntime: Send + Sync + 'static {
    async fn request(&self, req: TdRequest) -> Result<TdResponse, TdError>;
    /// Called exactly once by the runtime loop at boot; panics on second call.
    fn updates(&self) -> mpsc::Receiver<TdUpdate>;
}
```

**`TdlibRuntime` (`crates/app/src/td_runtime.rs`)** — the only module in the
workspace that imports `tdlib_rs`. Responsibilities: owns the tdlib client id;
sets TDLib log verbosity to file-only; runs the blocking `receive()` C call on a
dedicated `spawn_blocking` task that maps raw updates into `TdUpdate` and
forwards them; maps `TdRequest` → tdlib-rs function calls and raw results →
`TdResponse`; maps `(code, message)` errors into `TdError` including parsing
`FLOOD_WAIT` seconds; truncates reply excerpts to one line during mapping.

**`FakeTd` (`crates/core/src/td/fake.rs`)** — replays JSONL fixtures for
full-app integration tests without a network. Fixture format (judgment call,
§8): one serde-JSON `ScriptStep` per line — line-diffable in review, and it
reuses the serde derives that already exist on the boundary types.

```rust
// core/src/td/fake.rs
use serde::{Deserialize, Serialize};
use crate::td::error::TdError;
use crate::td::request::{TdRequest, TdResponse};
use crate::td::update::TdUpdate;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ScriptStep {
    /// Push this update to the updates channel immediately.
    Emit(TdUpdate),
    /// Block until a request matching `expect` arrives, then answer it.
    Await { expect: RequestMatcher, respond: RespondWith },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RequestMatcher {
    Any,
    /// Discriminant-only match ("a GetChatHistory, whatever its params").
    Kind(String),
    Exact(TdRequest),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RespondWith {
    Ok(TdResponse),
    Err(TdError),
}

pub struct FakeTd { /* fixture cursor, channels, request log */ }

impl FakeTd {
    pub fn from_jsonl(fixture: &str) -> Result<Self, serde_json::Error>;
    /// Every request ever received, for post-hoc assertions.
    pub fn received(&self) -> Vec<TdRequest>;
}
// FakeTd implements TdRuntime. Requests not matched by the current Await step
// receive TdResponse::Ok and are recorded; tests assert on `received()`.
```

```rust
// core/src/td/request.rs
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use crate::model::chat::ChatListId;
use crate::model::entity::FormattedText;
use crate::model::ids::{ChatId, FileId, MessageId};
use crate::model::message::{FileSnapshot, MessageView};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TdlibParams {
    pub api_id: i32,
    pub api_hash: String,
    pub database_directory: PathBuf,     // ~/.local/share/telegram-tui/td/, mode 0700
    pub database_encryption_key: Vec<u8>, // 32 bytes from macOS Keychain
    pub use_message_database: bool,       // true
    pub use_chat_info_database: bool,     // true
    pub use_file_database: bool,          // true
    pub use_secret_chats: bool,           // false — spec non-goal
    pub system_language_code: String,
    pub device_model: String,
    pub application_version: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum OutgoingFileKind { Photo, Video, Audio, Document }

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum TdRequest {
    SetTdlibParameters(TdlibParams),
    SetAuthenticationPhoneNumber { phone: String },
    CheckAuthenticationCode { code: String },
    CheckAuthenticationPassword { password: String },
    RequestQrCodeAuthentication,
    LogOut,
    LoadChats { list: ChatListId, limit: u32 },
    OpenChat { chat_id: ChatId },
    CloseChat { chat_id: ChatId },
    GetChatHistory {
        chat_id: ChatId,
        from_message_id: MessageId,
        limit: u8,
        only_local: bool,
    },
    /// Per-message capability flags (§7): TDLib serves them from
    /// `messageProperties`, not on `message`. Issued when a message is
    /// selected; completes as `TdResult::MessagePropertiesLoaded`.
    GetMessageProperties { chat_id: ChatId, message_id: MessageId },
    ViewMessages { chat_id: ChatId, message_ids: Vec<MessageId> },
    SendMessageText {
        chat_id: ChatId,
        reply_to: Option<MessageId>,
        text: FormattedText,
    },
    SendMessageFile {
        chat_id: ChatId,
        path: PathBuf,
        kind: OutgoingFileKind,
        caption: Option<FormattedText>,
    },
    EditMessageText { chat_id: ChatId, message_id: MessageId, text: FormattedText },
    DeleteMessages { chat_id: ChatId, message_ids: Vec<MessageId>, revoke: bool },
    ForwardMessages { to_chat_id: ChatId, from_chat_id: ChatId, message_ids: Vec<MessageId> },
    ToggleReaction { chat_id: ChatId, message_id: MessageId, emoji: String },
    DownloadFile { file_id: FileId, priority: i8 },
    CancelDownloadFile { file_id: FileId },
    SearchChatMessages {
        chat_id: ChatId,
        query: String,
        from_message_id: MessageId,
        limit: u8,
    },
}

impl TdRequest {
    /// Discriminant name for RequestMatcher::Kind and local logging.
    pub fn kind(&self) -> &'static str;
}
// `GetMessageProperties` + `TdResult::MessagePropertiesLoaded` (§4.3) execute
// the pending contract change recorded in §7; landed with T26. The
// `TdResponse` carrier for the caps and the runtime/dispatch mapping land
// with T32.

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum TdResponse {
    Ok,
    Chats { chat_ids: Vec<ChatId> },
    Messages { messages: Vec<MessageView> },
    Message(MessageView),
    FoundMessages { message_ids: Vec<MessageId> },
    File(FileSnapshot),
}
```

```rust
// core/src/td/update.rs
use serde::{Deserialize, Serialize};
use crate::model::chat::{ChatPositionEntry, ChatView, MessagePreview};
use crate::model::ids::{ChatId, MessageId, UserId};
use crate::model::message::{FileSnapshot, MessageContent, MessageView, ReactionView};
use crate::td::error::TdError;

/// Defined here (not in state/presence.rs) because TdUpdate carries it and the
/// td types are implemented before the state handlers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PresenceStatus { Online, Recently, Offline }

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum AuthPhase {
    WaitTdlibParameters,
    WaitPhoneNumber,
    WaitCode { delivery_hint: String, length: u8 },
    WaitPassword { hint: Option<String> },
    WaitOtherDeviceConfirmation { link: String },
    Ready,
    LoggingOut,
    Closing,
    Closed,
    /// States v1 does not implement (e.g. registration): rendered as a
    /// dead-end screen with the state name, never silently swallowed.
    Unsupported { name: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConnectionPhase { WaitingForNetwork, Connecting, ConnectingToProxy, Updating, Ready }

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum TdUpdate {
    Auth(AuthPhase),
    Connection(ConnectionPhase),
    NewChat(ChatView),
    ChatPosition { chat_id: ChatId, position: ChatPositionEntry },
    ChatLastMessage { chat_id: ChatId, preview: Option<MessagePreview>, positions: Vec<ChatPositionEntry> },
    ChatReadInbox { chat_id: ChatId, last_read_inbox_message_id: MessageId, unread_count: u32 },
    ChatReadOutbox { chat_id: ChatId, last_read_outbox_message_id: MessageId },
    ChatTitle { chat_id: ChatId, title: String },
    ChatUnreadMentionCount { chat_id: ChatId, count: u32 },
    ChatNotificationSettings { chat_id: ChatId, muted: bool },
    NewMessage(MessageView),
    MessageSendSucceeded { chat_id: ChatId, old_message_id: MessageId, message: MessageView },
    MessageSendFailed { chat_id: ChatId, old_message_id: MessageId, error: TdError },
    MessageContentChanged { chat_id: ChatId, message_id: MessageId, content: MessageContent },
    MessageInteractionInfo { chat_id: ChatId, message_id: MessageId, reactions: Vec<ReactionView> },
    MessagesDeleted { chat_id: ChatId, message_ids: Vec<MessageId> },
    File(FileSnapshot),
    UserStatus { user_id: UserId, status: PresenceStatus },
    ChatAction { chat_id: ChatId, user_id: UserId, is_typing: bool },
}
```

```rust
// core/src/td/error.rs
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, Error, Serialize, Deserialize)]
pub enum TdError {
    #[error("flood wait: retry in {seconds}s")]
    FloodWait { seconds: u32 },
    #[error("phone number invalid")]
    PhoneNumberInvalid,
    #[error("code invalid")]
    CodeInvalid,
    #[error("password invalid")]
    PasswordInvalid,
    #[error("unauthorized")]
    Unauthorized,
    #[error("network timeout")]
    NetTimeout,
    #[error("offline")]
    Offline,
    #[error("td error {code}: {message}")]
    Other { code: i32, message: String },
}

impl TdError {
    /// Allowlisted telemetry value (`error.kind`), from schema::error_kinds.
    pub fn telemetry_kind(&self) -> &'static str;
}
```

### 4.8 Telemetry — `core/src/telemetry/`

The allowlist is structural: `TelemetryEvent` fields are `&'static str` drawn
from `schema` constants, so arbitrary strings (names, titles, message text)
cannot be passed without a compile-visible constant addition, which the insta
snapshot turns into a reviewed diff.

```rust
// core/src/telemetry/schema.rs — THE COMPLETE allowlist. Additions are a
// deliberate, snapshotted, reviewed diff (spec §13.8).
pub mod keys {
    pub const APP_VERSION: &str = "app.version";
    pub const OS_VERSION: &str = "os.version";
    pub const TERM_PROGRAM: &str = "term.program";
    pub const TERM_GRAPHICS_PROTOCOL: &str = "term.graphics_protocol"; // kitty|iterm2|sixel|none
    pub const TERM_WIDTH_BUCKET: &str = "term.width_bucket";           // <80|80-120|120-160|>160
    pub const INSTALL_ID: &str = "install.id";
    pub const SESSION_ID: &str = "session.id";
    pub const ACTION: &str = "action";
    pub const OUTCOME: &str = "outcome";                               // ok|error|cancelled
    pub const ERROR_KIND: &str = "error.kind";
    pub const DURATION_MS: &str = "duration_ms";
    pub const CHAT_KIND: &str = "chat.kind";                           // private|group|supergroup|channel
    pub const CHAT_HASH: &str = "chat.hash";                           // HMAC-SHA256, 8 bytes, hex
    pub const HISTORY_PAGE_DEPTH: &str = "history.page_depth";
    pub const DOWNLOAD_SIZE_BUCKET: &str = "download.size_bucket";
    pub const PUBLIC_MARKER: &str = "telemetry.public";
}

pub const ALLOWED_KEYS: &[&str] = &[
    keys::APP_VERSION,
    keys::OS_VERSION,
    keys::TERM_PROGRAM,
    keys::TERM_GRAPHICS_PROTOCOL,
    keys::TERM_WIDTH_BUCKET,
    keys::INSTALL_ID,
    keys::SESSION_ID,
    keys::ACTION,
    keys::OUTCOME,
    keys::ERROR_KIND,
    keys::DURATION_MS,
    keys::CHAT_KIND,
    keys::CHAT_HASH,
    keys::HISTORY_PAGE_DEPTH,
    keys::DOWNLOAD_SIZE_BUCKET,
    keys::PUBLIC_MARKER,
];

pub mod actions {
    pub const APP_START: &str = "app.start";
    pub const APP_QUIT: &str = "app.quit";
    pub const QR_LOGIN: &str = "qr_login";
    pub const PHONE_LOGIN: &str = "phone_login";
    pub const CHAT_OPEN: &str = "chat.open";
    pub const MESSAGE_SEND: &str = "message.send";
    pub const MESSAGE_REPLY: &str = "message.reply";
    pub const MESSAGE_FORWARD: &str = "message.forward";
    pub const MESSAGE_DELETE: &str = "message.delete";
    pub const MESSAGE_EDIT: &str = "message.edit";
    pub const MESSAGE_REACT: &str = "message.react";
    pub const HISTORY_PAGE: &str = "history.page";
    pub const PALETTE_OPEN: &str = "palette.open";
    pub const SEARCH_RUN: &str = "search.run";
    pub const FILE_DOWNLOAD: &str = "file.download";
    pub const FILE_UPLOAD: &str = "file.upload";
    pub const THEME_CHANGE: &str = "theme.change";
}

pub mod error_kinds {
    pub const TD_FLOOD_WAIT: &str = "td.flood_wait";
    pub const TD_AUTH: &str = "td.auth";
    pub const TD_RATE_LIMIT: &str = "td.rate_limit";
    pub const TD_OTHER: &str = "td.other";
    pub const NET_TIMEOUT: &str = "net.timeout";
    pub const NET_OFFLINE: &str = "net.offline";
    pub const LAYOUT_PANIC: &str = "layout.panic";
    pub const IO_DENIED: &str = "io.denied";
    pub const IO_OTHER: &str = "io.other";
}

pub mod buckets {
    pub fn width(cols: u16) -> &'static str {
        match cols { 0..=79 => "<80", 80..=120 => "80-120", 121..=160 => "120-160", _ => ">160" }
    }
    pub fn download_size(bytes: u64) -> &'static str {
        const MB: u64 = 1_000_000;
        match bytes {
            b if b < MB => "<1MB",
            b if b < 10 * MB => "1-10MB",
            b if b < 100 * MB => "10-100MB",
            _ => ">100MB",
        }
    }
}
```

```rust
// core/src/telemetry/mod.rs
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome { Ok, Error, Cancelled }

impl Outcome {
    pub fn as_str(self) -> &'static str {
        match self { Outcome::Ok => "ok", Outcome::Error => "error", Outcome::Cancelled => "cancelled" }
    }
}

/// Every field is either a schema constant (&'static str) or a number/bucket.
/// Free-form strings are structurally impossible except chat_hash, which is
/// produced only by telemetry::hashing::hash_id.
#[derive(Debug, Clone, PartialEq)]
pub struct TelemetryEvent {
    pub action: &'static str,           // schema::actions::*
    pub outcome: Outcome,
    pub error_kind: Option<&'static str>, // schema::error_kinds::*
    pub duration_ms: Option<u64>,
    pub chat_kind: Option<&'static str>,  // ChatKind::telemetry_str()
    pub chat_hash: Option<String>,        // hashing::hash_id output only
    pub history_page_depth: Option<u32>,
    pub download_size_bucket: Option<&'static str>, // schema::buckets::download_size
}

impl TelemetryEvent {
    pub fn ok(action: &'static str) -> Self;
    pub fn error(action: &'static str, kind: &'static str) -> Self;
    pub fn cancelled(action: &'static str) -> Self;
    pub fn with_duration(self, ms: u64) -> Self;
    pub fn with_chat_kind(self, kind: &'static str) -> Self;
    pub fn with_chat_hash(self, hash: String) -> Self;
    pub fn with_page_depth(self, depth: u32) -> Self;
    pub fn with_download_bucket(self, bucket: &'static str) -> Self;
}
```

```rust
// core/src/telemetry/emit.rs — the ONLY path to the OTLP exporter.
// The subscriber layer in tgt-app exports only events carrying
// telemetry.public AND target "tgt_telemetry"; everything else stays in the
// local rolling file.
#[macro_export]
macro_rules! emit {
    ($event:expr) => {{
        let __ev: $crate::telemetry::TelemetryEvent = $event;
        // Field order note (T03): `action` (a non-dotted field) must sit
        // between `target:` and the first dotted field — tracing 0.1.44's
        // macro grammar hits a local ambiguity if a dotted field like
        // `telemetry.public` immediately follows `target:`. Same fields,
        // same values, reordered only.
        ::tracing::info!(
            target: "tgt_telemetry",
            action = __ev.action,
            telemetry.public = true,
            outcome = __ev.outcome.as_str(),
            error.kind = __ev.error_kind,
            duration_ms = __ev.duration_ms,
            chat.kind = __ev.chat_kind,
            chat.hash = __ev.chat_hash.as_deref(),
            history.page_depth = __ev.history_page_depth,
            download.size_bucket = __ev.download_size_bucket,
        );
    }};
}
```

```rust
// core/src/telemetry/hashing.rs
/// HMAC-SHA256(id, per-install salt), truncated to 8 bytes, lowercase hex.
/// Salt generated locally in tgt-app, never transmitted: stable within an
/// install, uncorrelatable across installs, irreversible.
pub fn hash_id(salt: &[u8; 32], id: i64) -> String;
```

Defense in depth, in order: (1) `update()` cannot do I/O, so telemetry leaves
core only as `Effect::Telemetry(TelemetryEvent)`, whose fields are schema
constants; (2) the dispatcher is the single `emit!` call site for those events
(impure layers may also call `emit!` directly, same constraint applies); (3) the
OTLP layer filter drops anything without the marker, so a stray
`tracing::info!` never exports; (4) the CI collector-stub test fails on any
exported key outside `ALLOWED_KEYS` (spec §13.8).

### 4.9 Theme and ui-side signatures — `crates/ui/src/`

```rust
// ui/src/theme/mod.rs
use ratatui::style::Color;

#[derive(Debug, Clone, PartialEq)]
pub struct Theme {
    pub accent: Color,
    pub accent_dim: Color,
    pub text: Color,
    pub text_muted: Color,
    pub surface: Color,
    pub surface_raised: Color,
    pub success: Color,
    pub warning: Color,
    pub danger: Color,
    pub selection: Color,
    pub rail_own: Color,
    pub rail_other: Color,
    /// Curated palette for deterministic per-sender accents.
    pub sender_palette: [Color; 8],
}

impl Theme {
    pub fn default_dark() -> Theme;
    /// Same sender → same color across sessions: seed % palette length.
    pub fn sender_color(&self, color_seed: i64) -> Color {
        self.sender_palette[(color_seed.unsigned_abs() % 8) as usize]
    }
    /// Truecolor → 256-color degradation for terminals without RGB.
    pub fn degraded(&self) -> Theme;
}
```

```rust
// ui/src/theme/loader.rs
use crate::theme::Theme;

#[derive(Debug)]
pub enum ThemeLoadError { Io(std::io::Error), Parse(String), BadColor { key: String, value: String } }

/// Parse a user theme TOML (same token names as the Theme fields, values
/// "#rrggbb" or named ANSI). Unknown keys warn (locally logged), not fail.
pub fn load_theme(path: &std::path::Path) -> Result<Theme, ThemeLoadError>;
pub fn builtin(name: &str) -> Option<Theme>;
```

```rust
// ui/src/render/offsets.rs — constraint 5's isolated pure function.
use std::ops::Range;

/// Convert a Telegram UTF-16 code-unit span into a byte range into `text`.
/// Returns None (caller renders the message unstyled and logs locally) when:
/// - the span falls outside the text,
/// - an endpoint lands inside a surrogate pair (mid-astral-character),
/// - offset + length overflows.
/// NEVER panics, NEVER slices on a non-boundary.
pub fn utf16_span_to_byte_range(text: &str, offset_utf16: u32, length_utf16: u32)
    -> Option<Range<usize>>;
```

```rust
// ui/src/render/wrap.rs
use ratatui::text::{Line, Span};

/// Wrap styled spans to `width` columns. Grapheme-cluster aware
/// (unicode-segmentation) and display-width aware (unicode-width): emoji, CJK
/// and combining marks never break column alignment. width >= 1; a grapheme
/// wider than `width` occupies its own line.
pub fn wrap_spans(spans: Vec<Span<'static>>, width: u16) -> Vec<Line<'static>>;
```

```rust
// ui/src/render/message_layout.rs — spec §8.1, verbatim signature.
use ratatui::text::Line;
use tgt_core::model::message::MessageView;
use crate::theme::Theme;

pub fn layout_message(msg: &MessageView, width: u16, theme: &Theme) -> Vec<Line<'static>>;

/// Grouping decision made by the caller (conversation view): consecutive
/// same-sender messages within this window share one header line.
pub const GROUP_WINDOW_SECS: i64 = 300;
```

```rust
// ui/src/render/cache.rs
use lru::LruCache;
use ratatui::text::Line;
use tgt_core::model::ids::MessageId;

/// Eviction policy (judgment call, §8): LRU bounded by TOTAL LINE COUNT, not
/// entry count — a 200-line pasted log and a "ok" cost what they cost. On
/// insert, least-recently-used entries are popped until the sum of cached
/// lines is <= MAX_CACHED_LINES. Width or theme change clears wholesale.
pub const MAX_CACHED_LINES: usize = 50_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LayoutKey {
    pub message_id: MessageId,
    pub width: u16,
    pub theme_generation: u64,
    pub spoilers_revealed: bool,
}

pub struct LayoutCache {
    entries: LruCache<LayoutKey, Vec<Line<'static>>>,
    total_lines: usize,
}

impl LayoutCache {
    pub fn new() -> Self;
    pub fn get_or_insert_with(
        &mut self,
        key: LayoutKey,
        f: impl FnOnce() -> Vec<Line<'static>>,
    ) -> &Vec<Line<'static>>;
    pub fn clear(&mut self);
    pub fn total_lines(&self) -> usize;
}
```

```rust
// ui/src/input/mod.rs — mechanical translation only; zero routing logic.
use crossterm::event::Event;
use tgt_core::action::Action;

pub fn map_event(ev: Event) -> Option<Action>;
```

```rust
// ui/src/lib.rs root view
use ratatui::Frame;
use tgt_core::app::AppState;
use crate::render::cache::LayoutCache;
use crate::theme::Theme;

pub fn view(state: &AppState, theme: &Theme, f: &mut Frame, cache: &mut LayoutCache);
```

Staging note (orchestrator, M1): until T21 creates `LayoutCache`, `view` is
`view(state, theme, f)` — T21/T23 add the `cache` parameter together with the
type. T08's runtime loop calls whichever arity currently exists.

---

## 5. Sequence diagrams

### 5.1 Cold start through authentication (phone path)

```mermaid
sequenceDiagram
    autonumber
    actor U as User
    participant M as tgt-app main
    participant L as runtime_loop
    participant A as core::App (pure)
    participant D as dispatcher
    participant T as TdlibRuntime
    participant TD as TDLib

    M->>M: parse CLI, load config, init file logging, install panic hook
    M->>M: first run → consent screen (before login, before any export)
    M->>M: Keychain: get-or-create 32-byte DB key
    M->>T: construct; spawn_blocking receive loop
    M->>L: enter select loop with App::new(Boot)
    TD-->>T: updateAuthorizationState(waitTdlibParameters)
    T-->>L: Action::Td(Auth(WaitTdlibParameters))
    L->>A: update(...)
    A-->>L: [Effect::Td(SetTdlibParameters)]
    L->>D: dispatch
    D->>T: request(SetTdlibParameters)
    T->>TD: setTdlibParameters(api_id, api_hash, db key, use_secret_chats=false)
    TD-->>T: updateAuthorizationState(waitPhoneNumber)
    T-->>L: Action::Td(Auth(WaitPhoneNumber))
    L->>A: update → auth wizard shows method choice
    U->>L: picks Phone, types number, Enter
    L->>A: update(Key(Enter))
    A-->>L: [Effect::Td(SetAuthenticationPhoneNumber)]
    D->>T: request(...)
    TD-->>T: updateAuthorizationState(waitCode{delivery})
    T-->>L: Action::Td(Auth(WaitCode))
    U->>L: types code, Enter
    A-->>L: [Effect::Td(CheckAuthenticationCode)]
    alt 2FA enabled
        TD-->>T: waitPassword{hint}
        T-->>L: Action::Td(Auth(WaitPassword))
        U->>L: password, Enter → Effect::Td(CheckAuthenticationPassword)
    end
    TD-->>T: updateAuthorizationState(ready)
    T-->>L: Action::Td(Auth(Ready))
    L->>A: update → Screen::Main
    A-->>L: [Effect::Td(LoadChats{Main, 200}), Effect::Telemetry(app.start)]
```

### 5.2 Sending a message: optimistic → confirmed

```mermaid
sequenceDiagram
    autonumber
    actor U as User
    participant L as runtime_loop
    participant A as core::App (pure)
    participant D as dispatcher
    participant T as TdlibRuntime
    participant TD as TDLib

    U->>L: Enter in composer (non-empty input)
    L->>A: update(Key(Enter))
    Note over A: composer: input → pending_send, input cleared<br/>(text is never discarded)
    A-->>L: [Effect::Td(SendMessageText)]
    L->>D: dispatch
    D->>T: request(SendMessageText)
    T->>TD: sendMessage(...)
    TD-->>T: Message{id: TEMP, sending_state: pending}
    T-->>D: TdResponse::Message(view, SendState::Sending)
    D-->>L: Action::TdResult(MessageSent{Ok(msg TEMP)})
    L->>A: update
    Note over A: append msg(TEMP, Sending) to conversation,<br/>drop pending_send, scroll→Bottom
    TD-->>T: updateMessageSendSucceeded{old_message_id: TEMP, message: FINAL}
    T-->>L: Action::Td(MessageSendSucceeded)
    L->>A: update
    Note over A: replace TEMP with FINAL id, SendState::Sent<br/>Effect::Telemetry(message.send, ok, duration)
    TD-->>T: updateChatReadOutbox{last_read >= FINAL}
    T-->>L: Action::Td(ChatReadOutbox)
    Note over A: render derives ✓✓ from last_read_outbox
    alt send fails
        TD-->>T: updateMessageSendFailed{TEMP, error}
        T-->>L: Action::Td(MessageSendFailed)
        Note over A: remove TEMP, restore pending_send → composer.input,<br/>toast + Effect::Telemetry(message.send, error)
    end
```

### 5.3 Scrolling up into history paging (with the empty-response trap)

```mermaid
sequenceDiagram
    autonumber
    actor U as User
    participant L as runtime_loop
    participant A as core::App (pure)
    participant D as dispatcher
    participant T as TdlibRuntime
    participant TD as TDLib

    U->>L: ↑ repeatedly in selection mode
    L->>A: update(Key(Up))
    Note over A: scroll anchor now within PAGE_TRIGGER_MESSAGES (20)<br/>of oldest loaded message; PagingState::Idle
    A-->>L: [Effect::Td(GetChatHistory{from: oldest, limit: 50, only_local: false})]
    Note over A: PagingState::Loading{attempt: 1}
    D->>T: request(GetChatHistory)
    T->>TD: getChatHistory(...)
    TD-->>T: messages: [] (local DB cold — NOT end of history)
    D-->>L: Action::TdResult(HistoryLoaded{outcome: Ok([])})
    L->>A: update
    Note over A: empty + attempt < MAX_EMPTY_ATTEMPTS (3)<br/>→ re-issue, Loading{attempt: 2}
    A-->>L: [Effect::Td(GetChatHistory{only_local: false})]
    D->>T: request(GetChatHistory)
    TD-->>T: messages: [50 older messages]
    D-->>L: Action::TdResult(HistoryLoaded{Ok(50 msgs)})
    L->>A: update
    Note over A: prepend, PagingState::Idle, scroll anchor preserved<br/>(anchored At{message_id}, not an index),<br/>evict newest beyond WINDOW_MAX_MESSAGES (500)
    alt still empty at attempt == 3 (non-local)
        Note over A: PagingState::Exhausted — TDLib has confirmed it
    end
    alt FLOOD_WAIT
        Note over A: PagingState::Cooldown{until: now + seconds}<br/>countdown vs AppState.now
    end
```

---

## 6. Dependency manifests

Everything pinned exact (`=`); renovate bumps them. Versions verified against
crates.io on 2026-07-30. Cargo.toml files are owned by the scaffold task and
written once with the full final set — no later task edits them (see
`docs/plan.md`, execution rules).

### 6.1 Root `Cargo.toml`

```toml
[workspace]
resolver = "3"
members = ["crates/core", "crates/ui", "crates/app"]

[workspace.package]
edition = "2024"
version = "0.1.0"
license = "MIT"
repository = "https://github.com/SpechtLabs/telegram-tui"

[workspace.dependencies]
tgt-core = { path = "crates/core" }
tgt-ui = { path = "crates/ui" }
tokio = "=1.53.1"
async-trait = "=0.1.91"
thiserror = "=2.0.19"
serde = { version = "=1.0.229", features = ["derive"] }
serde_json = "=1.0.151"
tracing = "=0.1.44"
insta = { version = "=1.48.0", features = ["json"] }

[profile.release]
lto = "thin"
strip = "debuginfo"
```

### 6.2 `crates/core/Cargo.toml`

```toml
[package]
name = "tgt-core"
edition.workspace = true
version.workspace = true

[dependencies]
tokio = { workspace = true, features = ["sync"] }
async-trait = { workspace = true }
thiserror = { workspace = true }
serde = { workspace = true }
serde_json = { workspace = true }
tracing = { workspace = true }
nucleo = "=0.5.0"
hmac = "=0.13.0"
sha2 = "=0.11.0"

[dev-dependencies]
insta = { workspace = true }
tokio = { workspace = true, features = ["sync", "macros", "rt"] }
```

No `ratatui`, no `crossterm`, no `tdlib-rs` — enforced by CI (§9.1). `nucleo`
is pure compute (fuzzy matching) and allowed in `update()`.

### 6.3 `crates/ui/Cargo.toml`

```toml
[package]
name = "tgt-ui"
edition.workspace = true
version.workspace = true

[dependencies]
tgt-core = { workspace = true }
ratatui = "=0.30.2"
crossterm = "=0.29.0"
unicode-segmentation = "=1.13.3"
unicode-width = "=0.2.2"
lru = "=0.18.1"
qrcode = "=0.14.1"
ratatui-image = { version = "=11.0.6", default-features = false, features = ["crossterm", "image-defaults"] }
image = "=0.25.10"
jiff = "=0.2.35"
serde = { workspace = true }
toml = "=1.1.4"
tracing = { workspace = true }

[dev-dependencies]
insta = { workspace = true }
```

`crossterm 0.29` is the matching pair for `ratatui 0.30`. `jiff` formats
message timestamps (`14:02`); `core` stores raw unix seconds only.

`ratatui-image` ships with default features off: its `chafa-dyn` default probes
for the system `chafa` C library at build time and fails without it, and
installing one would violate constraint 10 (no Homebrew / system packages).
Without chafa, the halfblocks fallback renders primitively — acceptable, since
the spec only requires that a placeholder fallback always exists (§8.3).
Kitty/iTerm2/Sixel protocols are unaffected.

### 6.4 `crates/app/Cargo.toml`

```toml
[package]
name = "tgt-app"
edition.workspace = true
version.workspace = true

[[bin]]
name = "tgt"
path = "src/main.rs"

[dependencies]
tgt-core = { workspace = true }
tgt-ui = { workspace = true }
async-trait = { workspace = true }
tdlib-rs = { version = "=1.4.0", default-features = false, features = ["download-tdlib"] }
tokio = { workspace = true, features = ["macros", "rt-multi-thread", "sync", "time", "signal", "process"] }
crossterm = { version = "=0.29.0", features = ["event-stream"] }
ratatui = "=0.30.2"
clap = { version = "=4.6.4", features = ["derive"] }
color-eyre = "=0.6.5"
etcetera = "=0.11.0"
keyring = "=4.1.5"
arboard = "=3.6.1"
rand = "=0.10.2"
serde = { workspace = true }
serde_json = { workspace = true }
toml = "=1.1.4"
tracing = { workspace = true }
tracing-subscriber = { version = "=0.3.23", features = ["env-filter", "registry"] }
tracing-appender = "=0.2.5"
tracing-batteries = { git = "https://github.com/sierrasoftworks/tracing-batteries-rs.git", rev = "f059e936623c2eb0ca67f6ae3301487c9443ffd0", default-features = false, features = ["opentelemetry"] }

[dev-dependencies]
insta = { workspace = true }
tempfile = "=3.27.0"
axum = "=0.8.9"
opentelemetry-proto = { version = "=0.32.0", features = ["gen-tonic-messages", "trace", "logs"] }
prost = "=0.14.4"
```

Notes:

- `keyring` 4.x has no `apple-native` cargo feature (that name never existed in
  any published version); its default `v1` feature already selects the
  macOS Keychain store (`apple-native-keyring-store`) on this platform, so the
  plain pin is correct and sufficient.
- `tracing-batteries` is not on crates.io; pinned to commit
  `f059e936623c2eb0ca67f6ae3301487c9443ffd0` (repo HEAD, 2026-07-21).
  `default-features = false` is load-bearing: the crate enables `sentry` by
  default and v1 ships exactly one egress destination (spec §13.1). If the repo
  goes stale, vendor it.
- `axum` + `opentelemetry-proto` + `prost` exist only for the CI allowlist test
  (§13.8): an in-process OTLP collector stub that decodes export requests and
  drains attribute keys.
- The vendor ingest proxy URL arrives at build time via `TGT_INGEST_ENDPOINT`;
  a build without it produces a binary whose vendor telemetry mode is inert.

### 6.5 `rust-toolchain.toml`, `.mise.toml`

```toml
# rust-toolchain.toml — authoritative for the compiler
[toolchain]
channel = "1.97.1"
components = ["rustfmt", "clippy"]
targets = ["aarch64-apple-darwin"]
```

```toml
# .mise.toml — local tooling, exact pins (repo convention)
[tools]
rust = "1.97.1"                # mirrors rust-toolchain.toml
"cargo:cargo-insta" = "1.48.0" # snapshot review workflow
```

TDLib itself arrives through the `download-tdlib` cargo feature — no Homebrew,
no system package, per constraint 10.

---

## 7. `TdRuntime` implementations

| | `TdlibRuntime` (`app/src/td_runtime.rs`) | `FakeTd` (`core/src/td/fake.rs`) |
|---|---|---|
| Transport | tdlib-rs FFI; blocking `receive(timeout)` on a dedicated `spawn_blocking` task | JSONL fixture cursor, in-memory channels |
| `request()` | serialize → tdlib send with `@extra` correlation id → await matching result → map to `TdResponse`/`TdError` | match current `Await` step → scripted response; unmatched requests get `Ok` and are logged |
| `updates()` | receiver fed by the receive task's `TdUpdate` mapping | receiver fed by `Emit` steps, drained in fixture order |
| Error mapping | `(code, message)` → `TdError`, incl. `FLOOD_WAIT_n` seconds parse | scripted `TdError` values verbatim |
| Used by | the real binary | full-app integration tests (spec §15.4) |

The mapping layer in `TdlibRuntime` is also where PII-bearing raw types die:
only `TdUpdate`/`TdResponse` (already reduced to what the app renders) cross
into `core`.

Empirical findings from T09 against TDLib ~1.8.61 / tdlib-rs 1.4.0 (binding
on later tasks):

- **`MessageCaps` is mostly unavailable on `message`.** TDLib moved
  `can_be_edited`/`can_be_deleted_*`/`can_be_forwarded` onto
  `messageProperties`, fetched per message via `getMessageProperties`; only
  `can_be_saved` still rides on the message. `TdlibRuntime` maps that one and
  defaults the rest to `false`. **T26 must add a
  `TdRequest::GetMessageProperties { chat_id, message_id }` variant (+
  matching `TdResult` completion) and fetch properties when a message is
  selected** — otherwise edit/delete/forward chips never light up. **Landed
  in T26**: §4.7 carries the request, §4.3 the completion; the `TdResponse`
  carrier and the runtime/dispatch mapping land with T32.
- **Reply excerpts are empty for same-chat replies** (TDLib only inlines
  quote/content for cross-chat replies). The conversation/selection layer
  (T16/T25/T26) fills `ReplyPreview.excerpt` from its own message window when
  the mapped excerpt is empty.
- `muted` is approximated as `!use_default_mute_for && mute_for > 0`; the
  scope-default case reads as unmuted (a second round trip would be needed
  for exactness — accepted for v1).
- `TdError::Offline` is never produced from a TDLib error string; offline-ness
  is signalled by `ConnectionPhase::WaitingForNetwork`.
- tdlib-rs handles `@extra` correlation internally; the receive loop must be
  running before any request (constructor guarantees it). `receive()` is a
  global 2 s-timeout blocking call serviced by one dedicated OS thread.

---

## 8. Decisions the spec delegated

| Decision | Choice | Rationale |
|---|---|---|
| `Action` granularity | Coarse top level (`Key`/`Paste`/`Resize`/`Tick`/`Td`/`TdResult`/`Io`), fine enums beneath; TDLib updates pre-digested into `TdUpdate`, not 1:1 raw | Core never touches tdlib-rs types, fixtures serialize cleanly, and key routing stays inside pure `update()` where it's testable. |
| Key routing location | `ui::input` is mechanical (crossterm → `Key`); all focus routing in `update()` | Spec §15.1 requires focus transitions covered by `update()` unit tests, which is only possible if routing lives in core. |
| `AppState` shape | Separate sub-state structs, one module each, thin router in `app.rs` | Disjoint file ownership is what makes parallel subagent execution safe; a flat struct funnels every task through one file. |
| Layout cache eviction | LRU bounded by **total line count** (50 000 lines), wholesale clear on width/theme change | Line count is proportional to actual memory; entry count treats a 200-line paste and a 1-line "ok" as equal. |
| Effect error propagation | Domain-specific completion actions (`TdResult::HistoryLoaded { outcome: Result<..> }`), no generic token correlation | Typed match arms with the domain context in the variant; no request-id bookkeeping inside pure state. |
| `FakeTd` fixture format | JSONL, one serde `ScriptStep` per line (`Emit` / `Await{expect, respond}`) | Line-diffable in review, streams for long sessions, reuses existing serde derives. |
| Telemetry from `update()` | `Effect::Telemetry(TelemetryEvent)`, emitted by the dispatcher via `emit!` | Keeps `update()` free of even nominally-side-effecting calls; impure layers call `emit!` directly. |
| Tick design | 250 ms housekeeping `Action::Tick { now }`; the 16 ms render coalescing gate lives in `runtime_loop`, not in the action stream | Time-dependent state (toasts, flood countdown, typing expiry) needs coarse ticks; render pacing is a loop concern, not a state concern. |
| Paging trigger unit | Within 20 **messages** of the oldest loaded (not rows) | Rows are a ui/layout concept; core cannot know them without breaking the crate boundary, and 20 messages ≥ one page of rows at any sane width. |
| Read receipts | Derived at render from `last_read_outbox`, not stored per message | One source of truth; `updateChatReadOutbox` flips one field instead of N messages. |

---

## 9. Cross-cutting enforcement

### 9.1 Crate boundary check — `scripts/check-crate-boundaries.sh`

```bash
#!/usr/bin/env bash
set -euo pipefail
fail=0
if cargo tree -p tgt-core -e normal --prefix none | grep -qE '^(ratatui|crossterm) v'; then
  echo "FORBIDDEN: tgt-core depends on ratatui/crossterm" >&2; fail=1
fi
if cargo tree -p tgt-ui -e normal --prefix none | grep -qE '^tdlib-rs v'; then
  echo "FORBIDDEN: tgt-ui depends on tdlib-rs" >&2; fail=1
fi
exit "$fail"
```

Runs in CI on every push and locally via the milestone gates in `docs/plan.md`.

### 9.2 macOS dylib `@rpath` — solved in milestone 1, mechanism

`download-tdlib` produces a dynamic `libtdjson.dylib`. Two consumers, one
mechanism, implemented in `crates/app/build.rs`:

1. **Link args** (both emitted unconditionally):
   `cargo:rustc-link-arg=-Wl,-rpath,@executable_path` and
   `cargo:rustc-link-arg=-Wl,-rpath,@executable_path/../lib`.
2. **Dev builds:** `build.rs` locates the downloaded dylib under
   `target/<profile>/build/tdlib-rs-*/out/**/libtdjson*.dylib` and copies it
   next to the binary (`target/<profile>/`), where the first rpath finds it.
   Empirical findings from T04 that T56 must respect: the downloaded archive
   contains both `libtdjson.dylib` and a versioned `libtdjson.<ver>.dylib`
   (e.g. `1.8.61`), and the dylib's `LC_ID_DYLIB` is the **versioned** name —
   so the versioned filename is what the linked binary's `LC_LOAD_DYLIB`
   references and what must exist next to the binary (dev) or in `../lib`
   (packaged). Also, cargo does not propagate `rustc-link-arg` from a
   dependency's build script, so tdlib-rs's own absolute rpath never reaches
   `tgt`; the copy step is load-bearing, not defensive.
3. **Packaged builds:** `scripts/package.sh` lays out `dist/tgt/bin/tgt` +
   `dist/tgt/lib/libtdjson.dylib`, runs
   `install_name_tool -id @rpath/libtdjson.dylib dist/tgt/lib/libtdjson.dylib`
   (and `-change` on the binary if the recorded install name is absolute), then
   verifies with `otool -L` and by **executing the binary from a moved
   directory**.

Acceptance is behavioral, not structural: `cargo run -p tgt-app -- --version`
must work in dev, and the packaged binary must run after `mv dist /tmp/else`.

### 9.3 Purity and I/O rules

- `update()` and everything it calls: no I/O, no spawning, no `Instant::now()`,
  no RNG. Clippy plus code review enforce; the practical guard is that `core`
  has no dependency capable of I/O beyond `tokio/sync`.
- Nothing writes to stdout/stderr while the TUI is active. The file logger is
  installed before terminal raw mode; the panic hook (`app/src/panic.rs`)
  leaves the alternate screen and disables raw mode **before** the panic
  message prints. TDLib's own logging is redirected to file via its log stream
  option inside `TdlibRuntime`.
- `session.shutdown()` for telemetry is wrapped in
  `tokio::time::timeout(Duration::from_secs(2), ...)` — quitting is never
  hostage to a retrying exporter.

### 9.4 Test topology (spec §15)

| Layer | Where | Backend |
|---|---|---|
| `update()` unit tests | `crates/core/src/state/*` (inline `#[cfg(test)]`) | none |
| `message_layout` + offsets + wrap | `crates/ui/src/render/*` inline tests | none |
| Frame snapshots | `crates/ui/tests/snapshots.rs` | `ratatui::backend::TestBackend` + insta, widths 80/100/140 |
| Full-app integration | `crates/app/tests/*.rs` | `FakeTd` + JSONL fixtures in `crates/app/tests/fixtures/` |
| Telemetry allowlist | `crates/app/tests/telemetry_allowlist.rs` | axum OTLP stub + `opentelemetry-proto` decode |
| Crate boundaries | `scripts/check-crate-boundaries.sh` | cargo tree |
