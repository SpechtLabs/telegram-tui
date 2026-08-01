---
pageLayout: home
externalLinkIcon: false

config:
  - type: doc-hero
    hero:
      name: Telegram TUI
      text: A keyboard-driven Telegram client for the terminal
      tagline: Two modifier chords in the whole application. Every action a message supports is a labelled chip on screen, not a shortcut you had to read about first.
      image: /images/SpechtLabsLogo.svg
      actions:
        - text: Get Started →
          link: /getting-started/quick
          theme: brand
          icon: mdi:rocket-launch
        - text: View Documentation →
          link: /getting-started/overview
          theme: alt
          icon: mdi:book-open-page-variant

  - type: features
    title: Why tgt?
    description: A terminal client you can use without learning it first.
    features:
      - title: Discoverable, not memorised
        icon: mdi:gesture-tap-button
        details: Select a message and its actions appear as chips - Reply, Forward, React, Copy, Delete. Press the chip's letter or walk to it with the arrow keys. Nothing is hidden behind a chord.
      - title: Only offers what will work
        icon: mdi:check-decagram
        details: Chips come from TDLib's per-message capability flags via GetMessageProperties. You are never offered a delete that the server will refuse.
      - title: One Esc, one level
        icon: mdi:keyboard-esc
        details: Escape pops exactly one thing off the focus stack, never two, because a single router in core owns every focus transition and handlers are forbidden from touching it.
      - title: Inline images where the terminal allows
        icon: mdi:image-outline
        details: Downloaded photos render as pictures on kitty, Ghostty, iTerm2 and WezTerm, with opt-in sixel. Everywhere else a photo is one honest descriptive line, never a broken block of escape codes.
      - title: Told before anything is sent
        icon: mdi:shield-lock-outline
        details: Crash reports are on unless you turn them off, and a first-run screen says so before login. They carry a stack trace, recent action names and the error message, and that message is written by whatever failed rather than drawn from a list, so it can include limited content such as a file path. Usage export stays off until you name your own collector, and there a CI test decodes the wire and fails on any attribute key outside the allowlist. --no-telemetry silences both.
      - title: Testable to the frame
        icon: mdi:test-tube
        details: update() does no I/O and reads no clock, so the whole client replays from recorded TDLib sessions with no network and no account. Rendering is pinned by insta snapshots.

  - type: custom

  - type: VPReleases
    repo: SpechtLabs/telegram-tui

  - type: VPContributors
    repo: SpechtLabs/telegram-tui
---

## Pre-1.0, and macOS is the platform that's actually been used

Release tarballs are built for macOS and Linux, on both Apple Silicon/Intel and x86_64/aarch64 — `brew install spechtlabs/tap/tgt`, the [install script](/getting-started/installation), or a tarball off the [releases page](https://github.com/SpechtLabs/telegram-tui/releases) all work on either. Linux needs glibc 2.39 or newer — Ubuntu 24.04+ and Debian 13 clear it, Debian 12 and Ubuntu 22.04 LTS don't, and building from source doesn't route around it, since TDLib ships only as a prebuilt library with the same requirement either way; see [Installation](/getting-started/installation) for the full list. Windows compiles and passes the test suite in CI but ships no artifact at all — its loader has no equivalent of the relocatable layout the other two rely on — and nobody has run the client on Linux or Windows day to day, so treat both as experimental outside of macOS and please [report what breaks](https://github.com/SpechtLabs/telegram-tui/issues/new/choose).

## What using it looks like

```text
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

::: cast src="/demo.cast" title="tgt — folders, a reply, a reaction, and revealing a spoiler" rows=30
:::

Recorded with `tgt --demo`, which runs the real client against a scripted, offline chat history instead of a Telegram account — no real conversation was recorded or risked to make this. What you're watching: the chat list with real folder tabs and unread badges, opening a chat with a reply quote and a reaction already on screen, then selection mode walking up to a spoiler and revealing it with `v`. The photo shows as its text card rather than an inline image on purpose — this player speaks no terminal graphics protocol, so that card is what a plain terminal actually shows.

Press `↑` on an empty composer and the newest message highlights; the hint bar turns into a chip row for whatever that message supports. Below 100 columns the two panes collapse into a single-pane stack drawn by the same components, so nothing new has to be learned at a narrow width.

## In v1, and not in v1

Working today: login by phone code or QR, 2FA passwords, chat list with real folder names and an archive, history with paging, sending, replies, edits, deletes, reactions, media upload and download with progress, inline images, search, and a command palette.

Deliberately out of scope for v1: multiple accounts, voice and video calls, secret chats.
