---
title: Quick Start
createTime: 2026/07/31 10:00:00
---

Two minutes, assuming you're already logged in. If you're not, [First login](login.md) is the page you want.

## Read something

You land on the chat list with the first row selected. <kbd>↑</kbd> and <kbd>↓</kbd> move; <kbd>Enter</kbd> opens the chat and drops focus into the composer, ready to type.

```text
 CHATS                       │ Alice Müller                              online
                             ├──────────────────────────────────────────────────
 ▏ Alice Müller            2 │ ▏ Alice · 14:02
   Team Rust               9 │ ▏ hey, did you see the PR?
   Mom                       │
   Archived                  │                              You · 14:03
                             │                     yeah, reviewing it now ✓✓ ▏
 ↑↓ move   ⏎ open   ctrl+p palette   ? help
```

The bottom line always tells you what the current context does. When you're unsure, read it before reaching for a shortcut.

## Send something

Type, press <kbd>Enter</kbd>. <kbd>Alt</kbd>+<kbd>Enter</kbd> inserts a newline instead of sending.

## Do something to a message

With the composer empty, press <kbd>↑</kbd>. The newest message highlights and the hint bar becomes a chip row:

```text
 ‹ [R Reply]  [F Forward]  [E React]  [C Copy]  [D Delete] ›
```

<kbd>↑</kbd>/<kbd>↓</kbd> pick a different message, <kbd>←</kbd>/<kbd>→</kbd> walk the chips, <kbd>Enter</kbd> invokes the focused one. Or just press the letter: <kbd>r</kbd> to reply, <kbd>x</kbd> to delete, <kbd>l</kbd> to download an attachment.

Which chips appear depends on the message. A message you can't edit has no Edit chip, so <kbd>d</kbd> does nothing there rather than producing an error. That comes from TDLib's capability flags for that specific message, fetched when you select it.

<kbd>Esc</kbd> leaves selection mode and puts you back in the composer. It always pops exactly one level, never two.

## Find something

<kbd>ctrl</kbd>+<kbd>p</kbd> opens the command palette: fuzzy-match over your chats and a handful of commands (toggle theme, log out, quit). Type, <kbd>↑</kbd>/<kbd>↓</kbd> to pick, <kbd>Enter</kbd> to run.

To search within a chat, get into selection mode first (<kbd>↑</kbd> on an empty composer), then press <kbd>/</kbd>. Type the query, <kbd>Enter</kbd> to run it, then <kbd>n</kbd> and <kbd>N</kbd> to step through hits.

## When you're stuck

<kbd>?</kbd> opens the help overlay for the current context. <kbd>ctrl</kbd>+<kbd>c</kbd> quits from anywhere, including mid-login.

## Where next

[Driving it from the keyboard](../guides/keyboard.md) walks the whole model properly, and the [keymap reference](../reference/keymap.md) is the flat table for when you know what you want and just need the key.
