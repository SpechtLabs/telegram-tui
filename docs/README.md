---
pageLayout: home
externalLinkIcon: false

config:
  - type: doc-hero
    hero:
      name: Arrows move, Enter selects, and the app tells you the rest.
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
      - title: Telemetry that cannot leak
        icon: mdi:shield-lock-outline
        details: One allowlist of attribute keys, one macro that can reach the exporter, and a CI test that boots the app against a collector stub and fails on any key outside the list. Message text is not on the list, so it cannot be exported.
      - title: Testable to the frame
        icon: mdi:test-tube
        details: update() does no I/O and reads no clock, so the whole client replays from recorded TDLib sessions with no network and no account. Rendering is pinned by insta snapshots.

  - type: VPReleases
    repo: SpechtLabs/telegram-tui

  - type: VPContributors
    repo: SpechtLabs/telegram-tui
---

## Pre-1.0, and macOS is the platform that's actually been used

Release tarballs are built for `aarch64-apple-darwin` only. Linux and Windows compile and pass the test suite in CI, but nobody has run the client on either, so treat them as experimental: build from source, and please [report what breaks](https://github.com/SpechtLabs/telegram-tui/issues/new/choose).

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

Press `↑` on an empty composer and the newest message highlights; the hint bar turns into a chip row for whatever that message supports. Below 100 columns the two panes collapse into a single-pane stack drawn by the same components, so nothing new has to be learned at a narrow width.

## In v1, and not in v1

Working today: login by phone code or QR, 2FA passwords, chat list with folders and archive, history with paging, sending, replies, edits, deletes, reactions, media download with progress, inline images, search, and a command palette.

Deliberately out of scope for v1: multiple accounts, voice and video calls, secret chats.
