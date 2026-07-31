---
title: First login
createTime: 2026/07/31 10:00:00
---

The first run walks through up to three screens before you see a chat: a telemetry disclosure, the API credentials wizard if you haven't supplied any, and the sign-in screen. After that, TDLib keeps the session and you go straight to the chat list.

## The telemetry screen comes first

Before login, before any TDLib traffic, and before either egress is constructed, `tgt` shows a panel titled "Before we start". It opens by saying that anonymous crash reports are on unless you turn them off, then lists what gets collected (app and OS version, terminal type, a stack trace and error message when something goes wrong, recent action names like "sent a message", outcomes, durations, and a random per-install id) and what doesn't (message text, contact names, phone numbers, chat titles, file names, your IP address, your computer's name). It also spells out the caveat: an error message is written by whatever failed rather than chosen from that list, so it can carry limited content such as a file path.

Arrow keys, <kbd>Tab</kbd>, or <kbd>Shift</kbd>+<kbd>Tab</kbd> flip between Enable and Disable; Enable is preselected, because reporting is on by default and the screen is there to make that visible rather than to pretend otherwise. <kbd>Enter</kbd> records your answer and moves on. Every other key is swallowed rather than passed through, so a keystroke can't leak to the screen behind it.

Two details worth knowing. Answering Disable sets the in-memory telemetry mode to off in the same tick, so no stale mode can mint events between your answer and the config write, and it persists as `[telemetry] enabled = false`. A build from source also sends nothing either way, but for a different reason than it used to: the Sentry DSN is baked in at compile time via `TGT_SENTRY_DSN`, and without one the client never initialises Sentry at all, so there's no panic hook and no uploader. [Telemetry controls](../guides/telemetry.md) covers the rest.

## Signing in

Two methods, chosen from a small "Sign in" panel:

```text
 ▶ Phone number
   QR code

 ↑↓ choose · p phone · q qr · ⏎ continue
```

<kbd>↑</kbd>/<kbd>↓</kbd> flip between them, or press <kbd>p</kbd> / <kbd>q</kbd> directly. <kbd>Enter</kbd> confirms.

Picking QR arms it without doing anything; the request only fires on <kbd>Enter</kbd>, and <kbd>↑</kbd>/<kbd>↓</kbd>/<kbd>p</kbd> can still flip you back to phone. That's deliberate: an arrow key should never cause network I/O.

### By phone number

Type the number in international form (leading `+`, country code, no spaces) and press <kbd>Enter</kbd>. Telegram sends a login code, usually to your existing Telegram apps rather than by SMS. Type it, <kbd>Enter</kbd> again.

Editing keys in these fields are the ones you'd guess: <kbd>←</kbd>/<kbd>→</kbd> for the caret, <kbd>Home</kbd>/<kbd>End</kbd>, <kbd>Backspace</kbd>, <kbd>Delete</kbd>. Everything else is ignored, including <kbd>Esc</kbd> (there's no way back a screen from auth, only <kbd>ctrl</kbd>+<kbd>c</kbd> to quit).

### By QR code

`tgt` renders the login link as an actual QR code in the terminal, drawn with half-block characters. Open Telegram on your phone, go to Settings → Devices → Link Desktop Device, and point the camera at it.

If the terminal is too small for the code to fit, the link is printed as text instead. It's a `tg://login?token=…` URL; you can't usefully type it, but you can copy it out of your scrollback.

The QR screen has no cancel key. Every key is claimed and discarded, so if you change your mind, quit with <kbd>ctrl</kbd>+<kbd>c</kbd> and start again.

### Two-factor password

If your account has a cloud password set, a password field appears after the code with Telegram's hint (if you configured one) shown alongside. Same editing keys, <kbd>Enter</kbd> submits.

## Flood waits

Telegram rate-limits authentication attempts hard. If you hit one, a countdown appears under the field and submissions are blocked until it expires. The countdown is rendered against the app's own tick clock rather than read from the system clock during a draw, so it decrements smoothly and doesn't jump when the terminal is idle.

While a submission is in flight, a `…` marker appears and further <kbd>Enter</kbd> presses are ignored. That prevents a double-tap turning into two `CheckAuthenticationCode` calls and burning the code.

## After login

`tgt` requests the first 200 chats for the main list and switches to the chat list. The session is stored in the TDLib database under `~/.local/share/telegram-tui/td/`, encrypted with a key held in your OS credential store, so the next start goes straight to your chats.

To log out, open the command palette with <kbd>ctrl</kbd>+<kbd>p</kbd> and run **Log out**. That issues a real `logOut` to Telegram and drops the session.
