---
title: Search & the command palette
createTime: 2026/07/31 10:00:00
---

Two different tools that both start with typing. The palette finds chats and runs commands; search finds messages inside the chat you're already in.

## The command palette

<kbd>ctrl</kbd>+<kbd>p</kbd> opens it, and <kbd>ctrl</kbd>+<kbd>p</kbd> again closes it. So does <kbd>Esc</kbd>.

Type to filter. Matching is fuzzy (nucleo, the same matcher Helix uses), and results are ranked by score, with chats ahead of commands on a tie, chats broken by TDLib's recency order, and commands in declaration order. An empty query lists every chat by recency followed by every command.

<kbd>↑</kbd>/<kbd>↓</kbd> pick, <kbd>Enter</kbd> runs. Picking a chat opens it and moves focus to the composer, same as selecting it in the sidebar.

The commands:

| Command | What it does |
| --- | --- |
| Toggle theme | Advances to the next built-in theme and saves the choice |
| Log out | Issues a real `logOut` and drops the session |
| Quit | Exits |
| Telemetry settings | Not implemented. Closes the palette and does nothing. |
| Send file | Not implemented. Use `/send <path>` in the composer instead. |

The last two are listed because they're in the palette and you'll see them; they're documented gaps, not features.

There's no caret movement inside the palette query. Typing and <kbd>Backspace</kbd> are the whole editing model there, same as the chat filter.

## Searching within a chat

Search is reachable from selection mode only. From the composer, press <kbd>↑</kbd> to get into selection mode, then <kbd>/</kbd>.

<kbd>/</kbd> isn't a search key in the composer because `/send` needs it as a literal character, and having the same key mean "slash" in one pane and "search" in another with no visible difference would be worse than requiring one extra keystroke.

Type the query and press <kbd>Enter</kbd> to run it. Hits come back in TDLib's order, which for a search from the newest message means newest-first.

Then <kbd>n</kbd> steps to the next hit and <kbd>N</kbd> (shift-n) to the previous, both wrapping around the ends. Because hits are newest-first, <kbd>n</kbd> walks you *backwards* in time.

::: warning n and N change meaning once you have hits
Before a search returns anything, <kbd>n</kbd> and <kbd>N</kbd> type into the query like any other letter. After hits exist, they become navigation and can no longer be typed. It's the only place in the application where a key's role depends on data rather than context, and if you need a literal `n` in a query, type it before running the search.
:::

<kbd>Esc</kbd> closes search and clears the highlights.

Jumping to a hit only moves the viewport anchor; if the hit is older than everything currently loaded, the near-top paging logic pulls the history in. That case is specifically handled: the anchor check treats "the anchor names a message older than the whole window" as a paging trigger, because the one anchor move that most needs history fetched shouldn't be the one that never asks for it.

## What search doesn't do

There's no global search across all chats; the palette's chat matching is over chat titles, not message content. Search results aren't a separate list you can page through, either. It's a highlight-and-step model over the conversation you're looking at.
