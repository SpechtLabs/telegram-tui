---
title: From the keyboard
createTime: 2026/07/31 10:00:00
---

The keyboard model has three rules, and once you've got them the rest follows without memorisation.

1. Arrows move within a context, <kbd>Enter</kbd> acts, <kbd>Esc</kbd> backs out one level.
2. Single letters do things, and they're printed on screen while they apply.
3. Only two chords exist: <kbd>ctrl</kbd>+<kbd>p</kbd> and <kbd>ctrl</kbd>+<kbd>c</kbd>.

Everything below is elaboration.

## The focus stack

The client keeps a stack of focus levels. The bottom is either the chat list or the composer, and things push on top of it: the chat filter, selection mode, in-chat search, the palette, the help overlay, a confirmation modal.

A key is offered to the top of the stack first. If that level doesn't claim it, it falls through to the layers below, and finally to the global layer. That's why <kbd>?</kbd> opens help from the chat list but types a literal `?` in the composer: the composer claims character keys, the chat list doesn't.

<kbd>Esc</kbd> is handled centrally rather than per-level, which is what makes "one <kbd>Esc</kbd>, one level" actually hold. It pops in this order: a visible toast first, then out of the archive if you're in it, then whatever's on top of the stack. If you're at the base in the composer, it takes you back to the chat list. At the very bottom it does nothing rather than quitting; the only quit key is <kbd>ctrl</kbd>+<kbd>c</kbd>.

## Moving between panes

<kbd>→</kbd> from the chat list moves to the composer. <kbd>Tab</kbd> and <kbd>Shift</kbd>+<kbd>Tab</kbd> toggle between them from either side.

<kbd>←</kbd> is deliberately not a pane key. The composer needs it for the caret and selection mode needs it for the chip row, and a key that sometimes moves your cursor and sometimes throws you into another pane is worse than an asymmetry. (The in-app help overlay still lists "← / → move focus between panes"; that row is stale, and <kbd>←</kbd> has never moved focus.)

Pane movement only works at the base of the stack. With an overlay up, <kbd>Tab</kbd> belongs to the overlay.

## In the chat list

<kbd>↑</kbd>/<kbd>↓</kbd> move the cursor, clamped at both ends (no wraparound). <kbd>Enter</kbd> opens the selected chat, which also moves focus to the composer.

<kbd>/</kbd> starts a filter. Type to narrow the list by title, case-insensitively; <kbd>↑</kbd>/<kbd>↓</kbd> still move the selection while you're typing, so you can filter and pick without leaving the field. <kbd>Enter</kbd> commits the filter and closes the input while keeping the list narrowed; <kbd>Esc</kbd> clears it. The filter field is append-and-backspace only, with no caret movement, which is the one place the editing model is thinner than you'd expect.

<kbd>a</kbd> toggles the archive. While you're in it, <kbd>Esc</kbd> comes back out (that's the archive special case in the escape order).

<kbd>[</kbd> and <kbd>]</kbd> cycle folders, wrapping. Only non-empty folders plus Main are in the cycle, and the archive isn't, so <kbd>[</kbd>/<kbd>]</kbd> do nothing while you're viewing archived chats.

## In the composer

Type. <kbd>Enter</kbd> sends, <kbd>Alt</kbd>+<kbd>Enter</kbd> inserts a newline. Caret movement is <kbd>←</kbd>/<kbd>→</kbd>/<kbd>Home</kbd>/<kbd>End</kbd>, deletion is <kbd>Backspace</kbd>/<kbd>Delete</kbd>, and <kbd>↑</kbd> moves the caret up a line when there's text to move within.

With the composer empty, <kbd>↑</kbd> means something else entirely: it enters selection mode on the newest message. That overload is the main way into message actions.

<kbd>Down</kbd>, <kbd>PageUp</kbd> and <kbd>PageDown</kbd> aren't claimed by the composer, so they fall through and scroll the conversation. There's no separate "conversation focus" to move into; the viewport scrolls from wherever you are.

Scrolling near the top of the loaded window triggers a history page automatically, so you don't have to ask for more.

### Sending a file

Type `/send <path>` in the composer and press <kbd>Enter</kbd>. A confirmation modal appears; <kbd>Enter</kbd> sends, <kbd>Esc</kbd> cancels. The path is expanded and checked outside the state machine, so a `~/` prefix works and a path that doesn't exist gets rejected there rather than at Telegram.

Pasting or dropping a bare path does the same thing without the `/send` prefix — bracketed paste is on, so a paste that looks like a single-line path is held as the same kind of pending offer instead of landing in the input as text. [Sending files](media.md#sending-files) has the details.

There's no file browser. The **Send file** palette command exists but is a deliberate no-op; `/send` (or pasting a path) is the only route in v1.

## Editing and replying

Both are armed from selection mode rather than being their own modes. Pressing <kbd>r</kbd> on a message sets the composer's reply target and drops you straight back into the composer with the reply banner showing; <kbd>d</kbd> loads the message text into the composer with the caret at the end and the edit banner showing. In both cases the next <kbd>Enter</kbd> does the right thing.

[Selection mode and chips](selection-mode.md) covers the rest of that surface, including Reveal (`v`, for a message with an unrevealed spoiler) and Cancel upload (`k`, for a file still uploading) — the two chips that aren't derived from a TDLib capability flag.

## What doesn't exist

Worth stating plainly, so you don't hunt for them:

- No <kbd>Alt</kbd> chords other than <kbd>Alt</kbd>+<kbd>Enter</kbd>. Any other <kbd>Alt</kbd>+key arrives as the bare key.
- No function keys. `F1`–`F12`, `Insert`, and media keys are dropped at the input layer.
- No row-level scrolling. The conversation moves a message at a time, not a line at a time.
- <kbd>?</kbd> and <kbd>ctrl</kbd>+<kbd>c</kbd> aren't configurable, despite living next to the palette binding in the code. Only `keys.palette` can be changed.
