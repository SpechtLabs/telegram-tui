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

use crossterm::event::Event;
use ratatui::DefaultTerminal;
use tokio::sync::{mpsc, watch};
use tokio::time::{self, MissedTickBehavior};

use tgt_core::action::Action;
use tgt_core::app::App;
use tgt_core::effect::Effect;
use tgt_core::model::time::Millis;
use tgt_core::td::runtime::TdRuntime;
use tgt_core::td::update::{AuthPhase, TdUpdate};
use tgt_ui::render::cache::LayoutCache;
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
}

/// One iteration's input, produced inside `select!` and handled outside it so
/// the handlers can borrow all of `self` (the select's futures each hold a
/// mutable borrow of one field).
enum Input {
    Quit,
    Action(Action),
    Term(Event),
    Td(TdUpdate),
    Tick(Millis),
}

/// The main loop minus the terminal. See the module docs.
pub struct Core {
    app: App,
    action_rx: mpsc::Receiver<Action>,
    term_events: mpsc::Receiver<Event>,
    td_updates: mpsc::Receiver<TdUpdate>,
    dispatcher: Dispatcher,
    quit_rx: watch::Receiver<bool>,
    tick: time::Interval,
    /// The only clock read outside `core`: `App::update` receives time
    /// exclusively via `Action::Tick { now }`, anchored to loop start.
    clock_start: Instant,
    effects: Vec<Effect>,
    /// T21's `LayoutCache` (architecture.md §4.9), threaded through `view`
    /// on every draw. Cleared wholesale on a terminal resize — the column
    /// width lives in `LayoutKey`, so a stale width's cached lines are wrong
    /// at the new width. Theme changes don't need a clear here:
    /// `theme_generation` is part of `LayoutKey` too, so a theme swap just
    /// misses forward without evicting anything explicitly.
    cache: LayoutCache,
}

impl Core {
    /// Takes the runtime's update receiver (once — the trait panics on a
    /// second call) and wires the dispatcher to the action channel.
    pub fn new(
        app: App,
        runtime: Arc<dyn TdRuntime>,
        config: Arc<Mutex<Config>>,
        td_boot: TdBootParams,
        term_events: mpsc::Receiver<Event>,
    ) -> Self {
        let (action_tx, action_rx) = mpsc::channel::<Action>(ACTION_CHANNEL_CAPACITY);
        let td_updates = runtime.updates();
        let (dispatcher, quit_rx) = Dispatcher::new(action_tx, runtime, config, td_boot);

        let mut tick = time::interval(TICK_PERIOD);
        tick.set_missed_tick_behavior(MissedTickBehavior::Delay);

        Core {
            app,
            action_rx,
            term_events,
            td_updates,
            dispatcher,
            quit_rx,
            tick,
            clock_start: Instant::now(),
            effects: Vec::new(),
            cache: LayoutCache::new(),
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
    pub async fn step(&mut self) -> Step {
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
                ..
            } = self;

            tokio::select! {
                // The dispatcher only ever sends `true`, so any change is
                // the quit signal; a closed channel means the dispatcher is
                // gone, which is also the end of the loop.
                _ = quit_rx.changed() => Input::Quit,
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
            Input::Action(action) => self.apply(action),
            Input::Term(event) => {
                // A resize invalidates every cached layout: they're wrapped
                // at the old column width. `LayoutKey::theme_generation`
                // handles theme swaps on its own, so only width needs this.
                if let Event::Resize(_, _) = event {
                    self.cache.clear();
                }
                if let Some(action) = tgt_ui::input::map_event(event) {
                    self.apply(resolve_pasted_path(action));
                }
            }
            Input::Td(update) => self.apply_td(update),
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
    fn apply_td(&mut self, update: TdUpdate) {
        let needs_parameters = matches!(update, TdUpdate::Auth(AuthPhase::WaitTdlibParameters));
        self.apply(Action::Td(update));
        if needs_parameters {
            self.dispatcher.request_tdlib_parameters();
        }
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

/// Runs `app` to completion. The caller owns terminal setup/teardown (raw
/// mode, alternate screen) around this call — `terminal` is only ever drawn
/// into here, never (re)configured.
pub async fn run(
    app: App,
    theme: &Theme,
    terminal: &mut DefaultTerminal,
    runtime: Arc<dyn TdRuntime>,
    config: Arc<Mutex<Config>>,
    td_boot: TdBootParams,
) -> io::Result<()> {
    let (term_events, event_reader_running) = spawn_terminal_event_reader();
    let mut core = Core::new(app, runtime, config, td_boot, term_events);

    let mut last_draw: Option<Instant> = None;
    // `App::new` starts dirty so the first screen renders before any action
    // arrives.
    draw_if_due(&mut core, theme, terminal, &mut last_draw)?;

    while core.step().await == Step::Continue {
        draw_if_due(&mut core, theme, terminal, &mut last_draw)?;
    }

    event_reader_running.store(false, Ordering::Relaxed);
    Ok(())
}

fn draw_if_due(
    core: &mut Core,
    theme: &Theme,
    terminal: &mut DefaultTerminal,
    last_draw: &mut Option<Instant>,
) -> io::Result<()> {
    let gate_ready = last_draw.is_none_or(|at| at.elapsed() >= DRAW_GATE);
    // `take_dirty` runs first to match the shape in architecture.md §3
    // exactly — it always clears the flag. The 16 ms gate is far shorter
    // than any human input cadence, so a change landing while it's still
    // closed simply waits for the next dirtying action rather than for the
    // gate alone; this app's inputs never come close to that rate.
    if core.take_dirty() && gate_ready {
        // Destructured so the draw closure borrows `app` (for its state)
        // and `cache` as the disjoint fields they are, rather than needing
        // both a `&core.app()` and a `&mut core.cache_mut()` live at once.
        let Core { app, cache, .. } = core;
        let state = app.state();
        terminal.draw(|f| tgt_ui::view(state, theme, f, cache))?;
        *last_draw = Some(Instant::now());
    }
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
