//! Terminal graphics protocol probe (spec §8.3).
//!
//! Decides once at startup whether inline images can render at all, and if
//! so, which wire protocol to speak. This is the only place in the
//! workspace allowed to read these environment variables — `tgt-ui`'s
//! `render::image` module never inspects the environment or queries the
//! terminal itself; it receives the result of this probe as plain data
//! (see that module's docs for the boundary rationale).
//!
//! `main` runs the probe once at startup and logs the result. Handing it to
//! the draw path is what turns it into actual pixels, and that is still
//! open: see the `T55/polish` note in `tgt_ui::view::conversation`, which
//! renders every photo as the T37 placeholder card for now.

use std::env;

/// Terminal graphics protocol detected at startup.
///
/// Telemetry records this once per session under `term.graphics_protocol`
/// (schema values `kitty|iterm2|sixel|none`, see
/// `tgt_core::telemetry::schema::TERM_GRAPHICS_PROTOCOL`); [`telemetry_str`]
/// produces the matching string.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphicsProtocol {
    Kitty,
    Iterm2,
    Sixel,
    None,
}

/// Probes the real process environment via [`std::env::var`].
///
/// See [`probe_from`] for the exact detection rules; this just supplies
/// `std::env::var` as the lookup function.
pub fn probe() -> GraphicsProtocol {
    probe_from(|key| env::var(key).ok())
}

/// Env-injectable probe.
///
/// Takes a lookup function rather than reading `std::env` directly so the
/// detection rules are unit-testable with faked variables, without
/// mutating the real process environment (parallel `cargo test` runs would
/// otherwise stomp on each other's `TERM` / `TERM_PROGRAM`).
///
/// Rules, first match wins:
/// 1. **Kitty** — `TERM == "xterm-kitty"`, or `KITTY_WINDOW_ID` is set, or
///    `TERM_PROGRAM == "ghostty"` / `TERM` contains `"ghostty"` (Ghostty
///    speaks the Kitty graphics protocol). Checked before iTerm2 so a
///    terminal that happens to set both signals still gets the protocol it
///    actually implements.
/// 2. **iTerm2** — `TERM_PROGRAM == "iTerm.app"`, or `TERM_PROGRAM ==
///    "WezTerm"` (WezTerm speaks the iTerm2 protocol, not its own).
/// 3. **Sixel** — explicit opt-in only: `TGT_SIXEL == "1"`. Sixel support
///    has no reliable terminal-identification env var (many terminals that
///    support it don't announce themselves distinctly), so it is never
///    guessed, only requested.
/// 4. **None** otherwise — the T37 placeholder card is always available as
///    a fallback regardless of this outcome.
pub fn probe_from(vars: impl Fn(&str) -> Option<String>) -> GraphicsProtocol {
    let term = vars("TERM").unwrap_or_default();

    if term == "xterm-kitty" || vars("KITTY_WINDOW_ID").is_some() {
        return GraphicsProtocol::Kitty;
    }

    let term_program = vars("TERM_PROGRAM").unwrap_or_default();
    if term_program == "ghostty" || term.contains("ghostty") {
        return GraphicsProtocol::Kitty;
    }

    if term_program == "iTerm.app" || term_program == "WezTerm" {
        return GraphicsProtocol::Iterm2;
    }

    if vars("TGT_SIXEL").as_deref() == Some("1") {
        return GraphicsProtocol::Sixel;
    }

    GraphicsProtocol::None
}

/// Telemetry string for `term.graphics_protocol`, matching
/// `tgt_core::telemetry::schema`'s allowed values exactly.
pub fn telemetry_str(protocol: GraphicsProtocol) -> &'static str {
    match protocol {
        GraphicsProtocol::Kitty => "kitty",
        GraphicsProtocol::Iterm2 => "iterm2",
        GraphicsProtocol::Sixel => "sixel",
        GraphicsProtocol::None => "none",
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    fn probe_with(pairs: &[(&str, &str)]) -> GraphicsProtocol {
        let map: HashMap<&str, &str> = pairs.iter().copied().collect();
        probe_from(|key| map.get(key).map(|v| v.to_string()))
    }

    #[test]
    fn iterm_term_program_is_detected() {
        assert_eq!(
            probe_with(&[("TERM_PROGRAM", "iTerm.app")]),
            GraphicsProtocol::Iterm2
        );
    }

    #[test]
    fn wezterm_term_program_maps_to_iterm2() {
        assert_eq!(
            probe_with(&[("TERM_PROGRAM", "WezTerm")]),
            GraphicsProtocol::Iterm2
        );
    }

    #[test]
    fn xterm_kitty_term_is_detected() {
        assert_eq!(
            probe_with(&[("TERM", "xterm-kitty")]),
            GraphicsProtocol::Kitty
        );
    }

    #[test]
    fn kitty_window_id_env_var_is_detected() {
        assert_eq!(
            probe_with(&[("KITTY_WINDOW_ID", "1")]),
            GraphicsProtocol::Kitty
        );
    }

    #[test]
    fn ghostty_term_program_is_detected_as_kitty() {
        assert_eq!(
            probe_with(&[("TERM_PROGRAM", "ghostty")]),
            GraphicsProtocol::Kitty
        );
    }

    #[test]
    fn ghostty_term_substring_is_detected_as_kitty() {
        assert_eq!(
            probe_with(&[("TERM", "xterm-ghostty")]),
            GraphicsProtocol::Kitty
        );
    }

    #[test]
    fn kitty_takes_priority_over_iterm2_when_both_present() {
        assert_eq!(
            probe_with(&[("TERM", "xterm-kitty"), ("TERM_PROGRAM", "iTerm.app")]),
            GraphicsProtocol::Kitty
        );
    }

    #[test]
    fn tgt_sixel_opt_in_is_detected() {
        assert_eq!(probe_with(&[("TGT_SIXEL", "1")]), GraphicsProtocol::Sixel);
    }

    #[test]
    fn tgt_sixel_requires_exact_value_1() {
        assert_eq!(probe_with(&[("TGT_SIXEL", "true")]), GraphicsProtocol::None);
    }

    #[test]
    fn no_recognized_vars_is_none() {
        assert_eq!(probe_with(&[]), GraphicsProtocol::None);
    }

    #[test]
    fn telemetry_str_matches_schema_values() {
        assert_eq!(telemetry_str(GraphicsProtocol::Kitty), "kitty");
        assert_eq!(telemetry_str(GraphicsProtocol::Iterm2), "iterm2");
        assert_eq!(telemetry_str(GraphicsProtocol::Sixel), "sixel");
        assert_eq!(telemetry_str(GraphicsProtocol::None), "none");
    }

    #[test]
    fn probe_wraps_the_real_process_environment_without_panicking() {
        // Smoke test only: exercises the `std::env` wiring that
        // `probe_from` itself skips, without asserting a specific result
        // (this test's own process environment is unspecified).
        let _ = probe();
    }
}
