# telegram-tui

A keyboard-driven terminal client for Telegram, written in Rust on top of TDLib
and [ratatui](https://ratatui.rs). The binary is called `tgt`.

The interaction model is deliberately shallow. Arrow keys move, Enter selects,
Escape goes back one level, and every contextual action on a message shows up as
a labeled chip instead of hiding behind a chord you had to read about first. The
only modifiers in the whole application are `ctrl+p` for the command palette and
`ctrl+c` to quit. If you can drive a file manager, you can drive this.

v1 targets macOS on Apple Silicon (`aarch64-apple-darwin`). Nothing in the
architecture forecloses Linux or Windows; they're just not built or tested yet.

## Status

Early development. `tgt` does not talk to Telegram yet, cannot log you in, and
is not usable as a client. Don't install this expecting to read messages with it.

Milestone 1, the skeleton, is nearly finished: the workspace, the domain model,
the TDLib boundary types, the telemetry allowlist and `emit!` macro, the macOS
dylib rpath handling, and the ratatui shell have all landed. The binary's main
loop and terminal setup are the piece being wired up right now.

| # | Milestone | State |
|---|---|---|
| 1 | Skeleton: workspace, TDLib linking, TEA loop, empty shell | in progress |
| 2 | Auth: credential wizard, phone and QR login, Keychain-backed DB key | not started |
| 3 | Read-only client: chat list, conversation view, message layout, paging | not started |
| 4 | Interaction: focus stack, selection mode, chips, composer | not started |
| 5 | Rich content: entities, reply quotes, reactions, receipts, presence | not started |
| 6 | Media: downloads with progress, inline images, sending files | not started |
| 7 | Search: `ctrl+p` palette, in-chat search, badges, archive, folders | not started |
| 8 | Observability: exporter wiring, consent screen, CI allowlist proof | not started |
| 9 | Polish: theme files, help overlay, snapshot suite, packaged binary | not started |

Full task breakdown lives in [`docs/plan.md`](docs/plan.md).

## What it's meant to look like

This is the design target from the spec, hand-drawn. It is not a screenshot, and
the code does not render this yet.

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

Pressing `↑` on an empty composer enters selection mode, which highlights a
message and swaps the hint bar for a chip row like
`‹ [R Reply] [F Forward] [E React] [C Copy] [D Delete] ›`. Each chip's leading
letter is its shortcut, and `←` `→` `⏎` work at the same time, so you can learn
the letters without ever being forced to. Which chips appear is derived from
TDLib's per-message capability flags, so an action that would fail is never
offered. Below 100 columns the two-pane layout collapses into a single-pane
stack rendered by the same view components.

## Requirements

- macOS on Apple Silicon.
- Your own Telegram `api_id` and `api_hash` from
  [my.telegram.org](https://my.telegram.org) under API development tools. No
  credentials are compiled into the binary; a first-run wizard will walk through
  getting them, and `TELEGRAM_API_ID` / `TELEGRAM_API_HASH` override the config
  file.
- [mise](https://mise.jdx.dev) for local tooling.

## Building

```sh
mise install          # rust 1.97.1, cargo-insta 1.48.0 (exact pins in .mise.toml)
cargo build --workspace
```

That's the whole setup. TDLib arrives through `tdlib-rs`'s `download-tdlib`
feature, which fetches a prebuilt binary from GitHub releases during the build:
no Homebrew, no system TDLib, no cmake and gperf and a C++ toolchain. The first
build downloads it and takes a while; later builds don't. `rust-toolchain.toml`
pins the compiler to 1.97.1 (edition 2024) independently of mise, so `cargo`
picks the right one even outside a mise shell.

The prebuilt TDLib is a dynamic library, so `crates/app/build.rs` emits
`@executable_path` and `@executable_path/../lib` rpaths and copies
`libtdjson.dylib` next to the dev binary. This is handled in milestone 1 rather
than discovered later during packaging.

Four commands have to pass before anything merges:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
./scripts/check-crate-boundaries.sh
```

CI runs the same four on `macos-15`.

## Architecture

Three crates, with a dependency direction that a CI script enforces rather than
a convention that a code review might catch:

| Crate | Contains |
|---|---|
| `tgt-core` | Domain model, `AppState`, the pure `update()`, TDLib boundary types, `FakeTd`, telemetry schema |
| `tgt-ui` | ratatui views, the message layout engine, layout cache, theme, crossterm-to-`Key` mapping |
| `tgt-app` | Binary `tgt`: config, CLI, main loop, effect dispatcher, `TdlibRuntime`, logging and OTLP wiring |

`tgt-core` must not depend on `ratatui` or `crossterm`; `tgt-ui` must not depend
on `tdlib-rs`. `scripts/check-crate-boundaries.sh` greps `cargo tree` for both
and fails the build if either shows up. That seam is what makes the domain
testable without a terminal and the rendering testable without a network or a
Telegram account.

Everything runs as the Elm architecture over one `tokio::sync::mpsc` channel.
Keystrokes, TDLib updates, download progress, timer ticks: all of it normalizes
into a single `Action` enum with exactly one owner of application state, so
there are no locks and no `Arc<RwLock<AppState>>` anywhere.

```rust
pub fn update(&mut self, action: Action) -> Vec<Effect>;
```

`update()` mutates in-memory state and returns *descriptions* of side effects
instead of performing them. It does no I/O, spawns nothing, and reads neither
the clock nor an RNG; time arrives as `Action::Tick { now }`. That makes it a
deterministic function of `(state, action)`, and it makes the application logic
testable by feeding a scripted sequence of actions and asserting on what comes
out. Rendering is dirty-flag driven behind a 16 ms gate, so a fast-moving group
chat can't drive the draw loop past 60 fps.

TDLib access funnels through one `TdRuntime` trait with two implementations:
`TdlibRuntime`, which wraps `tdlib-rs` and is the only module in the workspace
that imports it, and `FakeTd`, which replays recorded update sequences from JSONL
fixtures so the whole application can be integration-tested offline.

[`docs/architecture.md`](docs/architecture.md) has the module map, the
load-bearing type definitions, and the sequence diagrams.

## Telemetry and privacy

In a Telegram client, nearly every interesting value is personal data: message
text, chat titles, display names, phone numbers, file names. A rule that says
"remember not to log PII" will hold until the evening someone adds a debug line
during a bug hunt. So the guarantee here is structural rather than a policy.

`crates/core/src/telemetry/schema.rs` declares the complete set of permitted
event names and attribute keys as constants. An `emit!` macro is the only path to
the remote exporter; it tags each event with `telemetry.public = true` on the
`tgt_telemetry` target, and the OTLP subscriber layer exports nothing that lacks
both markers. A stray `tracing::info!("opening chat {}", chat.title)` therefore
cannot reach the network no matter who writes it or when. It lands in the local
log and stops there.

Two sinks, with different rules. The rolling file log under
`~/.local/state/telegram-tui/` keeps full debug detail, including chat ids and
titles, because that richness is what makes a bug tractable; it never leaves the
machine. The OTLP exporter carries allowlisted keys only: app and OS version,
terminal program, graphics protocol, a bucketed terminal width, install and
session id, the action name, an outcome, an error kind, a duration, and the chat
*kind* (private, group, supergroup, channel). Message text and its length,
display names, usernames, phone numbers, chat titles, file names, and raw
Telegram identifiers are all absent from the allowlist, which is the reason they
can't be exported.

Where correlation genuinely helps, say diagnosing repeated history-paging
failures against one specific chat, an identifier is exported as
`HMAC-SHA256(id, salt)` truncated to 8 bytes. The salt is generated locally and
never transmitted, so the value is stable within one install, uncorrelatable
across installs, and irreversible.

The same discipline covers terminal notifications. `tgt` emits
`OSC 777 ; notify` where the terminal supports it and falls back to `BEL`, but
the body is a fixed generic string; the `Effect::Alert` variant carries no
payload at all, so there is nothing for a sender name or message text to ride on
into a multiplexer's log or onto a shared screen.

How this gets proven, once milestone 8 lands: a CI test boots the application
against an in-process OTLP collector stub, drains every exported attribute key
across a scripted session, and fails on any key outside the allowlist. The
allowlist itself is `insta`-snapshotted, so adding a field shows up in review as
a deliberate diff rather than slipping past. A second test asserts that an event
emitted through raw `tracing::` macros never reaches the export layer.

Controls:

- A first-run consent screen, shown before login and before anything is sent,
  states in plain language what's collected and what isn't. Acknowledgement is
  required to continue; Disable is right there next to Enable.
- `telemetry.mode = "vendor" | "custom" | "off"` in
  `~/.config/telegram-tui/config.toml`
- `--no-telemetry`, `TELEGRAM_TUI_TELEMETRY=off`, and `DO_NOT_TRACK=1`
- `tgt telemetry show` prints exactly what a session would send
- `tgt telemetry reset-id` regenerates the install id and the HMAC salt

Under `mode = "custom"` your endpoint, protocol, and headers fully replace the
default destination. Data is never sent to both. The default vendor endpoint is
an ingest proxy whose URL is baked in at build time; a build without that
environment variable produces a binary whose vendor mode collects nothing at all
until you point it somewhere yourself. No ingest credential ships in the binary.

Most of this is designed and specified rather than running: the allowlist schema,
the `emit!` macro, and the id hashing exist in the tree today, and the exporter,
consent screen, and CI proof arrive with milestone 8.

## Contributing

[`docs/architecture.md`](docs/architecture.md) is the contract. Type definitions
there are what tasks build against, and renaming a shared type means editing that
document first. [`docs/plan.md`](docs/plan.md) is the task decomposition, with
file ownership per task so parallel work doesn't collide. The product behavior
is specified in
[`docs/superpowers/specs/2026-07-30-telegram-tui-design.md`](docs/superpowers/specs/2026-07-30-telegram-tui-design.md).

Work test-first, keep one commit per task with a conventional message, and don't
merge until the four definition-of-done commands above are green. `main` is never
left red.

## License

MIT.
