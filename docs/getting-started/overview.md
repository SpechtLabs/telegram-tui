---
title: What tgt is
createTime: 2026/07/31 10:00:00
---

`tgt` is a Telegram client that runs in your terminal. It talks to Telegram through [TDLib](https://core.telegram.org/tdlib), the same official client library the desktop and mobile apps use, and draws itself with [ratatui](https://ratatui.rs). It is not a bot framework, not a bridge, and not a wrapper around the Bot API: you log in as yourself, with your own account, and see your real chats.

## The one design decision that matters

Most terminal chat clients hand you a keymap and expect you to learn it. `tgt` inverts that. The application tells you what's available, on screen, at the moment it's available.

Select a message and the hint bar at the bottom turns into a row of chips:

```text
 ‹ [R Reply]  [F Forward]  [E React]  [C Copy]  [D Delete] ›
```

Walk between them with `←` and `→` and press `Enter`, or press the letter directly. Either way you didn't need to read anything first. And because that chip row is built from TDLib's per-message capability flags (`GetMessageProperties`, fetched when the message is selected), you're never shown an action the server would reject. There's no "press D, get an error toast" failure mode, because D isn't there on a message you can't delete.

The whole application has exactly two modifier chords: <kbd>ctrl</kbd>+<kbd>p</kbd> opens the command palette, <kbd>ctrl</kbd>+<kbd>c</kbd> quits. Everything else is arrows, `Enter`, `Esc`, and single letters that are visible while they apply.

## What it does today

Login by phone code or QR, with 2FA password support. A chat list with folders, archive, unread and mention badges. Message history that pages backwards as you scroll. Sending, replying, editing, deleting, reacting. File and photo download with a progress bar, sending files by path, and inline image rendering on terminals that support it. Full-text search within a chat, and a command palette for everything that isn't a key.

Not in v1, on purpose: multiple accounts, voice and video calls, secret chats. None of these are blocked by the architecture; they're scope decisions.

## Platform status

| Platform | Status |
| --- | --- |
| macOS (Apple Silicon) | Supported. The release tarball targets `aarch64-apple-darwin`. |
| Linux | Experimental. Builds and passes the test suite in CI; nobody has run the client on it. Build from source. |
| Windows | Experimental. Same as Linux, plus one known gap: the `0700` lockdown on the TDLib database directory and the telemetry salt is Unix-only, so on Windows those files inherit directory ACLs instead. That hardening is unfinished. |

The credential store differs by platform too, because `tgt` puts the TDLib database encryption key in the OS keychain rather than on disk. macOS uses the Keychain, Windows uses Credential Manager, and Linux needs a running Secret Service provider (gnome-keyring, KWallet, or equivalent) or the key can't be stored.

Everything is pre-1.0. Breaking changes bump the minor version until that changes.

## How it's put together

Three crates with a dependency direction that a CI script enforces: `tgt-core` holds the domain model, the state machine, and the TDLib boundary types; `tgt-ui` draws; `tgt-app` is the binary that wires the two to a terminal and a network. `tgt-core` cannot depend on `ratatui`, and `tgt-ui` cannot depend on `tdlib-rs`.

Every input (keystroke, mouse event, TDLib update, download progress tick) becomes an `Action` on one channel, and one function processes them:

```rust
pub fn update(&mut self, action: Action) -> Vec<Effect>;
```

`update()` performs no I/O, spawns nothing, reads no clock, and generates no randomness. It mutates memory and returns *descriptions* of side effects for the runtime to carry out. That constraint is what lets the entire client run against recorded TDLib sessions in tests, with no network and no account.

[The shape of the app](../understanding/architecture.md) has the details, including a diagram of the loop and the honest list of what the purity constraint costs.
