//! `clap` CLI surface (docs/architecture.md §2.3): `tgt`, `--no-telemetry`,
//! and the `tgt telemetry show|reset-id` subcommand. `--version` comes for
//! free from clap's derive.

use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(
    name = "tgt",
    version,
    about = "A keyboard-driven terminal Telegram client"
)]
pub struct Cli {
    /// Disable telemetry for this run, overriding config and environment.
    #[arg(long)]
    pub no_telemetry: bool,

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Inspect or reset telemetry settings.
    Telemetry {
        #[command(subcommand)]
        action: TelemetryAction,
    },
    /// Replace this install with the latest published release.
    Update {
        /// Refuse to install unless the release's signature verifies against
        /// this project's release workflow.
        ///
        /// Off by default because it needs `cosign` on your PATH, and a
        /// check that usually cannot run is not a check. Without it, the
        /// update reports exactly what it did verify — never implying a
        /// signature was checked when none was.
        #[arg(long)]
        require_signature: bool,
    },
}

#[derive(Debug, Subcommand)]
pub enum TelemetryAction {
    /// Print exactly what a session would send.
    Show,
    /// Regenerate the install id and HMAC salt.
    ResetId,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_update_and_its_signature_flag() {
        let cli = Cli::try_parse_from(["tgt", "update"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Update {
                require_signature: false
            })
        ));

        let cli = Cli::try_parse_from(["tgt", "update", "--require-signature"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Update {
                require_signature: true
            })
        ));
    }

    #[test]
    fn parses_telemetry_show_subcommand() {
        let cli = Cli::try_parse_from(["tgt", "telemetry", "show"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Telemetry {
                action: TelemetryAction::Show
            })
        ));
    }

    #[test]
    fn parses_telemetry_reset_id_subcommand() {
        let cli = Cli::try_parse_from(["tgt", "telemetry", "reset-id"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Telemetry {
                action: TelemetryAction::ResetId
            })
        ));
    }

    #[test]
    fn parses_no_telemetry_flag_with_no_subcommand() {
        let cli = Cli::try_parse_from(["tgt", "--no-telemetry"]).unwrap();
        assert!(cli.no_telemetry);
        assert!(cli.command.is_none());
    }

    #[test]
    fn bare_invocation_has_no_subcommand_and_telemetry_enabled() {
        let cli = Cli::try_parse_from(["tgt"]).unwrap();
        assert!(!cli.no_telemetry);
        assert!(cli.command.is_none());
    }
}
