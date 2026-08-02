# Keyboard navigation: pane focus, selection scrolling, jump-to-quote

Date: 2026-08-02

Three independent changes to how the keyboard drives the conversation pane.
They share no state and can be built and merged in any order; they are
specified together because all three touch `state/selection.rs` and the focus
routing table in `app.rs`.

## 1. `Ctrl+←` / `Ctrl+→` move pane focus

### Problem

Leaving the conversation side today means `Tab` or `Esc`. Architecture §6.2's
`←` never survived contact with the panes it moves between: in the composer
`←` is the caret key, and in selection mode it walks the chip row. So the
horizontal movement the spec calls for exists in one direction only, and the
key that reads as "go left" is unavailable exactly where it is most wanted.

`Ctrl`-modified arrows are free. They carry no meaning in the composer (the
caret moves by character, not by word) and none in the chip row.

### The key model does not currently distinguish them

`crates/ui/src/input/mod.rs` maps `KeyCode::Left => Some(Key::Left)` with no
reference to `ev.modifiers`, so `Ctrl+←` is today indistinguishable from `←`
by the time it reaches `update()`.

`Key` gains exactly two variants:

```rust
pub enum Key {
    // …
    CtrlLeft,
    CtrlRight,
}
```

`Ctrl+↑` / `Ctrl+↓` are deliberately not added. There is no third pane and no
vertical pane movement to bind them to; unroutable variants are dead surface.

`map_key_event` checks `ctrl` on the two arrow arms before the plain ones,
mirroring the existing `KeyCode::Enter if alt` arm.

### Routing

Lands in `app.rs` next to `move_pane_focus`. That file owns every focus-stack
transition; state handlers return `None` rather than touching `app.focus`, and
this change must not be the exception that breaks the one-stack rule.

| Focus | `Ctrl+←` | `Ctrl+→` |
| --- | --- | --- |
| `ChatList` | unclaimed | open selected chat, focus `Composer` |
| `Composer` | → `ChatList` | unclaimed |
| `Selection` | pop, → `ChatList` | unclaimed |
| `ChatFilter` | unclaimed | unclaimed |
| `ChatSearch` | unclaimed | unclaimed |
| `Palette`, `Help`, `Modal(_)` | unclaimed | unclaimed |

"Unclaimed" means the router falls through as it does for any unhandled key.
Nothing else binds `Ctrl+←`/`Ctrl+→`, so a fall-through is a no-op.

Three details are load-bearing:

- **`Selection` is a pushed focus level** (depth 2), and `move_pane_focus`
  runs at depth 1 only, so that an overlay never gets the pane swapped
  underneath it. `Ctrl+←` from `Selection` pops that one level and then swaps
  the base. This is a narrow exception for `Focus::Selection` specifically —
  the depth gate is not lifted, and the filter, the search overlay, the
  palette and modals keep their existing protection.
- **Swapping the base to `ChatList` must run
  `conversation::close_if_now_hidden`**, exactly as the `Esc` path in `app.rs`
  does. In two-pane layout this changes nothing; in single-pane it is the
  transition that stops rendering the conversation, and skipping it leaves the
  chat open behind a pane that no longer shows it.
- **`Ctrl+→` from `ChatList` reuses the existing open path**, the bracket
  `click_chat_row` already uses: push `Focus::ChatList`, drive
  `chat_list::handle_key(Key::Enter)`, pop, then `replace_base(Composer)` if a
  chat ended up open. A second open path would drift from the first.

`Ctrl+→` differs from the existing `→` binding: `→` moves focus to the
composer only when a chat is already open, while `Ctrl+→` opens the selected
chat first. Both bindings stay.

### Terminal support

`Ctrl`-modified arrows reach the application only if the terminal emits the
modified sequence (`CSI 1;5D` and friends). Ghostty, kitty, iTerm2, WezTerm
and Alacritty do. Apple Terminal.app does not by default, and there `Ctrl+←`
arrives as a plain `←`, i.e. the pre-existing behavior. `Tab` and `Esc` remain
the universally available way out of the conversation side, so no
configuration is required for the feature set to stay reachable. This is a
documentation matter, not a fallback to implement.

## 2. Selection movement stops dragging the viewport

### Problem

Arrowing up from the composer selects the newest message; arrowing up again
scrolls the whole conversation by one message, so the selected message is
always the last visible row. Walking back through ten messages on screen
scrolls the view ten times instead of zero.

### Cause

`state/selection.rs`'s `select()` ends with an unconditional

```rust
effects.extend(conversation::anchor_to(convo, chat_id, message_id, now));
```

and `view/conversation.rs` fills the viewport bottom-up from that anchor. The
selected message is pinned to the bottom row by construction.

### Core cannot see the viewport, and must not guess

`update()` is pure and never sees a laid-out frame. Message blocks have
variable height (a photo, a wrapped paragraph and a one-line reply are not
interchangeable), so any message-count approximation in core would be wrong
for real content while passing tests built on synthetic uniform messages.

The established pattern for this is architecture §7.5: resolve at the
boundary, hand core semantic data. It already applies to the mouse, and the
map it uses already carries the answer.

### The `HitMap` already knows

`crates/ui/src/render/hit.rs` stores `Vec<(Rect, HitTarget)>`, and
`view/conversation.rs` pushes a `HitTarget::Message(id)` rect for every
message block it draws. The set of on-screen messages is therefore already
recorded per frame, with no new render plumbing.

Additions:

- `HitMap::visible_messages(&self) -> Option<(MessageId, MessageId)>` — the
  minimum and maximum id among `HitTarget::Message` entries. `HitTarget::
  Spoiler` and `HitTarget::ReplyQuote` also carry ids, but they are sub-row
  regions inside blocks that already contribute a `Message` entry, so the
  accessor scans `Message` entries only rather than relying on that overlap.
- `Action::ViewportChanged { first: MessageId, last: MessageId }`, sent by
  `runtime_loop` immediately after each render and before awaiting the next
  event, and only when the range differs from the last one sent. Sharing the
  one mpsc channel with keys is what guarantees ordering: the range core holds
  when it processes a keystroke is the range from the frame the user was
  looking at when they pressed it.
- `AppState.visible_messages: Option<(MessageId, MessageId)>`, cleared when
  the open chat changes. One field suffices; only one conversation renders.

### The new rule in `select()`

```
target inside [first, last]  -> leave convo.scroll untouched
target older than first      -> step the anchor ONE message older
target newer than last       -> step the anchor ONE message newer
                                (Scroll::Bottom if that is the newest loaded)
```

"Step the anchor one message" means: resolve the current anchor to its index
in `convo.messages` (`Scroll::Bottom` resolves to the last index), move by one,
and set `Scroll::At { message_id: that id, line_offset: 0 }`. Because selection
also moves one message at a time, the two stay in lockstep once scrolling
starts.

`trigger_paging_if_near_top` still runs on every anchor move, so paging older
history in at the top of the window is unaffected.

### The `None` fallback is not a detail

When `visible_messages` is `None`, `select()` keeps the current behavior and
calls `anchor_to` unconditionally.

This is required, not defensive. Every `crates/core` unit test and every
`crates/app/tests/` integration test drives `update()` with no renderer
attached, so `visible_messages` is `None` throughout all of them. Treating
`None` as "everything is visible, never scroll" would leave the entire suite
green about a code path no user reaches — the same shape of failure recorded
in CLAUDE.md for `tgt update`'s symlink classification, which passed every
unit test while being unable to update any real install.

Tests for the new behavior set `visible_messages` explicitly to model a
viewport, and are therefore the only tests exercising the new branch. Each
must be watched failing before it is trusted.

### Out of scope

`conversation::jump_to_message` (the mouse reply-quote path, and §3's chip)
keeps moving the view unconditionally. A deliberate jump to an off-screen
message must scroll; only *stepping* the selection should not.

## 3. Jump to the quoted message

### Problem

`HitTarget::ReplyQuote { containing, quoted }` exists and left-clicking a
reply quote already calls `conversation::jump_to_message` (`app.rs:1136`).
There is no keyboard path to it. `MessageView.reply_to: Option<ReplyPreview>`
carries the target id, so the missing piece is a chip.

### The chip

```rust
Chip::JumpToQuoted   // 'j', "Jump to quote"
```

`'j'` is free; the letters in use are `r f e c d x l o s v k`.

It is appended by `state/selection.rs` after `chips_for` runs, gated on
`msg.reply_to.is_some()`, rather than added to `chips_for` itself — the same
"local rendering fact, not a TDLib capability" pattern `Chip::Reveal` and
`Chip::CancelUpload` already use, and which `model/chips.rs` documents.

The chip is offered whenever `reply_to` is `Some`, including when the quoted
message is not loaded. This is consistent with the module's "an action that
would fail is never offered" rule: with the hunt below, the action does not
fail. It either lands on the message or reports honestly that it gave up.

### Invoking it

**Target loaded** (`conversation::index_of` finds it): `select()` it, with a
real `anchor_to`. Selection follows the jump, so pressing `j` again walks a
reply chain. This deliberately bypasses §2's visibility rule.

Concretely, §2 splits `select()`'s tail into two named paths — one that steps
the anchor only when the target is off-screen (used by `↑`/`↓`) and one that
anchors unconditionally (the pre-existing `anchor_to` behavior). Every jump in
this section, including the hunt landing on its target below, takes the
unconditional path.

**Target not loaded**: start a hunt.

### The hunt

```rust
pub struct JumpHunt {
    target: MessageId,
    pages_spent: u8,
}
// on ConversationState:
pub hunt: Option<JumpHunt>,

pub const MAX_HUNT_PAGES: u8 = 20;   // 20 × PAGE_SIZE = 1000 messages
```

The hunt drives the existing `state/history.rs` `PagingState` machine rather
than opening a parallel request path. `on_scroll_near_top`'s `Idle` gate is
exactly the right condition after a page completes, so both callers — "the
anchor is near the top" and "a hunt is running" — issue through one shared
`request_next_page` helper.

After each history page is prepended, in order:

1. Target now loaded → `select()` it, clear `hunt`. Done.
2. `PagingState::Exhausted` → toast ("the quoted message is no longer
   available"), clear `hunt`. The start of history was reached.
3. `pages_spent >= MAX_HUNT_PAGES` → toast, clear `hunt`.
4. Otherwise `pages_spent += 1` and request the next page.

`history::on_history_error` firing during a hunt (FLOOD_WAIT or a transient
failure) toasts and clears the hunt rather than waiting out the `Cooldown`.
A hunt that silently stalls for a FLOOD_WAIT's duration is worse than one that
says it stopped; the user can press `j` again.

Toasts go through `toasts::on_action_failed(app, chat_id, body)`.

Cancellation: `Esc`, closing the chat, and manual scroll keys all clear
`hunt`.

### The eviction trap this design has to pay for

`conversation::evict_excess` drops from the **front** of the window whenever
the anchor is `Scroll::Bottom`. A hunt that left the anchor at the bottom
would therefore evict each page it fetched as soon as the window reached
`WINDOW_MAX_MESSAGES` (500) — it would page indefinitely and never find
anything, burning its whole budget and every request in it.

So the hunt sets the anchor to the oldest loaded message after each page.
`evict_excess` then computes `dist_front <= dist_back` as true and drops from
the back instead, and the window genuinely walks backward through history.

This also supplies the progress feedback for free: the view visibly scrolls
back while the hunt runs, so no spinner or "searching…" toast is needed.

### Rejected alternative: centered fetch

TDLib's `getChatHistory` takes an `offset` parameter that
`app/src/td_runtime.rs:257` currently hardcodes to `0`. With
`offset: -25, limit: 50` it returns a window *centered* on any message id, in
one round trip, which would make an arbitrarily distant jump instant.

Not adopted. The returned window is disjoint from the currently loaded one,
while `ConversationState.messages` is a contiguous ascending `VecDeque` that
prepend-on-page, append-on-new and `evict_excess` all assume. Inserting a
disjoint region would leave a hole that renders as continuous history — a
silently wrong view, not a visibly broken one. Closing that properly requires
forward paging, which v1 has never had (`mark_visible_read`'s doc comment
relies on "v1 only ever pages backwards" being true).

The hunt preserves contiguity by construction, and in practice a reply quote
is usually a message still in the window or one page behind it, so the common
case costs zero or one request.

## Cross-cutting work

- **`docs/architecture.md` is edited before the code.** This reshapes `Key`,
  `Chip`, `AppState`, `ConversationState` and `HitMap` — all shared types, all
  covered by the documents-are-the-contract rule.
- **Snapshot review.** New chips change the chip row; `Ctrl`-arrow bindings
  change the help overlay if it lists pane movement. `crates/ui/tests/
  snapshots/` and the per-component snapshots must be reviewed, not blanket
  accepted.
- **Telemetry.** If chip invocation emits an allowlisted event, `JumpToQuoted`
  needs a constant in `core/src/telemetry/schema.rs`, which surfaces as an
  insta diff in review. `crates/app/tests/telemetry_allowlist.rs` fails if an
  attribute escapes without one.
- **Verification.** Per CLAUDE.md, each new test is watched failing before it
  is trusted, with the `None`-fallback branch of §2 and each hunt termination
  arm of §3 broken separately — a carve-out where one half is dead while the
  other carries the suite is exactly what this repo has been bitten by.
- **`mise run check`** (fmt, clippy `-D warnings`, tests, crate boundaries) is
  the merge gate.
