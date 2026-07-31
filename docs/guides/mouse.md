---
title: Using the mouse
createTime: 2026/07/31 10:00:00
---

Mouse support is on by default and can be turned off with `[app] mouse = false`. It's a small, deliberate surface: clicks and the wheel, nothing else.

## What responds

| Gesture | Where | Result |
| --- | --- | --- |
| Left click | A chat row | Selects and opens it, exactly as <kbd>Enter</kbd> would |
| Left click | The "Archived N" row | Enters the archive |
| Left click | A folder tab | Switches to that folder and moves the selection to its first row |
| Left click | The composer | Moves focus to the composer (only when a chat is open) |
| Right click | A message | Enters selection mode on that message |
| Wheel | The chat list | Moves the chat-list selection, one row per step |
| Wheel | The conversation | Scrolls one message per step |

Left-clicking a *message* does nothing. That's explicit in v1, not an oversight: a left click has no obvious meaning on a message when right click already means "act on this one".

## What doesn't respond

Hover, drag, button release, the middle button, and horizontal scroll are all discarded at the translation layer. There's no drag-select and no hover state.

Clicking a cell that no region covers produces nothing at all. The client never snaps to the nearest thing, so the gap between folder tabs, the hint bar, the header and the rules between panes are all dead. That's a design choice: a click that does something you didn't mean is worse than a click that does nothing.

While a modal, the palette, or the help overlay is up, the whole hit map is thrown away and nothing is clickable or scrollable. The mouse is also dead on the consent and login screens.

## Wheel scrolling doesn't move focus

Wheel over the sidebar moves the chat-list selection even when the keyboard focus is in the composer, and it doesn't steal the focus while doing it. Same for the conversation. You can scroll back through history with the wheel while continuing to type.

Scrolling the conversation near the top of the loaded window triggers a history page, same as the keyboard route.

## Selecting text with the mouse

While mouse capture is on, your terminal hands mouse events to `tgt` rather than using them for its own selection. Hold <kbd>Shift</kbd> to bypass capture and select text natively; that's a terminal-level convention and works in most of them.

If you'd rather have native selection all the time, put this in `~/.config/telegram-tui/config.toml`:

```toml
[app]
mouse = false
```

Nothing in the client is mouse-only, so you lose no functionality.

## How it works, briefly

`tgt-core` can't map a click coordinate to a row, because it has no idea how anything was drawn. So the view builds a hit map while it draws (chat rows, folder tabs, the archive row, message rows, the composer box, plus two scrollable pane rectangles), the runtime resolves the coordinate against that map, and core receives a semantic `Click { target: HitTarget }` rather than a pair of numbers. The map is rebuilt from scratch every frame.

That indirection is what keeps `update()` pure and testable, and it's why the mouse behaves identically in the single-pane narrow layout without any extra code. [The shape of the app](../understanding/architecture.md) has the general pattern.
