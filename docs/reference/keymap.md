---
title: Keymap
createTime: 2026/07/31 10:00:00
---

Every binding in the client, by context. For the model behind it, see [From the keyboard](../guides/keyboard.md).

Keys are offered to contexts in a fixed order and the first claimant stops propagation, so a binding listed under a lower layer only fires when nothing above it claimed the key.

## Global

Reachable from wherever no pane or overlay claimed the key first.

| Key | Action |
| --- | --- |
| <kbd>ctrl</kbd>+<kbd>c</kbd> | Quit. Checked above everything else, so it works from every screen including modals, consent and login. |
| <kbd>ctrl</kbd>+<kbd>p</kbd> | Toggle the command palette. Not reachable from a modal, consent, or the login screens. |
| <kbd>?</kbd> | Open the help overlay. Types a literal `?` in any text field, since those claim character keys first. |
| <kbd>/</kbd> | Open in-chat search. Only from selection mode, and only with a chat open. |
| <kbd>Esc</kbd> | Pop one level. See below. |

Only <kbd>ctrl</kbd>+<kbd>p</kbd> is configurable (`keys.palette`). <kbd>?</kbd> and <kbd>ctrl</kbd>+<kbd>c</kbd> are hard-coded despite living beside it in the code.

### What Esc pops, in order

1. The newest visible toast, if any
2. Out of the archive, if the chat list is showing it
3. Selection mode, the chat filter (discarding it), the palette, or in-chat search (clearing highlights), depending on what's on top
4. Anything else on the stack
5. From the composer at the base of the stack: back to the chat list
6. At the very bottom: nothing. <kbd>Esc</kbd> never quits.

<kbd>Esc</kbd> is intercepted centrally, above the pane handlers, which is what makes "one <kbd>Esc</kbd>, one level" hold everywhere.

## Pane movement

Only at the base of the focus stack (never with an overlay, filter, selection or modal up).

| Key | From | To |
| --- | --- | --- |
| <kbd>→</kbd> | Chat list | Composer (requires an open chat) |
| <kbd>Tab</kbd> / <kbd>Shift</kbd>+<kbd>Tab</kbd> | Chat list | Composer (requires an open chat) |
| <kbd>Tab</kbd> / <kbd>Shift</kbd>+<kbd>Tab</kbd> | Composer | Chat list |

<kbd>←</kbd> is deliberately not a pane key. With two panes, <kbd>Tab</kbd> and <kbd>Shift</kbd>+<kbd>Tab</kbd> are indistinguishable.

## Chat list

| Key | Action |
| --- | --- |
| <kbd>↑</kbd> / <kbd>↓</kbd> | Move the selection, clamped, no wraparound |
| <kbd>Enter</kbd> | Open the selected chat and move focus to the composer |
| <kbd>/</kbd> | Start the title filter |
| <kbd>a</kbd> | Toggle the archive |
| <kbd>]</kbd> | Next folder, wrapping. No-op in the archive or with one folder. |
| <kbd>[</kbd> | Previous folder, same conditions |
| <kbd>Esc</kbd> | Leave the archive (only while in it) |

### Chat filter

| Key | Action |
| --- | --- |
| any character | Insert into the query |
| <kbd>Backspace</kbd> | Delete backwards |
| <kbd>↑</kbd> / <kbd>↓</kbd> | Move the selection within the filtered list |
| <kbd>Enter</kbd> | Commit: close the input, keep the list filtered |
| <kbd>Esc</kbd> | Cancel and clear the filter |

No caret movement. <kbd>←</kbd>, <kbd>→</kbd>, <kbd>Home</kbd>, <kbd>End</kbd> and <kbd>Delete</kbd> are inert here.

## Composer

| Key | Action |
| --- | --- |
| any character | Insert at the caret |
| <kbd>Enter</kbd> | Send, or apply an edit if one is armed, or confirm a `/send` |
| <kbd>Alt</kbd>+<kbd>Enter</kbd> | Insert a newline |
| <kbd>↑</kbd> on empty input | Enter selection mode on the newest message |
| <kbd>↑</kbd> with text | Move the caret up a line |
| <kbd>←</kbd> / <kbd>→</kbd> | Move the caret one character |
| <kbd>Home</kbd> / <kbd>End</kbd> | Start / end of the buffer |
| <kbd>Backspace</kbd> / <kbd>Delete</kbd> | Delete before / after the caret |
| <kbd>↓</kbd>, <kbd>PageUp</kbd>, <kbd>PageDown</kbd> | Not claimed: scroll the conversation |

`/send <path>` followed by <kbd>Enter</kbd> raises the send-file confirmation. Pasting or dropping a bare path does the same without typing `/send`: bracketed paste is on, and text that looks like a single-line path is held as a pending offer instead of inserted. <kbd>Enter</kbd> confirms it the same way, <kbd>Esc</kbd> discards it and returns to an empty composer. Anything else pasted arrives as ordinary text at the caret.

## Conversation scrolling

There is no separate conversation focus. These work from the composer and from selection mode.

| Key | Action | Available from |
| --- | --- | --- |
| <kbd>↓</kbd> | One message newer | Composer only (selection mode claims it) |
| <kbd>PageUp</kbd> | A page older | Composer and selection mode |
| <kbd>PageDown</kbd> | A page newer | Composer and selection mode |

Scrolling near the top of the loaded window triggers a history page. There is no row-level scrolling; movement is per message.

## Selection mode

| Key | Action |
| --- | --- |
| <kbd>↑</kbd> / <kbd>↓</kbd> | Select an older / newer message, clamped |
| <kbd>←</kbd> / <kbd>→</kbd> | Move the chip cursor, clamped. Up to five chips visible. |
| <kbd>Enter</kbd> | Invoke the focused chip |
| <kbd>Esc</kbd> | Leave selection mode |
| chip letter | Invoke that chip directly |

### Chip letters

| Key | Chip | Shown when |
| --- | --- | --- |
| <kbd>r</kbd> | Reply | Always |
| <kbd>f</kbd> | Forward | The message can be forwarded |
| <kbd>e</kbd> | React | Always. Toggles 👍. |
| <kbd>c</kbd> | Copy | The message can be saved |
| <kbd>d</kbd> | Edit | Your own message, editable, and text content only |
| <kbd>x</kbd> | Delete | Deletable for you or for everyone |
| <kbd>l</kbd> | Download | Has a file that isn't downloaded |
| <kbd>o</kbd> | Open | Has a file that is downloaded |
| <kbd>s</kbd> | Resend | The send failed. Failed messages show only Resend, Delete, and Cancel upload (if a file upload is still tracked for it). |
| <kbd>v</kbd> | Reveal | The message has an unrevealed spoiler. Never shown on a failed send: nothing server-confirmed to reveal. |
| <kbd>k</kbd> | Cancel upload | A file you sent from this message is still uploading. Shown even after a failed send, unlike Reveal, since an upload stuck mid-transfer is exactly what you'd want to abandon. |

A letter with no matching chip in the current row isn't swallowed; it falls through to the global layer, which is how <kbd>?</kbd> and <kbd>/</kbd> still work here.

## In-chat search

Entered with <kbd>/</kbd> from selection mode.

| Key | Action |
| --- | --- |
| any character | Insert into the query |
| <kbd>Backspace</kbd> | Delete backwards |
| <kbd>Enter</kbd> | Run the search |
| <kbd>n</kbd> | Next hit, wrapping. Only once hits exist; before that it types an `n`. |
| <kbd>N</kbd> | Previous hit, wrapping. Same condition. |
| <kbd>Esc</kbd> | Close search and clear the highlights |

Hits come back newest-first, so <kbd>n</kbd> walks toward older messages. Arrow keys, <kbd>Home</kbd>, <kbd>End</kbd> and <kbd>Delete</kbd> are inert.

## Command palette

| Key | Action |
| --- | --- |
| any character | Insert into the query and re-rank |
| <kbd>Backspace</kbd> | Delete backwards |
| <kbd>↑</kbd> / <kbd>↓</kbd> | Move the selection, clamped |
| <kbd>Enter</kbd> | Run the selected item |
| <kbd>ctrl</kbd>+<kbd>p</kbd> / <kbd>Esc</kbd> | Close |

No caret movement in the query. Commands: Toggle theme, Log out, Quit, plus Telemetry settings and Send file, which are both claimed no-ops.

## Modals

Modals swallow every key except <kbd>ctrl</kbd>+<kbd>c</kbd>. Neither the palette nor help is reachable while one is up.

**Confirm delete.** <kbd>Enter</kbd> deletes, <kbd>Esc</kbd> cancels. Arrow keys and <kbd>Tab</kbd> toggle between "Delete for me" and "Delete for everyone", but only when the message can be deleted for everyone; otherwise the choice is forced.

**Confirm send file.** <kbd>Enter</kbd> sends, <kbd>Esc</kbd> cancels. No cursor, no options.

## Help overlay

Opened with <kbd>?</kbd>, closed with <kbd>Esc</kbd>. It swallows every key while up, including a second <kbd>?</kbd>.

There is no scrolling. If the keymap doesn't fit the terminal height, the last visible row becomes a dim `…`, which happens at 80×24. The overlay's own table is also incomplete relative to this page: it omits the filter, palette and search bindings, the conversation scroll keys, and several <kbd>Esc</kbd> cases, and it lists <kbd>←</kbd>/<kbd>→</kbd> as pane movement, which <kbd>←</kbd> has never been.

## Consent screen

| Key | Action |
| --- | --- |
| <kbd>↑</kbd> <kbd>↓</kbd> <kbd>←</kbd> <kbd>→</kbd> <kbd>Tab</kbd> <kbd>Shift</kbd>+<kbd>Tab</kbd> | Flip between Enable and Disable |
| <kbd>Enter</kbd> | Record the answer and continue |

Enable is preselected. Every other key is swallowed.

## Login screens

### Method choice

| Key | Action |
| --- | --- |
| <kbd>↑</kbd> / <kbd>↓</kbd> | Flip between phone and QR |
| <kbd>p</kbd> / <kbd>q</kbd> | Pick phone / QR directly |
| <kbd>Enter</kbd> | Confirm. Phone opens the number field; QR fires the request. |

With QR picked but not yet confirmed, <kbd>↑</kbd>, <kbd>↓</kbd> and <kbd>p</kbd> flip back to phone and <kbd>Enter</kbd> fires the request.

### Credentials wizard

| Key | Action |
| --- | --- |
| <kbd>Tab</kbd> / <kbd>↓</kbd> | API id to API hash |
| <kbd>Shift</kbd>+<kbd>Tab</kbd> / <kbd>↑</kbd> | API hash to API id |
| <kbd>Enter</kbd> on API id | Move to API hash (does not submit) |
| <kbd>Enter</kbd> on API hash | Submit and save |

### Text fields (phone, code, password, credentials)

| Key | Action |
| --- | --- |
| any character | Insert at the caret |
| <kbd>Enter</kbd> | Submit |
| <kbd>←</kbd> / <kbd>→</kbd> | Move the caret |
| <kbd>Home</kbd> / <kbd>End</kbd> | Start / end |
| <kbd>Backspace</kbd> / <kbd>Delete</kbd> | Delete before / after |

Submission is blocked while a request is in flight or during a flood wait. The QR display screen and the intermediate phases claim every key and do nothing; <kbd>ctrl</kbd>+<kbd>c</kbd> is the only way out of them. <kbd>Esc</kbd> never reaches the escape handler while a login screen is up.

## Mouse

Requires `[app] mouse = true` (the default).

| Gesture | Target | Action |
| --- | --- | --- |
| Left click | Chat row | Select and open |
| Left click | Archive row | Enter the archive |
| Left click | Folder tab | Switch folder, select its first row |
| Left click | Composer | Focus the composer (needs an open chat) |
| Left click | Spoiler run in a message | Reveal it |
| Left click | Reply-quote line in a message | Jump to the quoted message |
| Right click | Message | Enter selection mode on it |
| Wheel | Chat list | Move the selection one row per step |
| Wheel | Conversation | Scroll one message per step |

Left-clicking ordinary message text is still a no-op. Only a masked spoiler run or a reply-quote line responds, and both are narrower hit targets than the message's full row. Hover, drag, release, the middle button and horizontal scroll are all discarded. Nothing is clickable while a modal, the palette or help is up, or on the consent and login screens.

## Not bound to anything

| What | Status |
| --- | --- |
| Function keys, <kbd>Insert</kbd>, media keys | Dropped at the input layer |
| <kbd>Alt</kbd>+ anything except <kbd>Enter</kbd> | Arrives as the bare key; there is no Alt modifier in the model |
| Rebinding <kbd>?</kbd> or <kbd>ctrl</kbd>+<kbd>c</kbd> | Not configurable |
