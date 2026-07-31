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

Usable for daily text conversations: login, chat list, history, sending, replies, edits, deletes, reactions, media download, search. Pre-1.0, so expect rough edges and occasional breaking changes.

Not in v1: multiple accounts, voice and video calls, secret chats.

### Platforms

| Platform | Status |
|---|---|
| macOS (Apple Silicon) | Supported. The only platform with a release build. |
| Linux | **Experimental, untested.** Compiles and passes the test suite in CI, nobody has run it. Build from source. |
| Windows | **Experimental, untested.** Same, with the caveats below. |

Linux and Windows exist because the code turned out to be portable, not because
anyone has used the client there. If you try either, please open an issue about
what broke — that feedback is the only way they stop being experimental.

Two things to know before you do. On Linux the prebuilt TDLib is linked against
LLVM's libc++, so you need `libc++1` (and `libc++abi1`) installed or the binary
will not start, and the credential store needs a running Secret Service provider
such as gnome-keyring. On Windows the `0700` lockdown that protects the TDLib
database directory and the telemetry salt is unix-only; those inherit ACLs
instead, and hardening them properly is unfinished work.

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
enabled = true            # master switch over both egresses
crash_reports = true      # anonymous crash reports; on unless turned off
# endpoint = "..."        # your own OTLP collector; opt-in, unset by default
```

Themes are TOML files under `~/.config/telegram-tui/themes/<name>.toml` defining twelve semantic color tokens plus an eight-color sender palette. Truecolor is used when the terminal supports it, with a defined 256-color fallback.

A photo you have downloaded renders as the picture itself on terminals that speak kitty, iTerm2, or sixel (kitty, Ghostty, iTerm2, and WezTerm are detected automatically; sixel is opt-in with `TGT_SIXEL=1`). Everywhere else — and inside tmux, which drops the escape sequences unless it is configured for passthrough — it stays a single descriptive line. If you have set tmux's `allow-passthrough` up, `TGT_FORCE_GRAPHICS=1` re-enables detection there.

## Privacy

Two things can leave your machine, they carry different guarantees, and a first-run screen describes both before anything is sent or you even log in.

**Anonymous crash reports, on unless you turn them off.** When the app panics or exits with an error it sends a stack trace, the error message and its cause chain, the app and OS version, and the last few actions as breadcrumbs. Your IP address, username and hostname are kept off it, the breadcrumbs are drawn from the allowlist below, and the pseudonymous install id is deliberately not attached so a crash can't be joined to a usage session. The error message is the honest caveat: whatever code failed writes it, so it can carry limited content such as a file path. Nothing in this client formats a chat title or message body into an error, but that's an observation about the code rather than something a test enforces. Builds from source carry no Sentry DSN and never initialise it at all.

**OTLP export, off unless you point it at your own collector.** The project runs no OTLP destination. This path is enforced structurally rather than by policy: `crates/core/src/telemetry/schema.rs` declares the complete set of permitted attribute keys, an `emit!` macro is its only entrance, and the export layer drops anything lacking its marker. A stray `tracing::info!("opening chat {}", chat.title)` therefore cannot reach a collector no matter who writes it. Message text, names, usernames, phone numbers, chat titles, and file names aren't on the allowlist, which is the reason they can't be exported this way. A CI test boots the app against an in-process collector, drains every exported attribute key, and fails on anything outside that list. It's an OTLP collector, so that proof says nothing about the crash-report path, and the test file says so itself.

The rolling local log under `~/.local/state/telegram-tui/` stays rich and never leaves the machine. Terminal notifications carry a fixed generic body, so nothing identifying rides an `OSC 777` into a multiplexer's log.

Controls, each of which disables both paths: `[telemetry] enabled = false`, `--no-telemetry`, `TELEGRAM_TUI_TELEMETRY=off`, `DO_NOT_TRACK=1`, or Disable on the first-run screen. `crash_reports = false` silences only the first. `tgt telemetry show` prints the live state of both, and `tgt telemetry reset-id` regenerates the pseudonymous install id.

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
