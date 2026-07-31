---
title: Why chat order mirrors TDLib
createTime: 2026/07/31 10:00:00
---

The chat list is never sorted by this client. Not by last-message time, not by unread count, not by anything. TDLib hands out an `order: i64` per (chat, list) pair and the client mirrors it into a sorted set. That's the entire ordering logic.

## Why not sort locally

The obvious implementation is "sort by the timestamp of the last message", and it's wrong in a way that's hard to notice and impossible to fix incrementally.

Telegram's ordering folds in pin state, last-message date, draft presence, scheduled messages, and server-side tie-breaking rules that aren't published and do change. A client that reconstructs the order from the fields it happens to have will agree with the phone app most of the time and disagree occasionally. And a chat list that disagrees with your phone reads as a bug, not as a preference. Nobody thinks "interesting, this client uses a different sort"; they think "where did that conversation go".

So the ordering is server-authoritative, and the client's job is to mirror it faithfully rather than to be clever.

## How the mirror works

There's one `BTreeSet<ChatOrderKey>` per chat list (Main, Archive, and one per folder). The key carries the order and the chat id:

```rust
/// Sort key mirroring TDLib: (order DESC, chat_id DESC).
pub struct ChatOrderKey { pub order: i64, pub chat_id: ChatId }

impl Ord for ChatOrderKey {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        other.order.cmp(&self.order).then(other.chat_id.cmp(&self.chat_id))
    }
}
```

Look at the operand order: `other` compares against `self`, so descending is baked into `Ord` itself. Walking the set *is* the display order. There is no `.sort()` call anywhere in the chat-list code, which means there's no sort to get wrong, no re-sort to forget after an update, and no window during which the set is stale-but-not-yet-sorted.

That's the structural move worth copying. Converting "keep this sorted" from a rule someone has to remember into a property of the data structure removes an entire category of bug rather than defending against it.

A chat can hold a different order in different lists at the same time, exactly as TDLib models it, which is why the sets are per-list rather than global.

## Order zero means gone

TDLib signals "this chat is not in this list" by sending `order: 0` rather than by sending a removal. The client treats it as a removal:

```rust
pub struct ChatPositionEntry {
    pub list: ChatListId,
    /// TDLib's order. 0 means "remove from this list". NEVER computed locally.
    pub order: i64,
    pub is_pinned: bool,
}
```

Treating zero as a very low sort value instead would be the natural mistake, and it would leave chats you'd moved out of a folder sitting at the bottom of it forever.

One function applies positions, and it's the only writer of the order sets. It removes the old key, inserts the new one unless the order is zero, and mirrors the same into the chat's own position list. Even the `NewChat` update goes through it: the chat's positions are emptied and each one replayed through the same path. One writer means there's exactly one place an ordering bug could live.

## Pinned chats: the one exception, and how it's contained

Pinned chats sort above unpinned ones, and that *is* client-side reordering. It's implemented as a stable partition over the already-ordered sequence, which is a meaningfully different thing from a second sort:

> Walks the active list's order set (already sorted by TDLib order; never reordered here), keeps rows whose title matches the filter case-insensitively, then stable-partitions the result so every pinned chat precedes every unpinned one. This is a partition, not a fresh sort: it never invents an ordering within a group that TDLib didn't already give us, it only pulls the pinned subsequence forward.

Within the pinned group and within the unpinned group, the relative order is still TDLib's. The comment says "partition, not a fresh sort" because that distinction is the thing keeping the exception from growing into a competing ordering.

Pinning is checked per *active list*, so a chat pinned in Main but not in Archive sorts differently in each. That falls out of the per-list model rather than being a special case.

## The view doesn't sort either

The renderer calls `visible_rows(list)` and draws what it gets. Its only ordering-adjacent job is finding the boundary between the pinned and unpinned groups so it can draw a separator, and it uses the same predicate to do it. There is no second opinion about order anywhere in the rendering layer.

## Selection follows the mirror

When the chat you had selected leaves the visible set (it got archived, or it dropped out of a filter), the cursor doesn't snap to the top of the list. It walks the unfiltered order vector outward from where the chat used to be, forward first and then backward, and takes the first surviving row. So a chat vanishing under your cursor moves you to its neighbour rather than to the top, which is what you'd want when the list is reordering under you constantly (and in a busy Telegram account, it is).

## Loading

One `loadChats` request for the main list on reaching the ready phase, limit 200. Its response carries no chats: it only reports that TDLib accepted the request. The chats themselves arrive as `NewChat` and `ChatPosition` pushes afterwards. A failed `loadChats` leaves the phase alone and the list keeps whatever it already holds, because there's nothing useful to do about it.

## The general principle

This is the same instinct as [history paging](history-paging.md), applied to a different problem: refuse to infer from a local view what only the server knows. There, a local database that says "no more messages" isn't proof there are no more messages. Here, a set of last-message timestamps isn't proof of an ordering. Both cases resolve the same way, by asking the server and mirroring the answer.
