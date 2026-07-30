//! Mechanical `crossterm::event::Event` → `Action` translation. No routing
//! logic lives here; `App::update` decides what a `Key` means.

use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use tgt_core::action::Action;
use tgt_core::model::key::Key;

pub fn map_event(ev: Event) -> Option<Action> {
    match ev {
        Event::Key(key_event) => map_key_event(key_event).map(Action::Key),
        Event::Paste(text) => Some(Action::Paste(text)),
        Event::Resize(width, height) => Some(Action::Resize { width, height }),
        Event::FocusGained | Event::FocusLost | Event::Mouse(_) => None,
    }
}

fn map_key_event(ev: KeyEvent) -> Option<Key> {
    // Release events (only reported by terminals that opt into the Kitty
    // keyboard protocol) are not part of this key model. Repeat events map
    // the same as Press: on kitty-protocol terminals (Ghostty, kitty — both
    // in the spec's OSC 777 target set), autorepeat while a key is held
    // arrives as Repeat rather than fresh Press events, and hold-to-scroll
    // depends on it.
    if ev.kind == KeyEventKind::Release {
        return None;
    }

    let ctrl = ev.modifiers.contains(KeyModifiers::CONTROL);
    let alt = ev.modifiers.contains(KeyModifiers::ALT);

    match ev.code {
        KeyCode::Enter if alt => Some(Key::AltEnter),
        KeyCode::Enter => Some(Key::Enter),
        KeyCode::Esc => Some(Key::Esc),
        KeyCode::Tab => Some(Key::Tab),
        KeyCode::BackTab => Some(Key::BackTab),
        KeyCode::Up => Some(Key::Up),
        KeyCode::Down => Some(Key::Down),
        KeyCode::Left => Some(Key::Left),
        KeyCode::Right => Some(Key::Right),
        KeyCode::Backspace => Some(Key::Backspace),
        KeyCode::Delete => Some(Key::Delete),
        KeyCode::Home => Some(Key::Home),
        KeyCode::End => Some(Key::End),
        KeyCode::PageUp => Some(Key::PageUp),
        KeyCode::PageDown => Some(Key::PageDown),
        KeyCode::Char(c) if ctrl => Some(Key::Ctrl(c.to_ascii_lowercase())),
        KeyCode::Char(c) => Some(Key::Char(c)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyEventState;

    fn press(code: KeyCode, modifiers: KeyModifiers) -> Event {
        Event::Key(KeyEvent {
            code,
            modifiers,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        })
    }

    #[test]
    fn map_event_translates_alt_enter_and_ctrl_keys() {
        assert!(matches!(
            map_event(press(KeyCode::Enter, KeyModifiers::ALT)),
            Some(Action::Key(Key::AltEnter))
        ));
        assert!(matches!(
            map_event(press(KeyCode::Char('p'), KeyModifiers::CONTROL)),
            Some(Action::Key(Key::Ctrl('p')))
        ));
        assert!(matches!(
            map_event(press(KeyCode::Char('a'), KeyModifiers::NONE)),
            Some(Action::Key(Key::Char('a')))
        ));
        assert!(matches!(
            map_event(press(KeyCode::Esc, KeyModifiers::NONE)),
            Some(Action::Key(Key::Esc))
        ));
        assert!(matches!(
            map_event(Event::Resize(80, 24)),
            Some(Action::Resize {
                width: 80,
                height: 24
            })
        ));
    }

    #[test]
    fn release_is_ignored_but_repeat_maps_like_press() {
        let release = Event::Key(KeyEvent {
            code: KeyCode::Char('a'),
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Release,
            state: KeyEventState::NONE,
        });
        assert!(map_event(release).is_none());

        let repeat = Event::Key(KeyEvent {
            code: KeyCode::Up,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Repeat,
            state: KeyEventState::NONE,
        });
        assert!(matches!(map_event(repeat), Some(Action::Key(Key::Up))));
    }

    #[test]
    fn paste_and_mouse_and_focus() {
        assert!(matches!(
            map_event(Event::Paste("hi".to_string())),
            Some(Action::Paste(s)) if s == "hi"
        ));
        assert!(map_event(Event::FocusGained).is_none());
        assert!(map_event(Event::FocusLost).is_none());
    }
}
