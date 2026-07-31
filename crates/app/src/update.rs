//! `tgt update`: replace this install with the latest published release.
//!
//! See docs/architecture.md §9.4. Runs without entering the TUI, like
//! `tgt telemetry show` — nothing may reach stdout while the alternate
//! screen is up, and this command is all stdout.
//!
//! # This module does not perform the swap
//!
//! Downloading, verifying and deciding *whether* to update are here. The
//! actual replacement — guard the root, stage, rename, probe, roll back,
//! symlink — is [`scripts/install.sh`]'s `swap_tree`, invoked through its
//! `--swap-from` entry point.
//!
//! That split is deliberate and the reasoning is worth keeping, because the
//! obvious design is to reimplement it here. The curl installer already
//! performs exactly that procedure and has been exercised against real
//! releases. A second implementation in Rust would drift from it, and the
//! drift would surface only when a rollback was needed — which is precisely
//! when nobody is watching and when the user has no working binary to ask.
//! Sharing it means every `curl | sh` install exercises the path this
//! command depends on.
//!
//! The boundary sits at verify-versus-swap rather than at Rust-versus-shell,
//! and that is what makes [`Verification::require_signature`] possible.
//! Handing the whole job to the script would mean either teaching it cosign
//! — which it deliberately does not do, since almost nobody has cosign
//! installed and that is the reason this command exists — or verifying a
//! signature on bytes the script then discards and re-downloads. The second
//! is not an inelegance; it is a check that proves nothing about what ends
//! up installed.
//!
//! It also costs nothing: `reqwest` is already a direct dependency for the
//! OTLP exporter, and leaving extraction to `tar` means this adds no new
//! crates beyond `sha2`, which `tgt-core` already resolves.
//!
//! # Runtime dependencies
//!
//! `sh` and `tar` must be present. Both are universal on macOS and Linux,
//! and no Windows artifact is published, so there is no platform gap — but
//! it is a real dependency and is named here rather than left to be
//! discovered. `cosign` is optional; see [`Verification`].
//!
//! # The script that performs the swap is the *new* one
//!
//! `package.sh` ships `install.sh` inside the tarball, and the copy that
//! runs is the one just extracted, not the one already installed. A release
//! therefore brings its own installer and a layout change applies itself.
//! Because the script rides inside the tarball, `--require-signature`
//! transitively covers the code doing the replacing.
//!
//! The cost of that is real: a broken *new* script breaks the rollback meant
//! to save the user. `sh -n` before invoking removes the whole "does not
//! parse" class for one syscall, and a non-zero exit is followed by a
//! coherence check rather than a bare propagation — see [`hand_off`]. What
//! remains is a release that ships a subtly broken installer, which is a
//! release-process failure rather than an update-mechanism one.

use std::path::{Path, PathBuf};
use std::process::Command;

use color_eyre::eyre;

/// The repository releases are published from.
const REPO: &str = "SpechtLabs/telegram-tui";

/// Keyless signing identity for release artifacts.
///
/// **This is a literal path and it is load-bearing.** Renaming
/// `.github/workflows/release.yaml` invalidates every signature check made
/// by a client built before the rename, and the failure surfaces as
/// "signature verification failed" on a perfectly good release — which reads
/// as tampering. That file carries a matching warning.
///
/// It ends in `@refs/heads/main`, not the tag, and that is not a mistake.
/// The release job checks out the tag, but GitHub's OIDC token asserts the
/// ref the *run* was triggered on, and release-please triggers it by pushing
/// to main. Verified by decoding the certificate in a published bundle:
/// deriving the identity from the tag — which looks obviously right — is
/// rejected by every real release.
const SIGNING_IDENTITY: &str = concat!(
    "https://github.com/SpechtLabs/telegram-tui",
    "/.github/workflows/release.yaml@refs/heads/main"
);

/// The issuer half of the pin. Without both, `cosign verify-blob` confirms
/// that *somebody* signed the blob rather than who, which is no check at all.
const OIDC_ISSUER: &str = "https://token.actions.githubusercontent.com";

/// Where this install lives, and whether it may be replaced in place.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Install {
    /// A private `bin/` + `lib/` tree this command owns and may swap.
    Private { root: PathBuf },
    /// Installed by Homebrew, which owns the prefix and tracks it in a
    /// manifest an in-place overwrite would desynchronise.
    Homebrew,
    /// Something else — a legacy shared-prefix install, or a tree that
    /// cannot be identified.
    Foreign { root: PathBuf },
}

/// The target triple this binary was built for, assembled from the two
/// constants the compiler exposes. Only the four published combinations can
/// occur in a released build; anything else means someone built for a target
/// with no artifact, and the download will fail with a message saying so.
pub fn target_triple() -> String {
    let arch = std::env::consts::ARCH;
    let os = match std::env::consts::OS {
        "macos" => "apple-darwin",
        "linux" => "unknown-linux-gnu",
        other => other,
    };
    format!("{arch}-{os}")
}

/// Resolves the install from the running executable.
///
/// `current_exe` resolves symlinks on both supported platforms, so a
/// `~/.local/bin/tgt` symlink lands on the real tree — which is what makes
/// the checks below meaningful rather than a test of where the user's PATH
/// entry happens to point.
pub fn resolve() -> eyre::Result<Install> {
    let exe = std::env::current_exe()?;
    // …/<root>/bin/tgt → <root>
    let Some(root) = exe.parent().and_then(Path::parent) else {
        return Ok(Install::Foreign { root: exe });
    };

    // Homebrew installs the tree under libexec inside a Cellar and symlinks
    // the binary; the Cellar component is the reliable signal.
    if root.components().any(|c| c.as_os_str() == "Cellar") {
        return Ok(Install::Homebrew);
    }

    if is_private_tree(root) {
        Ok(Install::Private {
            root: root.to_path_buf(),
        })
    } else {
        Ok(Install::Foreign {
            root: root.to_path_buf(),
        })
    }
}

/// Positive evidence that `root` is a tgt tree, mirroring `install.sh`'s
/// `assert_ours`. The marker is the certain answer; `bin/tgt` beside a
/// `lib/` is the fallback for installs predating it.
///
/// Deliberately not "does this directory look plausible": the swap renames
/// and eventually deletes whatever is here, so the test has to be for
/// evidence rather than for the absence of counter-evidence. A legacy
/// shared-prefix install (`~/.local` with `bin/` and `lib/` among other
/// things) fails it, which is the point — renaming that would move the
/// user's entire user-local tree.
fn is_private_tree(root: &Path) -> bool {
    if root.join(".tgt-install").is_file() {
        return true;
    }
    if !root.join("bin/tgt").is_file() || !root.join("lib").is_dir() {
        return false;
    }
    // No marker: accept only a directory that holds nothing but the two we
    // ship, so a shared prefix with `share/`, `state/` and friends is
    // refused. A fresh `~/.local` holding only `bin` and `lib` would pass
    // this alone, which is why the name is checked too.
    let Ok(entries) = std::fs::read_dir(root) else {
        return false;
    };
    let only_ours = entries.flatten().all(|e| {
        matches!(
            e.file_name().to_str(),
            Some("bin") | Some("lib") | Some("install.sh") | Some(".tgt-install")
        )
    });
    only_ours && root.file_name().and_then(|n| n.to_str()) == Some("tgt")
}

/// What was actually checked about the downloaded tarball, so the report can
/// say so rather than implying more.
///
/// The two are not equivalent and the copy must not treat them as such. A
/// SHA-256 match against `SHA256SUMS` proves the download was not corrupted
/// in transit; it proves nothing about tampering, because the sums file
/// arrives from the same host over the same TLS session as the tarball, and
/// anyone able to serve one can serve the other. The cosign signature, with
/// both identity and issuer pinned, is the only check that says something
/// TLS does not.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Verification {
    pub checksum: bool,
    pub signature: bool,
}

impl Verification {
    /// One line stating exactly what ran, for the user to read before
    /// deciding whether to trust the thing about to overwrite their client.
    pub fn describe(self) -> &'static str {
        match (self.signature, self.checksum) {
            (true, true) => {
                "signature verified (pinned to this repo's release workflow), checksum ok"
            }
            (true, false) => {
                "signature verified (pinned to this repo's release workflow); no SHA256SUMS published"
            }
            (false, true) => "checksum ok — corruption check only, no signature was verified",
            (false, false) => {
                "NOT VERIFIED — this release published no SHA256SUMS and cosign was not available"
            }
        }
    }
}

/// Verifies `tarball`, returning what was actually checked.
///
/// `cosign` is used when it is on `PATH` and the release published a bundle.
/// It is not required by default: hard-requiring it would make this command
/// fail for almost everyone, and a check that usually cannot run is not a
/// security feature but an unused path that rots. `require_signature` turns
/// it into a hard requirement for anyone who wants the guarantee.
///
/// There is deliberately no unpinned fallback. `cosign verify-blob` with
/// only `--bundle` confirms a signature exists and is internally consistent,
/// not who made it; reporting that as "verified" would be worse than
/// reporting nothing.
pub fn verify(
    tarball: &Path,
    bundle: Option<&Path>,
    expected_sha256: Option<&str>,
    require_signature: bool,
) -> Result<Verification, human_errors::Error> {
    let mut result = Verification::default();

    if let Some(expected) = expected_sha256 {
        let actual = sha256_hex(tarball).map_err(|err| {
            human_errors::system(
                format!("We couldn't read the download to check it: {err}"),
                &["Try again; if it persists, report it."],
            )
        })?;
        if actual != expected {
            return Err(human_errors::system(
                format!(
                    "The download does not match its published checksum.\n  expected {expected}\n  actual   {actual}"
                ),
                &[
                    "Try again — this is usually a truncated download.",
                    "If it keeps happening, report it rather than installing.",
                ],
            ));
        }
        result.checksum = true;
    }

    let cosign_available = Command::new("cosign")
        .arg("version")
        .output()
        .is_ok_and(|o| o.status.success());

    if let Some(bundle) = bundle.filter(|_| cosign_available) {
        let output = Command::new("cosign")
            .args(["verify-blob", "--bundle"])
            .arg(bundle)
            .args(["--certificate-identity", SIGNING_IDENTITY])
            .args(["--certificate-oidc-issuer", OIDC_ISSUER])
            .arg(tarball)
            .output()
            .map_err(|err| {
                human_errors::system(
                    format!("We couldn't run cosign: {err}"),
                    &["Install cosign, or run without --require-signature."],
                )
            })?;
        if !output.status.success() {
            return Err(human_errors::system(
                format!(
                    "The release's signature did not verify against this project's release workflow.\n{}",
                    String::from_utf8_lossy(&output.stderr).trim()
                ),
                &[
                    "Do not install this download.",
                    "Report it — a signature that fails this check is not merely a bad download.",
                ],
            ));
        }
        result.signature = true;
    }

    if require_signature && !result.signature {
        let why = if bundle.is_none() {
            "this release published no signature bundle"
        } else {
            "cosign is not installed"
        };
        return Err(human_errors::user(
            format!("--require-signature was given, but {why}."),
            &[
                "Install cosign (https://github.com/sigstore/cosign) and try again.",
                "Or drop --require-signature to install with only the checks that are available.",
            ],
        ));
    }

    Ok(result)
}

fn sha256_hex(path: &Path) -> std::io::Result<String> {
    use sha2::{Digest, Sha256};
    let bytes = std::fs::read(path)?;
    let digest = Sha256::digest(&bytes);
    Ok(digest.iter().map(|b| format!("{b:02x}")).collect())
}

/// Hands the swap to the freshly extracted `install.sh`.
///
/// `sh -n` first: a new release whose installer does not parse would
/// otherwise fail *after* renaming, with a rollback path that is itself
/// broken, which strands the user with no working install and no automatic
/// recovery. One syscall removes that whole class.
///
/// On a non-zero exit this does more than propagate the status, because the
/// user cannot ask a binary that no longer starts. It checks whether the
/// tree is coherent and, if it is not, names the staging and `.old` paths
/// that actually exist so recovery by hand is possible.
pub fn hand_off(extracted_tree: &Path, root: &Path) -> Result<(), human_errors::Error> {
    let script = extracted_tree.join("install.sh");
    if !script.is_file() {
        return Err(human_errors::system(
            format!(
                "The downloaded release has no installer at {}.",
                script.display()
            ),
            &["Report this. Install by hand from the release page in the meantime."],
        ));
    }

    let parses = Command::new("sh")
        .arg("-n")
        .arg(&script)
        .status()
        .map_err(|err| {
            human_errors::system(
                format!("We couldn't run sh to check the new installer: {err}"),
                &["`sh` is required to apply an update."],
            )
        })?;
    if !parses.success() {
        return Err(human_errors::system(
            "The installer shipped in this release does not parse; refusing to run it.".to_string(),
            &["Nothing has been changed. Report this and install by hand from the release page."],
        ));
    }

    let status = Command::new("sh")
        .arg(&script)
        .arg("--swap-from")
        .arg(extracted_tree)
        .env("TGT_INSTALL_ROOT", root)
        .status()
        .map_err(|err| {
            human_errors::system(
                format!("We couldn't run the installer: {err}"),
                &["`sh` is required to apply an update."],
            )
        })?;

    if status.success() {
        return Ok(());
    }
    Err(recovery_advice(root))
}

/// Builds the "here is how to fix this by hand" error for a failed swap.
///
/// Separated so it can be tested without staging a real failure: what
/// matters is that it names paths that exist, and it is only reachable at
/// the moment nobody can ask the binary anything.
fn recovery_advice(root: &Path) -> human_errors::Error {
    let healthy = root.join("bin/tgt").is_file();
    let leftovers = siblings_matching(root);

    if healthy && leftovers.is_empty() {
        return human_errors::system(
            "The update did not complete, but your install looks intact.".to_string(),
            &["Run `tgt --version` to confirm, then try again."],
        );
    }

    let mut message = format!(
        "The update did not complete and {} may be inconsistent.",
        root.display()
    );
    if !leftovers.is_empty() {
        message.push_str("\n\nThese were left behind:");
        for path in &leftovers {
            message.push_str(&format!("\n  {}", path.display()));
        }
        message.push_str("\n\nA path ending .old-<number> is your previous install: renaming it back\nover the tree above restores it. A path ending .new-<number> is an\nincomplete download and is safe to delete.");
    }
    human_errors::system(
        message,
        &["Reinstall with: curl -sSL https://tgt.specht-labs.de/install.sh | sh"],
    )
}

/// The `.new-<pid>` / `.old-<pid>` siblings `install.sh` creates. Enumerated
/// rather than guessed: the suffix is the script's pid, which this process
/// has no way to know.
fn siblings_matching(root: &Path) -> Vec<PathBuf> {
    let (Some(parent), Some(name)) = (root.parent(), root.file_name().and_then(|n| n.to_str()))
    else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(parent) else {
        return Vec::new();
    };
    let mut found: Vec<PathBuf> = entries
        .flatten()
        .filter(|e| {
            e.file_name().to_str().is_some_and(|n| {
                n.starts_with(&format!("{name}.new-")) || n.starts_with(&format!("{name}.old-"))
            })
        })
        .map(|e| e.path())
        .collect();
    found.sort();
    found
}

/// Fetches `url`, or `None` on any 4xx/5xx — used for the optional assets
/// (`SHA256SUMS`, the cosign bundle) whose absence is a normal state rather
/// than a failure. v0.1.4 published no `SHA256SUMS` at all.
fn fetch_optional(client: &reqwest::blocking::Client, url: &str) -> Option<Vec<u8>> {
    let response = client.get(url).send().ok()?;
    if !response.status().is_success() {
        return None;
    }
    response.bytes().ok().map(|b| b.to_vec())
}

/// `tgt update`. Runs entirely on stdout; the TUI is never started.
pub fn run(require_signature: bool) -> eyre::Result<()> {
    let install = resolve()?;
    let root = match install {
        Install::Private { root } => root,
        Install::Homebrew => {
            return Err(human_errors::user(
                "This copy of tgt was installed by Homebrew, which manages its own files.".to_string(),
                &["Run `brew upgrade tgt` instead — updating in place would desynchronise brew's manifest."],
            )
            .into());
        }
        Install::Foreign { root } => {
            return Err(human_errors::user(
                format!("{} does not look like a self-contained tgt install, so it cannot be replaced safely.", root.display()),
                &["Reinstall with: curl -sSL https://tgt.specht-labs.de/install.sh | sh",
                  "That lays out a private tree which future updates can replace in one step."],
            )
            .into());
        }
    };

    let client = reqwest::blocking::Client::builder()
        .user_agent(concat!("tgt/", env!("CARGO_PKG_VERSION")))
        .build()?;

    // The `latest` redirect resolves the tag without the API: no token, no
    // rate limit. Asset names embed the version, so this has to happen first.
    let latest = client
        .get(format!("https://github.com/{REPO}/releases/latest"))
        .send()?;
    let tag = latest
        .url()
        .path_segments()
        .and_then(|mut s| s.next_back())
        .unwrap_or_default()
        .to_string();
    if tag.is_empty() {
        return Err(human_errors::system(
            "We couldn't work out the latest release.".to_string(),
            &["Check your network, or download it by hand from the releases page."],
        )
        .into());
    }

    let version = tag.trim_start_matches('v');
    if version == env!("CARGO_PKG_VERSION") {
        println!("tgt {version} is already the latest release.");
        return Ok(());
    }

    let target = target_triple();
    let asset = format!("tgt-{version}-{target}.tar.gz");
    let base = format!("https://github.com/{REPO}/releases/download/{tag}");

    println!("updating tgt {} -> {version}", env!("CARGO_PKG_VERSION"));
    println!("  tree:     {}", root.display());
    println!("  platform: {target}");

    let tarball_bytes = {
        let response = client.get(format!("{base}/{asset}")).send()?;
        if !response.status().is_success() {
            return Err(human_errors::user(
                format!("{tag} has no published build for {target}."),
                &[
                    "Published builds are macOS and Linux, on aarch64 and x86_64.",
                    "Build from source if you need another platform.",
                ],
            )
            .into());
        }
        response.bytes()?
    };

    let workdir = tempfile::tempdir()?;
    let tarball = workdir.path().join(&asset);
    std::fs::write(&tarball, &tarball_bytes)?;

    // Both optional: absence is reported, never treated as verified.
    let expected = fetch_optional(&client, &format!("{base}/SHA256SUMS"))
        .and_then(|body| sha_for(&String::from_utf8_lossy(&body), &asset));
    let bundle = fetch_optional(&client, &format!("{base}/{asset}.cosign.bundle")).map(|body| {
        let path = workdir.path().join(format!("{asset}.cosign.bundle"));
        let _ = std::fs::write(&path, body);
        path
    });

    let checked = verify(
        &tarball,
        bundle.as_deref(),
        expected.as_deref(),
        require_signature,
    )?;
    println!("  {}", checked.describe());

    let unpacked = workdir.path().join("unpacked");
    std::fs::create_dir_all(&unpacked)?;
    let extracted = Command::new("tar")
        .arg("-xzf")
        .arg(&tarball)
        .arg("-C")
        .arg(&unpacked)
        .status()?;
    if !extracted.success() {
        return Err(human_errors::system(
            "The downloaded release could not be extracted.".to_string(),
            &["Try again; if it persists, report it."],
        )
        .into());
    }

    hand_off(&unpacked.join("tgt"), &root)?;
    Ok(())
}

/// Pulls the digest for `asset` out of a `SHA256SUMS` body.
fn sha_for(sums: &str, asset: &str) -> Option<String> {
    sums.lines()
        .find(|line| line.ends_with(asset))
        .and_then(|line| line.split_whitespace().next())
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree(dir: &Path, marker: bool) -> PathBuf {
        let root = dir.join("tgt");
        std::fs::create_dir_all(root.join("bin")).unwrap();
        std::fs::create_dir_all(root.join("lib")).unwrap();
        std::fs::write(root.join("bin/tgt"), b"#!/bin/sh\n").unwrap();
        if marker {
            std::fs::write(root.join(".tgt-install"), b"version=0.1.4\ntarget=x\n").unwrap();
        }
        root
    }

    #[test]
    fn sha_for_picks_the_line_for_its_own_asset() {
        let sums = "aaa  tgt-1.0.0-x86_64-apple-darwin.tar.gz\nbbb  tgt-1.0.0-aarch64-apple-darwin.tar.gz\n";
        assert_eq!(
            sha_for(sums, "tgt-1.0.0-aarch64-apple-darwin.tar.gz").as_deref(),
            Some("bbb"),
            "the wrong line here installs a checksum that can never match"
        );
        assert_eq!(
            sha_for(sums, "tgt-1.0.0-aarch64-unknown-linux-gnu.tar.gz"),
            None
        );
    }

    #[test]
    fn a_marked_tree_is_private() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(is_private_tree(&tree(tmp.path(), true)));
    }

    #[test]
    fn an_unmarked_tgt_tree_is_accepted_by_shape_and_name() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(is_private_tree(&tree(tmp.path(), false)));
    }

    /// The case that would rename someone's home directory. A shared prefix
    /// holds `bin` and `lib` too, so shape alone is not evidence — and a
    /// *fresh* `~/.local` holds only those two, which is why the directory
    /// name is part of the test.
    #[test]
    fn a_shared_prefix_is_never_private() {
        let tmp = tempfile::tempdir().unwrap();
        let local = tmp.path().join(".local");
        std::fs::create_dir_all(local.join("bin")).unwrap();
        std::fs::create_dir_all(local.join("lib")).unwrap();
        std::fs::create_dir_all(local.join("share")).unwrap();
        std::fs::write(local.join("bin/tgt"), b"x").unwrap();
        assert!(
            !is_private_tree(&local),
            "a shared prefix must never be swappable"
        );

        // And the same directory with nothing else in it yet, which is what
        // a new user's ~/.local actually looks like.
        let fresh = tmp.path().join("fresh").join(".local");
        std::fs::create_dir_all(fresh.join("bin")).unwrap();
        std::fs::create_dir_all(fresh.join("lib")).unwrap();
        std::fs::write(fresh.join("bin/tgt"), b"x").unwrap();
        assert!(
            !is_private_tree(&fresh),
            "a fresh ~/.local has only bin and lib, and renaming it is the worst outcome here"
        );
    }

    #[test]
    fn verification_never_claims_more_than_it_checked() {
        let none = Verification::default();
        assert!(none.describe().contains("NOT VERIFIED"));

        let sums_only = Verification {
            checksum: true,
            signature: false,
        };
        assert!(sums_only.describe().contains("corruption check only"));
        assert!(
            !sums_only
                .describe()
                .to_lowercase()
                .contains("signature verified"),
            "a checksum must never read as a signature: {}",
            sums_only.describe()
        );

        let signed = Verification {
            checksum: true,
            signature: true,
        };
        assert!(signed.describe().contains("signature verified"));
    }

    #[test]
    fn require_signature_fails_when_nothing_could_be_checked() {
        let tmp = tempfile::tempdir().unwrap();
        let blob = tmp.path().join("t.tar.gz");
        std::fs::write(&blob, b"payload").unwrap();

        let err = verify(&blob, None, None, true)
            .expect_err("--require-signature must fail when there is no bundle");
        assert!(err.message().contains("no signature bundle"));
        assert!(
            err.message().contains("cosign"),
            "the advice must name the tool"
        );
    }

    #[test]
    fn a_bad_checksum_refuses_rather_than_installing() {
        let tmp = tempfile::tempdir().unwrap();
        let blob = tmp.path().join("t.tar.gz");
        std::fs::write(&blob, b"payload").unwrap();
        let err = verify(&blob, None, Some("0000"), false).expect_err("mismatch must refuse");
        assert!(err.message().contains("does not match"));
    }

    #[test]
    fn a_good_checksum_reports_a_checksum_and_not_a_signature() {
        let tmp = tempfile::tempdir().unwrap();
        let blob = tmp.path().join("t.tar.gz");
        std::fs::write(&blob, b"payload").unwrap();
        let sha = sha256_hex(&blob).unwrap();
        let got = verify(&blob, None, Some(&sha), false).expect("matching checksum passes");
        assert!(got.checksum);
        assert!(
            !got.signature,
            "no bundle was given, so nothing was signed-checked"
        );
    }

    /// The recovery message is only ever read by someone whose client will
    /// not start, so it has to name paths that exist rather than describe
    /// them.
    #[test]
    fn recovery_advice_names_the_leftovers_it_can_see() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("tgt");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(tmp.path().join("tgt.old-4242")).unwrap();

        let err = recovery_advice(&root);
        assert!(err.message().contains("tgt.old-4242"), "{}", err.message());
        assert!(err.message().contains("previous install"));
    }

    #[test]
    fn recovery_advice_is_calm_when_the_tree_is_still_healthy() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tree(tmp.path(), true);
        let err = recovery_advice(&root);
        assert!(err.message().contains("looks intact"), "{}", err.message());
    }

    #[test]
    fn the_target_triple_is_one_of_the_published_four() {
        let published = [
            "aarch64-apple-darwin",
            "x86_64-apple-darwin",
            "aarch64-unknown-linux-gnu",
            "x86_64-unknown-linux-gnu",
        ];
        let triple = target_triple();
        if cfg!(any(target_os = "macos", target_os = "linux")) {
            assert!(
                published.contains(&triple.as_str()),
                "unexpected triple {triple}"
            );
        }
    }
}
