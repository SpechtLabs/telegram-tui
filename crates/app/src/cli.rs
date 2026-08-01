//! `clap` CLI surface (docs/architecture.md §2.3): `tgt`, `--no-telemetry`,
//! `--demo`, and the `tgt telemetry show|reset-id` subcommand. `--version`
//! comes for free from clap's derive.

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

    /// Run against a scripted, offline, in-memory chat history instead of a
    /// real Telegram account — for demos and screen recordings. Never reads
    /// or writes the real config, Keychain entry or TDLib database, and never
    /// opens a socket (see `crate::demo` module docs). Mutually exclusive
    /// with every subcommand: there is nothing to update or inspect about a
    /// session that never touches real credentials. Checked by
    /// [`Self::demo_conflicts_with_subcommand`] — `clap`'s declarative
    /// `conflicts_with` does not reach an optional `#[command(subcommand)]`
    /// field, which generates no argument id of its own to conflict against.
    #[arg(long)]
    pub demo: bool,

    #[command(subcommand)]
    pub command: Option<Command>,
}

impl Cli {
    /// Whether this invocation combines `--demo` with a subcommand — checked
    /// by `main.rs::dispatch_cli` before doing anything with either. See the
    /// doc comment on [`Self::demo`] for why this is a runtime check rather
    /// than a `clap` attribute.
    pub fn demo_conflicts_with_subcommand(&self) -> bool {
        self.demo && self.command.is_some()
    }
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

        /// Install the latest release even if it is the version already
        /// running.
        ///
        /// This is a repair: a tree with a partial extraction or a missing
        /// library is otherwise unfixable except by reinstalling by hand,
        /// and reporting "already the latest release" to someone whose
        /// install is broken is unhelpful.
        ///
        /// It changes only the decision to proceed. The download, both
        /// verification steps, the `sh -n` check, the swap, the probe and
        /// the rollback are the same ones the ordinary path runs — a
        /// `--force` that skipped any of them would stop being an exercise
        /// of the ordinary path, which is the other reason it exists.
        #[arg(long)]
        force: bool,
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
                require_signature: false,
                force: false
            })
        ));

        let cli = Cli::try_parse_from(["tgt", "update", "--require-signature"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Update {
                require_signature: true,
                force: false
            })
        ));
    }

    /// The two are independent, and the combination is the point: a verified
    /// download followed by a real swap is the only way to exercise the
    /// rename/probe/rollback sequence without waiting for a release newer
    /// than the one installed.
    #[test]
    fn force_and_require_signature_compose() {
        let cli = Cli::try_parse_from(["tgt", "update", "--force"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Update {
                require_signature: false,
                force: true
            })
        ));

        let cli = Cli::try_parse_from(["tgt", "update", "--force", "--require-signature"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Update {
                require_signature: true,
                force: true
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

    #[test]
    fn parses_demo_flag() {
        let cli = Cli::try_parse_from(["tgt", "--demo"]).unwrap();
        assert!(cli.demo);
        assert!(cli.command.is_none());
        assert!(!cli.demo_conflicts_with_subcommand());
    }

    #[test]
    fn demo_flag_combined_with_a_subcommand_is_flagged_as_a_conflict() {
        // clap happily parses this (see `Cli::demo`'s doc comment for why a
        // declarative `conflicts_with` can't catch it); `dispatch_cli` is
        // what turns the flag below into a rejection.
        let cli = Cli::try_parse_from(["tgt", "--demo", "telemetry", "show"]).unwrap();
        assert!(cli.demo_conflicts_with_subcommand());
    }
}
