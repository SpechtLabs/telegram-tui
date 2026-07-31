# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

`tgt`, a keyboard-driven terminal Telegram client on TDLib and ratatui, macOS/Apple Silicon only for v1. It works: you can log in, read, and send. See `README.md` for the product framing.

## Documents are the contract

Three documents outrank the code, and two of them are load-bearing when making changes:

- `docs/architecture.md` is the inter-module contract: every shared type, handler signature, module responsibility, and dependency pin. **Renaming or reshaping a shared type means editing this document first, then the code.** It also records amendments discovered during implementation (see "Gotchas" below), so read the section covering your area before assuming the spec's original text still holds.
- `docs/superpowers/specs/2026-07-30-telegram-tui-design.md` is the product spec. Behavior decisions there are settled.
- `docs/plan.md` is the (completed) 56-task build plan. Useful as an index of which task built what and why, since commit messages reference task numbers.

## Commands

`.mise.toml` holds the task definitions and CI calls those same tasks, so a green `mise run check` locally means a green pipeline. Prefer them over raw cargo invocations:

```sh
mise run check       # fmt-check + lint + test + boundaries: the merge gate
mise run test        # or fmt-check / lint / boundaries individually
mise run snapshots   # fail on any pending insta snapshot
mise run build       # release build
mise run package     # dist/ layout + tarball + relocation proof
mise run install     # into $TGT_PREFIX (default ~/.local): bin/tgt + lib/libtdjson
mise tasks           # the full list
```

The four gates behind `check` are `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`, and `./scripts/check-crate-boundaries.sh`. All four must pass before anything merges.

Toolchain comes from `mise` (rust 1.97.1, cargo-insta 1.48.0, both pinned exactly). `rust-toolchain.toml` pins the compiler independently, so plain `cargo` picks the right one outside a mise shell. `cargo-insta` may need `PATH="$HOME/.local/share/mise/shims:$PATH"` or `mise exec --`. Editing `.mise.toml` requires `mise trust` before the tasks run again.

Narrower runs while iterating:

```sh
cargo test -p tgt-core state::history          # one module's tests
cargo test -p tgt-app --test read_only         # one integration binary
cargo test -p tgt-app --test read_only empty_history   # one test by name
cargo test -p tgt-app keychain -- --ignored    # touches the real Keychain, may prompt
```

Snapshots (insta) live in three places: `crates/ui/src/{render,view}/snapshots/` for per-component tests and `crates/ui/tests/snapshots/` for the full-frame regression suite. Review with `cargo insta test -p tgt-ui --check`, accept with `cargo insta accept`. Read the diff before accepting; these snapshots are the only thing keeping the visual design from drifting.

Running the app needs Telegram API credentials (none are compiled in). `TELEGRAM_API_ID` / `TELEGRAM_API_HASH` override the config file; without either, a first-run wizard collects them.

```sh
cargo run -p tgt-app                    # the TUI
cargo run -p tgt-app -- --version       # exits without touching terminal modes
cargo run -p tgt-app -- telemetry show  # prints exactly what a session would send
./scripts/package.sh                    # release build + dist/ + relocation proof
```

## Architecture

### Three crates, enforced boundaries

`tgt-core` (pure domain, `AppState`, `update()`, TDLib boundary types, `FakeTd`, telemetry schema) → `tgt-ui` (ratatui views, message layout, layout cache, theme, input mapping) → `tgt-app` (binary `tgt`: config, CLI, main loop, effect dispatcher, `TdlibRuntime`, logging and OTLP wiring).

`tgt-core` must not depend on `ratatui` or `crossterm`; `tgt-ui` must not depend on `tdlib-rs`. `scripts/check-crate-boundaries.sh` greps `cargo tree` and fails the build otherwise. That seam is what makes the domain testable without a terminal and rendering testable without a network.

### The Elm loop, and what "pure" costs you

Everything (keystrokes, mouse events, TDLib updates, download progress, ticks) normalizes into one `Action` enum over a single `tokio::sync::mpsc` channel, with exactly one owner of state. No locks, no `Arc<RwLock<AppState>>`.

```rust
pub fn update(&mut self, action: Action) -> Vec<Effect>;
```

`update()` mutates memory and returns *descriptions* of side effects. No I/O, no spawning, no clock, no RNG. Time enters only as `Action::Tick { now }`; randomness (install id, HMAC salt) is generated in `tgt-app` at boot and passed in as plain data.

This constraint is the source of most non-obvious design in the codebase. When something can't be computed purely, the pattern is always the same: resolve it at the boundary and hand core semantic data.

- **Mouse** (architecture §7.5): core can't map a click coordinate to a row. The view records a `HitMap` while drawing, the runtime loop resolves coordinates against it, and core receives `Action::Click { target: HitTarget, .. }`. `tgt_ui::view(...)` returns that map.
- **Filesystem checks**: `/send <path>` existence and tilde expansion live in `tgt-app`'s `media_kind.rs`; core keeps only a pure `looks_like_path` heuristic.
- **`SetTdlibParameters`**: carries the Keychain database key, so `state::auth` deliberately emits nothing for `WaitTdlibParameters` and the dispatcher issues it. This is the one documented impure exception in the update flow.

### State modules and routing

Sub-states live one per file under `crates/core/src/state/`, each exposing plain functions rather than methods:

```rust
pub fn handle_key(app: &mut AppState, key: Key) -> Option<Vec<Effect>>;  // None = unclaimed
pub fn handle_td(app: &mut AppState, upd: &TdUpdate) -> Vec<Effect>;
pub fn handle_tick(app: &mut AppState, now: Millis) -> Vec<Effect>;
```

`app.rs` is a thin router implementing spec §6.2's table (modal → focused pane → global, first claimant stops propagation) and it owns every focus-stack transition. Handlers never touch `app.focus` themselves; they return `None` for `Esc` so the one stack rule lives in one place. `app.rs` is also where `Effect::Telemetry` events are attached, keyed off the effects a route produced, so an unconfirmed dialog never emits an event.

Read the module doc comment before editing any state file. Several encode contracts other modules depend on (the credentials wizard's `active_field` protocol, the palette's close-handshake, `modal.rs`'s extra `&mut ModalState` parameter).

### TDLib boundary

All access funnels through one `TdRuntime` trait with two implementations: `TdlibRuntime` (`crates/app/src/td_runtime.rs`, the only module importing `tdlib_rs`, where raw PII-bearing types die and become `TdUpdate`/`TdResponse`) and `FakeTd` (`crates/core/src/td/fake.rs`, replaying JSONL fixtures).

Full-app integration tests in `crates/app/tests/` drive the *real* `runtime_loop::Core` against `FakeTd` by `#[path]`-including the app modules. Consequences worth knowing: pulling `otel.rs` into such a test drags the whole OTLP stack, some `pub` items are dead in the bin target and carry documented `#[allow(dead_code)]`, and each test binary has an `#[ignore]`d `regenerate_fixtures` test that rewrites its `.jsonl` from the Rust script (`cargo test -p tgt-app --test <name> regenerate_fixtures -- --ignored`). Effects dispatch via `tokio::spawn`, so assert on state transitions or `FakeTd::received()` counts rather than reading immediately after a step.

### Two egresses, two different guarantees

`app/src/otel.rs` exports OTLP and `app/src/crash.rs` sends Sentry crash reports. Do not carry a claim about one over to the other; almost every mistake in this area is that carry.

**OTLP (opt-in, off unless `[telemetry].endpoint` is set) is structural, not a policy.** `crates/core/src/telemetry/schema.rs` declares the complete allowlist as constants. `emit!` is the only path to that exporter; it tags events with `telemetry.public = true` on target `tgt_telemetry`, and the export layer forwards nothing lacking both markers. A stray `tracing::info!("chat {}", title)` lands in the local rolling log and stops there. `crates/app/tests/telemetry_allowlist.rs` boots the app against an in-process OTLP collector stub and fails on any exported key outside `ALLOWED_KEYS`, with an anti-vacuity check so a dead exporter can't pass silently. Adding an attribute means adding a constant, which shows up as an insta snapshot diff in review.

**Crash reports (on unless opted out) have no allowlist and that test does not cover them.** A report is built from the failure's own stack trace and message, so the message can carry limited content such as a file path. `send_default_pii: false` plus a `before_send` that nulls `server_name` keep IP, username and hostname off; breadcrumbs come from `crash::record_action` and are allowlist-shaped; `install.id` is deliberately not attached. When writing docs or comments here, scope every absolute claim to the path where it's true, and mark "in practice" claims as such — spec §13.9 and `docs/understanding/telemetry-allowlist.md`'s "What the proof doesn't cover" are the reference wording.

`[telemetry].enabled = false`, `--no-telemetry`, `TELEGRAM_TUI_TELEMETRY=off`, `DO_NOT_TRACK` and a consent-screen Disable all switch off **both**, via `TelemetryMode::Off`. `ConfigPatch::ConsentAcknowledged` carries the choice and the acknowledgement together — don't split it back into two patches, and don't add a `ConfigPatch::TelemetryMode`: the split is what let a Disable record as "never answered" and re-prompt for ever.

The Sentry DSN comes from `option_env!("TGT_SENTRY_DSN")`, which only the release workflow sets, so **your build reports nothing** and the consent screen's copy branches on `AppState::crash_reports_available` to say so. Both branches are snapshotted; if you change that copy, check the one you aren't looking at.

The same discipline covers terminal alerts: `notify::alert` takes no content parameters at all, so no sender name or message text can ride an `OSC 777` into a multiplexer log.

### A failed config write ends the run

`Effect::SaveConfig` failing is fatal, not a toast (architecture §4.4.1). The error is a `human_errors::Error` built by `config::unwritable`, which names the path and carries advice; it travels `dispatch::save_config` → `fatal_tx` → `runtime_loop::run` → `run_tui`, so the `TerminalGuard` restores the terminal before `main::report_to_user` prints it. Never print it where it happens — the TUI still owns the screen there.

Before softening this: `state::auth::submit_credentials` advances the wizard before the write is dispatched and never checks the outcome, and `save_config` only sends TDLib its parameters after a successful write. The optimistic advance is safe only because the failure is fatal; make it a toast and login stalls for the whole session with nothing on screen.

## Gotchas

These bit during implementation and are recorded in `docs/architecture.md`:

- **UTF-16 offsets.** Telegram entity offsets are UTF-16 code units. Conversion to byte offsets happens in exactly one place, `tgt_ui::render::offsets`, tested against a 14-row table. Never slice message text by entity offsets anywhere else.
- **Chat order** comes from TDLib's `order: i64` via `updateChatPosition`, mirrored into a `BTreeSet`. Never computed locally.
- **`getChatHistory` returning zero messages is not end-of-history.** The `PagingState` machine retries with `only_local = false` before believing it. Chat open is local-first (instant cached render) followed by exactly one remote reconcile, loop-guarded on the completed request's `only_local` flag.
- **Layout cache keys** are `(message_id, width, theme_generation, spoilers_revealed)`. Anything that changes without those (reactions, receipts, download progress) must render as per-frame lines outside the cache, not inside cached blocks.
- **`MessageCaps`** aren't on `message` in current TDLib. They arrive via `GetMessageProperties`, fetched when a message is selected.
- **`tracing` 0.1.44 field order**: a dotted field immediately after `target:` fails to compile. `emit!` puts a plain field first.
- **Nothing writes to stdout/stderr while the TUI is active.** The panic hook restores the terminal (alternate screen, raw mode, mouse capture) before printing. The one deliberate exception is the alert escape sequence.
- **`tracing-batteries`: the Sentry battery is used, the OpenTelemetry one can't be.** `OpenTelemetry::setup` installs its own global subscriber and filters only by level, which can't coexist with the file log or the allowlist, so `otel.rs` drives the OTLP stack directly. `Sentry::setup` touches no subscriber (it just calls `sentry::init`, which binds a client to a process-global hub), so `crash.rs` uses it as-is. The rule: a battery that takes the global subscriber is unusable here, one that doesn't is fine. `sentry` is also a direct dep, purely to re-enable the `panic`/`backtrace`/`contexts`/`debug-images` features batteries turns off.
- **Dependency pins are exact (`=`)** and live only in the three `Cargo.toml` files. Several carry non-obvious feature choices (keyring has no `apple-native` feature; `ratatui-image` runs with default features off because `chafa-dyn` needs a system C library). Comments explain each.
