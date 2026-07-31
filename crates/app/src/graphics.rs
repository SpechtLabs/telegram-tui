//! Terminal graphics protocol probe (spec §8.3).
//!
//! Decides once at startup whether inline images can render at all, and if
//! so, which wire protocol to speak. This is the only place in the
//! workspace allowed to read these environment variables — `tgt-ui`'s
//! `render::image` module never inspects the environment or queries the
//! terminal itself; it receives the result of this probe as plain data
//! (see that module's docs for the boundary rationale).
//!
//! `main` runs the probe once at startup, logs the result, maps it into
//! `tgt_ui::render::image::Capability` and hands it to the draw path in the
//! `RenderState` it builds (architecture §4.9.1). A `None` here — for any of
//! the reasons below — is what makes every photo render as its one-line card
//! (docs/design-language.md §4) instead of a picture.

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
/// 0. **Multiplexer** — if `TMUX` is set, the answer is [`GraphicsProtocol::None`]
///    regardless of everything below, unless `TGT_FORCE_GRAPHICS` is exactly
///    `"1"`. tmux does not forward kitty or iTerm2 graphics escapes unless
///    it is built and configured with passthrough (`allow-passthrough`), and
///    the sequences it does not forward do not vanish — they land in the
///    pane as garbage and corrupt the display. The env vars the rules below
///    read are worse than useless inside tmux, too: `KITTY_WINDOW_ID` and
///    `TERM_PROGRAM` are inherited from the terminal tmux was *started*
///    from, which may not even be attached any more. Declining is the only
///    answer that is right in every one of those cases; users who have set
///    passthrough up can say so with `TGT_FORCE_GRAPHICS=1`.
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
    if vars("TMUX").is_some() && vars("TGT_FORCE_GRAPHICS").as_deref() != Some("1") {
        return GraphicsProtocol::None;
    }

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

    /// Inside tmux the inherited `KITTY_WINDOW_ID` still says "kitty", and
    /// believing it is what put escape-sequence garbage on the user's
    /// screen. Nothing below rule 0 gets a say.
    #[test]
    fn tmux_without_passthrough_reports_no_capability() {
        assert_eq!(
            probe_with(&[
                ("TMUX", "/tmp/tmux-501/default,1234,0"),
                ("TERM", "xterm-kitty")
            ]),
            GraphicsProtocol::None
        );
        assert_eq!(
            probe_with(&[
                ("TMUX", "/tmp/tmux-501/default,1234,0"),
                ("KITTY_WINDOW_ID", "1")
            ]),
            GraphicsProtocol::None
        );
        assert_eq!(
            probe_with(&[
                ("TMUX", "/tmp/tmux-501/default,1234,0"),
                ("TERM_PROGRAM", "iTerm.app")
            ]),
            GraphicsProtocol::None
        );
        // Even the explicit sixel opt-in: it is an opt-in to a protocol, not
        // to tmux forwarding it.
        assert_eq!(
            probe_with(&[("TMUX", "/tmp/tmux-501/default,1234,0"), ("TGT_SIXEL", "1")]),
            GraphicsProtocol::None
        );
    }

    /// `TGT_FORCE_GRAPHICS=1` is how someone who has set tmux's
    /// `allow-passthrough` up says so. It re-enables detection; it does not
    /// itself claim a protocol.
    #[test]
    fn tgt_force_graphics_overrides_the_tmux_veto() {
        assert_eq!(
            probe_with(&[
                ("TMUX", "/tmp/tmux-501/default,1234,0"),
                ("TERM", "xterm-kitty"),
                ("TGT_FORCE_GRAPHICS", "1")
            ]),
            GraphicsProtocol::Kitty
        );
        // Forcing inside tmux with nothing to detect is still nothing.
        assert_eq!(
            probe_with(&[
                ("TMUX", "/tmp/tmux-501/default,1234,0"),
                ("TGT_FORCE_GRAPHICS", "1")
            ]),
            GraphicsProtocol::None
        );
        // Same exact-value discipline as TGT_SIXEL: "true" is not "1".
        assert_eq!(
            probe_with(&[
                ("TMUX", "/tmp/tmux-501/default,1234,0"),
                ("TERM", "xterm-kitty"),
                ("TGT_FORCE_GRAPHICS", "true")
            ]),
            GraphicsProtocol::None
        );
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
