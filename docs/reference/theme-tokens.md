---
title: Theme tokens
createTime: 2026/07/31 10:00:00
---

The complete token set for a theme file. See [Themes](../guides/themes.md) for how to write and select one.

Theme files are flat TOML with no sections. Every key is optional; anything missing keeps the `default-dark` value. There is no `name` key inside a theme file: the name is the file stem.

## Colour tokens

| Token | Paints |
| --- | --- |
| `accent` | Focused composer border, text cursors, palette match highlight, hint-bar key labels, unread badges, the active folder tab, links and mentions and hashtags in message text, the `⏎ open` affordance, toast titles, help section headings, the typing indicator, auth and consent panel borders |
| `accent_dim` | Inactive chip labels. That is its only consumer, despite every built-in defining it as a distinct hue. |
| `text` | Primary body text: message bodies, chat titles, input text, code-span foreground |
| `text_muted` | Tertiary: timestamps, hints, presence, counts, read receipts |
| `surface` | The base background for the whole app, and the foreground colour on inverted cursor cells |
| `surface_raised` | Overlay panel backgrounds (palette, modal, help, toast), the selected chat row, the selected message, code blocks, search-hit tint, the focused chip |
| `success` | The consent screen's affirmative line. Nothing else. |
| `warning` | The "editing message" banner, the current search hit, mention badges, header and auth warnings |
| `danger` | The failed-send `✗` marker, auth error text, the consent screen's negative line |
| `selection` | Row backgrounds inside overlays: the palette result row, the selected modal button |
| `rail_own` | The `▏` rail on your own outgoing messages |
| `rail_other` | Nothing. See below. |
| `border` | The vertical rule between panes, the horizontal rule under the chat header, the unfocused composer box, overlay panel borders, chat-list separators |

::: warning rail_other is dead
It's in the struct, in the parser, and set by all eight built-in files, but no view reads it. Incoming message rails take their colour from the sender palette instead. Setting it in a custom theme has no visible effect.
:::

## `sender_palette`

An array of exactly eight colour strings. Any other length fails the load with `expected 8 entries, found N`.

```toml
sender_palette = [
  "#7aa2f7", "#9ece6a", "#e0af68", "#f7768e",
  "#bb9af7", "#7dcfff", "#ff9e64", "#41a6b5",
]
```

These identify who's talking in a group. Each sender gets a stable index derived from a per-sender seed modulo eight, and that colour is used for the sender's name in the group header and for the `▏` rail on their incoming messages.

The design language sets two requirements on a palette: all eight hues must be legible against that theme's own `surface`, and they must be distinguishable from each other. A palette where two hues read the same defeats the point.

## Colour formats

Two forms are accepted.

**Hex**: `#rrggbb`, exactly six hex digits after the `#`, case-insensitive. Three-digit shorthand, eight-digit alpha, and a bare `rrggbb` without the `#` are all rejected.

**Named ANSI**: `black`, `red`, `green`, `yellow`, `blue`, `magenta`, `cyan`, `white`, `gray`, `grey`, each optionally prefixed `bright_` or `bright-`.

Raw palette indices (`"12"`, `"color12"`) are not accepted. Values are trimmed before parsing.

A quirk in the name mapping worth knowing: `white`, `gray` and `grey` all map to dim ANSI white, and `bright_white`, `bright_gray` and `bright_grey` all map to bright white. `bright_black` maps to dark grey. If you want a specific shade, use hex.

The design language forbids raw ANSI brights for body text. They're unreadable on a large fraction of terminal colour schemes, and a theme that uses them will look broken on someone else's setup.

## 256-colour fallback

When `COLORTERM` doesn't contain `truecolor` or `24bit`, every RGB colour is mapped to the nearest cell of the 256-colour cube:

```text
c6    = (c * 5 + 127) / 255        per channel
index = 16 + 36*r6 + 6*g6 + b6
```

Named ANSI colours pass through untouched. The greyscale ramp at 232-255 isn't used; the cube covers it. As a worked example, `#61afef` becomes indexed colour 111.

## Error behaviour

| Situation | Result |
| --- | --- |
| Missing token | Filled from the defaults, silently |
| Malformed colour | The entire load fails; you get `default-dark`. The key and its raw value are named in the log. |
| `sender_palette` with the wrong length | Same as a malformed colour |
| Unknown key | Warned and ignored |
| Missing file | `default-dark`, with a log warning |

Half-applied themes would look like a rendering bug, so the loader refuses to produce one: a bad token fails the whole file rather than falling back for just that token. Startup never fails over a theme.

Every warning goes to `~/.local/state/telegram-tui/tgt.log.<date>`, because nothing writes to the terminal while the TUI is running.

## Built-in files

The eight built-ins are compiled in from `crates/ui/themes/`, one file per theme, in this order (which is also the palette cycle order): `default-dark`, `catppuccin-frappe`, `catppuccin-macchiato`, `catppuccin-mocha`, `catppuccin-latte`, `tokyo-night`, `gruvbox-dark`, `nord`.

Copying one of those files is the fastest way to start a custom theme, since they all define the complete token set.
