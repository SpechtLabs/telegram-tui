# telegram-tui

[![CI](https://github.com/SpechtLabs/telegram-tui/actions/workflows/ci.yml/badge.svg)](https://github.com/SpechtLabs/telegram-tui/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/SpechtLabs/telegram-tui?include_prereleases&sort=semver)](https://github.com/SpechtLabs/telegram-tui/releases)
[![License](https://img.shields.io/badge/license-MIT-blue.svg)](./LICENSE)

> A keyboard-driven Telegram client for the terminal. Arrow keys move, Enter selects, and every action a message supports is shown as a labeled chip instead of a chord you had to read about first.

Built on TDLib and [ratatui](https://ratatui.rs), the binary is called `tgt`. The only modifiers in the whole application are `ctrl+p` for the command palette and `ctrl+c` to quit. Everything else is arrows, Enter, Escape, and single letters that are always visible on screen while they apply. Mouse works too, if you want it.

```
 CHATS                       │ Alice Müller                              online
                             ├──────────────────────────────────────────────────
 ▏ Alice Müller            2 │ ▏ Alice · 14:02
   Team Rust               9 │ ▏ hey, did you see the PR?
   Mom                       │ ▏ also CI is red on main
   #rust-de                1 │
   Bob                       │                              You · 14:03
   Archived                  │                     yeah, reviewing it now ✓✓ ▏
                             │
                             │ ▏ Bob · 14:11
                             │ ▏ 📎 architecture.pdf · 2.4 MB · ⏎ download
                             │
                             │ ╭──────────────────────────────────────────────╮
                             │ │ ›  message…                                  │
                             │ ╰──────────────────────────────────────────────╯
 ↑↓ move   ⏎ open   ctrl+p palette   ? help
```

Press `↑` on an empty composer and you're in selection mode: the newest message highlights and the hint bar becomes a chip row like `‹ [R Reply] [F Forward] [E React] [C Copy] [D Delete] ›`. Which chips appear comes from TDLib's per-message capability flags, so an action that would fail is never offered. Below 100 columns the two panes collapse into a single-pane stack rendered by the same components.

## Status

Usable for daily text conversations: login, chat list, history, sending, replies, edits, deletes, reactions, media download, search. Pre-1.0 and macOS on Apple Silicon only, so expect rough edges and occasional breaking changes. Nothing in the architecture rules out Linux or Windows; they're simply not built or tested yet.

Not in v1: multiple accounts, voice and video calls, secret chats.

## Install

Grab a tarball from the [releases page](https://github.com/SpechtLabs/telegram-tui/releases), or build from source:

```shell
git clone https://github.com/SpechtLabs/telegram-tui.git
cd telegram-tui
mise run install          # builds, then installs to ~/.local/bin/tgt
```

Set `TGT_PREFIX` to install somewhere else. The binary loads TDLib from a dylib next to it (`$TGT_PREFIX/lib`), so keep the two together or unpack the release tarball as a unit.

Building needs [mise](https://mise.jdx.dev) and nothing else. TDLib arrives through `tdlib-rs`'s `download-tdlib` feature during the build: no Homebrew, no system TDLib, no cmake and gperf and a C++ toolchain. The first build fetches it and takes a while; later builds don't.

You also need your own Telegram `api_id` and `api_hash` from [my.telegram.org](https://my.telegram.org) under "API development tools". No credentials are compiled into the binary. On first run a wizard walks you through it, and `TELEGRAM_API_ID` / `TELEGRAM_API_HASH` override the config file if you'd rather use the environment.

## Keys

| Context | Keys |
|---|---|
| Global | `ctrl+p` palette · `?` help · `ctrl+c` quit |
| Panes | `←` `→` move focus · `tab` / `shift+tab` cycle |
| Chat list | `↑` `↓` move · `⏎` open · `/` filter · `a` archive · `[` `]` folders |
| Composer | type · `⏎` send · `alt+⏎` newline · `↑` on empty enters selection · `/send <path>` |
| Selection | `↑` `↓` message · `←` `→` chip · `⏎` invoke · `r f e c d x l o s` chips directly · `esc` back |
| Search | `/` in the message list · `n` / `N` step through hits |
| Mouse | click a chat, folder tab, or the composer · right-click a message for its chips · wheel scrolls both panes |

`Esc` always pops exactly one level and never more. `?` opens the full keymap for whatever context you're in.

## Configuration

TOML at `~/.config/telegram-tui/config.toml`, generated with comments on first run. Unknown keys warn instead of failing, so a config written by a newer build won't brick an older one.

```toml
[app]
theme = "default"
layout_breakpoint_cols = 100
mouse = true              # shift bypasses capture for native text selection
inline_images = true      # downloaded photos render inline where the terminal can

[keys]
palette = "ctrl+p"

[telemetry]
mode = "vendor"           # "vendor" | "custom" | "off"
```

Themes are TOML files under `~/.config/telegram-tui/themes/<name>.toml` defining twelve semantic color tokens plus an eight-color sender palette. Truecolor is used when the terminal supports it, with a defined 256-color fallback.

A photo you have downloaded renders as the picture itself on terminals that speak kitty, iTerm2, or sixel (kitty, Ghostty, iTerm2, and WezTerm are detected automatically; sixel is opt-in with `TGT_SIXEL=1`). Everywhere else — and inside tmux, which drops the escape sequences unless it is configured for passthrough — it stays a single descriptive line. If you have set tmux's `allow-passthrough` up, `TGT_FORCE_GRAPHICS=1` re-enables detection there.

## Privacy

Telemetry is opt-in-with-disclosure and enforced structurally rather than by policy. A first-run screen states what's collected before anything is sent or you even log in.

The guarantee: `crates/core/src/telemetry/schema.rs` declares the complete set of permitted attribute keys, an `emit!` macro is the only path to the exporter, and the OTLP layer drops anything lacking its marker. A stray `tracing::info!("opening chat {}", chat.title)` therefore cannot reach the network no matter who writes it. Message text, names, usernames, phone numbers, chat titles, and file names aren't on the allowlist, which is the reason they can't be exported. A CI test boots the app against an in-process collector, drains every exported attribute key, and fails on anything outside that list.

The rolling local log under `~/.local/state/telegram-tui/` stays rich (it never leaves the machine). Terminal notifications carry a fixed generic body, so nothing identifying rides an `OSC 777` into a multiplexer's log.

Controls: `telemetry.mode` in config, `--no-telemetry`, `TELEGRAM_TUI_TELEMETRY=off`, `DO_NOT_TRACK=1`, `tgt telemetry show` to print exactly what a session would send, and `tgt telemetry reset-id` to regenerate the pseudonymous install id.

## How it's built

Three crates with a dependency direction a CI script enforces: `tgt-core` (pure domain, state, and the TDLib boundary), `tgt-ui` (ratatui rendering), `tgt-app` (the binary that wires them together). `tgt-core` can't depend on `ratatui` or `crossterm`; `tgt-ui` can't depend on `tdlib-rs`.

Everything runs as the Elm architecture over one action channel with a single owner of state, no locks. `update()` performs no I/O and reads no clock, which makes the entire application logic testable by feeding it a scripted sequence of actions. Full-app integration tests replay recorded TDLib sessions from JSONL fixtures, so the whole client can be exercised without a network or an account.

[`docs/architecture.md`](docs/architecture.md) has the module map, the load-bearing types, and the sequence diagrams.

## Contributing

```shell
mise run check      # fmt, clippy, tests, crate boundaries: the gate CI runs
mise run test       # just the tests
mise run run        # the client, from source
mise tasks          # everything available
```

CI runs the same mise tasks you do, so a green `mise run check` locally means a green pipeline.

Commit and PR titles follow [Conventional Commits](https://www.conventionalcommits.org/en/v1.0.0/); release-please turns them into version bumps, changelog entries, and GitHub releases. While the project is pre-1.0, a breaking change bumps the minor version and everything else bumps the patch. A bot validates PR titles, so a wrong one gets caught before merge rather than becoming a wrong release.

[`docs/architecture.md`](docs/architecture.md) is the contract for shared types: renaming or reshaping one means editing that document first. [`.claude/CLAUDE.md`](.claude/CLAUDE.md) is a condensed orientation, useful whether or not you're an agent.

## License

MIT
