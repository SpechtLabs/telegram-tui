---
title: Media, downloads & inline images
createTime: 2026/07/31 10:00:00
---

Attachments render as one line each, and on a terminal that speaks a graphics protocol a downloaded photo is replaced by the picture itself.

## The attachment line

Every attachment is exactly one line, never two:

```text
 ▏ 📎 architecture.pdf · 2.4 MB · ⏎ download
 ▏ 🖼 photo · 1280×960 · ⏎ download
 ▏ 📎 report.pdf · 8.1 MB · ▓▓▓▓░░░░░░ 40%
 ▏ 🎬 clip.mp4 · 12.0 MB · ⏎ open
```

Icon, name, size or dimensions, and the live affordance. The one-line rule exists because an earlier version split it into a cached identity line plus a per-frame status line, which printed the file name twice and read as a bug.

That affordance is live state, so the whole line renders fresh each frame rather than coming out of the layout cache. Layout caching gains nothing on file content, which is fine; there's not much to lay out.

## Downloading

Select the message (<kbd>↑</kbd> on an empty composer, then <kbd>↑</kbd>/<kbd>↓</kbd> to reach it) and press <kbd>l</kbd> for Download. The progress bar updates in place. When it finishes, the Download chip is replaced by Open, and <kbd>o</kbd> hands the file to your system opener.

The opener defaults to `open`, which is right on macOS. Set `TGT_OPENER` to something else (`xdg-open` on Linux, for instance):

```shell
TGT_OPENER=xdg-open tgt
```

Files land wherever TDLib puts them, inside its own directory under `~/.local/share/telegram-tui/td/`.

## Automatic photo download

Photos in view download automatically; nothing else does. Video, audio and documents always wait for an explicit <kbd>l</kbd>, because auto-downloading a 400 MB video because it scrolled past would be hostile.

Turn it off with:

```toml
[app]
auto_download_photos = false
```

There's storm control on the automatic path: each photo is requested at most once per session, and a failure is retried a bounded number of times before the client gives up on it.

## Inline images

Where the terminal supports it and the photo has been downloaded, the placeholder line is replaced by the actual image, capped at 15 rows and the width of the pane, inset to the same column as the message rail.

Detection is by environment variable, in this order:

1. **tmux** vetoes everything. See below.
2. **Kitty protocol** if `TERM` is `xterm-kitty`, or `KITTY_WINDOW_ID` is set, or `TERM_PROGRAM` is `ghostty` or `TERM` contains `ghostty`. Ghostty speaks the kitty protocol.
3. **iTerm2 protocol** if `TERM_PROGRAM` is `iTerm.app` or `WezTerm`. WezTerm implements the iTerm2 protocol rather than its own.
4. **Sixel** only if `TGT_SIXEL=1`. Sixel support has no reliable identifying environment variable, so it's never guessed, only requested.
5. Otherwise no images, and the one-line card stands in.

Turn it off entirely regardless of terminal:

```toml
[app]
inline_images = false
```

### The tmux caveat

Inside tmux, image protocols are disabled outright. tmux swallows the escape sequences unless it's been configured for passthrough, and a client that emits them anyway leaves garbage in the pane. Vetoing is the answer that's correct in every case where you haven't configured passthrough, which is most of them.

If you *have* set `allow-passthrough` up, tell `tgt` so:

```shell
TGT_FORCE_GRAPHICS=1 tgt
```

The value has to be exactly `1`; `true` doesn't count. With it set, detection proceeds normally inside tmux.

### Why images sometimes look right and sometimes don't

Kitty and iTerm2 both place an image by pixel extent and work out how many cells that covers by dividing by the terminal's cell size. `tgt` asks the terminal for its cell size with `TIOCGWINSZ`, which needs no escape sequence and nothing read back, so it can be re-asked on every resize (a font-size change arrives as a resize). Terminals that report zeros there, which includes many, get a fallback cell size, and an image encoded against a guessed cell size can render taller than the rows reserved for it.

Scrolling invalidates placed images so protocol cells can't ghost into rows that have since moved.

## Sending files

`/send <path>` in the composer, then <kbd>Enter</kbd>, then <kbd>Enter</kbd> again on the confirmation modal. A leading `~/` is expanded against `$HOME`, and the file's existence is checked before anything is sent.

Dropping a file onto the terminal, or pasting a path directly, does the same thing without typing `/send`: most terminals paste a dropped file as its plain-text path, and if what arrives starts with `/`, `~/`, or `./` and looks like a single-line path, the composer holds it as a pending offer instead of inserting it as text. `Enter` confirms it through the same modal as `/send`; `Esc` discards the offer and the composer stays empty.

The file type is worked out at the boundary from the path, so a `.jpg` goes as a photo rather than as a generic document. The state machine only ever says "document"; the dispatcher upgrades it.

There's no file picker. The **Send file** palette command is a placeholder that does nothing.

## Uploading

An outgoing file shows the same kind of progress line downloads do, from the moment you confirm the send until TDLib finishes the transfer — the message card reads bytes-sent-so-far the same way the download bar reads bytes-received, both moving as real `updateFile` pushes arrive rather than jumping straight to "sent" partway through.

If you need to abandon one mid-flight, `↑` on the composer to enter [selection mode](selection-mode.md), select the message, and its chip row offers **Cancel upload** (`k`) for as long as the transfer is still in progress — including one that's already failed to send, where the chip stays available so you can clear it out.
