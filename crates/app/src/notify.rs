//! Terminal alert emission for `Effect::Alert` (spec §6.4, architecture
//! §2.3, §4.4).
//!
//! `alert` takes no content parameters. The body is a compile-time byte
//! constant with no interpolation site, so message text, sender names, and
//! chat titles cannot reach the escape sequence even by accident — the
//! same structural guarantee `Effect::Alert` gives at the `core` boundary
//! (it carries no payload at all) holds here too.
//!
//! `dispatch.rs`'s `Effect::Alert` arm is the only caller; see that module's
//! docs for why its write to stdout is safe while the TUI is up.

use std::io::{self, Write};

/// `OSC 777 ; notify ; <title> ; <body>`, terminated with ST (`ESC \`).
/// `tgt` is the notification title; the body is deliberately generic.
const OSC777_ALERT: &[u8] = b"\x1b]777;notify;tgt;New message\x1b\\";
/// Fallback for terminals that don't understand OSC 777: a plain bell.
const BEL: &[u8] = b"\x07";

/// Emits a terminal alert: OSC 777 where supported, `BEL` otherwise.
pub fn alert(out: &mut impl Write, supports_osc777: bool) -> io::Result<()> {
    if supports_osc777 {
        out.write_all(OSC777_ALERT)
    } else {
        out.write_all(BEL)
    }
}

/// Heuristic: does the current terminal understand OSC 777? Reads
/// `TERM_PROGRAM`, matching the terminals known to render it (WezTerm,
/// kitty, Ghostty). Anything else — including an unset `TERM_PROGRAM`,
/// e.g. inside plain tmux without passthrough — falls back to `BEL`.
pub fn supports_osc777() -> bool {
    supports_osc777_for(std::env::var("TERM_PROGRAM").ok().as_deref())
}

/// The env-injectable half of the heuristic, kept separate so tests don't
/// have to mutate process-global `TERM_PROGRAM` state.
fn supports_osc777_for(term_program: Option<&str>) -> bool {
    match term_program {
        Some(p) => {
            let p = p.to_ascii_lowercase();
            p.contains("wezterm") || p.contains("kitty") || p.contains("ghostty")
        }
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notify_osc_body_is_generic_constant() {
        // Byte-exact: no interpolation site exists in `alert`'s signature
        // (it takes `out` and a bool, nothing content-shaped), so this is
        // the only sequence it could ever emit on the OSC 777 path.
        let mut buf = Vec::new();
        alert(&mut buf, true).unwrap();
        assert_eq!(buf, b"\x1b]777;notify;tgt;New message\x1b\\".to_vec());
        assert_eq!(buf, OSC777_ALERT.to_vec());
    }

    #[test]
    fn notify_falls_back_to_bel_when_unsupported() {
        let mut buf = Vec::new();
        alert(&mut buf, false).unwrap();
        assert_eq!(buf, b"\x07".to_vec());
    }

    #[test]
    fn supports_osc777_recognizes_known_terminals() {
        assert!(supports_osc777_for(Some("WezTerm")));
        assert!(supports_osc777_for(Some("kitty")));
        assert!(supports_osc777_for(Some("ghostty")));
        // Case-insensitive.
        assert!(supports_osc777_for(Some("GHOSTTY")));
    }

    #[test]
    fn supports_osc777_rejects_unknown_or_missing_terminals() {
        assert!(!supports_osc777_for(Some("iTerm.app")));
        assert!(!supports_osc777_for(Some("Apple_Terminal")));
        assert!(!supports_osc777_for(Some("tmux")));
        assert!(!supports_osc777_for(None));
    }
}
