//! Terminal-agnostic key model. See docs/architecture.md §4.1.

use serde::{Deserialize, Serialize};

/// Terminal-agnostic key. `tgt-ui` converts crossterm events into this;
/// all routing happens inside `update()` against the focus stack, so focus
/// transitions are unit-testable without a terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Key {
    Char(char),
    Enter,
    AltEnter,
    Esc,
    Tab,
    BackTab,
    Up,
    Down,
    Left,
    Right,
    Backspace,
    Delete,
    Home,
    End,
    PageUp,
    PageDown,
    Ctrl(char), // Ctrl('c'), Ctrl('p'), …
}

/// Rebindable global keys, parsed from config ("ctrl+p" → Key::Ctrl('p')).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyBindings {
    pub palette: Key,
    pub help: Key,
    pub quit: Key,
}

impl Default for KeyBindings {
    fn default() -> Self {
        KeyBindings {
            palette: Key::Ctrl('p'),
            help: Key::Char('?'),
            quit: Key::Ctrl('c'),
        }
    }
}
