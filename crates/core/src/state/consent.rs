//! First-run telemetry consent screen state and handlers. See
//! docs/architecture.md §4.6; spec §13.5.
//!
//! ## The screen's contract
//!
//! `App::new` puts `Screen::Consent` up whenever `Boot.consent_needed` is
//! set (unacknowledged on disk) — before login, before any TDLib traffic,
//! before an exporter is ever constructed (`tgt-app`'s `main.rs` gates
//! exporter construction on `config.consent_acknowledged`, which this screen
//! is the only writer of). While it is up it traps every key except the quit
//! binding, the same way the auth wizard traps every key while
//! `Screen::Auth` is up (see `state/auth.rs`'s `handle_key`) — there is
//! nothing behind it to leak keys to.
//!
//! Up/Down/Left/Right/Tab all toggle the two-item choice rather than only
//! moving in one direction: there is nothing to "arrive at" past either
//! end, so every directional key means the same thing here. `Enter` is the
//! sole way out: it sets `acknowledged`, moves the screen to `Auth`, and
//! asks `tgt-app` to persist the choice via
//! `ConfigPatch::ConsentAcknowledged`. Disabling also flips
//! `AppState::telemetry_mode` to `Off` immediately, in the same tick — the
//! config patch is what survives a restart, but the in-memory mode is what
//! `App::telemetry_for` and the dispatcher's `Effect::Telemetry` consult for
//! the rest of *this* session, so a Disable must not leave a stale `Vendor`/
//! `Custom` value able to mint events for the few frames before the save
//! round-trips.

use crate::app::{AppState, Screen};
use crate::effect::{ConfigPatch, Effect, TelemetryMode};
use crate::model::key::Key;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConsentChoice {
    Enable,
    Disable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConsentState {
    pub selected: ConsentChoice, // Enable preselected (spec §13.5)
    pub acknowledged: bool,
}

/// Claims every key while `Screen::Consent` is up; `None` once it isn't, the
/// same unclaimed-elsewhere contract every other screen/pane handler follows.
pub fn handle_key(app: &mut AppState, key: Key) -> Option<Vec<Effect>> {
    if app.screen != Screen::Consent {
        return None;
    }
    Some(route_consent_key(app, key))
}

fn route_consent_key(app: &mut AppState, key: Key) -> Vec<Effect> {
    match key {
        Key::Up | Key::Down | Key::Left | Key::Right | Key::Tab | Key::BackTab => {
            app.consent.selected = match app.consent.selected {
                ConsentChoice::Enable => ConsentChoice::Disable,
                ConsentChoice::Disable => ConsentChoice::Enable,
            };
            Vec::new()
        }
        Key::Enter => {
            let enabled = app.consent.selected == ConsentChoice::Enable;
            app.consent.acknowledged = true;
            app.screen = Screen::Auth;
            if !enabled {
                app.telemetry_mode = TelemetryMode::Off;
            }
            vec![Effect::SaveConfig(ConfigPatch::ConsentAcknowledged {
                enabled,
            })]
        }
        // Every other key is swallowed, not passed through: spec §13.5's
        // screen has nothing else to do with input, and letting an
        // unrecognized key fall through would be the one crack that leaks a
        // keystroke to whatever screen comes after this one.
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{App, Boot};
    use crate::model::key::KeyBindings;

    /// Mirrors `app::tests::boot_fixture` (not reachable here: that helper is
    /// `pub(super)` to `crate::app` alone), with `consent_needed` on.
    fn boot_fixture() -> Boot {
        Boot {
            theme_name: "dark".to_string(),
            bindings: KeyBindings::default(),
            layout_breakpoint_cols: 100,
            telemetry_mode: TelemetryMode::Vendor,
            telemetry_salt: [0u8; 32],
            consent_needed: true,
            has_credentials: false,
            width: 120,
            height: 40,
            auto_download_photos: true,
        }
    }

    fn consent_app() -> App {
        App::new(boot_fixture())
    }

    #[test]
    fn boots_to_consent_with_enable_preselected() {
        let app = consent_app();
        assert_eq!(app.state().screen, Screen::Consent);
        assert_eq!(app.state().consent.selected, ConsentChoice::Enable);
        assert!(!app.state().consent.acknowledged);
    }

    #[test]
    fn direction_keys_toggle_the_choice() {
        let mut app = consent_app();
        for key in [
            Key::Down,
            Key::Right,
            Key::Tab,
            Key::Up,
            Key::Left,
            Key::BackTab,
        ] {
            let before = app.state().consent.selected;
            app.update(crate::action::Action::Key(key));
            assert_ne!(
                app.state().consent.selected,
                before,
                "key {key:?} did not toggle"
            );
        }
    }

    #[test]
    fn enable_acknowledges_and_advances_to_auth() {
        let mut app = consent_app();
        let effects = app.update(crate::action::Action::Key(Key::Enter));
        assert_eq!(app.state().screen, Screen::Auth);
        assert!(app.state().consent.acknowledged);
        assert_eq!(app.state().telemetry_mode, TelemetryMode::Vendor);
        assert!(matches!(
            effects.as_slice(),
            [Effect::SaveConfig(ConfigPatch::ConsentAcknowledged {
                enabled: true
            })]
        ));
    }

    #[test]
    fn disable_sets_mode_off() {
        let mut app = consent_app();
        app.update(crate::action::Action::Key(Key::Down));
        assert_eq!(app.state().consent.selected, ConsentChoice::Disable);

        let effects = app.update(crate::action::Action::Key(Key::Enter));
        assert_eq!(app.state().screen, Screen::Auth);
        assert!(app.state().consent.acknowledged);
        assert_eq!(app.state().telemetry_mode, TelemetryMode::Off);
        assert!(matches!(
            effects.as_slice(),
            [Effect::SaveConfig(ConfigPatch::ConsentAcknowledged {
                enabled: false
            })]
        ));
    }

    #[test]
    fn consent_blocks_all_other_screens_until_acknowledged() {
        let mut app = consent_app();

        // Keys that would otherwise open the palette, move pane focus, or
        // reach the auth wizard never do anything but toggle the choice (or
        // nothing at all) while Consent is up.
        for key in [
            Key::Ctrl('p'),
            Key::Char('a'),
            Key::Char('q'),
            Key::Esc,
            Key::PageDown,
        ] {
            app.update(crate::action::Action::Key(key));
            assert_eq!(
                app.state().screen,
                Screen::Consent,
                "key {key:?} escaped the consent screen"
            );
        }

        // ctrl+c is still reserved above every screen, consent included.
        let bindings = KeyBindings::default();
        let effects = app.update(crate::action::Action::Key(bindings.quit));
        assert!(matches!(effects.as_slice(), [Effect::Quit]));
        assert_eq!(app.state().screen, Screen::Consent);

        let effects = app.update(crate::action::Action::Key(Key::Enter));
        assert!(matches!(effects.as_slice(), [Effect::SaveConfig(_)]));
        assert_eq!(app.state().screen, Screen::Auth);
    }
}
