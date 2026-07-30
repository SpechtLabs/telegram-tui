# telegram-tui — Design Specification

**Date:** 2026-07-30
**Status:** Approved for planning
**Repo:** `github.com/SpechtLabs/telegram-tui`
**Binary:** `tgt`

---

## 1. Summary

A modern, keyboard-driven terminal client for Telegram, written in Rust on top of
TDLib and ratatui. The interaction model is deliberately shallow: arrow keys move,
Enter selects, and every contextual action is displayed as a labeled chip rather
than hidden behind a memorized chord. The visual target is a crafted TUI in the
spirit of Claude Code and Codex CLI, not a terminal log viewer.

### Goals

- Usable as a daily-driver Telegram client for text conversations.
- Discoverable without documentation. A new user should need only arrow keys,
  Enter, and Escape.
- Visually deliberate: consistent spacing, semantic color, and grouping that reads
  as designed rather than decorated.
- Fully instrumented with OpenTelemetry, with a structurally enforced guarantee
  that no personally identifiable information leaves the machine.

### Non-goals for v1

- Multiple simultaneous accounts.
- Windows and Linux support (architecture must not preclude them; they are simply
  not tested or shipped in v1).
- Voice and video calls.
- Secret chats. TDLib is configured with `use_secret_chats = false`.
- Desktop notifications via the OS notification center.

---

## 2. Platform and toolchain

| Concern | Decision |
|---|---|
| Target platform (v1) | macOS on Apple Silicon (`aarch64-apple-darwin`) |
| Rust edition | 2024, pinned via `rust-toolchain.toml` |
| Local tool management | `mise`, per repository convention. Pin exact versions. |
| TDLib acquisition | `tdlib-rs` with the `download-tdlib` feature |

`tdlib-rs` v1.4.0 exposes a `download-tdlib` feature that fetches prebuilt TDLib
binaries from GitHub releases at build time. This is the default for development
and CI. It avoids requiring cmake, gperf, and a C++ build, and it avoids
installing TDLib through a system package manager.

The `pkg-config` and `local-tdlib` features remain available for downstream
packagers who prefer a system TDLib.

**Known consequence:** the prebuilt TDLib is a dynamic library. Producing a
relocatable binary requires `@rpath` handling (`install_name_tool`, or a
`build.rs` that emits `cargo:rustc-link-arg=-Wl,-rpath,@executable_path/../lib`).
This must be solved before the first distributable release, and is called out as
a milestone deliverable rather than left to discovery.

---

## 3. Workspace layout

```
telegram-tui/
├── crates/
│   ├── core/          domain model, state, update(), TDLib boundary
│   ├── ui/            ratatui widgets, layout, theme, input mapping
│   └── app/           binary `tgt`: composition root, config, CLI
├── docs/
├── .mise.toml
└── rust-toolchain.toml
```

The dependency direction is enforced and non-negotiable:

- `core` **must not** depend on `ratatui` or `crossterm`.
- `ui` **must not** depend on `tdlib-rs`.
- `app` depends on both and wires them together.

`ui` consumes only plain data types re-exported from `core`. This seam is what
makes `core` testable without a terminal and `ui` testable without a network or a
Telegram account. A CI check asserts the forbidden dependencies are absent.

---

## 4. Runtime architecture

### 4.1 The Elm Architecture over a single action channel

Every input to the system — keystrokes, TDLib updates, file download progress,
timer ticks, telemetry flush signals — is normalized into a single `Action` enum
delivered over one `tokio::sync::mpsc` channel. There is exactly one owner of
application state. There are no locks and no `Arc<RwLock<AppState>>`.

```rust
pub fn update(&mut self, action: Action) -> Vec<Effect>;
```

`update` mutates in-memory state and *returns descriptions* of side effects
rather than performing them. It performs no I/O, spawns no tasks, and touches no
clock or RNG directly (both are injected). It is therefore a deterministic
function of `(state, action)`, which makes the entire application logic testable
by feeding a scripted sequence of actions and asserting on the result.

Main loop:

```rust
loop {
    tokio::select! {
        Some(a)  = actions.recv()      => effects.extend(app.update(a)),
        Some(ev) = term_events.next()  => {
            if let Some(a) = app.map_input(ev) { effects.extend(app.update(a)) }
        }
    }
    for eff in effects.drain(..) {
        runtime.dispatch(eff);          // async; results return as Actions
    }
    if app.take_dirty() {
        terminal.draw(|f| ui::view(&app, f))?;
    }
}
```

### 4.2 Redraw policy

Rendering is dirty-flag driven and coalesced. A 16 ms tick batches bursts of
updates so that a busy group chat cannot drive the render loop faster than
60 fps. `SIGWINCH` invalidates the layout cache and forces a full redraw.

### 4.3 The TDLib boundary

```rust
#[async_trait]
pub trait TdRuntime: Send + Sync {
    async fn request(&self, req: TdRequest) -> Result<TdResponse, TdError>;
    fn updates(&self) -> mpsc::Receiver<TdUpdate>;
}
```

Two implementations:

- `TdlibRuntime` — wraps `tdlib-rs`. Owns the client id. TDLib's `receive()` is a
  blocking C call with a timeout, so it runs on a dedicated `spawn_blocking` task
  that forwards updates into the action channel.
- `FakeTd` — replays recorded update sequences from fixture files. Used for
  integration tests of the full application without a network or an account.

All TDLib access funnels through this trait. No other module calls `tdlib-rs`.

---

## 5. Domain state

### 5.1 Chat list ordering

Chat ordering **mirrors TDLib** and is never computed locally. TDLib supplies an
`order: i64` per chat per chat list via `updateChatPosition`. The client maintains
a sorted set keyed on `(order DESC, chat_id DESC)` and reacts to:

- `updateNewChat`
- `updateChatPosition`
- `updateChatLastMessage`
- `updateChatReadInbox` / `updateChatReadOutbox`
- `updateChatTitle`, `updateChatPhoto`, `updateChatNotificationSettings`

Rationale: any locally-invented ordering will drift from every other Telegram
client the user runs, and the divergence is invisible until it is confusing.

### 5.2 History paging

`core/state/history.rs` owns a per-chat state machine: `Idle | Loading |
Exhausted`.

**Critical TDLib behavior that must be encoded:** `getChatHistory` returns
messages from the local database first. On the first call for a chat it may
return **zero messages** while it fetches from the server, even though more
history exists. A short or empty response is therefore *not* proof of
end-of-history. The implementation must re-issue the request (bounded retry, with
`only_local = false`) and only transition to `Exhausted` when TDLib confirms it.

Paging triggers when the viewport is within N rows of the top of the loaded
window. Loaded windows are bounded; messages far outside the viewport are evicted
to keep memory flat in long-lived sessions.

### 5.3 Message capabilities

Per-message action availability is read directly from TDLib's flags —
`can_be_edited`, `can_be_deleted_for_all_users`, `can_be_deleted_only_for_self`,
`can_be_forwarded`, `can_be_saved`. Chips are derived from these flags and never
hardcoded, so an action that would fail is never offered.

---

## 6. Interaction model

### 6.1 Responsive layout

Two-pane above **100 columns**, single-pane stack below. Both modes are rendered
by the same view components with a different arrangement; the stack is not a
second implementation. The breakpoint is configurable.

**Two-pane (≥ 100 cols):**

```
┌ telegram-tui ───────────────────────────────── cedi@specht ─┐
│ CHATS          │ Alice Müller                    online     │
│────────────────┼────────────────────────────────────────────│
│▸ Alice      2  │  Alice · 14:02                             │
│  Team Rust  9  │  ▏hey, did you see the PR?                 │
│  Mom           │                                            │
│  #rust-de   1  │                        You · 14:03         │
│  Bob           │            yeah, reviewing now ▏        ✓✓ │
│  Archived  12  │                                            │
│                │  ╭──────────────────────────────────────╮  │
│                │  │ ›  message…                          │  │
│                │  ╰──────────────────────────────────────╯  │
├────────────────┴────────────────────────────────────────────┤
│ ↑↓ move   ⏎ open   ctrl+p palette   ? help                  │
└─────────────────────────────────────────────────────────────┘
```

**Single-pane stack (< 100 cols):** full-width chat list → Enter → full-width
conversation → Esc back, with a breadcrumb header (`telegram ▸ Alice Müller`).

### 6.2 Focus and key routing

Focus is a stack. Keys route top-down: **modal → focused pane → global**. The
first handler to claim a key stops propagation.

| Context | Keys |
|---|---|
| Global | `ctrl+p` palette · `?` help · `ctrl+c` quit |
| Pane movement | `←` `→` move focus, `tab` / `shift+tab` cycle |
| Chat list | `↑` `↓` move · `⏎` open · `/` filter |
| Composer | typing · `⏎` send · `alt+⏎` newline · `↑` on empty input enters selection mode |
| Selection mode | `↑` `↓` message · `←` `→` action chip · `⏎` invoke · letter shortcut · `esc` back to composer |
| Modal | `esc` dismiss · `⏎` confirm |

`Esc` always pops exactly one level of the focus stack, never more. There is no
key that requires a modifier chord beyond `ctrl+p` and `ctrl+c`.

### 6.3 Message selection and action chips

Entering selection mode highlights the newest message and replaces the hint bar
with a row of action chips. Each chip's leading letter is its shortcut, rendered
in the accent color. The focused chip is highlighted; `←` `→` move between chips
and `⏎` invokes the focused one. Both driving modes are always live
simultaneously.

```
│ ╭─ selected ────────────────────────────────────────────╮ │
│ │ Alice · 14:05                                         │ │
│ │ take your time, no rush at all                        │ │
│ ╰───────────────────────────────────────────────────────╯ │
├───────────────────────────────────────────────────────────┤
│ ‹ [R Reply] [F Forward] [E React] [C Copy] [D Delete] ›    │
└───────────────────────────────────────────────────────────┘
```

Chips exceeding the available width scroll horizontally with `‹` `›` affordances.
The chip set is computed per-message from TDLib capability flags (§5.3).

Destructive actions require confirmation. `Delete` opens a modal offering
"Delete for me" and, when `can_be_deleted_for_all_users` is set, "Delete for
everyone".

### 6.4 Notifications

No OS notification center integration in v1. Alerting is in-app plus a terminal
escape sequence, which also means it works correctly over SSH.

- **In-app:** unread and mention badges on sidebar rows, chat list reordering, and
  a transient toast in the lower-right for messages arriving in a chat other than
  the focused one. Toasts stack to a maximum of three, expire after 4 seconds, and
  are dismissible with `esc`.
- **Terminal:** emit `OSC 777 ; notify ; <title> ; <body>` where supported, and
  fall back to `BEL` otherwise, letting the terminal emulator or multiplexer
  (tmux, WezTerm, Ghostty) decide how to alert. **The body must never contain
  message text, sender names, or chat titles** — it carries only a generic
  string such as `New message`. The same PII discipline that governs telemetry
  (§13.2) applies here, because a terminal notification can be logged by the
  multiplexer or displayed on a shared screen.

Telegram's own per-chat mute settings (`updateChatNotificationSettings`) are
respected: a muted chat updates its badge but never produces a toast or a bell.
Alerting is suppressed entirely for the currently focused chat.

---

## 7. Visual design

### 7.1 Message rendering: grouped accent-rail

No per-message boxes. Each block carries a colored rail; sender and timestamp
form a light header; consecutive messages from the same sender within a short
window group under one header. Own messages are right-aligned with a dim rail.

```
│  Alice Müller · 14:02                                   │
│  ▏hey, did you see the PR?                              │
│  ▏also — CI is red on main, `cargo test` blows up       │
│                                                         │
│                                        You · 14:03      │
│                        yeah, reviewing it now ▏         │
│                                            ✓✓           │
│                                                         │
│  Alice Müller · 14:05                                   │
│  ▏↳ yeah, reviewing it now                              │
│  ▏take your time 🙏                                     │
│                                                         │
│  Bob · 14:11                                            │
│  ▏📎 architecture.pdf · 2.4 MB · ⏎ download             │
```

This costs zero extra rows per message, which matters in fast-moving group chats.
Per-sender accent colors are derived deterministically from the sender id against
a curated palette, so the same person is the same color across sessions.

Reply quotes render as a single dimmed `↳` line above the message body,
truncated to one line, and are selectable to jump to the original.

### 7.2 Theme tokens

Colors are never written at call sites. A `Theme` struct exposes semantic tokens
(`accent`, `accent_dim`, `text`, `text_muted`, `surface`, `surface_raised`,
`success`, `warning`, `danger`, `selection`, `rail_own`, `rail_other`). Built-in
themes ship as TOML; users may supply their own file. Truecolor is used when
available with a defined 256-color degradation path.

A `theme_generation` counter increments on theme change and participates in the
render cache key (§8.2).

---

## 8. Rendering internals

### 8.1 Message layout engine — `ui/render/message_layout.rs`

The single trickiest unit in the codebase, therefore fully isolated as a pure
function with no dependency on application state:

```rust
pub fn layout_message(msg: &MessageView, width: u16, theme: &Theme) -> Vec<Line<'static>>;
```

Two hazards it exists to contain:

1. **Telegram entity offsets are UTF-16 code units.** They must be converted to
   byte offsets before slicing Rust strings. Any message containing an emoji or a
   non-BMP character before a styled span will mis-slice otherwise. This is the
   single most common correctness bug in third-party Telegram clients.
2. **Width is not character count.** Wrapping must be grapheme-cluster aware
   (`unicode-segmentation`) and width-aware (`unicode-width`), or emoji, CJK, and
   combining marks will break column alignment.

Supported entities: bold, italic, underline, strikethrough, spoiler, code, pre
(with language label), blockquote, text_url, url, mention, hashtag. Spoilers
render as a filled block until revealed with `⏎` on the selected message.

### 8.2 Layout cache — `ui/render/cache.rs`

Laid-out lines are cached keyed on `(message_id, width, theme_generation,
revealed_spoilers)`. Without this, scrolling re-wraps and re-styles hundreds of
messages every frame. The cache is an LRU bounded by total line count and is
cleared wholesale on width or theme change.

### 8.3 Inline images

Detect terminal graphics support at startup (Kitty graphics protocol, iTerm2,
Sixel) via `ratatui-image`. Where supported, photos render inline at a bounded
height. Where not, they render as the placeholder card shown in §7.1.

Images are only rendered for already-downloaded files; an undownloaded photo
shows a placeholder with a download affordance. Graphics-protocol cells must be
invalidated on scroll to avoid ghosting.

---

## 9. Authentication

The auth UI is a **projection of TDLib's `updateAuthorizationState`**, not a
parallel state machine. The wizard renders whatever state TDLib reports.

### 9.1 Credentials

`api_id` / `api_hash` are supplied by the user. On first run, when no credentials
are configured, a wizard screen explains the my.telegram.org → API development
tools flow, accepts the pair, and writes it to config. Environment overrides
`TELEGRAM_API_ID` / `TELEGRAM_API_HASH` are honored.

No credentials are compiled into the binary.

### 9.2 Login paths

Both are offered on one screen; the user picks.

- **Phone** → `setAuthenticationPhoneNumber` → code entry
  (`authorizationStateWaitCode`, showing the delivery method TDLib reports) →
  optional `authorizationStateWaitPassword` for 2FA, with the password hint
  displayed.
- **QR** → `requestQrCodeAuthentication` →
  `authorizationStateWaitOtherDeviceConfirmation { link }` → render the link as a
  QR code using the `qrcode` crate drawn with Unicode half-blocks, sized to fit
  the viewport. Falls back to displaying the raw link if the terminal is too
  small. The QR refreshes when TDLib issues a new link.

Error states (`PHONE_NUMBER_INVALID`, `PHONE_CODE_INVALID`, `FLOOD_WAIT_n`) are
surfaced inline on the relevant field, with flood-wait rendered as a live
countdown rather than an opaque error.

### 9.3 Storage

- TDLib database: `~/.local/share/telegram-tui/td/`, created mode `0700`.
- `database_encryption_key`: random 32 bytes on first run, stored in the macOS
  Keychain via the `keyring` crate. Never written to disk in plaintext.
- `use_secret_chats = false`, `use_message_database = true`,
  `use_chat_info_database = true`, `use_file_database = true`.

---

## 10. Media

**Receiving.** `downloadFile` with a priority derived from viewport proximity.
`updateFile` progress becomes an `Action` and drives a progress bar on the
message. Completion is signalled by `local.is_downloading_completed`; the local
path is then usable. Opening hands off to macOS `open`.

**Sending.** A `/send <path>` composer command and a palette action opening a
small file browser. Terminals paste dropped files as plain text paths, so the
composer detects a bare existing path and offers to send it. Uploads render as a
pending message with a progress bar and are cancellable.

MIME type determines whether TDLib is asked to send a photo, video, audio, or
document.

---

## 11. Search and the command palette

`ctrl+p` opens a centered palette with fuzzy matching over a unified result set,
using the `nucleo` crate:

- Chats, ranked by match score then recency.
- Commands (toggle theme, settings, telemetry, log out, quit).

In-chat message search is a separate mode bound to `/` while the message list is
focused, backed by `searchChatMessages`, with `n` / `N` to step between hits and
the matched range highlighted.

Sidebar organization surfaces pinned chats above the list, unread and mention
badges per row, the archived folder as a pseudo-row, and Telegram chat folders as
switchable chat lists.

---

## 12. Configuration

TOML at `~/.config/telegram-tui/config.toml`, resolved via `etcetera` with an
`XDG_CONFIG_HOME` override. Generated on first run with comments.

```toml
[app]
theme = "default"
layout_breakpoint_cols = 100

[keys]
palette = "ctrl+p"

[telemetry]
mode = "vendor"        # "vendor" | "custom" | "off"
# endpoint = "https://otlp.example.com"
# protocol = "http/protobuf"
# [telemetry.headers]
# Authorization = "Basic …"
```

Unknown keys produce a warning rather than a hard failure, so a config written by
a newer version does not brick an older binary.

---

## 13. Observability

### 13.1 Stack

`tracing-batteries` from SierraSoftworks, as a **git dependency** — it is not
published to crates.io.

```toml
tracing-batteries = { git = "https://github.com/sierrasoftworks/tracing-batteries-rs.git",
                      default-features = false, features = ["opentelemetry"] }
```

`default-features = false` is required: the crate enables `sentry` by default and
v1 ships exactly one egress destination.

```rust
let session = Session::new("telegram-tui", env!("CARGO_PKG_VERSION"))
    .with_context("install.id", install_id)
    .with_battery(
        OpenTelemetry::new(endpoint)
            .with_protocol(OpenTelemetryProtocol::HttpProtobuf)
            .with_header("x-tgt-client", env!("CARGO_PKG_VERSION")),
    );
```

### 13.2 Allowlist, not denylist

In a Telegram client nearly every interesting value is PII — message text, chat
titles, display names, phone numbers, file names. A rule of "remember not to log
PII" fails eventually. The guarantee is therefore structural.

- `core/telemetry/schema.rs` defines the **complete** set of permitted event names
  and attribute keys as constants.
- A `telemetry::emit!` macro is the only path to the remote exporter. It sets a
  `telemetry.public = true` marker field.
- The OTLP `tracing_subscriber` layer filters on that marker. Any event without
  it is dropped before export.

A stray `tracing::info!("opening chat {}", chat.title)` therefore cannot reach the
network. It lands only in the local log.

### 13.3 Two sinks

| Sink | Contents | Leaves machine |
|---|---|---|
| Rolling file log, `~/.local/state/telegram-tui/` | Full debug detail; may contain chat ids and titles | Never |
| OTLP exporter | Allowlisted keys only | Yes |

The local log stays rich because richness is what makes a bug tractable. The wire
stays clean because it must.

Nothing is ever written to stdout or stderr while the TUI is active; doing so
corrupts the display.

### 13.4 Permitted attributes

```
app.version            os.version              term.program
term.graphics_protocol {kitty|iterm2|sixel|none}
term.width_bucket      {<80|80-120|120-160|>160}
install.id             session.id
action                 {message.send|message.reply|message.forward|message.delete|
                        message.edit|chat.open|palette.open|search.run|qr_login|
                        file.download|file.upload|theme.change|…}
outcome                {ok|error|cancelled}
error.kind             {td.flood_wait|td.auth|td.rate_limit|net.timeout|
                        net.offline|layout.panic|io.denied|…}
duration_ms            chat.kind {private|group|supergroup|channel}
history.page_depth     download.size_bucket
```

**Never exported:** message text or its length, display names, usernames, phone
numbers, chat titles, file names, entity contents, raw Telegram identifiers.

Where correlation genuinely helps — for example diagnosing repeated history-paging
failures on one specific chat — identifiers are exported as
`HMAC-SHA256(id, per_install_salt)` truncated to 8 bytes. The salt is generated
locally and never transmitted, making the value stable within an install,
uncorrelatable across installs, and irreversible.

### 13.5 Consent and control

`install.id` is a pseudonymous identifier, so it is disclosed rather than buried.

A dedicated first-run screen, shown **before login and before any data is sent**,
states in plain language what is collected, what is not, and where it goes, with
Enable (preselected) and Disable. Acknowledgement is required to proceed.

Controls:

- `telemetry.mode` in config
- `--no-telemetry` flag
- `TELEGRAM_TUI_TELEMETRY=off`
- `tgt telemetry reset-id` regenerates `install.id` and the HMAC salt
- `tgt telemetry show` prints exactly what would be sent
- `DO_NOT_TRACK=1` is honored

Under `mode = "custom"` the user's endpoint, protocol, and headers **fully
replace** the vendor destination. Data is never dual-shipped. Standard
`OTEL_EXPORTER_OTLP_ENDPOINT` / `_HEADERS` / `_PROTOCOL` environment variables are
honored, as `tracing-batteries` already reads them.

### 13.6 Vendor ingest

The default endpoint is an **ingest proxy** operated by the project, which holds
the real Grafana Cloud credentials and forwards OTLP. No secret is compiled into
the binary. The proxy allows rate limiting, abuse rejection, and credential
rotation without shipping a release.

The proxy URL is set at build time via an environment variable. A build without
it produces a binary whose vendor mode is inert — it collects nothing until the
user configures their own endpoint.

### 13.7 The exporter must never hurt the TUI

- Bounded queue with drop-on-full. Telemetry backpressure never reaches the
  render loop.
- Export runs on its own task; no telemetry call blocks `update()` or `view()`.
- `session.shutdown()` is wrapped in a hard **2-second** timeout. A chat client
  that takes four seconds to quit because an exporter is retrying is worse than
  one with no telemetry at all.
- Failure to reach the endpoint is logged locally at debug level and never
  surfaced to the user.

### 13.8 Proving it

- A CI test boots the app against a local OTLP collector stub, drains every
  exported attribute key across a scripted session, and asserts the set is a
  **subset** of the allowlist. The test fails on any unknown key.
- The allowlist is `insta`-snapshotted, so adding a field appears in review as a
  deliberate diff rather than slipping in unnoticed.
- A unit test asserts that an event emitted via raw `tracing::` macros does not
  reach the OTLP layer.

This turns "no PII" from an intention into a property with a failing test behind
it.

---

## 14. Errors, logging, resilience

- `thiserror` for typed errors in `core`; `color-eyre` for the binary.
- TDLib errors arrive as `(code, message)` pairs and are mapped into a typed
  `TdError` with named variants for the cases that need distinct handling —
  notably `FloodWait { seconds }`, which drives a visible countdown rather than a
  generic failure.
- A panic hook restores the terminal (leaves alternate screen, disables raw mode)
  **before** printing. A TUI that panics with raw mode still enabled leaves the
  user with an unusable shell.
- `updateConnectionState` renders as a header indicator, so
  "connecting…" / "updating…" is visible rather than manifesting as mysterious
  silence.
- Send failures leave the message in the composer rather than discarding typed
  text.

---

## 15. Testing strategy

Four layers, each cheap because of the seams established above.

1. **`update()` unit tests.** Action in, state asserted out. No terminal, no
   network. Covers focus transitions, selection mode, chip derivation, paging
   state machine, auth state projection.
2. **`message_layout` tests.** UTF-16 entity offsets, emoji, CJK, RTL, combining
   marks, nested entities, code blocks, spoilers, pathological widths (1 column),
   and messages consisting solely of an emoji.
3. **Frame snapshot tests.** `ratatui::backend::TestBackend` plus `insta`, over a
   fixed set of fabricated states at several widths, including both sides of the
   100-column breakpoint. This is what keeps the visual design from silently
   regressing.
4. **Full-app integration tests** against `FakeTd` replaying recorded update
   sequences: cold start → auth → chat list populate → open chat → page history →
   send → receive → error injection.

Plus the telemetry allowlist test (§13.8) and the crate-boundary dependency check
(§3).

---

## 16. Milestones

Each milestone is independently demonstrable.

| # | Milestone | Contents |
|---|---|---|
| 1 | Skeleton | Workspace, mise toolchain, `tdlib-rs` linking and `@rpath` resolved, TEA loop, empty ratatui shell, panic hook, file logging |
| 2 | Auth | `TdRuntime` trait + real impl, auth state projection, credential wizard, phone and QR login, Keychain-backed DB key |
| 3 | Read-only client | Chat list with TDLib ordering, conversation view, grouped accent-rail rendering, message layout engine, layout cache, history paging |
| 4 | Interaction | Focus stack, keymap, selection mode, action chips, reply/forward/delete/edit/copy, composer, responsive breakpoint |
| 5 | Rich content | Entity styling, reply quotes, reactions, read receipts, typing indicators, presence |
| 6 | Media | Download with progress, placeholder cards, inline images with protocol detection, sending files |
| 7 | Search | `ctrl+p` palette, in-chat search, unread badges, pinned, archive, folders |
| 8 | Observability | `tracing-batteries` wiring, allowlist schema and macro, consent screen, config modes, CI allowlist test |
| 9 | Polish | Theme file loading, help overlay, snapshot test suite, distributable binary |

Milestone 8 may be pulled earlier if usage insight during development is wanted;
the allowlist macro should exist from milestone 1 so instrumentation is added as
code is written rather than retrofitted.

---

## 17. Risks

| Risk | Mitigation |
|---|---|
| TDLib dylib relocation on macOS | Solved explicitly in milestone 1, not deferred |
| `tracing-batteries` is an unpinned git dependency | Pin to a specific commit; vendor if it goes stale |
| UTF-16 entity offset bugs | Isolated pure function with exhaustive tests |
| Graphics protocol ghosting on scroll | Explicit invalidation; placeholder fallback always available |
| Extractable ingest credentials | Solved by the proxy design; no secret in the binary |
| Telemetry perceived as sneaky | Mandatory first-run disclosure, `DO_NOT_TRACK`, `tgt telemetry show` |
| Retrofitting multi-account | Accepted. Contained to going from one TDLib actor to N |
