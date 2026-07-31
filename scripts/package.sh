#!/usr/bin/env bash
# Builds and packages the `tgt` release binary into a relocatable
# dist/tgt/{bin,lib} tree whose dynamic loader path is self-contained, then
# tars it up. See docs/architecture.md §9.2 and crates/app/build.rs for the
# mechanism this script completes for packaged (non-dev) builds.
#
# One script, two platforms. macOS and Linux express the same idea in different
# dialects — `@executable_path` + `@rpath` install names rewritten by
# `install_name_tool`, against `$ORIGIN` + `DT_RUNPATH` — so the layout, the
# version parsing, the tarball name and, above all, the relocation proof are
# shared, and only the loader surgery forks. Adding a platform means adding a
# `fixup_*` and a `verify_relocated_*`; the guarantee at the end is the same one.
#
# TARGET selects the rust target triple and defaults to the host's. Every
# published target is built on a runner of its own architecture (see
# release.yaml), so the proof at the end always *executes* the binary it built
# rather than asserting something structural about a foreign one.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

bin_name="tgt"
dist_dir="dist/tgt"

target="${TARGET:-$(rustc -vV | sed -n 's/^host: //p')}"
case "$target" in
    *-apple-darwin)
        platform="macos"
        lib_glob='libtdjson*.dylib'
        unversioned_name="libtdjson.dylib"
        ;;
    *-unknown-linux-gnu)
        platform="linux"
        lib_glob='libtdjson.so*'
        unversioned_name="libtdjson.so"
        ;;
    *)
        echo "error: no packaging recipe for target '$target'" >&2
        echo "       supported: *-apple-darwin, *-unknown-linux-gnu" >&2
        echo "       (Windows builds and tests in CI but ships no artifact; its" >&2
        echo "        loader has no rpath equivalent, so the layout below is moot)" >&2
        exit 1
        ;;
esac
target_dir="target/$target/release"

# Libraries copied in beside libtdjson so the tree carries its own copies; the
# relocation proof asserts each one resolves from inside the moved tree. Filled
# in by fixup_linux, and read by verify_relocated_linux.
bundled_libs=()

# ---------------------------------------------------------------------------
# macOS: rewrite install names to @rpath, then re-sign.
# ---------------------------------------------------------------------------
fixup_macos() {
    echo "==> Fixing install names with install_name_tool"
    # The dylib's own LC_ID_DYLIB is derived from its build-time absolute path
    # (or is already @rpath-relative, depending on how tdlib-rs built it) —
    # force it to the versioned @rpath name we are shipping under.
    install_name_tool -id "@rpath/$versioned_name" "$dist_dir/lib/$versioned_name"

    # If the binary's LC_LOAD_DYLIB for tdjson recorded an absolute build-machine
    # path (rather than @rpath already), rewrite it to match.
    local recorded_path
    recorded_path="$(otool -L "$dist_dir/bin/$bin_name" | awk '/libtdjson/{print $1; exit}')"
    if [[ "$recorded_path" == /* ]]; then
        echo "    binary references absolute path: $recorded_path"
        echo "    rewriting to @rpath/$versioned_name"
        install_name_tool -change "$recorded_path" "@rpath/$versioned_name" "$dist_dir/bin/$bin_name"
    else
        echo "    binary already references: $recorded_path (no rewrite needed)"
    fi

    echo "==> Verifying otool -L (tdjson entry must be @rpath, not absolute)"
    otool -L "$dist_dir/bin/$bin_name"
    if otool -L "$dist_dir/bin/$bin_name" | grep -i tdjson | grep -qv '@rpath'; then
        echo "error: binary still references a non-@rpath libtdjson path" >&2
        exit 1
    fi

    echo "==> Verifying LC_RPATH entries"
    otool -l "$dist_dir/bin/$bin_name" | grep -A2 LC_RPATH
    if ! otool -l "$dist_dir/bin/$bin_name" | grep -q '@executable_path/../lib'; then
        echo "error: binary is missing the @executable_path/../lib rpath" >&2
        exit 1
    fi

    # rustc gives arm64 Mach-O binaries an ad-hoc signature at link time, and
    # arm64 refuses to execute anything unsigned. install_name_tool has just
    # invalidated that signature, so re-sign unconditionally rather than
    # discovering it via a failed execution.
    echo "==> Re-signing (ad-hoc) after install_name_tool"
    codesign --force --sign - "$dist_dir/lib/$versioned_name"
    codesign --force --sign - "$dist_dir/bin/$bin_name"
}

verify_relocated_macos() {
    : # otool assertions above are position-independent; the run is the proof.
}

# ---------------------------------------------------------------------------
# Linux: $ORIGIN is baked in at link time, so there is nothing to rewrite on the
# binary — but TDLib's own dependencies have to be brought along.
# ---------------------------------------------------------------------------
fixup_linux() {
    echo "==> Verifying the binary's runpath"
    # Either tag carries the same string; which one the linker emitted depends
    # on --enable-new-dtags. Splitting on the brackets pulls the value out
    # without a GNU-only regex.
    local runpath
    runpath="$(readelf -d "$dist_dir/bin/$bin_name" |
        awk -F'[][]' '/\(RUNPATH\)|\(RPATH\)/ { print $2; exit }')"
    echo "    runpath: ${runpath:-<none>}"
    case ":$runpath:" in
        *':$ORIGIN/../lib:'*) ;;
        *)
            echo 'error: binary is missing the $ORIGIN/../lib runpath' >&2
            exit 1
            ;;
    esac

    # The prebuilt Linux TDLib is linked against LLVM's libc++, which most
    # distributions do not ship (README.md's platform note, and the reason
    # ci.yml apt-installs libc++1). A tarball that assumes it is a tarball that
    # dies on `--version`, and a Homebrew formula cannot apt-get anything — so
    # bring the C++ runtime along and give each bundled library its own
    # $ORIGIN runpath. DT_RUNPATH is not inherited transitively, so every link
    # in the chain (libtdjson -> libc++ -> libc++abi -> libunwind) needs its
    # own. Everything else libtdjson wants is glibc and libgcc, which are base
    # system contracts and would be actively harmful to ship.
    local soname resolved
    for soname in libc++.so.1 libc++abi.so.1 libunwind.so.1; do
        resolved="$(ldd "$dist_dir/lib/$versioned_name" |
            awk -v s="$soname" '$1 == s && $2 == "=>" { print $3; exit }')"
        if [[ -z "$resolved" || ! -e "$resolved" ]]; then
            continue
        fi
        echo "    bundling $soname (from $resolved)"
        cp -L "$resolved" "$dist_dir/lib/$soname"
        chmod +w "$dist_dir/lib/$soname"
        bundled_libs+=("$soname")
    done

    if [[ ${#bundled_libs[@]} -eq 0 ]]; then
        echo "    no C++ runtime to bundle (libtdjson links none)"
        return
    fi

    if ! command -v patchelf >/dev/null 2>&1; then
        echo "error: patchelf is required to package on Linux (apt-get install patchelf)" >&2
        echo "       the bundled ${bundled_libs[*]} would not be found without it" >&2
        exit 1
    fi

    echo "==> Setting \$ORIGIN runpath on the bundled libraries"
    for soname in "$versioned_name" "${bundled_libs[@]}"; do
        patchelf --set-rpath '$ORIGIN' "$dist_dir/lib/$soname"
    done
}

verify_relocated_linux() {
    local root="$1"
    echo "==> Checking the moved tree resolves its libraries from inside itself"
    local out
    out="$(ldd "$root/bin/$bin_name")"
    echo "$out" | sed 's/^/    /'

    local soname resolved
    for soname in "$versioned_name" ${bundled_libs[@]+"${bundled_libs[@]}"}; do
        resolved="$(echo "$out" | awk -v s="$soname" '$1 == s && $2 == "=>" { print $3; exit }')"
        if [[ "$resolved" != "$root/"* ]]; then
            echo "error: $soname resolved to '${resolved:-<not found>}'," >&2
            echo "       expected a path under $root — the tree is not self-contained" >&2
            exit 1
        fi
    done
}

# ---------------------------------------------------------------------------

echo "==> Packaging $bin_name for $target ($platform)"

echo "==> Building release binary (cargo build --release -p tgt-app --target $target)"
if command -v rustup >/dev/null 2>&1; then
    rustup target add "$target" >/dev/null
fi
cargo build --release -p tgt-app --target "$target"

bin_src="$target_dir/$bin_name"
if [[ ! -f "$bin_src" ]]; then
    echo "error: expected binary at $bin_src, not found" >&2
    exit 1
fi

# Per T04's empirical findings (documented in build.rs and architecture.md
# §9.2), the tdlib-rs download contains BOTH an unversioned name and a
# versioned one, and the library's own recorded identity — LC_ID_DYLIB on
# macOS, SONAME on Linux — and therefore the linked binary's dependency entry,
# references the VERSIONED name.
#
# Take them from tdlib-rs's own OUT_DIR, which is both the copy the binary was
# actually linked against and the only one guaranteed to be complete.
# crates/app/build.rs also mirrors them next to the binary so `cargo run` and
# `cargo test` resolve them, but cargo has no ordering edge between that build
# script and tdlib-rs's, so on a cold build the mirror can capture the zip
# mid-extraction. Observed on a fresh target dir: a 10,697,896-byte copy of the
# 22,416,520-byte dylib, still at the extractor's 0644, which install_name_tool
# then rejects as a "truncated or malformed object". Reading the source of
# truth costs nothing and does not depend on who won the race.
# (bash 3.2 on macOS has no mapfile)
libs=()
while IFS= read -r line; do
    libs+=("$line")
done < <(find "$target_dir/build" -path '*/tdlib-rs-*/out/*' -name "$lib_glob" -type f 2>/dev/null)
if [[ ${#libs[@]} -eq 0 ]]; then
    echo "    no tdlib-rs OUT_DIR under $target_dir/build; falling back to the build.rs mirror"
    while IFS= read -r line; do
        libs+=("$line")
    done < <(find "$target_dir" -maxdepth 1 -name "$lib_glob" -type f)
fi
if [[ ${#libs[@]} -eq 0 ]]; then
    echo "error: no $lib_glob found under $target_dir (did the build run?)" >&2
    exit 1
fi

# Stale tdlib-rs-<hash>/out directories from earlier dependency versions can
# coexist in one target tree; the live build is always the most recent.
newest() {
    local paths=()
    local path
    while IFS= read -r path; do
        paths+=("$path")
    done
    [[ ${#paths[@]} -gt 0 ]] || return 1
    ls -t "${paths[@]}" | head -1
}

versioned_candidates=()
unversioned_candidates=()
for lib in "${libs[@]}"; do
    if [[ "$(basename "$lib")" == "$unversioned_name" ]]; then
        unversioned_candidates+=("$lib")
    else
        versioned_candidates+=("$lib")
    fi
done

if [[ ${#versioned_candidates[@]} -eq 0 ]]; then
    echo "error: no versioned tdjson library found under $target_dir" >&2
    exit 1
fi
versioned_lib="$(printf '%s\n' "${versioned_candidates[@]}" | newest)"
unversioned_lib=""
if [[ ${#unversioned_candidates[@]} -gt 0 ]]; then
    unversioned_lib="$(printf '%s\n' "${unversioned_candidates[@]}" | newest)"
fi
versioned_name="$(basename "$versioned_lib")"

echo "==> Found tdjson library: $versioned_name"
echo "    versioned:   $versioned_lib"
echo "    unversioned: ${unversioned_lib:-<none found, will symlink>}"

echo "==> Laying out $dist_dir"
rm -rf "$dist_dir"
mkdir -p "$dist_dir/bin" "$dist_dir/lib"

cp "$bin_src" "$dist_dir/bin/$bin_name"
chmod +x "$dist_dir/bin/$bin_name"

cp "$versioned_lib" "$dist_dir/lib/$versioned_name"
chmod +w "$dist_dir/lib/$versioned_name"
# Ship the unversioned name too (as a symlink to the real, versioned file) for
# any tool/consumer that looks for the bare name. The binary itself only names
# the versioned one, so this is belt-and-suspenders, not load-bearing for `tgt`.
ln -sf "$versioned_name" "$dist_dir/lib/$unversioned_name"

# A malformed library otherwise fails three steps later with a message about
# install names, or not until it reaches a user. Name it where it happens.
case "$platform" in
    macos) header_check=(otool -l) ;;
    linux) header_check=(readelf -h) ;;
esac
if ! "${header_check[@]}" "$dist_dir/lib/$versioned_name" >/dev/null 2>&1; then
    echo "error: $versioned_name is not a well-formed shared library" >&2
    echo "       source: $versioned_lib ($(wc -c <"$versioned_lib") bytes)" >&2
    exit 1
fi

"fixup_$platform"

echo "==> Relocation proof: running the packaged binary from a moved directory"
proof_dir="$(mktemp -d)"
trap 'rm -rf "$proof_dir"' EXIT
cp -R "$dist_dir" "$proof_dir/tgt"

"verify_relocated_$platform" "$proof_dir/tgt"

version_output="$("$proof_dir/tgt/bin/$bin_name" --version)"
echo "    $bin_name --version => $version_output"
echo "$version_output" | grep -q "$bin_name" || {
    echo "error: --version output did not contain expected binary name" >&2
    exit 1
}

echo "==> Relocation proof passed"

# Take only what is inside the quotes: the line carries a trailing
# `# x-release-please-version` annotation, and dragging that into the filename
# produced an artifact with spaces (and a `#`) in its name, which silently
# truncated the release upload command.
pkg_version="$(sed -n 's/^version = "\([^"]*\)".*/\1/p' Cargo.toml | head -1)"
if [[ ! "$pkg_version" =~ ^[0-9]+\.[0-9]+\.[0-9]+ ]]; then
    echo "error: could not parse a version from Cargo.toml (got '$pkg_version')" >&2
    exit 1
fi
# A marker in the tarball root, so anything that later replaces this tree can
# prove it is replacing a tgt install rather than inferring it from the shape
# of the directory. It rides in the tarball, so it reaches every install route
# at once: the curl installer, a manual extraction, and Homebrew.
#
# The target triple is here beside the version because a tree and a download
# have to agree on it. Replacing an aarch64-apple-darwin tree with an
# x86_64-unknown-linux-gnu one fails at dyld load, which is the unrecoverable
# shape: the binary that would repair it can no longer start.
# The installer rides along, so `tgt update` can hand the swap back to it
# instead of carrying a second implementation of stage/rename/probe/rollback.
# It is the NEW release's copy that performs the swap, so a layout change
# applies itself rather than being executed by an older script that predates
# it — and because it is inside the tarball, `--require-signature` covers the
# code doing the replacing, not just the bytes being installed.
install -m 755 scripts/install.sh "$dist_dir/install.sh"

cat >"$dist_dir/.tgt-install" <<EOF
# Written by scripts/package.sh. Read by scripts/install.sh and \`tgt update\`
# to confirm a directory is a tgt tree before replacing it.
version=$pkg_version
target=$target
EOF

tarball="dist/${bin_name}-${pkg_version}-${target}.tar.gz"

echo "==> Creating tarball: $tarball"
tar -czf "$tarball" -C dist tgt

echo "==> Done"
echo "    dist dir: $dist_dir"
echo "    tarball:  $tarball"
