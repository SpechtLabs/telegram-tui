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

`tgt` shows a QR code by default. There's no "phone or QR?" choice up front. While it's still waiting on the code from Telegram, a "Requesting a QR code…" placeholder holds the spot; once it lands, it renders as an actual QR code in the terminal, drawn with half-block characters:

```text
 Scan this QR code with Telegram on another device

                  █▀▀▀▀▀█ ▄▀▀▄█ █▀▀▀▀▀█
                  █ ███ █ ▀▄▀▀▄ █ ███ █
                  █▄▄▄▄▄█ █▄▀▄█ █▄▄▄▄▄█

           Sign in with phone number instead
      Settings → Devices → Link Desktop Device
                ↑↓ select · ⏎ confirm
```

Open Telegram on your phone, go to Settings → Devices → Link Desktop Device, and point the camera at it. If the terminal is too small for the code to fit, the link prints as text instead: a `tg://login?token=…` URL, not something you'd usefully type, but you can copy it out of your scrollback.

Underneath the QR, <kbd>↑</kbd>/<kbd>↓</kbd> highlights "Sign in with phone number instead," and <kbd>Enter</kbd> swaps it in for a phone number field. Type the number in international form (leading `+`, country code, no spaces) and press <kbd>Enter</kbd>; Telegram sends a login code, usually to your existing Telegram apps rather than by SMS. Type it, <kbd>Enter</kbd> again.

Switching to phone works at any point, including after the QR code has rendered. Telegram won't accept a phone number on a connection that has already issued a QR link, so `tgt` signs that attempt out and reconnects before sending it. That takes a couple of seconds, the screen says `Closed` briefly while it happens, and the number you typed is still there when the phone field comes back. Press <kbd>Enter</kbd> to send it.

Editing keys in these fields are the ones you'd guess: <kbd>←</kbd>/<kbd>→</kbd> for the caret, <kbd>Home</kbd>/<kbd>End</kbd>, <kbd>Backspace</kbd>, <kbd>Delete</kbd>. Everything else is ignored, including <kbd>Esc</kbd> (there's no way back a screen from auth, only <kbd>ctrl</kbd>+<kbd>c</kbd> to quit).

### Two-factor password

If your account has a cloud password set, a password field appears after the code with Telegram's hint (if you configured one) shown alongside. Same editing keys, <kbd>Enter</kbd> submits.

## Flood waits

Telegram rate-limits authentication attempts hard. If you hit one, a countdown appears under the field and submissions are blocked until it expires. The countdown is rendered against the app's own tick clock rather than read from the system clock during a draw, so it decrements smoothly and doesn't jump when the terminal is idle.

While a submission is in flight, a `…` marker appears and further <kbd>Enter</kbd> presses are ignored. That prevents a double-tap turning into two `CheckAuthenticationCode` calls and burning the code.

## After login

`tgt` requests the first 200 chats for the main list and switches to the chat list. The session is stored in the TDLib database under `~/.local/share/telegram-tui/td/`, encrypted with a key held in your OS credential store, so the next start goes straight to your chats.

To log out, open the command palette with <kbd>ctrl</kbd>+<kbd>p</kbd> and run **Log out**. That issues a real `logOut` to Telegram and drops the session.
