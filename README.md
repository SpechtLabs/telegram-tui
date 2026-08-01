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

**[▶ Watch it running](https://tgt.specht-labs.de/#what-using-it-looks-like)** — a short recording of the chat list, a reply, a reaction, and revealing a spoiler, made with `tgt --demo` against mock data rather than a real account. GitHub can't play it inline, so it lives on the docs site.

Press `↑` on an empty composer and you're in selection mode: the newest message highlights and the hint bar becomes a chip row like `‹ [R Reply] [F Forward] [E React] [C Copy] [D Delete] ›`. Which chips appear comes from TDLib's per-message capability flags, so an action that would fail is never offered. Below 100 columns the two panes collapse into a single-pane stack rendered by the same components.

## Status

Usable for daily text conversations: login, chat list, history, sending, replies, edits, deletes, reactions, media upload and download, search. Pre-1.0, so expect rough edges and occasional breaking changes.

Not in v1: multiple accounts, voice and video calls, secret chats.

### Platforms

| Platform | Status |
|---|---|
| macOS (Apple Silicon, Intel) | Supported. Release builds for both architectures. |
| Linux (x86_64, aarch64) | Supported. Release builds for both architectures. |
| Windows | **Experimental, untested.** Built and tested in CI; ships no release artifact and nobody has run the client there. |

Windows exists because the code turned out to be portable, not because anyone
has used the client there — its loader has no equivalent of the relocatable
`bin/` + `lib/` layout the other two rely on, so there is nothing to publish.
If you try it anyway, please open an issue about what broke.

Two things to know on Linux. First, a real floor: **glibc 2.39 or newer**.
TDLib publishes only a prebuilt library — this project links against it
rather than compiling TDLib itself, on every install route including from
source — and that prebuilt binary carries its own glibc requirement no build
configuration on our end can lower. Confirmed working: Ubuntu 24.04+, Ubuntu
24.10, Debian 13 (trixie). Confirmed *not* working: Debian 12 bookworm — the
current Debian stable — Ubuntu 22.04 LTS, and RHEL/Rocky 9. Below 2.39 the
binary won't start at all (`version 'GLIBC_2.39' not found`), on a source
build the same as on a release tarball, since the same download runs either
way. The install script checks and tells you before it downloads anything.
Second, the credential store needs a running Secret Service provider, such as
gnome-keyring.

## Install

Three ways in, all equally supported.

**Homebrew**, on macOS or Linux:

```shell
brew install spechtlabs/tap/tgt
```

Updates from then on go through `brew upgrade tgt` — `tgt update` refuses on a
Homebrew install and tells you so, since brew already tracks the files in its
own manifest and an in-place overwrite would desynchronise it.

**The install script**, macOS and Linux, no Homebrew required:

```shell
curl -sSL https://tgt.specht-labs.de/install.sh | sh
```

It downloads the release for your platform, installs the tree to `~/.local/share/tgt`, and symlinks `~/.local/bin/tgt`. It says what it verified — releases carry a cosign bundle and, from 0.1.5 on, a `SHA256SUMS` — and it keeps your previous install until the new one has proved it starts.

If piping a script into a shell makes you uncomfortable, that's a reasonable instinct: read it first at [tgt.specht-labs.de/install.sh](https://tgt.specht-labs.de/install.sh), or take a tarball from the [releases page](https://github.com/SpechtLabs/telegram-tui/releases) and unpack it yourself.

**From source:**

```shell
git clone https://github.com/SpechtLabs/telegram-tui.git
cd telegram-tui
mise run install          # builds, then installs to ~/.local/bin/tgt
```

The install script and building from source lay the tree out the same way: a private `bin/` + `lib/` pair with the binary symlinked onto your PATH. The binary loads TDLib from a dylib (macOS) or shared object (Linux) beside it, so those two have to stay siblings — which is why they go somewhere of their own rather than into a shared prefix. `TGT_INSTALL_ROOT` and `TGT_BIN_DIR` override either half. The Homebrew formula uses the identical layout under its own `libexec`.

Once installed via the script or from source, `tgt update` replaces it with the latest release: it refuses if the install isn't a private tree it owns (Homebrew, or a legacy shared prefix), keeps the old one until the new binary has proved it starts, and reports exactly what it verified. `tgt update --require-signature` refuses unless the release's cosign signature verifies against this project's release workflow — that needs `cosign` installed, which is why it's opt-in rather than the default.

On Linux, see the [glibc 2.39 floor](#platforms) above — it applies here too, since building from source downloads the same prebuilt TDLib the release tarball ships.

Building needs [mise](https://mise.jdx.dev) and nothing else. TDLib arrives through `tdlib-rs`'s `download-tdlib` feature during the build: no Homebrew, no system TDLib, no cmake and gperf and a C++ toolchain. The first build fetches it and takes a while; later builds don't.

You also need your own Telegram `api_id` and `api_hash` from [my.telegram.org](https://my.telegram.org) under "API development tools". No credentials are compiled into the binary. On first run a wizard walks you through it, and `TELEGRAM_API_ID` / `TELEGRAM_API_HASH` override the config file if you'd rather use the environment.

## Keys

| Context | Keys |
|---|---|
| Global | `ctrl+p` palette · `?` help · `ctrl+c` quit |
| Panes | `←` `→` move focus · `tab` / `shift+tab` cycle |
| Chat list | `↑` `↓` move · `⏎` open · `/` filter · `a` archive · `[` `]` folders |
| Composer | type · `⏎` send · `alt+⏎` newline · `↑` on empty enters selection · `/send <path>` |
| Selection | `↑` `↓` message · `←` `→` chip · `⏎` invoke · `r f e c d x l o s v k` chips directly · `esc` back |
| Search | `/` in the message list · `n` / `N` step through hits |
| Mouse | click a chat, folder tab, or the composer · right-click a message for its chips · left-click a spoiler to reveal it or a reply quote to jump to it · wheel scrolls both panes |

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
