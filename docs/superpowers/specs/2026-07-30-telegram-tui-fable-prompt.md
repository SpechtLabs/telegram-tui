# Handoff prompt for fable

Paste everything below the line into fable. It references
`docs/superpowers/specs/2026-07-30-telegram-tui-design.md`, which must be present
in the repo.

---

You are architecting `telegram-tui` (binary name `tgt`): a keyboard-driven
terminal Telegram client in Rust, built on TDLib and ratatui.

An approved design specification exists at
`docs/superpowers/specs/2026-07-30-telegram-tui-design.md`. **Read it first and
in full.** It is the product of a completed design session; its decisions are
settled, not suggestions. Your job is to turn it into a concrete architecture and
an implementation plan that a fleet of subagents can execute in parallel.

## What to produce

Two artifacts.

**1. `docs/architecture.md`** — the architecture as-built-to-be:

- The exact crate graph, every module with its path, and a one-line statement of
  each module's single responsibility.
- Full type definitions for the load-bearing types: `Action`, `Effect`,
  `AppState` and its sub-states, `Focus`, `TdRequest` / `TdResponse` / `TdUpdate`,
  `TdError`, `MessageView`, `ChatView`, `Theme`, and the telemetry schema
  constants. Write these as real Rust, not prose descriptions.
- The `TdRuntime` trait signature and both implementations' responsibilities.
- The complete `Cargo.toml` dependency set for each crate, with pinned versions,
  including the `tdlib-rs` feature selection and the `tracing-batteries` git
  dependency pinned to a specific commit.
- A sequence diagram (mermaid) for three flows: cold start through
  authentication, sending a message including the optimistic-then-confirmed
  lifecycle, and scrolling up far enough to trigger history paging.

**2. `docs/plan.md`** — the implementation plan, structured for parallel subagent
execution:

- Decompose into discrete tasks along the milestone boundaries in §16 of the spec.
- Each task must state: its goal, the exact files it owns, the files it may read
  but must not modify, its dependencies on other tasks, its acceptance criteria as
  runnable commands, and the tests it must ship with.
- **File ownership must be disjoint within any group of tasks marked
  parallel-safe.** Two concurrent subagents editing the same file is the primary
  failure mode of this execution model; the plan's structure is what prevents it.
- Mark the critical path explicitly and identify which tasks can run concurrently.
- Order tasks so every milestone ends at a demonstrable, compiling, test-passing
  state. No task may leave `main` broken.

## Non-negotiable constraints

These came out of the design session and are not open for re-litigation:

1. **Crate boundaries.** `core` must not depend on `ratatui` or `crossterm`.
   `ui` must not depend on `tdlib-rs`. Include a CI check that enforces this.
2. **`update()` is pure.** `App::update(&mut self, Action) -> Vec<Effect>` does no
   I/O, spawns no tasks, and reads no clock or RNG directly — both are injected.
   All side effects are returned as `Effect` values and dispatched elsewhere. This
   is what makes the application testable without a network or a terminal; do not
   compromise it for convenience.
3. **One action channel.** Keystrokes, TDLib updates, download progress, and
   ticks all normalize to `Action` on a single mpsc. No `Arc<RwLock<AppState>>`,
   no shared mutable state, no locks in the render path.
4. **Telemetry is allowlist-enforced.** A `telemetry::emit!` macro is the only
   path to the OTLP exporter, gated on a `telemetry.public = true` marker that the
   subscriber layer filters on. Message text, names, usernames, phone numbers,
   chat titles, file names, and raw Telegram identifiers must be structurally
   incapable of reaching the network. Ship the CI test that proves it (spec
   §13.8). Treat this as a correctness requirement, not a policy preference.
5. **Telegram entity offsets are UTF-16 code units.** The message layout engine
   must convert them to byte offsets before slicing, and must wrap
   grapheme-aware and width-aware. This is the highest-probability correctness bug
   in the project; give it its own isolated pure function and an exhaustive test
   table.
6. **TDLib ordering is authoritative.** Chat list order comes from TDLib's
   `order: i64` via `updateChatPosition`. Never compute it locally.
7. **`getChatHistory` may return zero messages while more history exists.** The
   paging state machine must encode this rather than treating a short response as
   end-of-history.
8. **Nothing writes to stdout or stderr while the TUI is active.** Logging goes to
   a rolling file. A panic hook restores the terminal before printing.
9. **macOS / Apple Silicon only for v1.** Do not add Linux or Windows
   conditionals, but do not architect in a way that forecloses them.
10. **Local tooling is managed by `mise`** with exact pinned versions. Do not
    introduce Homebrew, `curl | bash`, or global package installs. TDLib arrives
    through the `tdlib-rs` `download-tdlib` cargo feature, not a system package.

## Where to apply judgment

The spec deliberately leaves these to you. Decide them, state the decision, and
give a one-sentence rationale:

- The precise `Action` and `Effect` enum decomposition — how coarse or fine, and
  whether TDLib updates map 1:1 to actions or are pre-digested.
- Whether `AppState` sub-states are separate structs with their own `update`
  functions or a single flat struct.
- The layout cache eviction policy and its bound.
- Error propagation from `Effect` dispatch back into the action stream.
- Test fixture format for `FakeTd` recorded update sequences.

## Quality bar

- Every type in `docs/architecture.md` must be real, compilable Rust — no
  `// ...` elisions in the load-bearing definitions.
- Every acceptance criterion in `docs/plan.md` must be a command that can be run
  and observed to pass or fail. "Works correctly" is not an acceptance criterion;
  `cargo test -p core state::history` is.
- Solve the macOS TDLib dylib `@rpath` problem concretely in milestone 1 with a
  specific mechanism, rather than noting it as a risk. It will otherwise surface
  at packaging time when it is most expensive.
- Prefer many small focused modules over few large ones. A file that has grown
  past a few hundred lines is a signal that it holds more than one
  responsibility — and subagents edit small focused files far more reliably.

Ask before you begin if anything in the spec is ambiguous or appears internally
inconsistent. Otherwise, produce both documents.
