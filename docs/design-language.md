# telegram-tui — visual design language

**Status:** Approved for implementation (2026-07-31)
**Supersedes:** the chrome implied by spec §6.1's ASCII mock. The *layout* in that
mock (two-pane above the breakpoint, single-pane stack below, hint bar at the
bottom) still holds; the *decoration* does not.

The v1 shell drew a rectangle around everything: an outer frame, a boxed
sidebar, a boxed header, a boxed conversation. Boxes were how TUIs separated
regions when terminals had no color depth and no space to spare. They read as
dated now, they cost four columns and four rows of usable space per nested box,
and they add visual noise that competes with the content.

The rule that replaces them: **separate regions with space and contrast, not
with lines.** Draw a line only where two regions must be told apart and
whitespace alone cannot do it.

## 1. Chrome

- **No outer frame.** The application fills the terminal. Nothing is drawn
  around the whole viewport.
- **No boxes around panes.** The sidebar, header, and conversation are regions,
  not widgets.
- **One vertical rule** between sidebar and conversation, drawn in `border`
  (a dim tone, never `text`). One horizontal rule under the chat header. That is
  the entire budget for lines in the main view.
- **The composer keeps its rounded box.** It is an input affordance, and a
  border is the clearest way to say "type here". Border in `border`, not accent,
  unless the composer is focused.
- **Padding is mandatory.** Every region carries one column of padding on each
  side and one blank row above its first content row. Content never touches a
  rule or the terminal edge.
- **Overlays** (palette, modal, help) are rounded, one-line-bordered panels on
  `surface_raised`, centered, with two columns of internal padding.

## 2. Hierarchy

Three weights, applied consistently:

| Weight | Used for | Style |
|---|---|---|
| Primary | message bodies, chat titles, input text | `text` |
| Secondary | sender names, section labels, active tab | sender color or `accent`, bold |
| Tertiary | timestamps, hints, counts, presence, muted rows | `text_muted` |

Timestamps are always tertiary. A timestamp that reads as loudly as the message
is the single biggest contributor to the "log output" look.

## 3. Messages

- Incoming: a `▏` rail in the sender's color, one space, then the body. The rail
  runs the full height of the block, including wrapped lines.
- Own: right-aligned, rail on the right in `rail_own` (dim). Never brighter than
  the body.
- Header line per group: `Sender · 14:02`, sender in its color and bold,
  separator and time in `text_muted`.
- Exactly one blank row between groups. None within a group.
- **Receipts render inline**, appended to the last line of an own message as a
  trailing ` ✓` / ` ✓✓` in `text_muted` (`✗` in `danger`, `⋯` while sending).
  They never occupy their own row, and they never form a column at the frame
  edge.
- Reply quote: one dimmed `↳ excerpt` line above the body.

## 4. Attachments

**One line per attachment, never two.** The v1 split (a cached identity line
plus a per-frame status line) rendered the file name twice and read as a bug.

The single line carries icon, name, and the live affordance:

```
🖼 photo · 323×94 · ⏎ download
📎 spec.pdf · 2.4 MB · ▓▓▓▓░░░░░░ 40%
🎞 clip.mp4 · 8.1 MB · ⏎ open
```

Because the affordance is live state that no cache key covers, the whole line is
rendered per frame by the view, and the cached layout contributes nothing for
file content. A downloaded photo is replaced entirely by its inline image (§6)
when the terminal supports one.

## 5. Selection and emphasis

- Selected chat row: a `▏` bar in `accent` at the left edge plus a
  `surface_raised` background across the row. Not a full-width inverse block.
- Selected message: `surface_raised` background across its lines, no border.
- Unread badge: the count in `accent`, bold, right-aligned. Mentions in
  `warning`. No brackets, no parentheses.
- Focus is shown by the accent bar and background, never by a color change to
  the body text.

## 6. Inline images

Where the terminal supports a graphics protocol (kitty, iTerm2, sixel) and the
photo has been downloaded, the placeholder line is replaced by the image itself,
bounded to `MAX_IMAGE_ROWS` and to the pane width, left-inset to the rail
column. Everywhere else the §4 line stands in. Scrolling invalidates placed
images so protocol cells cannot ghost.

## 7. Themes

Built-in themes ship as TOML compiled into the binary and are selectable by name
from config or the palette. The catalog is deliberately small and curated:
`default-dark`, `catppuccin-frappe`, `catppuccin-macchiato`, `catppuccin-mocha`,
`catppuccin-latte`, `tokyo-night`, `gruvbox-dark`, `nord`.

Every theme defines the full token set. Sender palettes are theme-specific:
eight hues that stay legible against that theme's `surface` and remain
distinguishable from each other. No theme may use raw ANSI brights for body
text.
