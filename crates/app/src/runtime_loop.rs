//! The `tokio::select!` main loop (docs/architecture.md §3): one action
//! channel, terminal events, TDLib updates, a 250 ms housekeeping tick, and a
//! 16 ms coalescing draw gate. Exits once the dispatcher observes
//! `Effect::Quit`.
//!
//! [`Core`] is the loop without the terminal: the action channel, the pure
//! [`App`], the dispatcher and the TDLib update stream. [`run`] wraps it with
//! terminal setup and drawing; the full-app integration tests
//! (`crates/app/tests/`) drive the same `Core` against `FakeTd`, so what they
//! exercise is the real machinery rather than a re-implementation of it.

use std::io;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use color_eyre::eyre;
use crossterm::event::{Event, MouseButton, MouseEvent, MouseEventKind};
use ratatui::DefaultTerminal;
use ratatui::backend::Backend;
use tokio::sync::{mpsc, watch};
use tokio::time::{self, MissedTickBehavior};

use tgt_core::action::Action;
use tgt_core::app::App;
use tgt_core::effect::Effect;
use tgt_core::model::hit::ClickButton;
use tgt_core::model::time::Millis;
use tgt_core::td::runtime::TdRuntime;
use tgt_core::td::update::{AuthPhase, TdUpdate};
use tgt_ui::render::hit::HitMap;
use tgt_ui::render::image::{Capability, CellSize};
use tgt_ui::render::state::RenderState;
use tgt_ui::theme::Theme;

use crate::config::Config;
use crate::dispatch::{Dispatcher, TdBootParams};

/// Comfortably absorbs a burst of effect completions between draws without
/// ever blocking a sender.
const ACTION_CHANNEL_CAPACITY: usize = 256;
/// Housekeeping cadence for time-dependent state (toasts, flood countdown,
/// typing expiry) — architecture.md §8's "tick design" decision.
const TICK_PERIOD: Duration = Duration::from_millis(250);
/// Coalesces draw bursts: a redraw happens at most this often, independent
/// of how many actions land between frames.
const DRAW_GATE: Duration = Duration::from_millis(16);
/// How often the background terminal-event reader checks whether the loop
/// it feeds is still around.
const EVENT_POLL_TIMEOUT: Duration = Duration::from_millis(50);

/// What one [`Core::step`] decided about the loop's future.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Continue,
    Quit,
    /// A config write failed. The error itself is parked in
    /// [`Core::fatal`] rather than carried here, so `Step` stays
    /// comparable — the loop only needs to know it must stop.
    Fatal,
}

/// One iteration's input, produced inside `select!` and handled outside it so
/// the handlers can borrow all of `self` (the select's futures each hold a
/// mutable borrow of one field).
enum Input {
    Quit,
    /// A config write failed; the run ends with this error. See
    /// `config::unwritable`.
    Fatal(human_errors::Error),
    Action(Action),
    Term(Event),
    Td(TdUpdate),
    Tick(Millis),
    /// The draw gate reopened while a frame was pending. Applies nothing.
    DrawDue,
}

/// Builds a replacement TDLib client after the previous one closed.
///
/// A factory rather than a spare instance because creating the real one
/// starts an OS thread and talks to TDLib, and because `updates()` may only
/// be called once per instance — so a restart genuinely needs a *new* one,
/// not a second handle on the old.
///
/// Opt-in via [`Core::with_restart`]: `Core::new` is unchanged, so no test
/// that does not care about restarting has to say anything about it.
pub type RuntimeFactory = Arc<
    dyn Fn() -> std::pin::Pin<Box<dyn std::future::Future<Output = Arc<dyn TdRuntime>> + Send>>
        + Send
        + Sync,
>;

/// The main loop minus the terminal. See the module docs.
pub struct Core {
    app: App,
    action_rx: mpsc::Receiver<Action>,
    term_events: mpsc::Receiver<Event>,
    td_updates: mpsc::Receiver<TdUpdate>,
    dispatcher: Dispatcher,
    quit_rx: watch::Receiver<bool>,
    fatal_rx: mpsc::Receiver<human_errors::Error>,
    /// Set by the step that saw [`Step::Fatal`]; taken by [`run`].
    fatal: Option<human_errors::Error>,
    tick: time::Interval,
    /// The only clock read outside `core`: `App::update` receives time
    /// exclusively via `Action::Tick { now }`, anchored to loop start.
    clock_start: Instant,
    effects: Vec<Effect>,
    /// What one frame leaves for the next (architecture.md §4.9.1): T21's
    /// `LayoutCache`, the per-message inline images, and the graphics
    /// capability `main` probed at startup. Cleared wholesale on a terminal
    /// resize — cached lines are wrapped at the old column width, and placed
    /// images are addressed in cells that have just moved. Theme changes
    /// don't need the cache half of that (`theme_generation` is part of
    /// `LayoutKey`, so a swap misses forward), but they do need the image
    /// half, which `RenderState` handles itself.
    render: RenderState,
    /// Where the last drawn frame put everything a mouse can hit
    /// (architecture §7.5), refreshed by `draw_if_due` from `view`'s return
    /// value. Empty until the first frame is drawn and whenever an overlay
    /// is up, so a click that arrives before or under either resolves to
    /// nothing rather than to a stale frame's geometry.
    last_hits: HitMap,
    /// Builds a replacement TDLib client. `None` — the default, and what
    /// every test that is not about restarting gets — means a client that
    /// reaches `Closed` stays closed, exactly as before this existed.
    restart: Option<RuntimeFactory>,
    /// Whether the *current* client has ever reached `AuthPhase::Ready`.
    /// Reset with every replacement. See [`Core::apply_td`] for what it
    /// gates and why the gate is deliberately narrow.
    authorized: bool,
    /// Whether the terminal's cell size needs (re)measuring before the next
    /// frame. Starts true, so the first frame is drawn against a measured
    /// size rather than `CellSize::FALLBACK`.
    ///
    /// Measuring is an ioctl on the real terminal, which is `run`'s business
    /// and not this struct's: the integration tests `#[path]`-include this
    /// module without `main.rs` or `graphics.rs` and drive `Core` directly
    /// against `FakeTd`, with no terminal to ask (the same reason
    /// [`ThemeResolver`] is a `fn` pointer rather than a `crate::` call).
    cell_size_stale: bool,
}

impl Core {
    /// Enables client restarts, using `factory` to build the replacement.
    ///
    /// Without this a closed client stays closed, which is what every
    /// pre-existing caller wants and what they all still get by saying
    /// nothing.
    pub fn with_restart(mut self, factory: RuntimeFactory) -> Self {
        self.restart = Some(factory);
        self
    }

    /// Takes the runtime's update receiver (once — the trait panics on a
    /// second call) and wires the dispatcher to the action channel.
    pub fn new(
        app: App,
        runtime: Arc<dyn TdRuntime>,
        config: Arc<Mutex<Config>>,
        td_boot: TdBootParams,
        term_events: mpsc::Receiver<Event>,
        graphics: Option<Capability>,
    ) -> Self {
        let (action_tx, action_rx) = mpsc::channel::<Action>(ACTION_CHANNEL_CAPACITY);
        let td_updates = runtime.updates();
        let (dispatcher, quit_rx, fatal_rx) = Dispatcher::new(action_tx, runtime, config, td_boot);

        let mut tick = time::interval(TICK_PERIOD);
        tick.set_missed_tick_behavior(MissedTickBehavior::Delay);

        Core {
            app,
            action_rx,
            term_events,
            td_updates,
            dispatcher,
            quit_rx,
            fatal_rx,
            fatal: None,
            restart: None,
            authorized: false,
            tick,
            clock_start: Instant::now(),
            effects: Vec::new(),
            render: RenderState::new(graphics),
            last_hits: HitMap::new(),
            cell_size_stale: true,
        }
    }

    /// The state a frame would be rendered from.
    // Dead in the bin target itself: consumed by the integration tests, which
    // `#[path]`-include this module (see tests/auth_flow.rs).
    #[allow(dead_code)]
    pub fn app(&self) -> &App {
        &self.app
    }

    /// True once per render-worthy change; cleared on read.
    pub fn take_dirty(&mut self) -> bool {
        self.app.take_dirty()
    }

    /// Waits for the next input from any source, applies it, and dispatches
    /// whatever effects it produced.
    // Dead in the bin target, which always passes a draw deadline: this is
    // the entry point the integration tests use (they `#[path]`-include this
    // module and have no gate to answer to).
    #[allow(dead_code)]
    pub async fn step(&mut self) -> Step {
        self.step_until(None).await
    }

    /// [`step`](Self::step), but also woken by `draw_deadline`. The loop
    /// passes the instant the draw gate reopens whenever it is holding a
    /// frame back, so a change that lands mid-gate is painted as soon as the
    /// gate allows rather than waiting for the next input to arrive.
    pub async fn step_until(&mut self, draw_deadline: Option<Instant>) -> Step {
        let clock_start = self.clock_start;
        let input = {
            // Destructured so each `select!` future borrows exactly one
            // field and the arms stay free of `self`.
            let Core {
                action_rx,
                term_events,
                td_updates,
                tick,
                quit_rx,
                fatal_rx,
                ..
            } = self;

            tokio::select! {
                // Fires only while a frame is being held back by the gate;
                // `Input::DrawDue` applies nothing and exists purely to
                // return control to the loop so it can draw.
                () = async {
                    match draw_deadline {
                        Some(at) => time::sleep_until(time::Instant::from_std(at)).await,
                        None => std::future::pending().await,
                    }
                } => Input::DrawDue,
                // The dispatcher only ever sends `true`, so any change is
                // the quit signal; a closed channel means the dispatcher is
                // gone, which is also the end of the loop.
                _ = quit_rx.changed() => Input::Quit,
                // A failed config write. Ranked with quit rather than with
                // the action stream: it is not something `update()` gets a
                // say in, it is the run ending.
                Some(err) = fatal_rx.recv() => Input::Fatal(err),
                Some(action) = action_rx.recv() => Input::Action(action),
                Some(event) = term_events.recv() => Input::Term(event),
                Some(update) = td_updates.recv() => Input::Td(update),
                _ = tick.tick() => {
                    Input::Tick(Millis(clock_start.elapsed().as_millis() as u64))
                }
            }
        };

        match input {
            Input::Quit => return Step::Quit,
            Input::Fatal(err) => {
                self.fatal = Some(err);
                return Step::Fatal;
            }
            Input::Action(action) => self.apply(action),
            // Mouse events are the one kind `tgt_ui::input::map_event` can't
            // translate on its own: they only mean something relative to the
            // frame that is on screen (see `translate_mouse`).
            Input::Term(Event::Mouse(mouse)) => {
                if let Some(action) = translate_mouse(&self.last_hits, mouse) {
                    self.apply(action);
                }
            }
            Input::Term(event) => {
                // A resize invalidates every cached layout: they're wrapped
                // at the old column width. It invalidates every placed image
                // too — protocol cells do not move with the reflow, they
                // ghost (spec §8.3). `LayoutKey::theme_generation` handles
                // theme swaps on its own, so only width needs the cache half
                // of this.
                if let Event::Resize(_, _) = event {
                    self.render.clear();
                    // A resize is also what arrives when the user changes
                    // the font size, so the cell size inline images are
                    // encoded against has to be measured again. Measuring is
                    // `run`'s job, not this one's — see the field.
                    self.cell_size_stale = true;
                }
                if let Some(action) = tgt_ui::input::map_event(event) {
                    self.apply(resolve_pasted_path(action));
                }
            }
            Input::Td(update) => self.apply_td(update).await,
            // Nothing to apply: the loop draws on the way back around.
            Input::DrawDue => {}
            Input::Tick(now) => self.apply(Action::Tick { now }),
        }

        for effect in self.effects.drain(..) {
            self.dispatcher.dispatch(effect);
        }
        Step::Continue
    }

    fn apply(&mut self, action: Action) {
        let effects = self.app.update(action);
        self.effects.extend(effects);
    }

    /// TDLib updates enter `update()` like any other action, with exactly one
    /// impure exception: `WaitTdlibParameters`. `SetTdlibParameters` carries
    /// the api credentials, the Keychain database key and the database
    /// directory — boot facts `tgt-core` deliberately does not hold — so
    /// `state::auth::handle_td` projects the phase and emits nothing, and the
    /// dispatcher issues the request (architecture §5.1, and `dispatch.rs`'s
    /// module docs). This is the only place the loop looks inside an update
    /// rather than just forwarding it.
    async fn apply_td(&mut self, update: TdUpdate) {
        let needs_parameters = matches!(update, TdUpdate::Auth(AuthPhase::WaitTdlibParameters));
        if matches!(update, TdUpdate::Auth(AuthPhase::Ready)) {
            self.authorized = true;
        }
        let closed = matches!(update, TdUpdate::Auth(AuthPhase::Closed));
        self.apply(Action::Td(update));
        if needs_parameters {
            self.dispatcher.request_tdlib_parameters();
        }
        if closed {
            self.restart_client().await;
        }
    }

    /// Replaces a TDLib client that has reached `authorizationStateClosed`,
    /// which is terminal for that instance — only a new client can get back
    /// to a usable state.
    ///
    /// # Why this declines to fire in the case it most obviously applies to
    ///
    /// It restarts **only when the closed client never reached
    /// `AuthPhase::Ready`**, and that narrowness is deliberate. Chats are
    /// loaded from exactly one place, `state::auth`'s `Ready` arm, so a
    /// client that never authorized cannot have left any account-scoped
    /// state behind: no chats, no conversations, no cached media. Replacing
    /// it is therefore complete on its own.
    ///
    /// A signed-in client that closes — `/logout`, or TDLib tearing itself
    /// down on a local error — is a different problem. `AppState` still
    /// holds the previous session's chats, and `tgt-app` cannot clear them
    /// because `update()` is pure and clearing needs a core action that does
    /// not exist yet. Restarting anyway would leave the app rendering a
    /// signed-out user's chat list against a fresh unauthenticated client:
    /// alive-looking, and showing exactly the content they asked to be rid
    /// of. Today's behaviour — it visibly stops — is worse in the abstract
    /// and better in practice, because it is honest.
    ///
    /// So this is half a fix on purpose. Task #64 adds the account-state
    /// reset; widening the condition belongs in that change and not before
    /// it. If you are here because a restart "obviously should have fired",
    /// that is the reason, and removing the `authorized` check without the
    /// reset reintroduces the bug this comment exists to prevent.
    async fn restart_client(&mut self) {
        let Some(factory) = self.restart.clone() else {
            return;
        };
        if self.authorized {
            tracing::warn!(
                "the tdlib client closed after authorizing; not restarting, because \
                 account state would survive into the new session (task #64)"
            );
            return;
        }

        // Joined, not merely asked to stop: the receive thread reads a
        // process-global queue and discards updates belonging to other
        // clients, so one still running would eat the replacement's
        // `WaitTdlibParameters`. See `td_runtime::TdlibRuntime::shutdown`.
        self.dispatcher.runtime().shutdown().await;

        let runtime = factory().await;
        // Taken before the swap: `updates()` panics on a second call, so if
        // it is going to fail it should fail before the dispatcher has been
        // pointed at a runtime whose updates nobody is reading.
        self.td_updates = runtime.updates();
        self.dispatcher.replace_runtime(runtime);
        self.authorized = false;
        tracing::info!("tdlib client restarted after close");
    }
}

/// Resolves a crossterm mouse event against the last drawn frame's regions
/// (architecture §7.5). This is the whole of the mouse boundary: everything
/// past it is a semantic `Action` that `App::update` routes like any other.
///
/// Only three kinds of event produce one. A left or right button press
/// becomes `Action::Click` for whatever target covers the cell; a wheel step
/// becomes `Action::Scroll` for the pane under the pointer. Motion, drags,
/// button releases and the middle button are all ignored outright — v1 has
/// no drag-select, no hover state and nothing bound to the middle button, so
/// forwarding them would only mean actions core has to discard. A press over
/// a cell no region covers (the frame border, the hint bar, the gap between
/// two folder tabs) is likewise nothing at all, not a click on the nearest
/// thing.
fn translate_mouse(hits: &HitMap, ev: MouseEvent) -> Option<Action> {
    match ev.kind {
        MouseEventKind::Down(MouseButton::Left) => {
            hits.target_at(ev.column, ev.row)
                .map(|target| Action::Click {
                    target,
                    button: ClickButton::Left,
                })
        }
        MouseEventKind::Down(MouseButton::Right) => {
            hits.target_at(ev.column, ev.row)
                .map(|target| Action::Click {
                    target,
                    button: ClickButton::Right,
                })
        }
        MouseEventKind::ScrollUp => hits
            .area_at(ev.column, ev.row)
            .map(|area| Action::Scroll { area, up: true }),
        MouseEventKind::ScrollDown => hits
            .area_at(ev.column, ev.row)
            .map(|area| Action::Scroll { area, up: false }),
        _ => None,
    }
}

/// Expands a pasted `~/…` path against `$HOME` when the file it names is
/// really there, so the send-file offer `state::composer::handle_paste`
/// raises carries a path TDLib can open. Core cannot do this itself: `$HOME`
/// and the filesystem are both off-limits to it (architecture §9.3). Any
/// other action, and any paste that doesn't resolve, passes through
/// unchanged.
///
/// DECISION (plan T40): a paste that *looks* like a path but names nothing
/// on disk still raises the offer, rather than being rewritten or suppressed
/// here. Suppressing it would need an action variant meaning "insert this as
/// plain text, no offer", which does not exist, and inventing one to catch a
/// rare mis-paste is not worth the widening of the action surface. The
/// failure it leaves is bounded and already handled: confirming such an
/// offer never reaches TDLib — `dispatch::resolve_outgoing_file` rejects the
/// path and completes the send as a failure — so the worst case is one
/// dismissable modal, not a bad request or lost text.
fn resolve_pasted_path(action: Action) -> Action {
    let Action::Paste(text) = &action else {
        return action;
    };
    // Only pay for a `stat` on text that could plausibly be a path; every
    // ordinary paste (a URL, a paragraph, a code snippet) stops here.
    if !tgt_core::state::composer::looks_like_path(text) {
        return action;
    }
    match crate::media_kind::existing_path(text.trim()) {
        Some(path) => Action::Paste(path.to_string_lossy().into_owned()),
        None => action,
    }
}

/// The resolved `Theme` plus the `AppState::theme_generation` it was
/// resolved from, so `draw_if_due` can tell a live theme switch (T60's
/// `state::palette::CommandId::ToggleTheme`, or any future writer of
/// `theme_generation`) apart from "nothing changed" without re-resolving on
/// every single frame.
struct LiveTheme {
    theme: Theme,
    generation: u64,
}

/// Re-resolves a theme by name mid-session. Always `main.rs::resolve_theme`
/// in the real binary — taken as a plain `fn` pointer (not a `crate::`
/// reference) rather than called directly, because `runtime_loop.rs` is
/// `#[path]`-included by several `crates/app/tests/*.rs` integration test
/// binaries that never pull in `main.rs`, and would fail to compile against
/// a hard dependency on `crate::resolve_theme`. None of those tests call
/// [`run`], so they never need to supply one that does anything, but this
/// module still has to typecheck standalone either way.
type ThemeResolver = fn(&str) -> Theme;

/// Measures the terminal's cell size in pixels. Always
/// `graphics::cell_size` in the real binary; a `fn` pointer for the same
/// reason [`ThemeResolver`] is one.
type CellMeasure = fn() -> Option<CellSize>;

/// How a frame is put on screen, as opposed to what is in it: everything
/// [`run`] needs for drawing and nothing it needs for running the app.
/// `main.rs` resolves all three from config and the startup probe.
pub struct Presentation {
    /// The theme resolved for the app's *starting* `theme_generation`.
    pub theme: Theme,
    /// How a later generation is resolved. See [`ThemeResolver`].
    pub resolve_theme: ThemeResolver,
    /// The terminal graphics protocol, or `None` for the design-language §4
    /// fallback (`main.rs::graphics_capability`).
    pub graphics: Option<Capability>,
    /// How the terminal's cell size is measured. See [`CellMeasure`].
    pub measure_cell: CellMeasure,
}

/// Runs `app` to completion. The caller owns terminal setup/teardown (raw
/// mode, alternate screen) around this call — `terminal` is only ever drawn
/// into here, never (re)configured.
///
/// After the first frame, `draw_if_due` is the sole place a theme is
/// resolved, always through `presentation.resolve_theme` — the same builtin
/// → user-file → `default_dark` chain `main.rs` uses at startup (it's the
/// same function, passed in by the caller — see [`ThemeResolver`]), so a
/// mid-session switch and the initial resolution can never disagree about
/// what a theme name means.
pub async fn run(
    app: App,
    terminal: &mut DefaultTerminal,
    runtime: Arc<dyn TdRuntime>,
    config: Arc<Mutex<Config>>,
    td_boot: TdBootParams,
    presentation: Presentation,
    restart: Option<RuntimeFactory>,
) -> eyre::Result<()> {
    let Presentation {
        theme,
        resolve_theme,
        graphics,
        measure_cell,
    } = presentation;
    let (term_events, event_reader_running) = spawn_terminal_event_reader();
    let mut core = Core::new(app, runtime, config, td_boot, term_events, graphics);
    if let Some(factory) = restart {
        core = core.with_restart(factory);
    }
    let mut live_theme = LiveTheme {
        generation: core.app().state().theme_generation,
        theme,
    };

    let mut gate = DrawGate::default();
    // `App::new` starts dirty so the first screen renders before any action
    // arrives.
    draw_if_due(
        &mut core,
        &mut live_theme,
        resolve_theme,
        measure_cell,
        terminal,
        &mut gate,
    )?;

    while core.step_until(gate.next_deadline()).await == Step::Continue {
        draw_if_due(
            &mut core,
            &mut live_theme,
            resolve_theme,
            measure_cell,
            terminal,
            &mut gate,
        )?;
    }

    event_reader_running.store(false, Ordering::Relaxed);

    // Deliberately after the loop rather than from inside it: the caller
    // still has to drop its `TerminalGuard` before anything prints, and
    // returning here is what lets it. Nothing is drawn on this path — the
    // frame on screen is whatever the last successful draw left, and it is
    // about to be replaced by the restored shell.
    match core.fatal.take() {
        Some(err) => Err(err.into()),
        None => Ok(()),
    }
}

/// Coalesces draws to at most one per [`DRAW_GATE`] without ever losing one.
///
/// The dirty flag is consumed on read, so it is accumulated here instead of
/// being tested together with the gate; a change that arrives while the gate
/// is shut stays pending and is drawn the moment it reopens. `next_deadline`
/// is what lets the loop wake itself for that, rather than waiting for an
/// unrelated input to come along.
#[derive(Debug, Default)]
pub struct DrawGate {
    last_draw: Option<Instant>,
    pending: bool,
}

impl DrawGate {
    fn note_dirty(&mut self, dirty: bool) {
        self.pending |= dirty;
    }

    fn should_draw(&self, now: Instant) -> bool {
        self.pending && self.gate_open(now)
    }

    fn gate_open(&self, now: Instant) -> bool {
        self.last_draw
            .is_none_or(|at| now.duration_since(at) >= DRAW_GATE)
    }

    fn mark_drawn(&mut self, now: Instant) {
        self.last_draw = Some(now);
        self.pending = false;
    }

    /// When the loop must wake to paint a frame it is holding back, if any.
    fn next_deadline(&self) -> Option<Instant> {
        match (self.pending, self.last_draw) {
            (true, Some(at)) => Some(at + DRAW_GATE),
            (true, None) => Some(Instant::now()),
            (false, _) => None,
        }
    }
}

fn draw_if_due(
    core: &mut Core,
    live_theme: &mut LiveTheme,
    resolve_theme: ThemeResolver,
    measure_cell: CellMeasure,
    terminal: &mut DefaultTerminal,
    gate: &mut DrawGate,
) -> io::Result<()> {
    // `take_dirty` ALWAYS clears the flag, so it has to be accumulated
    // rather than tested alongside the gate: `take_dirty() && gate_ready`
    // silently discards the change whenever the gate is shut. Human input
    // never arrives fast enough to notice, but TDLib update bursts do — a
    // chat opening and its first history page landing inside the same 16 ms
    // left the pane reading "no messages yet" until the next keypress.
    gate.note_dirty(core.take_dirty());
    if gate.should_draw(Instant::now()) {
        // Destructured so the draw closure borrows `app` (for its state)
        // and `render`/`last_hits` as the disjoint fields they are, rather
        // than needing both a `&core.app()` and a `&mut core.render_mut()`
        // live at once.
        let Core {
            app,
            render,
            last_hits,
            cell_size_stale,
            ..
        } = core;
        let state = app.state();

        // Before anything is laid out: inline images are encoded at a pixel
        // size derived from this, and the terminal decides how many cells
        // that covers by dividing by the same number. A stale one draws a
        // photo over more cells than the layout reserved for it. A terminal
        // that reports nothing leaves the last good measurement in place
        // rather than replacing it with a guess.
        if std::mem::take(cell_size_stale)
            && let Some(cell) = measure_cell()
        {
            render.set_cell_size(cell);
        }

        // A theme switch (currently only `state::palette`'s `ToggleTheme`)
        // bumps `theme_generation`; notice it here and re-resolve rather
        // than at the point of the bump, since only this call site knows
        // which `Theme` is currently on screen. `LayoutKey` already keys on
        // `theme_generation`, so the stale entries left behind by the old
        // generation would only ever miss forward and never render wrong —
        // clearing here just stops them from accumulating as dead weight.
        // (Placed images need no attention here: `theme_generation` is part
        // of what `RenderState::note_viewport` fingerprints, so the frame
        // this resolves for drops them itself.)
        if state.theme_generation != live_theme.generation {
            live_theme.theme = resolve_theme(&state.theme_name);
            live_theme.generation = state.theme_generation;
            render.cache.clear();
        }

        // Every frame replaces the hit map wholesale: it describes the frame
        // now on screen, and a click can only ever mean something against
        // the frame the user was looking at.
        terminal.draw(|f| *last_hits = tgt_ui::view(state, &live_theme.theme, f, render))?;

        // A frame that changed which inline images are placed leaves the
        // terminal holding pixels for the ones that went away: they live in
        // the terminal's own layer, and ratatui's diff will not rewrite the
        // cells it thinks are unchanged blanks underneath them (see
        // `tgt_ui::render::state`'s "Erasing, as opposed to forgetting").
        //
        // Drawing twice rather than deferring to the next frame, because
        // there may not be a next frame: the loop draws on change, so a
        // scroll that settles would leave the fragments on screen until the
        // user happened to do something else. The redraw cannot ask for a
        // third — the second pass finds every slot already placed, sweeps
        // nothing, and moves no viewport.
        if render.take_repaint_request() {
            repaint(terminal)?;
            terminal.draw(|f| *last_hits = tgt_ui::view(state, &live_theme.theme, f, render))?;
        }
        gate.mark_drawn(Instant::now());
    }
    Ok(())
}

/// Blanks the screen and makes the next `draw` a full repaint rather than a
/// diff, so that every cell is written again — including the ones a graphics
/// protocol placed pixels over, which is the whole point (see
/// `tgt_ui::render::state`'s "Erasing, as opposed to forgetting").
///
/// Deliberately *not* `Terminal::clear()`, which does exactly this but reads
/// the cursor position first — and reading it means writing `ESC[6n` and
/// waiting on stdin for the reply, on a process that already has
/// [`spawn_terminal_event_reader`]'s thread parked in crossterm's reader. The
/// two contend for crossterm's internal reader lock, with a two-second
/// timeout on the loser, and this runs on every frame where a placement
/// changed. The cursor position is worth nothing here anyway: the draw that
/// follows sets it.
///
/// The pair is what does it. Blanking the screen alone would leave ratatui
/// believing the old frame is still displayed and diffing away every cell of
/// the redraw; resetting the buffers alone would leave the terminal's
/// graphics layer untouched, which is the bug. `swap_buffers` resets the
/// inactive buffer and flips, so calling it once out of band leaves both
/// buffers blank — matching the screen that was just cleared.
fn repaint(terminal: &mut DefaultTerminal) -> io::Result<()> {
    terminal.backend_mut().clear()?;
    terminal.swap_buffers();
    Ok(())
}

/// Reads terminal events on a plain OS thread using crossterm's synchronous
/// `poll`/`read`, forwarding them over a channel the loop selects on. A
/// plain thread rather than `tokio::task::spawn_blocking`: this polls
/// forever (with a short timeout to notice shutdown), and parking an
/// unbounded loop on the blocking pool would make the runtime wait for it on
/// shutdown. A detached thread is simply killed when the process exits.
fn spawn_terminal_event_reader() -> (mpsc::Receiver<Event>, Arc<AtomicBool>) {
    let (tx, rx) = mpsc::channel::<Event>(32);
    let running = Arc::new(AtomicBool::new(true));
    let reader_running = Arc::clone(&running);

    std::thread::spawn(move || {
        while reader_running.load(Ordering::Relaxed) {
            match crossterm::event::poll(EVENT_POLL_TIMEOUT) {
                Ok(true) => match crossterm::event::read() {
                    Ok(event) => {
                        if tx.blocking_send(event).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                },
                Ok(false) => continue,
                Err(_) => break,
            }
        }
    });

    (rx, running)
}

#[cfg(test)]
mod draw_gate_tests {
    use super::{DRAW_GATE, DrawGate};
    use std::time::Instant;

    /// The regression: a change landing while the gate is shut used to be
    /// consumed and thrown away, leaving a chat reading "no messages yet"
    /// until an unrelated keypress repainted it.
    #[test]
    fn a_change_arriving_mid_gate_is_still_drawn_once_the_gate_reopens() {
        let mut gate = DrawGate::default();
        let start = Instant::now();

        gate.note_dirty(true);
        assert!(gate.should_draw(start), "first frame draws immediately");
        gate.mark_drawn(start);

        // Mid-gate burst: dirty, but not yet drawable.
        gate.note_dirty(true);
        assert!(!gate.should_draw(start + DRAW_GATE / 2));
        // ...and it survives to be drawn when the gate reopens.
        assert!(gate.should_draw(start + DRAW_GATE));
    }

    #[test]
    fn a_clean_frame_never_draws_and_asks_for_no_wakeup() {
        let mut gate = DrawGate::default();
        gate.note_dirty(false);
        assert!(!gate.should_draw(Instant::now()));
        assert!(gate.next_deadline().is_none());
    }

    #[test]
    fn a_held_frame_asks_to_be_woken_when_the_gate_reopens() {
        let mut gate = DrawGate::default();
        let start = Instant::now();
        gate.mark_drawn(start);
        gate.note_dirty(true);
        assert_eq!(gate.next_deadline(), Some(start + DRAW_GATE));
    }

    #[test]
    fn drawing_clears_the_pending_frame() {
        let mut gate = DrawGate::default();
        let start = Instant::now();
        gate.note_dirty(true);
        gate.mark_drawn(start);
        assert!(!gate.should_draw(start + DRAW_GATE * 2));
        assert!(gate.next_deadline().is_none());
    }
}

#[cfg(test)]
mod tests {
    use crossterm::event::KeyModifiers;
    use ratatui::layout::Rect;
    use tgt_core::model::hit::{HitTarget, ScrollArea};
    use tgt_core::model::ids::{ChatId, MessageId};

    use super::*;

    /// A sidebar row, a message row and both panes — the geometry a real
    /// frame publishes, small enough to reason about by hand.
    fn hit_map() -> HitMap {
        let mut hits = HitMap::new();
        hits.push_area(Rect::new(0, 0, 30, 20), ScrollArea::ChatList);
        hits.push_area(Rect::new(30, 0, 90, 20), ScrollArea::Conversation);
        hits.push(Rect::new(0, 2, 30, 1), HitTarget::ChatRow(ChatId(7)));
        hits.push(Rect::new(30, 5, 90, 1), HitTarget::Message(MessageId(3)));
        hits
    }

    fn mouse(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    #[test]
    fn a_button_press_becomes_a_click_on_whatever_it_landed_on() {
        let hits = hit_map();

        assert!(matches!(
            translate_mouse(&hits, mouse(MouseEventKind::Down(MouseButton::Left), 5, 2)),
            Some(Action::Click {
                target: HitTarget::ChatRow(ChatId(7)),
                button: ClickButton::Left,
            })
        ));
        assert!(matches!(
            translate_mouse(
                &hits,
                mouse(MouseEventKind::Down(MouseButton::Right), 40, 5)
            ),
            Some(Action::Click {
                target: HitTarget::Message(MessageId(3)),
                button: ClickButton::Right,
            })
        ));
    }

    #[test]
    fn a_wheel_step_scrolls_the_pane_under_the_pointer() {
        let hits = hit_map();

        assert!(matches!(
            translate_mouse(&hits, mouse(MouseEventKind::ScrollUp, 5, 10)),
            Some(Action::Scroll {
                area: ScrollArea::ChatList,
                up: true,
            })
        ));
        assert!(matches!(
            translate_mouse(&hits, mouse(MouseEventKind::ScrollDown, 40, 10)),
            Some(Action::Scroll {
                area: ScrollArea::Conversation,
                up: false,
            })
        ));
        // Over a chat row the wheel still scrolls the sidebar; the click
        // target sitting there does not shadow the pane.
        assert!(matches!(
            translate_mouse(&hits, mouse(MouseEventKind::ScrollDown, 5, 2)),
            Some(Action::Scroll {
                area: ScrollArea::ChatList,
                ..
            })
        ));
    }

    #[test]
    fn unresolved_coordinates_and_uninteresting_kinds_produce_nothing() {
        let hits = hit_map();

        // Below both panes: inside the frame, outside every region.
        assert!(
            translate_mouse(&hits, mouse(MouseEventKind::Down(MouseButton::Left), 5, 30)).is_none()
        );
        assert!(translate_mouse(&hits, mouse(MouseEventKind::ScrollUp, 5, 30)).is_none());
        // A press inside a pane but not on any target is not a click on the
        // pane itself.
        assert!(
            translate_mouse(&hits, mouse(MouseEventKind::Down(MouseButton::Left), 5, 10)).is_none()
        );

        // Kinds v1 has nothing to do with, all over a live target.
        for kind in [
            MouseEventKind::Up(MouseButton::Left),
            MouseEventKind::Down(MouseButton::Middle),
            MouseEventKind::Drag(MouseButton::Left),
            MouseEventKind::Moved,
        ] {
            assert!(
                translate_mouse(&hits, mouse(kind, 5, 2)).is_none(),
                "{kind:?} should not produce an action"
            );
        }

        // An empty map is what an overlay frame publishes: nothing resolves.
        assert!(
            translate_mouse(
                &HitMap::new(),
                mouse(MouseEventKind::Down(MouseButton::Left), 5, 2)
            )
            .is_none()
        );
    }
}
