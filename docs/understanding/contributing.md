---
title: Contributing
createTime: 2026/07/31 10:00:00
---

The build is a handful of mise tasks, CI runs the same ones, and a green `mise run check` locally means a green pipeline.

## The gate

```shell
mise run check      # fmt-check, clippy, tests, crate boundaries
mise run test       # just the tests
mise run run        # the client, from source
mise tasks          # everything available
```

Four gates sit behind `check`: `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`, and `./scripts/check-crate-boundaries.sh`. All four have to pass before anything merges.

The last one is worth explaining. It greps `cargo tree` and fails if `tgt-core` has picked up `ratatui` or `crossterm`, or if `tgt-ui` has picked up `tdlib-rs`. Those bans are what keep the domain testable without a terminal and the renderer testable without a network, and a transitive dependency can break them by accident, so they're checked rather than trusted.

Toolchain comes from mise: Rust 1.97.1 and cargo-insta 1.48.0, both pinned exactly. `rust-toolchain.toml` pins the compiler independently so plain `cargo` picks the right one outside a mise shell. Editing `.mise.toml` needs a `mise trust` before the tasks run again.

## Snapshots

Rendering is pinned by [insta](https://insta.rs) snapshots in three places: `crates/ui/src/render/snapshots/`, `crates/ui/src/view/snapshots/`, and `crates/ui/tests/snapshots/` for full-frame regressions.

```shell
mise run snapshots              # fail on any pending snapshot
cargo insta test -p tgt-ui --check
cargo insta accept
```

Read the diff before accepting. These snapshots are the only thing keeping the visual design from drifting, and accepting one without looking is how a layout regression ships.

## Testing without an account

Full-app integration tests in `crates/app/tests/` drive the real runtime loop against `FakeTd`, which replays recorded TDLib sessions from JSONL fixtures. No network, no account, no credentials.

Each test binary carries an `#[ignore]`d `regenerate_fixtures` test that rewrites its `.jsonl` from a Rust script:

```shell
cargo test -p tgt-app --test <name> regenerate_fixtures -- --ignored
```

Effects dispatch through `tokio::spawn`, so assert on state transitions or on what `FakeTd` received, not on state read immediately after a step.

## Documents are the contract

Three engineering documents live in the repo and are deliberately not part of this website. They're written for people changing the code, they're long, and two of them outrank the code when the two disagree.

| Document | What it is |
| --- | --- |
| [`docs/architecture.md`](https://github.com/SpechtLabs/telegram-tui/blob/main/docs/architecture.md) | The inter-module contract: every shared type, handler signature, module responsibility and dependency pin, plus the amendments discovered during implementation. Renaming or reshaping a shared type means editing this document first, then the code. |
| [`docs/design-language.md`](https://github.com/SpechtLabs/telegram-tui/blob/main/docs/design-language.md) | The visual rules: chrome, hierarchy, message rendering, attachments, selection, inline images, themes. "Separate regions with space and contrast, not with lines" is the founding rule, and the line budget for the main view is exactly two rules. |
| [`docs/plan.md`](https://github.com/SpechtLabs/telegram-tui/blob/main/docs/plan.md) | The completed build plan. Useful as an index of which task built what, since commit messages reference task numbers. |
| [`docs/superpowers/specs/`](https://github.com/SpechtLabs/telegram-tui/tree/main/docs/superpowers/specs) | The product spec. Behaviour decisions there are settled. |

[`.claude/CLAUDE.md`](https://github.com/SpechtLabs/telegram-tui/blob/main/.claude/CLAUDE.md) is a condensed orientation covering the same ground, and it's useful whether or not you're an agent. Its "Gotchas" section is the fastest way to avoid re-discovering things that already bit someone.

## Gotchas in one place

The short version of that section, because these are the ones that cost time:

- **Telegram entity offsets are UTF-16 code units.** Conversion to byte offsets happens in exactly one module, tested against a 14-row table. Never slice message text by entity offsets anywhere else.
- **Chat order comes from TDLib and is never computed locally.** See [why](chat-order.md).
- **An empty `getChatHistory` is not end-of-history.** See [how that's handled](history-paging.md).
- **Layout cache keys are `(message_id, width, theme_generation, spoilers_revealed)`.** Anything that changes without one of those (reactions, receipts, download progress) must render outside the cached block.
- **`MessageCaps` don't arrive on `message`.** They come from `GetMessageProperties`, fetched when a message is selected.
- **Nothing writes to stdout or stderr while the TUI is active.** The panic hook restores the terminal before printing.
- **Dependency pins are exact (`=`)** and live only in the three `Cargo.toml` files, several with non-obvious feature choices explained in comments.

## Commits and releases

Commit and PR titles follow [Conventional Commits](https://www.conventionalcommits.org/en/v1.0.0/). release-please turns them into version bumps, changelog entries and GitHub releases. While the project is pre-1.0, a breaking change bumps the minor version and everything else bumps the patch. A bot validates PR titles, so a wrong one gets caught before merge rather than becoming a wrong release.

## This website

The site lives under `docs/`, built with VuePress and the Plume theme, using bun.

```shell
cd docs
bun install
bun run dev      # local preview with hot reload
bun run build    # what CI builds
```

The engineering documents above sit in the same directory and are excluded from the built site via `pagePatterns` in `docs/.vuepress/config.ts`. If you add a new one, exclude it there and in `.markdownlint-cli2.yaml` too.

Markdown is linted with markdownlint-cli2 over `docs/**/*.md`, the same glob the workflow uses.
