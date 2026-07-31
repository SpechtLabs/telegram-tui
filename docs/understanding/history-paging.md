---
title: How history paging survives TDLib
createTime: 2026/07/31 10:00:00
---

`getChatHistory` can return zero messages for a chat that has thousands. Not as an error, not as a bug in TDLib, but as normal operation: the local database hasn't been filled yet and TDLib hasn't gone to the server. Any client that reads an empty response as "you've reached the beginning" will silently break scroll-up on exactly the chats you open least often.

This is the single nastiest piece of TDLib behaviour the client has to handle, and the handling is a small state machine with a doc comment that names the trap.

## The trap

```rust
//! See docs/architecture.md §4.6 and design spec §5.2 (the empty-response
//! trap: `getChatHistory` may legitimately return zero messages on the first
//! call for a chat while TDLib fetches from the server, even though more
//! history exists. A short or empty response is therefore never treated as
//! proof of end-of-history on its own).
```

The word doing the work is *alone*. An empty response is evidence, but weak evidence, and how weak depends on what you asked for.

## The rules

Four states: `Idle`, `Loading { attempt, only_local }`, `Cooldown { until }`, and `Exhausted`. The machine returns a directive rather than performing I/O, which is what keeps it inside the pure `update()`.

The interesting function is the one that handles a completed request:

- **Any non-empty response, however short, is progress.** Back to `Idle`, prepend what arrived. One message is not exhaustion.
- **An empty response to an `only_local: true` request proves nothing.** TDLib hasn't asked the server yet. Re-issue it remote, unconditionally. Local empties never advance the attempt counter and never count toward the retry budget.
- **An empty, non-local response is the only kind that can end paging.** Retry up to three times, incrementing the attempt, then latch `Exhausted`.
- **A completion arriving while the state isn't `Loading`** (a stale response from a request that's been superseded) leaves everything untouched.

```rust
if received > 0 {
    *paging = PagingState::Idle;
    return PagingDirective::None;
}

// received == 0 from here on.
if was_only_local {
    *paging = PagingState::Loading { attempt: 1, only_local: false };
} else if attempt < MAX_EMPTY_ATTEMPTS {
    *paging = PagingState::Loading { attempt: attempt + 1, only_local: false };
} else {
    *paging = PagingState::Exhausted;
    return PagingDirective::None;
}
```

Separating the local budget from the remote budget matters more than it looks. If local empties consumed retries, a cold chat could burn the entire ladder before TDLib ever asked the server, latch `Exhausted`, and lose scroll-up for the rest of the session.

## Opening a chat is local-first, then reconciled exactly once

Opening a chat issues `getChatHistory` with `only_local: true` so the cached messages render instantly. Then, if that came back with anything, exactly one remote request follows to reconcile against the server.

"Exactly one" is the hard part, and the loop guard is specific about how it's achieved:

> keyed strictly off this call's `only_local` parameter, never off `convo.paging`, which a scroll-up page racing the reconcile could otherwise put back into `Loading`, so the reconcile's own completion (always `only_local: false`) can never spawn another one.

Reading the guard off mutable state instead of off the completed request's own parameter is the bug that would have been easy to write and hard to reproduce: it needs a scroll-up to land in the same window as the reconcile.

## Two edges the machine can't handle alone

**Nothing to anchor a retry on.** The state machine is generic and doesn't know about TDLib's "id 0 means newest message" sentinel. On the very first request for a chat there's nothing loaded, so when the machine says "retry" there's no message id to retry *from*. The caller supplies the sentinel in that one case.

**Short-but-non-empty on a never-opened chat.** This is the milder sibling of the empty-response trap, and it's subtle enough to be worth spelling out. For a chat this client has never opened, TDLib's local database holds exactly one message: the preview delivered by `updateChatLastMessage` for the chat list. The opening local request therefore returns one message, which is a legitimate non-empty response, so the machine goes to `Idle` and you're left looking at a single line in a chat with years of history.

The fix is a separate viewport-fill loop, and it deliberately does *not* drive the paging machine. The reasoning is written down:

> Driving the machine would hand an empty answer to `history::on_history_loaded`, which correctly re-asks up to `MAX_EMPTY_ATTEMPTS` times and then latches `Exhausted`. Spending that ladder at *open* time, on a chat whose server sync merely hasn't caught up yet, would leave `Exhausted` latched for the rest of the session and kill scroll-up for that chat entirely: a worse bug than the one being fixed.

The fill loop asks for more until it has 50 messages, stops when a page adds nothing new, and stops if paging is no longer idle. "Adds nothing new" is counted *after* deduplication, because a page of pure overlap would otherwise look like progress and the loop would never terminate.

It can't ping-pong with the reconcile either: the reconcile always asks from the sentinel and is only ever spawned by a local completion, while the fill always asks from the oldest loaded message and is always remote. Neither can trigger the other.

## Paging while you scroll

A page is requested when the anchor's index falls below 20 messages from the top of the loaded window. Twenty *messages*, not rows, because rows are a rendering concept and the state machine has no business knowing about wrapping.

There's a second trigger that matters: the anchor naming a message older than the entire window. That happens when in-chat search jumps you to a hit far back in history. Without it, the anchor move that most needs history fetched would be the one that never asks for it.

## Errors and cooldown

A failed request enters `Cooldown` from any state, using TDLib's own `retry_after` when a flood-wait carries one and 3 seconds otherwise. The cooldown expires on the next scroll rather than on a timer, which fits the pull model: there's no point retrying history nobody is looking at.

## Anchors are message ids, not indices

`Scroll::At { message_id, line_offset }` names a message. Prepending a page therefore needs no index fixups anywhere, and the viewport stays visually still while 50 messages appear above it. An index-based anchor would need every scroll position adjusted on every prepend, and the first place someone forgot would be a jump you'd only see on slow connections.

## Proven end to end

`crates/app/tests/read_only.rs` drives the whole trap against the fake TDLib runtime and asserts on exactly four `getChatHistory` requests: the opening local one, the reconcile, the empty scroll round, and the retry. Both scroll-triggered requests ask from the same message id, because the empty response moved nothing and the retry has to ask for the same page rather than skipping it. A separate test covers the local-empty fallback.

The general shape here is the same as [chat ordering](chat-order.md): don't infer from a local view what only the server knows.
