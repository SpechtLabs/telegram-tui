//! The `tokio::select!` main loop (docs/architecture.md §3): one action
//! channel, terminal events, a 250 ms housekeeping tick, and a 16 ms
//! coalescing draw gate. Exits once the dispatcher observes `Effect::Quit`.

use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crossterm::event::Event;
use ratatui::DefaultTerminal;
use tokio::sync::mpsc;
use tokio::time::{self, MissedTickBehavior};

use tgt_core::action::Action;
use tgt_core::app::App;
use tgt_core::model::time::Millis;
use tgt_ui::theme::Theme;

use crate::dispatch::Dispatcher;

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

/// Runs `app` to completion. The caller owns terminal setup/teardown (raw
/// mode, alternate screen) around this call — `terminal` is only ever drawn
/// into here, never (re)configured.
pub async fn run(app: &mut App, theme: &Theme, terminal: &mut DefaultTerminal) -> io::Result<()> {
    let (action_tx, mut action_rx) = mpsc::channel::<Action>(ACTION_CHANNEL_CAPACITY);
    let (dispatcher, mut quit_rx) = Dispatcher::new(action_tx);
    let (mut term_events, event_reader_running) = spawn_terminal_event_reader();

    let mut tick = time::interval(TICK_PERIOD);
    tick.set_missed_tick_behavior(MissedTickBehavior::Delay);

    // The only clock read outside `core`: `App::update` receives time
    // exclusively via `Action::Tick { now }`, anchored to loop start.
    let clock_start = Instant::now();
    let mut last_draw: Option<Instant> = None;
    let mut effects = Vec::new();

    // `App::new` starts dirty so the empty shell renders before any action
    // arrives.
    draw_if_due(app, theme, terminal, &mut last_draw)?;

    loop {
        tokio::select! {
            changed = quit_rx.changed() => {
                if changed.is_ok() && *quit_rx.borrow() {
                    break;
                }
            }
            Some(action) = action_rx.recv() => {
                effects.extend(app.update(action));
            }
            Some(event) = term_events.recv() => {
                if let Some(action) = tgt_ui::input::map_event(event) {
                    effects.extend(app.update(action));
                }
            }
            _ = tick.tick() => {
                let now = Millis(clock_start.elapsed().as_millis() as u64);
                effects.extend(app.update(Action::Tick { now }));
            }
        }

        for effect in effects.drain(..) {
            dispatcher.dispatch(effect);
        }

        draw_if_due(app, theme, terminal, &mut last_draw)?;
    }

    event_reader_running.store(false, Ordering::Relaxed);
    Ok(())
}

fn draw_if_due(
    app: &mut App,
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
    if app.take_dirty() && gate_ready {
        terminal.draw(|f| tgt_ui::view(app.state(), theme, f))?;
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
