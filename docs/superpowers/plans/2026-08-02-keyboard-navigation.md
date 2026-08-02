# Keyboard Navigation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add `Ctrl+←`/`Ctrl+→` pane movement, stop selection movement from dragging the viewport, and add a keyboard path to the message a reply quotes.

**Architecture:** Three independent changes on the existing Elm loop. Pane movement is a new pair of `Key` variants routed in `app.rs`, which owns every focus transition. Selection scrolling learns what is on screen by reading the `HitMap` the view already returns, fed back as an `Action` — the architecture §7.5 pattern of resolving at the boundary and handing core semantic data. Jump-to-quote adds a chip and a bounded backward history hunt driven by the existing `state/history.rs` paging machine.

**Tech Stack:** Rust 1.97.1, ratatui, crossterm, tokio mpsc, insta snapshots. Toolchain from `mise`.

**Spec:** `docs/superpowers/specs/2026-08-02-keyboard-navigation-design.md`

## Global Constraints

- `tgt-core` must not depend on `ratatui` or `crossterm`; `tgt-ui` must not depend on `tdlib-rs`. `./scripts/check-crate-boundaries.sh` enforces this.
- `update()` is pure: no I/O, no spawning, no clock, no RNG. Time enters only as `Action::Tick { now }`.
- **`docs/architecture.md` is edited before the code** for any shared-type change. This plan reshapes `Key`, `Chip`, `AppState`, `ConversationState`, `Action` and `HitMap` — all shared.
- State handlers never touch `app.focus`. They return `None` for `Esc` and every focus transition lives in `app.rs`.
- `mise run check` (fmt-check, `clippy --workspace --all-targets -D warnings`, `cargo test --workspace`, crate boundaries) is the merge gate.
- **Every new test is watched failing before it is trusted.** Break the code deliberately, confirm the specific test goes red with the expected message, restore. Where a task adds a branch and its fallback, break each half separately — a carve-out where one half is dead while the other carries the suite is a failure mode this repo has already paid for.
- Snapshots are reviewed, never blanket-accepted: `cargo insta test -p tgt-ui --check`, read the diff, then `cargo insta accept`.
- Dependency pins stay exact (`=`). This plan adds no dependencies.

## File Structure

**Modified:**

- `crates/core/src/model/key.rs` — `Key::CtrlLeft`, `Key::CtrlRight`.
- `crates/ui/src/input/mod.rs` — crossterm → `Key` mapping for modified arrows.
- `crates/core/src/app.rs` — `AppState.visible_messages`; `Ctrl`-arrow routing; `Action::ViewportChanged` arm.
- `crates/core/src/action.rs` — `Action::ViewportChanged`.
- `crates/ui/src/render/hit.rs` — `HitMap::visible_messages`.
- `crates/app/src/runtime_loop.rs` — send `ViewportChanged` after each draw.
- `crates/core/src/model/chips.rs` — `Chip::JumpToQuoted`.
- `crates/core/src/state/selection.rs` — `AnchorPolicy`, chip offering, chip invocation.
- `crates/core/src/state/conversation.rs` — `step_anchor`, `JumpHunt`, hunt driving in `apply_history_page`.
- `crates/ui/src/view/help.rs` — new key rows.

**Two struct additions break every fixture literal in the workspace.** `AppState` is built by struct literal in 22 places and `ConversationState` in 15 (test fixtures in both crates). The compiler enumerates all of them; each needs one field line. The affected files are listed in the tasks that add the fields.

---

### Task 1: `Ctrl+←` / `Ctrl+→` reach `update()`

Today `crates/ui/src/input/mod.rs:44` maps `KeyCode::Left => Some(Key::Left)` without consulting `ev.modifiers`, so `Ctrl+←` is indistinguishable from `←` by the time core sees it.

**Files:**
- Modify: `crates/core/src/model/key.rs:9-27`
- Modify: `crates/ui/src/input/mod.rs:37-56`
- Modify: `docs/architecture.md` (the `Key` enum definition in §4.1)

**Interfaces:**
- Produces: `Key::CtrlLeft` and `Key::CtrlRight` variants on `tgt_core::model::key::Key`.

`Ctrl+↑`/`Ctrl+↓` are deliberately **not** added: there is no vertical pane movement to bind them to, and unroutable variants are dead surface.

- [ ] **Step 1: Edit `docs/architecture.md` first**

Find the `Key` enum in §4.1 and add the two variants to the listing, with a one-line note that they carry pane movement and that terminals which do not emit modified arrow sequences (Apple Terminal.app) deliver them as plain `Left`/`Right`.

- [ ] **Step 2: Write the failing test**

In `crates/ui/src/input/mod.rs`, inside `mod tests`:

```rust
#[test]
fn ctrl_arrows_map_to_their_own_variants() {
    assert!(matches!(
        map_event(press(KeyCode::Left, KeyModifiers::CONTROL)),
        Some(Action::Key(Key::CtrlLeft))
    ));
    assert!(matches!(
        map_event(press(KeyCode::Right, KeyModifiers::CONTROL)),
        Some(Action::Key(Key::CtrlRight))
    ));
    // Unmodified arrows are untouched, and a modifier that is not Ctrl
    // still yields the plain variant.
    assert!(matches!(
        map_event(press(KeyCode::Left, KeyModifiers::NONE)),
        Some(Action::Key(Key::Left))
    ));
    assert!(matches!(
        map_event(press(KeyCode::Left, KeyModifiers::ALT)),
        Some(Action::Key(Key::Left))
    ));
}
```

- [ ] **Step 3: Run it and confirm it fails**

Run: `cargo test -p tgt-ui input::tests::ctrl_arrows_map_to_their_own_variants`
Expected: compile error, `no variant named CtrlLeft found for enum Key`.

- [ ] **Step 4: Add the variants**

In `crates/core/src/model/key.rs`, in `enum Key`, after `Right`:

```rust
    /// Pane movement (architecture §6.2). Terminals that do not emit
    /// modified arrow sequences (notably Apple Terminal.app) deliver these
    /// as plain `Left`/`Right`; `Tab` and `Esc` remain the universally
    /// available way out of the conversation side.
    CtrlLeft,
    CtrlRight,
```

- [ ] **Step 5: Map them**

In `crates/ui/src/input/mod.rs`, place these two arms **above** the existing `KeyCode::Left`/`KeyCode::Right` arms, mirroring the existing `KeyCode::Enter if alt` guard:

```rust
        KeyCode::Left if ctrl => Some(Key::CtrlLeft),
        KeyCode::Right if ctrl => Some(Key::CtrlRight),
```

- [ ] **Step 6: Run it and confirm it passes**

Run: `cargo test -p tgt-ui input::tests::ctrl_arrows_map_to_their_own_variants`
Expected: PASS.

- [ ] **Step 7: Watch it fail on purpose**

Temporarily change `KeyCode::Left if ctrl` to `KeyCode::Left if alt`. Re-run: the first assertion must fail. Restore.

- [ ] **Step 8: Full check and commit**

```bash
mise run check
git add crates/core/src/model/key.rs crates/ui/src/input/mod.rs docs/architecture.md
git commit -m "feat(input): distinguish ctrl+arrow keys from plain arrows"
```

---

### Task 2: Route `Ctrl+←` / `Ctrl+→` to pane movement

**Files:**
- Modify: `crates/core/src/app.rs:1035-1052` (`move_pane_focus`)
- Modify: `crates/ui/src/view/help.rs:90-98` (Navigation group) and `:150-172` (Selection mode group)
- Modify: `docs/architecture.md` §6.2 routing table
- Test: `crates/core/src/app.rs` `mod tests`

**Interfaces:**
- Consumes: `Key::CtrlLeft`, `Key::CtrlRight` from Task 1.
- Produces: nothing later tasks depend on.

Target behavior:

| Focus | `Ctrl+←` | `Ctrl+→` |
| --- | --- | --- |
| `ChatList` | unclaimed | open selected chat, focus `Composer` |
| `Composer` | → `ChatList` | unclaimed |
| `Selection` | pop, → `ChatList` | unclaimed |
| everything else | unclaimed | unclaimed |

- [ ] **Step 1: Write the failing tests**

Add to `crates/core/src/app.rs`'s `mod tests`. `chat_open()` is the module's existing fixture: a chat open with one page of history, focus resting on the composer, `CHAT == ChatId(1)`.

```rust
#[test]
fn ctrl_left_moves_from_composer_to_the_chat_list() {
    let mut app = chat_open();
    assert_eq!(*app.state().focus.current(), Focus::Composer);

    app.update(Action::Key(Key::CtrlLeft));

    assert_eq!(*app.state().focus.current(), Focus::ChatList);
    assert_eq!(app.state().focus.depth(), 1);
}

#[test]
fn ctrl_left_pops_selection_mode_on_its_way_to_the_chat_list() {
    let mut app = chat_open();
    // `↑` on the empty composer is how selection mode is entered (T25).
    app.update(Action::Key(Key::Up));
    assert_eq!(*app.state().focus.current(), Focus::Selection);

    app.update(Action::Key(Key::CtrlLeft));

    // One keystroke, not two: the pushed level is unwound AND the base
    // swapped, and the selection itself is dropped rather than left
    // dangling under a pane it does not belong to.
    assert_eq!(*app.state().focus.current(), Focus::ChatList);
    assert_eq!(app.state().focus.depth(), 1);
    assert!(app.state().conversations[&CHAT].selection.is_none());
}

#[test]
fn ctrl_left_leaves_overlays_alone() {
    for overlay in [Focus::Palette, Focus::Help, Focus::ChatFilter] {
        let mut app = chat_open();
        app.state.focus.push(overlay.clone());

        app.update(Action::Key(Key::CtrlLeft));

        assert_eq!(
            *app.state().focus.current(),
            overlay,
            "ctrl+left must not swap the pane under an overlay"
        );
    }
}

#[test]
fn ctrl_right_opens_the_selected_chat_from_the_chat_list() {
    // `logged_in()` plus one chat, deliberately NOT opened — `chat_open()`
    // has already taken the open path this test is about.
    let mut app = logged_in();
    app.update(Action::Td(chat(1, "Ada", 10)));
    app.update(Action::Key(Key::Down));
    assert!(app.state().open_chat.is_none());
    assert_eq!(*app.state().focus.current(), Focus::ChatList);

    app.update(Action::Key(Key::CtrlRight));

    assert_eq!(app.state().open_chat, Some(CHAT));
    assert_eq!(*app.state().focus.current(), Focus::Composer);
}
```

`chat_open`, `logged_in` and `chat(id, title, order)` all already exist in that test module. `app.state` is reachable directly from these tests (same crate); `app.state()` is the accessor used elsewhere.

- [ ] **Step 2: Run them and confirm they fail**

Run: `cargo test -p tgt-core app::tests::ctrl_`
Expected: all four FAIL — the keys currently fall through unclaimed, so focus does not change.

- [ ] **Step 3: Extend `move_pane_focus`**

`move_pane_focus` currently returns early on `depth() != 1`, which is what stops an overlay from getting the pane swapped underneath it. `Focus::Selection` gets a narrow exception; the gate is **not** lifted for the filter, search, palette or modals.

Replace the depth guard and match in `crates/core/src/app.rs`:

```rust
    fn move_pane_focus(&mut self, key: Key) -> bool {
        // `Ctrl+←` out of selection mode is the one movement allowed above
        // depth 1: selection sits on the conversation side and "go left"
        // means the same thing there as it does in the composer. Every
        // other overlay (filter, search, palette, modal) keeps the depth
        // gate — swapping the pane under one would leave it on a pane it
        // does not belong to.
        let popping_selection =
            key == Key::CtrlLeft && *self.state.focus.current() == Focus::Selection;
        if self.state.focus.depth() != 1 && !popping_selection {
            return false;
        }
        if popping_selection {
            selection::exit(&mut self.state);
            self.state.focus.pop();
        }

        let target = match (self.state.focus.current(), key) {
            (Focus::ChatList, Key::Right | Key::Tab | Key::BackTab) => Focus::Composer,
            (Focus::Composer, Key::Tab | Key::BackTab | Key::CtrlLeft) => Focus::ChatList,
            _ => return popping_selection,
        };
        if target == Focus::Composer && self.state.open_chat.is_none() {
            return false;
        }
        self.state.focus.replace_base(target);
        true
    }
```

Note the `_ => return popping_selection` arm: after unwinding selection the current focus is `Composer`, which the `(Focus::Composer, Key::CtrlLeft)` arm then catches, so this fallthrough only fires if selection sat over something else — and the pop still counts as handled.

- [ ] **Step 4: Make the swap close a hidden conversation**

Find the caller of `move_pane_focus` in `dispatch_key`. It must run `conversation::close_if_now_hidden` around the transition exactly as the `Esc` path near `app.rs:1010` does — in single-pane layout, swapping the base to `ChatList` is what stops rendering the conversation, and skipping the close leaves the chat open behind a pane that no longer shows it.

```rust
        let was_visible = conversation_pane_visible(&self.state);
        if self.move_pane_focus(key) {
            let mut effects = conversation::close_if_now_hidden(&self.state, was_visible);
            effects.extend(/* whatever the existing call site already collected */);
            self.dirty = true;
            return Some(effects);
        }
```

Match the surrounding call site's exact shape — read it before editing rather than pasting this over it.

- [ ] **Step 5: Add the `Ctrl+→` open path**

`Ctrl+→` from the chat list is the same open path `⏎` and a left-click take. Reuse `click_chat_row`'s proven bracket rather than writing a second one. In `dispatch_key`, before the pane routing:

```rust
        if key == Key::CtrlRight
            && self.state.focus.depth() == 1
            && *self.state.focus.current() == Focus::ChatList
            && let Some(chat_id) = self.state.chat_list.selected
        {
            self.dirty = true;
            return Some(self.click_chat_row(chat_id));
        }
```

- [ ] **Step 6: Run the tests and confirm they pass**

Run: `cargo test -p tgt-core app::tests::ctrl_`
Expected: all four PASS.

- [ ] **Step 7: Watch each half fail**

Three separate breakages, restoring between each:
1. Remove `Key::CtrlLeft` from the `(Focus::Composer, …)` arm → `ctrl_left_moves_from_composer_to_the_chat_list` red.
2. Remove the `popping_selection` exception → `ctrl_left_pops_selection_mode_on_its_way_to_the_chat_list` red, `ctrl_left_leaves_overlays_alone` still green.
3. Remove the `Ctrl+→` block → `ctrl_right_opens_the_selected_chat_from_the_chat_list` red.

If `ctrl_left_leaves_overlays_alone` stays green through *all three*, it is not testing the exception — check it is actually reaching `move_pane_focus` rather than being stopped by an earlier layer of `dispatch_key`.

- [ ] **Step 8: Update the help overlay**

In `crates/ui/src/view/help.rs`, add to the Navigation group (after the `tab / shift+tab` row):

```rust
            Row {
                key: "ctrl+← / ctrl+→",
                desc: "move pane focus (ctrl+→ opens the selected chat)",
            },
```

and to the Selection mode group, before the `esc` row:

```rust
            Row {
                key: "ctrl+←",
                desc: "back to the chat list",
            },
```

- [ ] **Step 9: Review snapshots**

Run: `cargo insta test -p tgt-ui --check`
Read every diff. Only the help overlay should have changed, by exactly the added rows. Then `cargo insta accept`.

- [ ] **Step 10: Update `docs/architecture.md` §6.2**

Add the two keys to the routing table with the `Focus::Selection` exception noted.

- [ ] **Step 11: Full check and commit**

```bash
mise run check
git add crates/core/src/app.rs crates/ui/src/view/help.rs crates/ui/src/render/snapshots docs/architecture.md
git commit -m "feat(nav): move pane focus with ctrl+arrow keys"
```

---

### Task 3: `HitMap` reports which messages are on screen

**Files:**
- Modify: `crates/ui/src/render/hit.rs`
- Test: `crates/ui/src/render/hit.rs` `mod tests`

**Interfaces:**
- Produces: `HitMap::visible_messages(&self) -> Option<(MessageId, MessageId)>` — `(oldest, newest)` on screen, `None` when no message was drawn.

`view/conversation.rs:223` already pushes a `HitTarget::Message(id)` rect per drawn block, so the data exists; this only reads it. `HitTarget::Spoiler` and `HitTarget::ReplyQuote` also carry ids, but they are sub-row regions inside blocks that already contribute a `Message` entry — the accessor scans `Message` entries **only**, rather than relying on that overlap holding.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn visible_messages_reports_the_drawn_id_range() {
    let mut hits = HitMap::new();
    assert_eq!(hits.visible_messages(), None);

    hits.push(Rect::new(0, 0, 10, 2), HitTarget::Message(MessageId(7)));
    hits.push(Rect::new(0, 2, 10, 2), HitTarget::Message(MessageId(9)));
    hits.push(Rect::new(0, 4, 10, 1), HitTarget::Message(MessageId(3)));
    // Non-message targets carrying ids must not widen the range: a
    // ReplyQuote names a message that may not be on screen at all.
    hits.push(
        Rect::new(0, 4, 10, 1),
        HitTarget::ReplyQuote {
            containing: MessageId(3),
            quoted: MessageId(1),
        },
    );
    hits.push(Rect::new(0, 6, 4, 1), HitTarget::Spoiler(MessageId(99)));
    hits.push(Rect::new(0, 8, 10, 1), HitTarget::ChatRow(ChatId(1)));

    assert_eq!(
        hits.visible_messages(),
        Some((MessageId(3), MessageId(9)))
    );
}
```

- [ ] **Step 2: Run it and confirm it fails**

Run: `cargo test -p tgt-ui render::hit::tests::visible_messages_reports_the_drawn_id_range`
Expected: compile error, `no method named visible_messages`.

- [ ] **Step 3: Implement it**

In `impl HitMap`:

```rust
    /// The oldest and newest message id drawn in this frame, or `None` if no
    /// message block was drawn at all (an overlay, a chat with no history,
    /// or before the first frame).
    ///
    /// This is what lets `update()` know where the viewport is without ever
    /// seeing a `Rect` (architecture §7.5): the coordinates are resolved
    /// here, and core receives two message ids.
    ///
    /// Only `HitTarget::Message` entries count. `Spoiler` and `ReplyQuote`
    /// also carry ids — the first for a sub-run of a block that is already
    /// counted, the second for a message that may not be loaded at all —
    /// and either would report a range the user is not looking at.
    pub fn visible_messages(&self) -> Option<(MessageId, MessageId)> {
        let mut range: Option<(MessageId, MessageId)> = None;
        for (_, target) in &self.entries {
            let HitTarget::Message(id) = target else {
                continue;
            };
            range = Some(match range {
                None => (*id, *id),
                Some((lo, hi)) => (lo.min(*id), hi.max(*id)),
            });
        }
        range
    }
```

`MessageId` must be `Ord` for `min`/`max` — it already is (`conversation::index_of` binary-searches on it). Add `use tgt_core::model::ids::MessageId;` to the file's imports if the non-test scope lacks it.

- [ ] **Step 4: Run it and confirm it passes**

Run: `cargo test -p tgt-ui render::hit::tests::visible_messages_reports_the_drawn_id_range`
Expected: PASS.

- [ ] **Step 5: Watch it fail on purpose**

Change the `let HitTarget::Message(id) = target else { continue }` filter to also accept `HitTarget::Spoiler`. The test must go red on the `MessageId(99)` upper bound. Restore.

- [ ] **Step 6: Full check and commit**

```bash
mise run check
git add crates/ui/src/render/hit.rs
git commit -m "feat(ui): report the on-screen message id range from the hit map"
```

---

### Task 4: Core learns the viewport

Plumbing only — no behavior changes in this task. Task 5 consumes it.

**Files:**
- Modify: `crates/core/src/action.rs` (`Action` enum)
- Modify: `crates/core/src/app.rs` (`AppState` field, `update` arm, chat-change clearing)
- Modify: `docs/architecture.md` (`Action` and `AppState` definitions)
- Test: `crates/core/src/app.rs` `mod tests`

**Interfaces:**
- Produces: `Action::ViewportChanged { first: MessageId, last: MessageId }` and `AppState.visible_messages: Option<(MessageId, MessageId)>`.

- [ ] **Step 1: Edit `docs/architecture.md` first**

Add the `Action` variant and the `AppState` field to their definitions, with the note that the field is **not** render-affecting and therefore must never set `dirty`.

- [ ] **Step 2: Write the failing test**

```rust
#[test]
fn viewport_changed_records_the_range_without_requesting_a_redraw() {
    let mut app = chat_open();
    let _ = app.take_dirty();

    let effects = app.update(Action::ViewportChanged {
        first: MessageId(4),
        last: MessageId(9),
    });

    assert!(effects.is_empty());
    assert_eq!(
        app.state().visible_messages,
        Some((MessageId(4), MessageId(9)))
    );
    // Critical: the range is produced BY rendering. Marking it dirty would
    // make every frame schedule another frame.
    assert!(!app.take_dirty());
}

#[test]
fn opening_a_different_chat_clears_the_recorded_viewport() {
    let mut app = chat_open();
    app.update(Action::ViewportChanged {
        first: MessageId(4),
        last: MessageId(9),
    });

    // A second chat, selected and opened from the list.
    app.update(Action::Td(chat(2, "Bob", 20)));
    app.state.focus.replace_base(Focus::ChatList);
    app.state.chat_list.selected = Some(ChatId(2));
    app.update(Action::Key(Key::Enter));
    assert_eq!(app.state().open_chat, Some(ChatId(2)));

    // The old chat's ids say nothing about the new chat's viewport, and a
    // stale range would suppress the first scroll in it.
    assert_eq!(app.state().visible_messages, None);
}
```

- [ ] **Step 3: Run them and confirm they fail**

Run: `cargo test -p tgt-core app::tests::viewport`
Expected: compile error, no `ViewportChanged` variant.

- [ ] **Step 4: Add the action**

In `crates/core/src/action.rs`, in `enum Action`:

```rust
    /// Which messages the last drawn frame actually put on screen
    /// (architecture §7.5). Like `Click`, the coordinates are resolved at
    /// the `tgt-ui` boundary — `update()` receives two message ids, never a
    /// `Rect`. Sent by `runtime_loop` after each draw, and only when the
    /// range changed.
    ///
    /// Deliberately does NOT set `dirty`: this action is produced *by*
    /// rendering, so marking it render-worthy would make every frame
    /// schedule another one.
    ViewportChanged {
        first: MessageId,
        last: MessageId,
    },
```

Add `use crate::model::ids::MessageId;` if absent.

- [ ] **Step 5: Add the field**

In `crates/core/src/app.rs`, in `struct AppState`:

```rust
    /// The oldest and newest message the last drawn frame put on screen, or
    /// `None` before the first frame and whenever no message was drawn.
    ///
    /// Read only by `state::selection`'s anchor policy. `None` means "no
    /// information", and every consumer must fall back to its pre-existing
    /// behavior — every unit and integration test in this workspace drives
    /// `update()` with no renderer attached, so `None` is the value they all
    /// see, and treating it as "everything is visible" would leave the suite
    /// green about a path no user reaches.
    pub visible_messages: Option<(MessageId, MessageId)>,
```

Initialize to `None` in `App::new`.

- [ ] **Step 6: Fix every fixture**

Run `cargo build --workspace --all-targets`. It lists every `AppState { … }` literal missing the field — 22 of them, across:

`crates/ui/tests/fixtures/states.rs`, `crates/ui/src/lib.rs`, `crates/ui/src/view/{composer,conversation,chips,chat_list,modal,toast,palette,header,root,help}.rs`, `crates/core/src/state/{presence,palette,composer,media,conversation,toasts,modal,selection,chat_list,search}.rs`.

Add `visible_messages: None,` to each.

- [ ] **Step 7: Handle the action**

In `App::update`'s match:

```rust
            Action::ViewportChanged { first, last } => {
                // No `self.dirty = true` — see the variant's docs.
                self.state.visible_messages = Some((first, last));
                Vec::new()
            }
```

- [ ] **Step 8: Clear it when the open chat changes**

Find where `open_chat` is assigned (`conversation::open`'s caller in `app.rs`, and `click_chat_row`). Set `self.state.visible_messages = None` wherever `open_chat` changes to a different chat. Prefer one place: if `route_chat_list_key` already compares `open_before != self.state.open_chat`, put it there.

- [ ] **Step 9: Run the tests and confirm they pass**

Run: `cargo test -p tgt-core app::tests::viewport`
Expected: both PASS.

- [ ] **Step 10: Watch each half fail**

1. Add `self.dirty = true;` to the new arm → `viewport_changed_records_the_range_without_requesting_a_redraw` red on the last assertion.
2. Remove the clearing from step 8 → `opening_a_different_chat_clears_the_recorded_viewport` red.

Restore after each.

- [ ] **Step 11: Full check and commit**

```bash
mise run check
git add -A
git commit -m "feat(core): record which messages the last frame drew"
```

---

### Task 5: Selection movement stops dragging the viewport

**Files:**
- Modify: `crates/core/src/state/conversation.rs` (add `step_anchor`)
- Modify: `crates/core/src/state/selection.rs:238-275` (`select`) and its callers
- Test: `crates/core/src/state/selection.rs` `mod tests`

**Interfaces:**
- Consumes: `AppState.visible_messages` from Task 4.
- Produces: `conversation::step_anchor(convo, chat_id, delta, now) -> Vec<Effect>` (`pub(crate)`); `selection::AnchorPolicy { KeepVisible, Jump }`, and `select` gains it as a fourth parameter.

- [ ] **Step 1: Write the failing tests**

In `crates/core/src/state/selection.rs`'s `mod tests`. `with_messages` parks paging in `Exhausted`, so effect lists carry no history requests.

```rust
#[test]
fn stepping_the_selection_within_the_viewport_does_not_scroll() {
    let mut app = with_messages((1..=10).map(msg).collect());
    // Frame showed messages 5..=10; the newest is at the bottom.
    app.visible_messages = Some((MessageId(5), MessageId(10)));
    enter(&mut app);
    assert_eq!(app.conversations[&CHAT].scroll, Scroll::Bottom);

    // Four steps up, all landing on messages already on screen.
    for _ in 0..4 {
        handle_key(&mut app, Key::Up);
    }

    assert_eq!(selection(&app).message_id, MessageId(6));
    assert_eq!(
        app.conversations[&CHAT].scroll,
        Scroll::Bottom,
        "the viewport must not move while the cursor is on screen"
    );
}

#[test]
fn stepping_off_the_top_edge_scrolls_by_exactly_one_message() {
    let mut app = with_messages((1..=10).map(msg).collect());
    app.visible_messages = Some((MessageId(5), MessageId(10)));
    enter(&mut app);
    for _ in 0..5 {
        handle_key(&mut app, Key::Up);
    }
    // Cursor is on 5, the topmost visible message; nothing has scrolled.
    assert_eq!(selection(&app).message_id, MessageId(5));
    assert_eq!(app.conversations[&CHAT].scroll, Scroll::Bottom);

    handle_key(&mut app, Key::Up);

    assert_eq!(selection(&app).message_id, MessageId(4));
    assert_eq!(
        app.conversations[&CHAT].scroll,
        Scroll::At {
            message_id: MessageId(9),
            line_offset: 0,
        },
        "one message of scroll, not a jump to the cursor"
    );
}

#[test]
fn stepping_back_down_off_the_bottom_edge_scrolls_one_message() {
    let mut app = with_messages((1..=10).map(msg).collect());
    app.visible_messages = Some((MessageId(5), MessageId(10)));
    enter(&mut app);
    // Park the anchor and the cursor above the bottom edge.
    app.conversations.get_mut(&CHAT).unwrap().scroll = Scroll::At {
        message_id: MessageId(8),
        line_offset: 0,
    };
    app.visible_messages = Some((MessageId(3), MessageId(8)));
    let convo = app.conversations.get_mut(&CHAT).unwrap();
    convo.selection.as_mut().unwrap().message_id = MessageId(8);

    handle_key(&mut app, Key::Down);

    assert_eq!(selection(&app).message_id, MessageId(9));
    assert_eq!(
        app.conversations[&CHAT].scroll,
        Scroll::At {
            message_id: MessageId(9),
            line_offset: 0,
        }
    );
}

#[test]
fn with_no_viewport_information_the_anchor_follows_the_selection() {
    let mut app = with_messages((1..=10).map(msg).collect());
    assert_eq!(app.visible_messages, None);
    enter(&mut app);

    handle_key(&mut app, Key::Up);

    // The pre-existing behavior, unchanged. Every test in this workspace
    // and every headless caller lands here; it must not become "never
    // scroll", or the suite would be green about a path no user reaches.
    assert_eq!(selection(&app).message_id, MessageId(9));
    assert_eq!(
        app.conversations[&CHAT].scroll,
        Scroll::At {
            message_id: MessageId(9),
            line_offset: 0,
        }
    );
}
```

Import `Scroll` and `MessageId` into the test module if not already there.

- [ ] **Step 2: Run them and confirm they fail**

Run: `cargo test -p tgt-core state::selection::tests::`
Expected: the first three FAIL (the anchor tracks the cursor exactly), the fourth PASSES already (it pins current behavior).

That fourth test passing now is the point — it is the regression guard for the fallback, and it must stay green through the whole task.

- [ ] **Step 3: Add `step_anchor`**

In `crates/core/src/state/conversation.rs`, next to `anchor_to`:

```rust
/// Moves the scroll anchor by one message in `delta`'s direction, clamped to
/// the loaded window, and re-triggers paging like any other anchor move.
///
/// This is the minimum-scroll counterpart to [`anchor_to`]: selection
/// movement uses it when the cursor walks off an edge, so the viewport
/// follows by one message instead of jumping to wherever the cursor went.
pub(crate) fn step_anchor(
    convo: &mut ConversationState,
    chat_id: ChatId,
    delta: isize,
    now: Millis,
) -> Vec<Effect> {
    if convo.messages.is_empty() {
        return Vec::new();
    }
    let last = convo.messages.len() - 1;
    let current = match convo.scroll {
        Scroll::Bottom => last,
        Scroll::At { message_id, .. } => {
            index_of(&convo.messages, message_id).unwrap_or(last)
        }
    };
    let target = (current as isize + delta).clamp(0, last as isize) as usize;
    let id = convo.messages[target].id;
    anchor_to(convo, chat_id, id, now)
}
```

`anchor_to` already converts to `Scroll::Bottom` when the target is the newest loaded message and calls `trigger_paging_if_near_top`, so both come for free.

- [ ] **Step 4: Add the anchor policy**

In `crates/core/src/state/selection.rs`, above `select`:

```rust
/// How [`select`] should treat the scroll anchor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AnchorPolicy {
    /// `↑`/`↓`: hold the viewport still while the target is on screen, and
    /// scroll by exactly one message when the cursor walks off an edge.
    KeepVisible,
    /// A deliberate jump (entering selection, the reply-quote chip): bring
    /// the target into view unconditionally.
    Jump,
}
```

- [ ] **Step 5: Thread it through `select`**

Change the signature to `fn select(app: &mut AppState, chat_id: ChatId, message_id: MessageId, policy: AnchorPolicy) -> Vec<Effect>`, capture the viewport before taking the mutable borrow, and replace the unconditional `anchor_to` line:

```rust
    let visible = app.visible_messages;
    // … existing body up to and including `convo.selection = Some(…)` …

    effects.extend(match (policy, visible) {
        // On screen: leave the anchor completely alone. This is the whole
        // fix — `anchor_to` here is what pinned the cursor to the last row.
        (AnchorPolicy::KeepVisible, Some((first, last)))
            if message_id >= first && message_id <= last =>
        {
            Vec::new()
        }
        (AnchorPolicy::KeepVisible, Some((first, _))) if message_id < first => {
            conversation::step_anchor(convo, chat_id, -1, now)
        }
        (AnchorPolicy::KeepVisible, Some(_)) => {
            conversation::step_anchor(convo, chat_id, 1, now)
        }
        // No frame has reported a viewport (every headless caller, and the
        // first selection after a chat opens), or this is a deliberate
        // jump: the anchor follows the target, exactly as before.
        (AnchorPolicy::KeepVisible, None) | (AnchorPolicy::Jump, _) => {
            conversation::anchor_to(convo, chat_id, message_id, now)
        }
    });
```

- [ ] **Step 6: Update the call sites**

- `move_selection` → `AnchorPolicy::KeepVisible`
- `enter_at` → `AnchorPolicy::Jump`
- every other caller of `select` → `AnchorPolicy::Jump`

The compiler enumerates them.

- [ ] **Step 7: Run the tests and confirm they pass**

Run: `cargo test -p tgt-core state::selection::tests::`
Expected: all four PASS.

Then the whole workspace: `cargo test --workspace`. Nothing else should move — every existing test has `visible_messages: None` and therefore takes the `Jump`-equivalent fallback.

- [ ] **Step 8: Watch each branch fail**

Four separate breakages, restoring between each:
1. Replace the on-screen arm's `Vec::new()` with `conversation::anchor_to(convo, chat_id, message_id, now)` → `stepping_the_selection_within_the_viewport_does_not_scroll` red, fallback test still green.
2. Change `step_anchor(convo, chat_id, -1, now)` to `-2` → `stepping_off_the_top_edge_scrolls_by_exactly_one_message` red.
3. Change the down arm's `1` to `-1` → `stepping_back_down_off_the_bottom_edge_scrolls_one_message` red.
4. Change the `(KeepVisible, None)` arm to `Vec::new()` → `with_no_viewport_information_the_anchor_follows_the_selection` red. **If it stays green, stop:** the fallback is not covered, and the whole suite is passing on a branch users never take.

- [ ] **Step 9: Full check and commit**

```bash
mise run check
git add crates/core/src/state/selection.rs crates/core/src/state/conversation.rs
git commit -m "fix(selection): keep the viewport still while the cursor is on screen"
```

---

### Task 6: Wire the viewport report into the runtime loop

Until this task, `visible_messages` is always `None` in the real app and Task 5's new branches are dead. This is what makes them live.

**Files:**
- Modify: `crates/app/src/runtime_loop.rs` (`Core` field, `viewport_report`, `draw_if_due`)
- Test: `crates/app/src/runtime_loop.rs` `mod tests`

**Interfaces:**
- Consumes: `HitMap::visible_messages` (Task 3), `Action::ViewportChanged` (Task 4).
- Produces: `fn viewport_report(hits: &HitMap, last: Option<(MessageId, MessageId)>) -> Option<Action>`.

The decision is a free function taking a `&HitMap`, deliberately shaped like the `translate_mouse(hits, ev) -> Option<Action>` that already lives beside it. That is what makes it testable: `runtime_loop.rs`'s own `mod tests` already builds a `HitMap` fixture and tests `translate_mouse` against it without constructing a `Core`, and this reuses that fixture. Burying the logic inside `draw_if_due` would put it behind a `DefaultTerminal` no test can build.

- [ ] **Step 1: Add the field**

In `struct Core`, next to `last_hits`:

```rust
    /// The last viewport range handed to `update()`. Kept so the report is
    /// sent only when it changes: a frame that draws the same messages as
    /// the one before it produces no action at all.
    last_viewport: Option<(MessageId, MessageId)>,
```

Initialize to `None` in the constructor alongside `last_hits: HitMap::new()`.

- [ ] **Step 2: Write the failing test**

In `crates/app/src/runtime_loop.rs`'s `mod tests`. The existing `hit_map()` fixture already pushes `HitTarget::Message(MessageId(3))` plus a chat row and both pane areas.

```rust
#[test]
fn a_frame_reports_its_message_range_once_per_change() {
    let hits = hit_map();

    // First frame: nothing reported yet, so the range is news.
    assert!(matches!(
        viewport_report(&hits, None),
        Some(Action::ViewportChanged {
            first: MessageId(3),
            last: MessageId(3),
        })
    ));

    // Same range as last time: no action at all. Without this, every frame
    // would push an action and the loop would never go quiet.
    assert!(viewport_report(&hits, Some((MessageId(3), MessageId(3)))).is_none());

    // A different range is news again.
    assert!(matches!(
        viewport_report(&hits, Some((MessageId(1), MessageId(2)))),
        Some(Action::ViewportChanged { .. })
    ));

    // A frame with no message on it (an overlay, an empty chat) reports
    // nothing rather than reporting an empty range.
    let mut chrome_only = HitMap::new();
    chrome_only.push_area(Rect::new(0, 0, 30, 20), ScrollArea::ChatList);
    assert!(viewport_report(&chrome_only, None).is_none());
}
```

- [ ] **Step 3: Run it and confirm it fails**

Run: `cargo test -p tgt-app --bin tgt runtime_loop::tests::a_frame_reports_its_message_range_once_per_change`
Expected: compile error, `cannot find function viewport_report`.

- [ ] **Step 4: Implement it**

Next to `translate_mouse` in `crates/app/src/runtime_loop.rs`:

```rust
/// What viewport report a freshly drawn frame owes `update()`, given the
/// range last reported. `None` means nothing to send: either the frame drew
/// no messages, or it drew the same ones as the frame before it.
///
/// Shaped like [`translate_mouse`] and for the same reason — the frame's
/// geometry is resolved here so that `update()` receives message ids and
/// never a `Rect` (architecture §7.5).
fn viewport_report(hits: &HitMap, last: Option<(MessageId, MessageId)>) -> Option<Action> {
    let range = hits.visible_messages()?;
    if Some(range) == last {
        return None;
    }
    Some(Action::ViewportChanged {
        first: range.0,
        last: range.1,
    })
}
```

- [ ] **Step 5: Call it after each draw**

At the end of `draw_if_due`, after `gate.mark_drawn(Instant::now());` — the destructuring borrow of `core` has ended by then, so `core.apply` is callable:

```rust
        // The frame now on screen is the only frame a keystroke can mean
        // anything against, so the report goes out here, before the loop
        // goes back to awaiting input. Sharing the one action channel with
        // keys is what orders them: `update()` always holds the range from
        // the frame the user was looking at when they pressed a key.
        //
        // `Action::ViewportChanged` sets no dirty flag, so this cannot
        // drive the loop round again.
        if let Some(action) = viewport_report(&core.last_hits, core.last_viewport) {
            core.last_viewport = core.last_hits.visible_messages();
            core.apply(action);
        }
```

- [ ] **Step 6: Run it and confirm it passes**

Run: `cargo test -p tgt-app --bin tgt runtime_loop::tests::a_frame_reports_its_message_range_once_per_change`
Expected: PASS.

- [ ] **Step 7: Watch each assertion fail**

Two breakages, restoring between each:
1. Delete the `if Some(range) == last { return None; }` guard → the second assertion red.
2. Change `hits.visible_messages()?` to unwrap-or-default over all targets → the `chrome_only` assertion red.

- [ ] **Step 8: Prove the loop does not spin**

Manual check, and worth doing: run the app (`cargo run -p tgt-app` with credentials), open a chat, and leave it idle. The draw gate must settle. If frames keep painting with no input, `Action::ViewportChanged` is setting `dirty` somewhere — the send-only-on-change guard alone is not the protection, the absent dirty flag is.

- [ ] **Step 9: Verify the whole fix by hand**

Open a chat with more than a screen of history. Press `↑` from the composer, then `↑` repeatedly. The messages must not move until the highlight reaches the top row, then scroll one message per press.

- [ ] **Step 10: Full check and commit**

```bash
mise run check
git add crates/app/src/runtime_loop.rs
git commit -m "feat(app): report each frame's message range to the update loop"
```

---

### Task 7: Offer a jump-to-quote chip

**Files:**
- Modify: `crates/core/src/model/chips.rs`
- Modify: `crates/core/src/state/selection.rs:357-384` (`chips_for_message`)
- Modify: `crates/ui/src/view/help.rs:160-168`
- Modify: `docs/architecture.md` §4.2 chip listing
- Test: `crates/core/src/model/chips.rs` and `crates/core/src/state/selection.rs` test modules

**Interfaces:**
- Produces: `Chip::JumpToQuoted`, shortcut `'j'`, label `"Jump to quote"`.

Like `Chip::Reveal` and `Chip::CancelUpload`, this is appended by `selection.rs` after `chips_for` runs rather than folded into `chips_for` — it is gated by a local fact (`msg.reply_to.is_some()`), not a TDLib capability flag.

- [ ] **Step 1: Write the failing tests**

In `crates/core/src/model/chips.rs`'s `mod tests`, add `Chip::JumpToQuoted` to the `ALL` constant (this alone makes `chip_shortcut_letters_unique_per_row` and `labels_are_distinct` cover it), and:

```rust
#[test]
fn jump_to_quoted_is_not_derived_from_caps() {
    // It is a local fact about the message, so `chips_for` — which sees
    // only capability flags — must never produce it.
    for bits in 0u8..32 {
        let caps = caps(
            bits & 1 != 0,
            bits & 2 != 0,
            bits & 4 != 0,
            bits & 8 != 0,
            bits & 16 != 0,
        );
        for flags in 0u8..16 {
            let row = chips_for(
                &caps,
                flags & 1 != 0,
                flags & 2 != 0,
                flags & 4 != 0,
                flags & 8 != 0,
            );
            assert!(!row.contains(&Chip::JumpToQuoted));
        }
    }
}
```

In `crates/core/src/state/selection.rs`'s `mod tests`:

```rust
#[test]
fn a_reply_offers_the_jump_chip_and_a_plain_message_does_not() {
    let mut replying = msg(2);
    replying.reply_to = Some(ReplyPreview {
        message_id: MessageId(1),
        sender_name: "Ada".to_string(),
        excerpt: "earlier".to_string(),
    });
    let mut app = with_messages(vec![msg(1), replying]);
    enter(&mut app);

    assert!(selection(&app).chips.contains(&Chip::JumpToQuoted));

    handle_key(&mut app, Key::Up);
    assert_eq!(selection(&app).message_id, MessageId(1));
    assert!(!selection(&app).chips.contains(&Chip::JumpToQuoted));
}
```

- [ ] **Step 2: Run them and confirm they fail**

Run: `cargo test -p tgt-core chips`
Expected: compile error, no `Chip::JumpToQuoted`.

- [ ] **Step 3: Add the variant**

In `crates/core/src/model/chips.rs`, in `enum Chip`, after `CancelUpload`:

```rust
    /// Jump to the message this one quotes. Like [`Chip::Reveal`] and
    /// [`Chip::CancelUpload`] it is not a TDLib capability — `reply_to`
    /// being `Some` is the local fact that gates it — so `selection.rs`
    /// appends it after `chips_for` runs.
    ///
    /// Offered even when the quoted message is not in the loaded window:
    /// the chip starts a bounded search for it rather than failing, so the
    /// row stays a truthful statement about what is possible.
    JumpToQuoted, // 'j'  (this message quotes another)
```

Add the two match arms:

```rust
            Chip::JumpToQuoted => 'j',
```
```rust
            Chip::JumpToQuoted => "Jump to quote",
```

- [ ] **Step 4: Offer it**

In `chips_for_message`, after the `CancelUpload` block:

```rust
    // Not gated on `send_failed`: a message that failed to send can still
    // quote one that arrived fine, and the quoted message is the context
    // the user needs to decide whether to resend.
    if msg.reply_to.is_some() {
        chips.push(Chip::JumpToQuoted);
    }
```

- [ ] **Step 5: Handle it in `invoke`**

`invoke`'s match on `Chip` is exhaustive, so it will not compile without an arm. Add a placeholder that Task 8 replaces:

```rust
        Chip::JumpToQuoted => Vec::new(),
```

- [ ] **Step 6: Run the tests and confirm they pass**

Run: `cargo test -p tgt-core chips` and `cargo test -p tgt-core state::selection`
Expected: PASS.

- [ ] **Step 7: Watch it fail on purpose**

Change the gate to `if msg.reply_to.is_none()`. `a_reply_offers_the_jump_chip_and_a_plain_message_does_not` must go red on the first assertion. Restore.

- [ ] **Step 8: Update the help overlay**

In `crates/ui/src/view/help.rs`, the Selection mode chip-shortcut row currently reads:

```rust
                key: "r f e c d x l o s",
                desc: "chip shortcuts: reply forward react copy edit delete download open resend",
```

Replace with:

```rust
                key: "r f e c d x l o s v k j",
                desc: "chip shortcuts: reply forward react copy edit delete download open resend reveal cancel-upload jump-to-quote",
```

(`v` and `k` were already missing from this row — the module doc comment at `help.rs:14` lists only `r/f/e/c/d/x/l/o/s`. Fixing that here is in scope because this task changes the same line.)

Update the module doc comment on line 15 to match.

- [ ] **Step 9: Review snapshots**

Run: `cargo insta test -p tgt-ui --check`
Read the diffs — the help overlay and any chip-row snapshot showing a reply. Then `cargo insta accept`.

- [ ] **Step 10: Update `docs/architecture.md` §4.2**

Add `JumpToQuoted` to the chip listing.

- [ ] **Step 11: Full check and commit**

```bash
mise run check
git add -A
git commit -m "feat(selection): offer a jump-to-quote chip on replies"
```

---

### Task 8: Jump to a loaded quoted message

**Files:**
- Modify: `crates/core/src/state/selection.rs` (`invoke`'s `JumpToQuoted` arm)
- Test: `crates/core/src/state/selection.rs` `mod tests`

**Interfaces:**
- Consumes: `Chip::JumpToQuoted` (Task 7), `AnchorPolicy::Jump` (Task 5).

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn the_jump_chip_selects_the_quoted_message_when_it_is_loaded() {
    let mut replying = msg(9);
    replying.reply_to = Some(ReplyPreview {
        message_id: MessageId(3),
        sender_name: "Ada".to_string(),
        excerpt: "earlier".to_string(),
    });
    let mut app = with_messages((1..=8).map(msg).chain([replying]).collect());
    // A viewport that does NOT contain the quoted message: a jump must
    // move the view even though a plain `↑` step would not.
    app.visible_messages = Some((MessageId(7), MessageId(9)));
    enter(&mut app);

    handle_key(&mut app, Key::Char('j'));

    assert_eq!(selection(&app).message_id, MessageId(3));
    assert_eq!(
        app.conversations[&CHAT].scroll,
        Scroll::At {
            message_id: MessageId(3),
            line_offset: 0,
        }
    );
}
```

- [ ] **Step 2: Run it and confirm it fails**

Run: `cargo test -p tgt-core the_jump_chip_selects_the_quoted_message_when_it_is_loaded`
Expected: FAIL — the placeholder arm returns `Vec::new()`, so the selection stays on message 9.

- [ ] **Step 3: Implement the arm**

`invoke` already clones the selected `MessageView` into a local `msg` before its `match chip`, so the reply id is in hand. Replace the placeholder:

```rust
        Chip::JumpToQuoted => {
            let Some(quoted) = msg.reply_to.as_ref().map(|r| r.message_id) else {
                return Vec::new();
            };
            let loaded = app.conversations.get(&chat_id).is_some_and(|convo| {
                conversation::index_of(&convo.messages, quoted).is_some()
            });
            if loaded {
                // A deliberate jump: the view follows unconditionally,
                // unlike an `↑`/`↓` step (see `AnchorPolicy`).
                select(app, chat_id, quoted, AnchorPolicy::Jump)
            } else {
                // Task 9 replaces this with the hunt.
                Vec::new()
            }
        }
```

- [ ] **Step 4: Run it and confirm it passes**

Run: `cargo test -p tgt-core the_jump_chip_selects_the_quoted_message_when_it_is_loaded`
Expected: PASS.

- [ ] **Step 5: Watch it fail on purpose**

Change `AnchorPolicy::Jump` to `AnchorPolicy::KeepVisible`. The scroll assertion must go red (a `KeepVisible` step would move the anchor by one message, not to message 3). Restore.

That breakage is the one that proves the two policies are actually distinct here — if the test stays green, the jump is not exercising the branch it claims to.

- [ ] **Step 6: Full check and commit**

```bash
mise run check
git add crates/core/src/state/selection.rs
git commit -m "feat(selection): jump to a loaded quoted message"
```

---

### Task 9: Hunt for an unloaded quoted message

**Files:**
- Modify: `crates/core/src/state/conversation.rs` (`JumpHunt`, `ConversationState.hunt`, hunt driving in `apply_history_page`)
- Modify: `crates/core/src/state/selection.rs` (start the hunt)
- Modify: `docs/architecture.md` (`ConversationState` definition)
- Test: `crates/core/src/state/conversation.rs` `mod tests`

**Interfaces:**
- Produces: `conversation::start_hunt(app, chat_id, target) -> Vec<Effect>`, `conversation::cancel_hunt(convo)`, `ConversationState.hunt: Option<JumpHunt>`.

**The eviction trap this task exists to pay for:** `evict_excess` drops from the **front** of the window whenever the anchor is `Scroll::Bottom`. A hunt that left the anchor at the bottom would evict every page it fetched the moment the window reached `WINDOW_MAX_MESSAGES` (500) — it would page for its whole budget and never find anything. So the hunt moves the anchor to the oldest loaded message after each page. `evict_excess` then computes `dist_front <= dist_back` as true and drops from the back instead. This also supplies the progress feedback for free: the view visibly scrolls back while the hunt runs.

- [ ] **Step 1: Edit `docs/architecture.md` first**

Add `hunt: Option<JumpHunt>` to the `ConversationState` definition with the eviction note above.

- [ ] **Step 2: Write the failing tests**

In `crates/core/src/state/conversation.rs`'s `mod tests`:

```rust
#[test]
fn a_hunt_pages_backward_and_moves_the_anchor_so_pages_are_not_evicted() {
    let mut app = fixture_state();
    open(&mut app, CHAT);
    let convo = app.conversations.get_mut(&CHAT).unwrap();
    for id in 50..60 {
        convo.messages.push_back(msg(id));
    }
    convo.scroll = Scroll::Bottom;
    convo.paging = PagingState::Idle;

    let effects = start_hunt(&mut app, CHAT, MessageId(20));

    assert!(matches!(
        effects.as_slice(),
        [Effect::Td(TdRequest::GetChatHistory { from_message_id, .. })]
            if *from_message_id == MessageId(50)
    ));
    assert!(app.conversations[&CHAT].hunt.is_some());
    // The anchor left the bottom, or eviction would drop the pages the
    // hunt is about to fetch.
    assert_ne!(app.conversations[&CHAT].scroll, Scroll::Bottom);
}

#[test]
fn a_hunt_lands_when_its_target_arrives() {
    let mut app = fixture_state();
    open(&mut app, CHAT);
    let convo = app.conversations.get_mut(&CHAT).unwrap();
    for id in 50..60 {
        convo.messages.push_back(msg(id));
    }
    convo.paging = PagingState::Loading {
        attempt: 1,
        only_local: false,
    };
    start_hunt(&mut app, CHAT, MessageId(45));

    let page: Vec<MessageView> = (40..50).map(msg).collect();
    apply_history_page(&mut app, CHAT, false, &Ok(page));

    assert!(app.conversations[&CHAT].hunt.is_none(), "hunt cleared");
    assert_eq!(
        app.conversations[&CHAT].scroll,
        Scroll::At {
            message_id: MessageId(45),
            line_offset: 0,
        }
    );
}

#[test]
fn a_hunt_gives_up_after_max_pages_and_says_so() {
    let mut app = fixture_state();
    open(&mut app, CHAT);
    let convo = app.conversations.get_mut(&CHAT).unwrap();
    for id in 900..1000 {
        convo.messages.push_back(msg(id));
    }
    convo.paging = PagingState::Loading {
        attempt: 1,
        only_local: false,
    };
    start_hunt(&mut app, CHAT, MessageId(1));
    app.conversations.get_mut(&CHAT).unwrap().hunt =
        Some(JumpHunt { target: MessageId(1), pages_spent: MAX_HUNT_PAGES });

    let page: Vec<MessageView> = (800..900).map(msg).collect();
    apply_history_page(&mut app, CHAT, false, &Ok(page));

    assert!(app.conversations[&CHAT].hunt.is_none());
    assert!(
        !app.toasts.toasts.is_empty(),
        "giving up silently is the failure this bound exists to make visible"
    );
}

#[test]
fn a_hunt_stops_at_the_start_of_history() {
    let mut app = fixture_state();
    open(&mut app, CHAT);
    let convo = app.conversations.get_mut(&CHAT).unwrap();
    convo.messages.push_back(msg(5));
    convo.paging = PagingState::Loading {
        attempt: history::MAX_EMPTY_ATTEMPTS,
        only_local: false,
    };
    start_hunt(&mut app, CHAT, MessageId(1));

    // An empty non-local page at max attempts latches `Exhausted`.
    apply_history_page(&mut app, CHAT, false, &Ok(Vec::new()));

    assert_eq!(app.conversations[&CHAT].paging, PagingState::Exhausted);
    assert!(app.conversations[&CHAT].hunt.is_none());
    assert!(!app.toasts.toasts.is_empty());
}

#[test]
fn a_history_error_ends_the_hunt_rather_than_stalling_it() {
    let mut app = fixture_state();
    open(&mut app, CHAT);
    let convo = app.conversations.get_mut(&CHAT).unwrap();
    convo.messages.push_back(msg(50));
    convo.paging = PagingState::Loading {
        attempt: 1,
        only_local: false,
    };
    start_hunt(&mut app, CHAT, MessageId(1));

    apply_history_page(
        &mut app,
        CHAT,
        false,
        &Err(TdError::FloodWait { seconds: 30 }),
    );

    // Waiting out a 30-second cooldown with no sign of life is worse than
    // saying it stopped; the user can press `j` again.
    assert!(app.conversations[&CHAT].hunt.is_none());
    assert!(!app.toasts.toasts.is_empty());
}
```

`ToastState` is `{ toasts: VecDeque<Toast> }`, so `app.toasts.toasts` is correct.

- [ ] **Step 3: Run them and confirm they fail**

Run: `cargo test -p tgt-core state::conversation::tests::a_hunt`
Expected: compile error, no `JumpHunt`.

- [ ] **Step 4: Add the state**

In `crates/core/src/state/conversation.rs`:

```rust
/// A search for a message older than the loaded window, started by the
/// jump-to-quote chip. Pages backward until the target arrives, the start of
/// history is reached, or the budget runs out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JumpHunt {
    pub target: MessageId,
    pub pages_spent: u8,
}

/// 20 × `history::PAGE_SIZE` = 1000 messages. A reply quote is nearly always
/// a message still in the window or a page behind it; this bounds the
/// pathological case (quoting something from a year ago) rather than sizing
/// the common one.
pub const MAX_HUNT_PAGES: u8 = 20;
```

Add to `struct ConversationState`:

```rust
    /// An in-flight jump-to-quote search, or `None`. See [`JumpHunt`].
    pub hunt: Option<JumpHunt>,
```

- [ ] **Step 5: Fix every fixture**

Run `cargo build --workspace --all-targets`. It lists all 15 `ConversationState { … }` literals. Add `hunt: None,` to each.

- [ ] **Step 6: Add `start_hunt` and `cancel_hunt`**

```rust
/// Begins a backward search for `target`. Moves the anchor to the oldest
/// loaded message first: `evict_excess` drops from the FRONT while the
/// anchor is at the bottom, which would evict each page the hunt fetches as
/// soon as the window hit `WINDOW_MAX_MESSAGES` — the hunt would spend its
/// whole budget and find nothing. With the anchor at the front, eviction
/// drops from the back and the window walks backward instead. The moving
/// anchor is also the progress indicator, which is why there is no spinner.
pub fn start_hunt(app: &mut AppState, chat_id: ChatId, target: MessageId) -> Vec<Effect> {
    let now = app.now;
    let Some(convo) = app.conversations.get_mut(&chat_id) else {
        return Vec::new();
    };
    let Some(oldest) = convo.messages.front().map(|m| m.id) else {
        return Vec::new();
    };
    convo.hunt = Some(JumpHunt {
        target,
        pages_spent: 0,
    });
    let mut effects = anchor_to(convo, chat_id, oldest, now);
    if effects.is_empty() {
        // `anchor_to`'s paging trigger is a no-op when the machine is not
        // `Idle`; ask directly so the hunt always has a page in flight.
        effects.push(Effect::Td(TdRequest::GetChatHistory {
            chat_id,
            from_message_id: oldest,
            limit: history::PAGE_SIZE,
            only_local: false,
        }));
    }
    effects
}

/// Abandons an in-flight hunt. Called when the user takes over navigation:
/// `Esc`, closing the chat, or a manual scroll key.
pub fn cancel_hunt(convo: &mut ConversationState) {
    convo.hunt = None;
}
```

- [ ] **Step 7: Drive the hunt from `apply_history_page`**

In the `Ok(msgs)` branch, after `drop_selection_if_gone(convo);` and before `effects.extend(fill_viewport(…))`, add a call to a new helper; and in the `Err(e)` branch, after `history::on_history_error(…)`, clear the hunt and toast.

```rust
/// One step of an in-flight hunt, run after a page has been prepended.
/// Returns the request that continues it, or nothing when it ended.
fn advance_hunt(app: &mut AppState, chat_id: ChatId) -> Vec<Effect> {
    let Some(convo) = app.conversations.get_mut(&chat_id) else {
        return Vec::new();
    };
    let Some(hunt) = convo.hunt else {
        return Vec::new();
    };
    let now = app.now;

    if index_of(&convo.messages, hunt.target).is_some() {
        convo.hunt = None;
        return anchor_to(convo, chat_id, hunt.target, now);
    }
    if matches!(convo.paging, PagingState::Exhausted) {
        convo.hunt = None;
        return toasts::on_action_failed(
            app,
            chat_id,
            "the quoted message is no longer available".to_string(),
        );
    }
    if hunt.pages_spent >= MAX_HUNT_PAGES {
        convo.hunt = None;
        return toasts::on_action_failed(
            app,
            chat_id,
            "could not find the quoted message".to_string(),
        );
    }
    let Some(oldest) = convo.messages.front().map(|m| m.id) else {
        convo.hunt = None;
        return Vec::new();
    };
    convo.hunt = Some(JumpHunt {
        target: hunt.target,
        pages_spent: hunt.pages_spent + 1,
    });
    let mut effects = anchor_to(convo, chat_id, oldest, now);
    effects.push(Effect::Td(TdRequest::GetChatHistory {
        chat_id,
        from_message_id: oldest,
        limit: history::PAGE_SIZE,
        only_local: false,
    }));
    effects
}
```

The borrow checker will object to holding `convo` across the `toasts::on_action_failed(app, …)` calls. Restructure so the mutable borrow is dropped first: decide the outcome into a local `enum`, drop the borrow, then act on it.

Landing the hunt sets only the anchor. Selecting the message too is the caller's concern; `selection.rs` owns selection, and reaching into it from here would put a second selection path in the module that does not own one.

- [ ] **Step 8: Start the hunt from the chip**

In `selection.rs`'s `JumpToQuoted` arm, replace Task 8's `Vec::new()` placeholder for the unloaded case:

```rust
                conversation::start_hunt(app, chat_id, quoted)
```

- [ ] **Step 9: Cancel it where the user takes over**

- `conversation::handle_key`'s scroll arms (`Up`/`Down`/`PageUp`/`PageDown`): `cancel_hunt(convo)` before moving the anchor.
- Wherever `open_chat` changes in `app.rs`: cancel the hunt on the chat being left.
- `app.rs`'s `escape()` path, when it pops out of selection mode.

- [ ] **Step 10: Run the tests and confirm they pass**

Run: `cargo test -p tgt-core state::conversation::tests::a_hunt` and `a_history_error_ends_the_hunt`
Expected: all five PASS. Then `cargo test --workspace`.

- [ ] **Step 11: Watch each termination arm fail separately**

Five breakages, restoring between each — this is the task where a dead arm is most likely to hide:
1. Delete the `anchor_to(convo, chat_id, oldest, now)` in `start_hunt` → `a_hunt_pages_backward_and_moves_the_anchor_so_pages_are_not_evicted` red on the `assert_ne!`.
2. Delete the target-found branch → `a_hunt_lands_when_its_target_arrives` red.
3. Change `>= MAX_HUNT_PAGES` to `>= u8::MAX` → `a_hunt_gives_up_after_max_pages_and_says_so` red.
4. Delete the `Exhausted` branch → `a_hunt_stops_at_the_start_of_history` red.
5. Delete the `Err` branch's hunt clearing → `a_history_error_ends_the_hunt_rather_than_stalling_it` red.

- [ ] **Step 12: Verify by hand**

Open a busy chat, scroll to load a few pages, find a message quoting something well above the window, select it and press `j`. The view must walk backward and land on the quoted message. Press `j` on a reply whose target is on screen: it must jump immediately with no paging.

- [ ] **Step 13: Full check and commit**

```bash
mise run check
git add -A
git commit -m "feat(conversation): hunt backward for an unloaded quoted message"
```

---

### Task 10: Telemetry allowlist and final sweep

**Files:**
- Modify: `crates/core/src/telemetry/schema.rs` (only if chip invocation emits events)
- Test: `crates/app/tests/telemetry_allowlist.rs`

- [ ] **Step 1: Check whether chips emit telemetry**

Run: `grep -n "Chip" crates/core/src/app.rs crates/core/src/telemetry/schema.rs`

If `app.rs` attaches an `Effect::Telemetry` keyed off a chip invocation, `JumpToQuoted` needs a constant in `schema.rs`. If chips emit nothing, skip to Step 3.

- [ ] **Step 2: Add the constant if needed**

Add it to `ALLOWED_KEYS` in `crates/core/src/telemetry/schema.rs` following the existing naming, then review the insta snapshot diff on the schema.

- [ ] **Step 3: Run the allowlist proof**

Run: `cargo test -p tgt-app --test telemetry_allowlist`
Expected: PASS. This boots the app against an in-process OTLP collector stub and fails on any exported key outside the allowlist, with an anti-vacuity check so a dead exporter cannot pass silently.

- [ ] **Step 4: Full gate**

```bash
mise run check
mise run snapshots
```

Both must be clean — `mise run snapshots` fails on any pending insta snapshot, which catches an accepted-but-uncommitted review.

- [ ] **Step 5: Commit anything outstanding**

```bash
git add -A
git commit -m "chore: telemetry allowlist and snapshot sweep for keyboard navigation"
```
