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

use std::cmp::Ordering;
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
pub fn resolve() -> eyre::Result<Install> {
    Ok(classify(&std::env::current_exe()?))
}

/// The half of [`resolve`] that touches no process state, so the symlink and
/// shared-prefix cases can be exercised against real directories.
///
/// The path is canonicalised first, and that is load-bearing rather than
/// tidiness. `current_exe` does not resolve symlinks everywhere: Linux reads
/// `/proc/self/exe` and gets the real file, but macOS returns the path the
/// process was invoked *through*. Both supported install methods put a
/// symlink on PATH — `install.sh` links `~/.local/bin/tgt` into the private
/// tree, Homebrew links its prefix into the Cellar — so on macOS the
/// derivation below saw `~/.local` or `/opt/homebrew` and called every real
/// install [`Install::Foreign`]. `tgt update` then refused to run for
/// anyone who did not type the full path to the binary, and the Homebrew
/// branch could never be reached at all.
fn classify(exe: &Path) -> Install {
    let exe = std::fs::canonicalize(exe).unwrap_or_else(|_| exe.to_path_buf());
    // …/<root>/bin/tgt → <root>
    let Some(root) = exe.parent().and_then(Path::parent) else {
        return Install::Foreign { root: exe };
    };

    // Homebrew installs the tree under libexec inside a Cellar and symlinks
    // the binary; the Cellar component is the reliable signal.
    if root.components().any(|c| c.as_os_str() == "Cellar") {
        return Install::Homebrew;
    }

    if is_private_tree(root) {
        Install::Private {
            root: root.to_path_buf(),
        }
    } else {
        Install::Foreign {
            root: root.to_path_buf(),
        }
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
                // Reversed on purpose; see `advice_reads_in_the_order_it_was
                // _meant_to`. `human-errors` renders advice back to front, so
                // the first thing to try has to be written last.
                &[
                    "If it keeps happening, report it rather than installing.",
                    "Try again — this is usually a truncated download.",
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
                // Reversed; see the note on the checksum advice above. Getting
                // this one backwards leads with "report it" and buries "do not
                // install", which is the only line that matters here.
                &[
                    "Report it — a signature that fails this check is not merely a bad download.",
                    "Do not install this download.",
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
            // Reversed; see the note on the checksum advice above.
            &[
                "Or drop --require-signature to install with only the checks that are available.",
                "Install cosign (https://github.com/sigstore/cosign) and try again.",
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
pub fn run(require_signature: bool, force: bool) -> eyre::Result<()> {
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
                // Reversed; see the note on the checksum advice above. This one
                // shipped backwards in 0.1.5, where the second line arrived
                // first and read as a sentence with no subject.
                &["That lays out a private tree which future updates can replace in one step.",
                  "Reinstall with: curl -sSL https://tgt.specht-labs.de/install.sh | sh"],
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
    // `force` is spent entirely in this decision. Nothing below reads it,
    // which is what makes it an exercise of the ordinary path rather than a
    // second, laxer one: same download, same verification, same `sh -n`,
    // same swap, same probe, same rollback.
    let lines = match decide(version, env!("CARGO_PKG_VERSION"), force) {
        Decision::UpToDate(lines) => {
            for line in lines {
                println!("{line}");
            }
            return Ok(());
        }
        Decision::Refuse(message) => {
            return Err(human_errors::user(
                message,
                // Reversed; see the note on the checksum advice above.
                &[
                    "Run `tgt update --force` to install it anyway.",
                    "Nothing has been changed.",
                ],
            )
            .into());
        }
        Decision::Proceed(lines) => lines,
    };

    let target = target_triple();
    let asset = format!("tgt-{version}-{target}.tar.gz");
    let base = format!("https://github.com/{REPO}/releases/download/{tag}");

    for line in lines {
        println!("{line}");
    }
    println!("  tree:     {}", root.display());
    println!("  platform: {target}");

    let tarball_bytes = {
        let response = client.get(format!("{base}/{asset}")).send()?;
        if !response.status().is_success() {
            return Err(human_errors::user(
                format!("{tag} has no published build for {target}."),
                // Reversed; see the note on the checksum advice above.
                &[
                    "Build from source if you need another platform.",
                    "Published builds are macOS and Linux, on aarch64 and x86_64.",
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
/// What [`run`] should do about the release it found, and what to say first.
///
/// Separated from `run` because `run` needs the network and a real install
/// tree, so every branch here would otherwise only be reachable by building
/// a binary with a doctored version and pointing it at a real release. That
/// is exactly the shape of thing that ships broken.
#[derive(Debug, PartialEq, Eq)]
enum Decision {
    /// Nothing to do. Print these and exit successfully.
    UpToDate(Vec<String>),
    /// Refuse, with this message.
    Refuse(String),
    /// Go ahead, after printing these.
    Proceed(Vec<String>),
}

/// Decides whether to install `latest` over `installed`.
///
/// A downgrade is refused rather than announced when `force` is not given,
/// because that case is not opt-in: an ordinary `tgt update` that replaced a
/// newer build with an older one would be the least expected thing this
/// command could do. Under `--force` it proceeds and names itself, since a
/// developer running a local build on a broken tree needs a way back to the
/// published release — that repair being what the flag is for.
fn decide(latest: &str, installed: &str, force: bool) -> Decision {
    let order = compare_versions(latest, installed);
    let same = latest == installed || order == Some(Ordering::Equal);

    if same && !force {
        return Decision::UpToDate(vec![
            format!("tgt {latest} is already the latest release."),
            "  Run `tgt update --force` to download and reinstall it anyway.".to_string(),
        ]);
    }
    if order == Some(Ordering::Less) && !force {
        return Decision::Refuse(format!(
            "The latest published release is {latest}, which is older than the {installed} you are running."
        ));
    }

    Decision::Proceed(match (same, order) {
        (true, _) => vec![format!("reinstalling tgt {latest}")],
        (_, Some(Ordering::Less)) => vec![
            format!("downgrading tgt {installed} -> {latest}"),
            "  the latest published release is older than what you are running".to_string(),
        ],
        (_, Some(Ordering::Greater)) => vec![format!("updating tgt {installed} -> {latest}")],
        // Neither side parsed as major.minor.patch, so the direction is
        // genuinely unknown and is not guessed at.
        _ => vec![format!("installing tgt {latest} over {installed}")],
    })
}

/// Orders two versions by their numeric `major.minor.patch`, or `None` when
/// either is not that shape.
///
/// A pre-release suffix is dropped before comparing, so `0.1.7-dev` and
/// `0.1.7` compare equal. That is deliberately not semver, where the
/// pre-release sorts first: the only question asked here is which release
/// the user would end up with, and answering it "equal" makes the command
/// offer a reinstall rather than a silent downgrade.
///
/// `None` means the direction is unknown, and the caller says so rather than
/// guessing — a locally built binary can carry any version string at all.
fn compare_versions(a: &str, b: &str) -> Option<Ordering> {
    fn triple(v: &str) -> Option<(u64, u64, u64)> {
        let core = v.split(['-', '+']).next()?;
        let mut parts = core.split('.');
        let major = parts.next()?.parse().ok()?;
        let minor = parts.next()?.parse().ok()?;
        let patch = parts.next()?.parse().ok()?;
        if parts.next().is_some() {
            return None;
        }
        Some((major, minor, patch))
    }
    Some(triple(a)?.cmp(&triple(b)?))
}

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

    /// The shipped layout: the binary lives in a private tree and PATH holds
    /// a symlink to it. Classifying the symlink's own directory instead of
    /// the tree it points into is what made `tgt update` refuse to run for
    /// every install `install.sh` had ever made.
    #[test]
    #[cfg(unix)]
    fn a_symlink_on_path_resolves_to_the_tree_it_points_into() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tree(tmp.path(), true);
        let bin_dir = tmp.path().join(".local/bin");
        std::fs::create_dir_all(&bin_dir).unwrap();
        let link = bin_dir.join("tgt");
        std::os::unix::fs::symlink(root.join("bin/tgt"), &link).unwrap();

        assert_eq!(
            classify(&link),
            Install::Private {
                root: std::fs::canonicalize(&root).unwrap()
            },
            "invoking through the PATH symlink must reach the real tree"
        );
    }

    /// Homebrew symlinks its prefix into the Cellar, so the Cellar component
    /// only appears once the link is followed. Before it was, a brew user
    /// updating in place was told their install was unrecognisable instead of
    /// being pointed at `brew upgrade`.
    #[test]
    #[cfg(unix)]
    fn a_homebrew_symlink_is_recognised_through_the_cellar() {
        let tmp = tempfile::tempdir().unwrap();
        let cellar = tmp.path().join("Cellar/tgt/0.1.5/libexec/bin");
        std::fs::create_dir_all(&cellar).unwrap();
        std::fs::write(cellar.join("tgt"), b"#!/bin/sh\n").unwrap();
        let prefix_bin = tmp.path().join("bin");
        std::fs::create_dir_all(&prefix_bin).unwrap();
        let link = prefix_bin.join("tgt");
        std::os::unix::fs::symlink(cellar.join("tgt"), &link).unwrap();

        assert_eq!(classify(&link), Install::Homebrew);
    }

    /// The whole decision table, including the branches that need a release
    /// older than the running binary — a state no test could otherwise reach
    /// without publishing one.
    #[test]
    fn the_update_decision_covers_every_direction() {
        // Newer: update, with or without --force.
        assert_eq!(
            decide("0.1.6", "0.1.5", false),
            Decision::Proceed(vec!["updating tgt 0.1.5 -> 0.1.6".to_string()])
        );
        assert_eq!(
            decide("0.1.6", "0.1.5", true),
            Decision::Proceed(vec!["updating tgt 0.1.5 -> 0.1.6".to_string()])
        );

        // Same: stop, and say how to reinstall anyway.
        let Decision::UpToDate(lines) = decide("0.1.5", "0.1.5", false) else {
            panic!("the same version must not download anything without --force");
        };
        assert_eq!(lines[0], "tgt 0.1.5 is already the latest release.");
        assert!(lines[1].contains("--force"), "{lines:?}");

        // Same, forced: the reinstall that makes the swap exercisable.
        assert_eq!(
            decide("0.1.5", "0.1.5", true),
            Decision::Proceed(vec!["reinstalling tgt 0.1.5".to_string()])
        );

        // Older: refused unless asked for explicitly.
        let Decision::Refuse(message) = decide("0.1.5", "0.1.7", false) else {
            panic!("an ordinary update must never walk a version backwards");
        };
        assert!(message.contains("0.1.5"), "{message}");
        assert!(message.contains("older"), "{message}");

        // Older, forced: proceeds, and names itself a downgrade rather than
        // reporting it as an update.
        let Decision::Proceed(lines) = decide("0.1.5", "0.1.7", true) else {
            panic!("--force must allow the deliberate downgrade");
        };
        assert!(lines[0].starts_with("downgrading"), "{lines:?}");

        // Unorderable: proceeds, and claims no direction it cannot justify.
        let Decision::Proceed(lines) = decide("0.1.5", "nightly", false) else {
            panic!("an unparseable local version must not block updating");
        };
        assert_eq!(lines, vec!["installing tgt 0.1.5 over nightly".to_string()]);
    }

    /// The comparison `--force` rests on. Getting `Less` wrong in either
    /// direction is the expensive case: reported as `Greater` it downgrades
    /// someone silently, reported as `Less` it refuses a real update.
    #[test]
    fn versions_order_by_number_and_admit_when_they_cannot() {
        assert_eq!(compare_versions("0.1.6", "0.1.5"), Some(Ordering::Greater));
        assert_eq!(compare_versions("0.1.5", "0.1.6"), Some(Ordering::Less));
        assert_eq!(compare_versions("0.1.5", "0.1.5"), Some(Ordering::Equal));
        assert_eq!(compare_versions("0.2.0", "0.1.99"), Some(Ordering::Greater));
        assert_eq!(compare_versions("0.10.0", "0.9.0"), Some(Ordering::Greater));

        // A pre-release suffix compares as its release, so a local 0.1.7-dev
        // is offered a reinstall of 0.1.7 rather than a downgrade to it.
        assert_eq!(
            compare_versions("0.1.7", "0.1.7-dev"),
            Some(Ordering::Equal)
        );

        // Not major.minor.patch: unknown, and the caller says so.
        assert_eq!(compare_versions("0.1", "0.1.5"), None);
        assert_eq!(compare_versions("nightly", "0.1.5"), None);
        assert_eq!(compare_versions("0.1.5.1", "0.1.5"), None);
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

    /// `human-errors` renders advice back to front: `Error::advice()` walks
    /// the cause chain appending as it goes, then reverses the lot so the
    /// innermost error's advice comes first. For an error that carries its own
    /// advice — which is all of ours — that reverses the array as written, and
    /// nothing about the call site says so. 0.1.5 shipped one that read as a
    /// sentence with no subject because of it.
    ///
    /// So every array in this module is written back to front on purpose, and
    /// this asserts on what the user actually sees rather than on the literal.
    /// A reordering that looks like a tidy-up fails here.
    #[test]
    fn advice_reads_in_the_order_it_was_meant_to() {
        let tmp = tempfile::tempdir().unwrap();
        let blob = tmp.path().join("t.tar.gz");
        std::fs::write(&blob, b"payload").unwrap();

        let err =
            verify(&blob, None, Some("deadbeef"), false).expect_err("checksum must not match");
        assert_eq!(
            err.advice().first().copied(),
            Some("Try again — this is usually a truncated download."),
            "the first thing to try must be read first: {:?}",
            err.advice()
        );

        let err = verify(&blob, None, None, true).expect_err("no bundle, signature required");
        assert_eq!(
            err.advice().first().copied(),
            Some("Install cosign (https://github.com/sigstore/cosign) and try again."),
            "an alternative beginning \"Or\" must not arrive first: {:?}",
            err.advice()
        );
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
