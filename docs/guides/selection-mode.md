---
title: Selection mode & chips
createTime: 2026/07/31 10:00:00
---

Selection mode is where everything you can do *to* a message lives. It's the reason the client doesn't need a shortcut sheet.

## Getting in and out

With the composer empty, press <kbd>↑</kbd>. The newest message highlights (a `surface_raised` background across its lines, no border) and the hint bar turns into a chip row:

```text
 ‹ [R Reply]  [F Forward]  [E React]  [C Copy]  [D Delete] ›
```

<kbd>Esc</kbd> leaves, putting you back in the composer.

You can also right-click a message to enter selection mode directly on it. See [Using the mouse](mouse.md).

## Moving

| Key | What it moves |
| --- | --- |
| <kbd>↑</kbd> / <kbd>↓</kbd> | The selected message, one at a time, clamped at both ends |
| <kbd>←</kbd> / <kbd>→</kbd> | The chip cursor within the row |
| <kbd>Enter</kbd> | Invokes the focused chip |
| letter | Invokes that chip directly |

The chip row scrolls if there are more chips than fit; the `‹` and `›` markers tell you there's more in that direction. At most five chips are visible at once.

Changing the selected message does a fair amount of work behind the scenes: it fills the reply excerpt, recomputes the chip row, re-anchors the viewport, fires a `GetMessageProperties` request for the new message's capabilities, and kicks off photo auto-download for anything newly in view.

## The chips

| Letter | Chip | What it does |
| --- | --- | --- |
| <kbd>r</kbd> | Reply | Arms the composer's reply target and returns you to the composer. Nothing is sent. |
| <kbd>f</kbd> | Forward | Forwards to whatever chat the sidebar cursor is on. |
| <kbd>e</kbd> | React | Toggles 👍, the fixed default reaction. |
| <kbd>c</kbd> | Copy | Copies the message text to the clipboard, or the file name for video, audio and documents. |
| <kbd>d</kbd> | Edit | Loads the text into the composer with the caret at the end and switches the composer into edit mode. |
| <kbd>x</kbd> | Delete | Opens a confirmation modal. |
| <kbd>l</kbd> | Download | Starts downloading the attachment. |
| <kbd>o</kbd> | Open | Hands the downloaded file to the system opener. |
| <kbd>s</kbd> | Resend | Only on a message whose send failed: drops it and sends it again with the original reply target. |
| <kbd>v</kbd> | Reveal | Only on a message with an unrevealed spoiler: reveals it. |
| <kbd>k</kbd> | Cancel upload | Only while a file you sent is still uploading: abandons it. |

Note that Edit is <kbd>d</kbd>, not <kbd>e</kbd>. React took <kbd>e</kbd> first.

## Why a chip is missing

The row is built from TDLib's per-message capability flags, requested with `GetMessageProperties` when you select the message. The rules:

- Reply and React are always offered.
- Forward appears when the message can be forwarded, Copy when it can be saved, Delete when it can be deleted for you or for everyone.
- Edit appears only on your own messages that Telegram says are editable, and only on text messages. Caption editing isn't built.
- Download appears when the message has a file that isn't downloaded yet; Open replaces it once it is.
- A message whose send failed short-circuits to Resend and Delete, plus Cancel upload too if there's still a file upload tracked for it. That's the one chip *not* suppressed by a failed send, since an upload stuck mid-transfer is exactly the thing you'd want to abandon.

Reveal and Cancel upload are the two exceptions to "built from TDLib's capability flags" in the first place: neither is a `MessageCaps` field. Reveal is gated on a local rendering fact (an unrevealed spoiler entity in the message, not yet clicked or chipped past) and never appears on a failed send: there's nothing server-confirmed to reveal. Cancel upload is gated on this client still tracking an upload for the message at all.

Until the capability response comes back, the row shows the pessimistic set. It fills in a moment later.

A letter that no chip in the current row answers to isn't swallowed; it falls through to the layers below. That's exactly how <kbd>?</kbd> and <kbd>/</kbd> still work from selection mode.

## Delete

<kbd>x</kbd> opens a modal rather than deleting. If Telegram says the message can be deleted for everyone, arrow keys toggle between "Delete for me" and "Delete for everyone"; otherwise the choice is forced to "for me" and the arrows do nothing. <kbd>Enter</kbd> confirms, <kbd>Esc</kbd> cancels.

Modals swallow every key except <kbd>ctrl</kbd>+<kbd>c</kbd>. The palette and help are unreachable while one is up, on purpose.

## Rough edges

Two, and both are honest v1 limitations rather than bugs to be worked around:

**Forward has no destination picker.** It sends to whatever chat the chat-list cursor happens to be on. If nothing is selected there, <kbd>f</kbd> silently does nothing. Check the sidebar before pressing it.

**Edit is text-only.** On a photo with a caption, <kbd>d</kbd> won't even appear, because the chip is gated on the content being text.
