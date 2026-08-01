#!/bin/sh
# Install the latest published tgt release.
#
#   curl -sSL https://tgt.specht-labs.de/install.sh | sh
#
# This is the canonical copy. `mise run docs-build` copies it into
# docs/.vuepress/public/ so the URL above serves exactly this file — there is
# no second copy to drift.
#
# POSIX sh, not bash: it is piped straight into `sh`, so it may not assume
# anything bash-only. No `local`, no arrays, no `[[`.
#
# ## The layout, and why it is not $PREFIX/bin + $PREFIX/lib
#
# bin/tgt finds libtdjson through a runpath relative to the executable
# (@executable_path/../lib on macOS, $ORIGIN/../lib on Linux — architecture
# §9.2), so the two must stay siblings. Scattering them into a shared prefix
# would work right up until something else owned one of those directories.
#
# So the tree is installed privately and the binary is symlinked onto PATH,
# exactly as the Homebrew formula does it (scripts/brew-formula.sh):
#
#   $XDG_DATA_HOME/tgt/{bin,lib}    the tree, exclusively ours
#   $HOME/.local/bin/tgt            a symlink to the above
#
# That the tree is private is what lets `tgt update` replace it with a single
# atomic rename. A shared prefix cannot be updated atomically at all, because
# bin/tgt and lib/libtdjson.* would have to move as one and there is no
# multi-rename. Keep this layout and the updater stays safe by construction.
#
# Deliberately not $XDG_DATA_HOME/telegram-tui: that name already holds the
# user's TDLib database and logs. The program and the user's chat history do
# not belong in one directory.
#
# ## Tone
#
# This runs on a stranger's machine with no review. It is boring on purpose:
# it says what it is about to do and refuses when unsure.
#
# The dangerous direction is the opposite of the obvious one. The risk is not
# removing a path this script derived — it is removing the path it was *given*.
# TGT_INSTALL_ROOT is arbitrary user input, and this script renames the
# directory at that path and deletes the old copy on success. Pointed at $HOME
# it would rename and then delete a home directory. So an existing root is
# never touched without positive evidence that it is a tgt tree: the
# .tgt-install marker package.sh writes, or a bin/tgt to fall back on for
# installs that predate it. Absent both, refuse. An unnecessary refusal is a
# message; a wrong rm -rf is somebody's files.

set -eu

REPO="SpechtLabs/telegram-tui"
RELEASES="https://github.com/$REPO/releases/latest/download"

DATA_HOME="${XDG_DATA_HOME:-$HOME/.local/share}"
INSTALL_ROOT="${TGT_INSTALL_ROOT:-$DATA_HOME/tgt}"
BIN_DIR="${TGT_BIN_DIR:-$HOME/.local/bin}"

say() { printf '%s\n' "$*"; }
warn() { printf '%s\n' "$*" >&2; }
die() {
    printf 'error: %s\n' "$*" >&2
    exit 1
}

need() {
    command -v "$1" >/dev/null 2>&1 || die "this installer needs $1, which is not on your PATH"
}

# --- what are we running on -------------------------------------------------

detect_target() {
    os="$(uname -s)"
    arch="$(uname -m)"

    case "$os" in
        Darwin) os_part="apple-darwin" ;;
        Linux) os_part="unknown-linux-gnu" ;;
        MINGW* | MSYS* | CYGWIN* | Windows_NT)
            die "tgt publishes no Windows build. It compiles there but is untested and ships no artifact; build from source if you want to try it."
            ;;
        *)
            die "unsupported operating system '$os'. tgt publishes macOS and Linux builds only."
            ;;
    esac

    case "$arch" in
        arm64 | aarch64) arch_part="aarch64" ;;
        x86_64 | amd64) arch_part="x86_64" ;;
        *)
            die "unsupported architecture '$arch'. tgt publishes aarch64 and x86_64 builds only."
            ;;
    esac

    printf '%s-%s' "$arch_part" "$os_part"
}

# This used to warn about a missing libc++, which is bundled in the tarball
# and patched to its own runpath (scripts/package.sh) and so is not actually
# a requirement any more -- confirmed on a real machine with zero system
# libc++, which installed and ran fine. What that same round of testing
# found instead: a real glibc floor. The prebuilt TDLib and the bundled
# libc++ both carry glibc symbol versions from the newest GitHub-hosted
# runners the release build ran on, currently 2.39, and there is no build
# configuration that lowers it -- it comes from what TDLib itself links
# against, not from how tgt is compiled. Below that floor the binary does
# not start at all ("version `GLIBC_2.39' not found"), confirmed on real
# aarch64 Debian 12 hardware (glibc 2.36).
#
# This check is advisory, not a gate, for the same reason the libc++ one it
# replaced should have been: `getconf` reporting something this parser
# doesn't recognise (no GNU_LIBC_VERSION at all, e.g. musl on Alpine; or an
# unparseable value) is a reason to stay quiet, not to guess. The
# `--version` probe after install is the real backstop regardless -- it
# already fails and rolls back correctly on too-old glibc, which is how
# this was found in the first place. Skipping straight to that probe would
# work too; this just saves a download and an extraction on the way to the
# same answer.
check_linux_runtime() {
    [ "$(uname -s)" = "Linux" ] || return 0

    glibc="$(getconf GNU_LIBC_VERSION 2>/dev/null | cut -d' ' -f2)" || return 0
    [ -n "$glibc" ] || return 0

    major="$(printf '%s' "$glibc" | cut -d. -f1)"
    minor="$(printf '%s' "$glibc" | cut -d. -f2)"
    case "$major" in '' | *[!0-9]*) return 0 ;; esac
    case "$minor" in '' | *[!0-9]*) return 0 ;; esac

    if [ "$major" -gt 2 ] || { [ "$major" -eq 2 ] && [ "$minor" -ge 39 ]; }; then
        return 0
    fi

    warn ""
    warn "note: this machine reports glibc $glibc; tgt's prebuilt TDLib needs 2.39 or newer"
    warn "      and will very likely fail to start (\"version 'GLIBC_2.39' not found\")."
    warn "      Confirmed working: Ubuntu 24.04+, Debian 13 (trixie)."
    warn "      Confirmed NOT working: Debian 12, Ubuntu 22.04 LTS, RHEL/Rocky 9."
    warn "      Building tgt from source does not help: it links the same prebuilt"
    warn "      TDLib, which carries this floor no matter who compiles tgt itself."
    warn "      Installing anyway; the version check right after install will confirm either way."
    warn ""
}

# --- is this directory ours ------------------------------------------------

# Reads a `key=value` out of a .tgt-install marker, empty if absent.
marker_value() {
    [ -f "$1/.tgt-install" ] || { printf ''; return 0; }
    sed -n "s/^$2=//p" "$1/.tgt-install" 2>/dev/null | head -1
}

# Refuses unless $INSTALL_ROOT is demonstrably a tgt tree. Only called when the
# directory already exists — creating a fresh one is always fine.
#
# Two levels of evidence, and the caller is told which one was used, because
# "we checked a marker" and "we guessed from a filename" are different claims.
assert_ours() {
    root="$1"
    expected_target="$2"

    if [ -f "$root/.tgt-install" ]; then
        found="$(marker_value "$root" target)"
        if [ -n "$found" ] && [ "$found" != "$expected_target" ]; then
            die "$root holds a $found install, but this machine needs $expected_target.
Replacing it would leave a binary that cannot load its library.
Remove it by hand if that is really what you want, or set TGT_INSTALL_ROOT elsewhere."
        fi
        say "  existing:  tgt $(marker_value "$root" version) ($found) — verified by marker"
        return 0
    fi

    if [ -x "$root/bin/tgt" ]; then
        say "  existing:  a tgt install with no marker — inferred from bin/tgt"
        return 0
    fi

    die "$root already exists and does not look like a tgt install.
Refusing to replace it: this script renames that directory and deletes the old
copy once the new one works, so it will not touch anything it cannot identify.
Remove it by hand, or set TGT_INSTALL_ROOT to a different path."
}

# --- integrity --------------------------------------------------------------

sha256_of() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | cut -d' ' -f1
    elif command -v shasum >/dev/null 2>&1; then
        shasum -a 256 "$1" | cut -d' ' -f1
    else
        printf ''
    fi
}

# Verifies the tarball against SHA256SUMS when the release published one, and
# says plainly what it did. v0.1.4 shipped without it (the checksums job
# failed), so absence is a real case rather than a hypothetical.
#
# What this check is worth, stated honestly: the sums file comes from the same
# host over the same TLS session as the tarball, so it detects a corrupted or
# truncated download and nothing else. Anyone able to serve you a modified
# tarball can serve you a matching sums file. The signature that would mean
# more is the cosign bundle each release also publishes; verifying it needs
# cosign installed, which `tgt update --require-signature` handles.
verify_checksum() {
    tarball="$1"
    asset="$2"
    workdir="$3"

    if ! curl -fsSL -o "$workdir/SHA256SUMS" "$RELEASES/SHA256SUMS" 2>/dev/null; then
        say "  checksum: not published for this release — download not verified"
        return 0
    fi

    expected="$(grep " $asset\$" "$workdir/SHA256SUMS" 2>/dev/null | cut -d' ' -f1 || true)"
    if [ -z "$expected" ]; then
        say "  checksum: SHA256SUMS has no entry for $asset — download not verified"
        return 0
    fi

    actual="$(sha256_of "$tarball")"
    if [ -z "$actual" ]; then
        say "  checksum: no sha256sum or shasum available — download not verified"
        return 0
    fi

    [ "$actual" = "$expected" ] || die "checksum mismatch for $asset.
  expected $expected
  actual   $actual
Refusing to install. Try again; if it persists, report it."

    say "  checksum: ok (corruption check only — see the docs on signatures)"
}

# --- install ----------------------------------------------------------------

main() {
    need curl
    need tar

    target="$(detect_target)"

    say "tgt installer"
    say "  platform:  $target"
    say "  tree:      $INSTALL_ROOT"
    say "  symlink:   $BIN_DIR/tgt"
    say ""

    workdir="$(mktemp -d)"
    # Only ever removes the directory mktemp just gave us.
    trap 'rm -rf "$workdir"' EXIT INT TERM

    # Asset names embed the version, so the tag has to be resolved first.
    # The `latest` redirect gives it without the API, which means no token
    # and no rate limit.
    tag="$(curl -fsSLI -o /dev/null -w '%{url_effective}' \
        "https://github.com/$REPO/releases/latest" 2>/dev/null |
        sed 's#.*/tag/##')"
    [ -n "$tag" ] || die "could not work out the latest release.
Check your network, or install by hand from https://github.com/$REPO/releases"

    version="${tag#v}"
    asset="tgt-$version-$target.tar.gz"
    say "  release:   $tag"
    say ""

    say "downloading ${asset}…"
    curl -fSL --progress-bar -o "$workdir/tgt.tar.gz" "$RELEASES/$asset" ||
        die "no published build for $target in $tag.
Published builds are macOS and Linux, on aarch64 and x86_64.
See https://github.com/$REPO/releases/tag/$tag"

    verify_checksum "$workdir/tgt.tar.gz" "$asset" "$workdir"

    say "extracting…"
    mkdir -p "$workdir/unpacked"
    tar -xzf "$workdir/tgt.tar.gz" -C "$workdir/unpacked"

    # The tarball contains a single tgt/ directory holding bin/ and lib/.
    # Anything else means the layout changed and this script is out of date;
    # refuse rather than guess where to put things.
    [ -x "$workdir/unpacked/tgt/bin/tgt" ] ||
        die "the downloaded archive does not look like a tgt release (no tgt/bin/tgt).
Refusing to install. Please report this."

    check_linux_runtime
    swap_tree "$workdir/unpacked/tgt" "$target" adopt
}

# --- the swap ---------------------------------------------------------------

# Replaces $INSTALL_ROOT with the already-extracted tree at $1, keeping the old
# one until the new binary has proved it starts.
#
# This is the dangerous part of both entry points, and it exists once on
# purpose. `tgt update` does not reimplement it: it downloads, verifies
# (including the pinned cosign check this script deliberately does not do),
# extracts, and then calls this same function through `--swap-from`. Two
# implementations of a stage/rename/probe/rollback would drift silently, and
# the drift would only show up when a rollback was needed — which is exactly
# when nobody is watching. Sharing it also means every curl install exercises
# the path the updater depends on.
# $3 decides what happens to the PATH symlink, and the two entry points want
# different things. An install *adopts* it: the user just asked for this tree
# to be the tgt they run, so taking the name over is the request. An update
# only *keeps* what is already its own, because it is invoked for one
# particular tree — the one whose binary is running — and $BIN_DIR/tgt may
# belong to a different install entirely, a Homebrew one or a second private
# tree. Repointing it there would change which tgt the user runs, as a side
# effect of updating something else, with nothing on screen saying so. That is
# not hypothetical: it happened the first time `tgt update` was run against a
# real release, on a tree that was not the one on PATH.
swap_tree() {
    source_tree="$1"
    target="$2"
    link_policy="${3:-adopt}"

    # The tarball contains a single tgt/ directory holding bin/ and lib/.
    # Anything else means the layout changed and this script is out of date;
    # refuse rather than guess where to put things.
    [ -x "$source_tree/bin/tgt" ] ||
        die "$source_tree does not look like a tgt tree (no bin/tgt).
Refusing to install. Please report this."

    # Before anything is renamed or removed. An install into a fresh path
    # skips this; only an existing directory has to prove itself.
    if [ -e "$INSTALL_ROOT" ]; then
        assert_ours "$INSTALL_ROOT" "$target"
    fi

    mkdir -p "$(dirname "$INSTALL_ROOT")" "$BIN_DIR"
    staged="$INSTALL_ROOT.new-$$"
    previous="$INSTALL_ROOT.old-$$"
    rm -rf "$staged"
    cp -a "$source_tree" "$staged"

    if [ -e "$INSTALL_ROOT" ]; then
        mv "$INSTALL_ROOT" "$previous"
    fi
    mv "$staged" "$INSTALL_ROOT"

    # The probe, and the reason the old tree still exists at this point: a
    # binary paired with the wrong libtdjson fails at dyld load, and this is
    # the only moment recovery is still possible. It runs the NEW tree's
    # binary, which is what makes it a test of the thing just installed.
    if ! "$INSTALL_ROOT/bin/tgt" --version >/dev/null 2>&1; then
        rm -rf "$INSTALL_ROOT"
        if [ -e "$previous" ]; then
            mv "$previous" "$INSTALL_ROOT"
            die "the newly installed tgt could not start; your previous install has been restored."
        fi
        die "the newly installed tgt could not start, and there was no previous install to restore.
On Linux this is usually the glibc floor noted above (2.39 or newer required)."
    fi
    [ -e "$previous" ] && rm -rf "$previous"

    # `-e` is false for a dangling symlink, so both tests are needed to mean
    # "nothing is there". A regular file at that path is somebody else's
    # binary and is left alone for the same reason a foreign symlink is.
    if [ "$link_policy" = adopt ]; then
        link_it=yes
    elif [ ! -e "$BIN_DIR/tgt" ] && [ ! -L "$BIN_DIR/tgt" ]; then
        link_it=yes
    elif [ -L "$BIN_DIR/tgt" ] && [ "$(readlink "$BIN_DIR/tgt")" = "$INSTALL_ROOT/bin/tgt" ]; then
        link_it=yes
    else
        link_it=no
    fi

    if [ "$link_it" = yes ]; then
        ln -sfn "$INSTALL_ROOT/bin/tgt" "$BIN_DIR/tgt"
    fi

    say ""
    say "installed $("$INSTALL_ROOT/bin/tgt" --version)"
    if [ "$link_it" = yes ]; then
        say "  $BIN_DIR/tgt -> $INSTALL_ROOT/bin/tgt"

        case ":$PATH:" in
            *":$BIN_DIR:"*) ;;
            *)
                say ""
                say "note: $BIN_DIR is not on your PATH. Add it:"
                say "  export PATH=\"$BIN_DIR:\$PATH\""
                ;;
        esac
    else
        say "  in place at $INSTALL_ROOT"
        say ""
        say "note: $BIN_DIR/tgt was left alone — it is not this tree's symlink."
        say "      The tgt on your PATH is still whatever it pointed at."
    fi
}

# `tgt update` hands off here: it has already downloaded, verified and
# extracted, so this skips straight to the swap. Deliberately does NOT need
# curl — the network half belongs to whoever called us.
if [ "${1:-}" = "--swap-from" ]; then
    [ -n "${2:-}" ] || die "--swap-from needs the path of an extracted tgt tree"
    swap_tree "$2" "$(detect_target)" keep
    exit 0
fi

main "$@"
