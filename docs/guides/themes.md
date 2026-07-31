---
title: Themes
createTime: 2026/07/31 10:00:00
---

Eight themes ship compiled into the binary, and you can write your own as a TOML file of colour tokens. There's no live reload; a custom theme is picked up at startup.

## The catalogue

| Name | Character |
| --- | --- |
| `default-dark` | Desaturated slate surface with One Dark-family accents. The default. |
| `catppuccin-frappe` | Catppuccin Frappe: dark, medium contrast, pastel blue accent |
| `catppuccin-macchiato` | Catppuccin Macchiato: darker, slightly cooler |
| `catppuccin-mocha` | Catppuccin Mocha: the darkest of the three |
| `catppuccin-latte` | Catppuccin Latte. The only light theme in the catalogue. |
| `tokyo-night` | Cool blue and violet, the "night" variant |
| `gruvbox-dark` | Warm retro, medium contrast, cream text on `#282828` |
| `nord` | Cool desaturated arctic, frost accent |

Names are matched case-insensitively with `_` folded to `-`, so `Catppuccin_Mocha` and `catppuccin-mocha` are the same thing. `default` and `default_dark` are both aliases for `default-dark`.

## Picking one

In `~/.config/telegram-tui/config.toml`:

```toml
[app]
theme = "gruvbox-dark"
```

Or from the command palette: <kbd>ctrl</kbd>+<kbd>p</kbd>, then **Toggle theme**, which advances to the next entry in the catalogue and writes the choice back to your config. It takes effect immediately, without a restart, because the layout cache is keyed partly on a theme generation counter that the switch bumps.

::: warning Toggle theme only cycles the built-ins
The cycle list is the eight built-in names. If you're on a custom theme and press it, you land on `default-dark` and there's no way to cycle back; edit the config to return. Treat the palette command as a way to try the built-ins, not as a general theme switcher.
:::

A theme name that matches neither a built-in nor a file falls back to `default-dark` with a warning in the log file. Nothing appears on screen, because nothing writes to the terminal while the TUI is up. If your theme silently isn't applying, check `~/.local/state/telegram-tui/tgt.log.<date>`.

## Writing your own

Drop a TOML file at `~/.config/telegram-tui/themes/<name>.toml` and set `theme = "<name>"`. The file is flat: no sections, one key per token.

```toml
# ~/.config/telegram-tui/themes/mine.toml
accent          = "#7aa2f7"
accent_dim      = "#3d59a1"
text            = "#c0caf5"
text_muted      = "#565f89"
surface         = "#1a1b26"
surface_raised  = "#24283b"
success         = "#9ece6a"
warning         = "#e0af68"
danger          = "#f7768e"
selection       = "#283457"
rail_own        = "#3d59a1"
rail_other      = "#565f89"
border          = "#292e42"

sender_palette = [
  "#7aa2f7", "#9ece6a", "#e0af68", "#f7768e",
  "#bb9af7", "#7dcfff", "#ff9e64", "#41a6b5",
]
```

Every key is optional. Anything you leave out keeps the `default-dark` value, so a two-line file that only overrides `accent` and `surface` is legal.

`#rrggbb` (six hex digits, case-insensitive) or an ANSI colour name: `black`, `red`, `green`, `yellow`, `blue`, `magenta`, `cyan`, `white`, `gray`/`grey`, each optionally prefixed `bright_`. Three-digit shorthand, eight-digit alpha, bare `rrggbb` without the `#`, and raw palette indices like `"12"` are all rejected.

`sender_palette` must have exactly eight entries or the load fails.

The [theme token reference](../reference/theme-tokens.md) says what each token actually paints, including which ones currently paint nothing.

## When a theme file is broken

The loader distinguishes two failure modes, and they behave differently:

- **A missing token** is filled in from the defaults, silently. Partial themes are a supported thing to write.
- **A malformed token** fails the entire load and you get `default-dark`, with the offending key and its raw value named in the log. Half-applied themes would look like a rendering bug, so the loader refuses to produce one.

Unknown keys warn and are ignored, so a theme written for a newer build doesn't break an older one.

Startup never fails over a theme. Whatever goes wrong, you get a usable client and a log line.

## Terminals without truecolor

`tgt` reads `COLORTERM` and treats the terminal as truecolor when the value contains `truecolor` or `24bit`. Otherwise every RGB colour in the theme is mapped to the nearest cell of the 256-colour cube (`index = 16 + 36r + 6g + b`, with each channel quantised as `(c*5 + 127) / 255`). Named ANSI colours pass through untouched.

The greyscale ramp at 232–255 isn't used; the colour cube alone covers it. As a worked example, `#61afef` becomes indexed colour 111.

## Two constraints on a good theme

The design language sets two hard rules, and the built-ins follow both. Every theme must define a full sender palette of eight hues that are legible against that theme's own `surface` and distinguishable from each other, since those hues identify who's talking in a group. And no theme may use raw ANSI brights for body text; they're unreadable on half the terminal colour schemes in existence.

If you'd rather start from something that already works, copy one of the built-in files out of `crates/ui/themes/` and edit it.

## Not available

Themes aren't watched for changes, so editing a file while `tgt` is running does nothing until you restart. There's no `--theme` flag and no `--list-themes`. There's also no way to override a built-in by naming your file after it: built-ins are checked first, so a `~/.config/telegram-tui/themes/nord.toml` is never read.
