#!/usr/bin/env bash
# Builds and packages the `tgt` release binary into a relocatable
# dist/tgt/{bin,lib} layout with a self-contained @rpath to libtdjson,
# then tars it up. See docs/architecture.md §9.2 and crates/app/build.rs
# for the mechanism this script completes for packaged (non-dev) builds.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

bin_name="tgt"
target_dir="target/release"
dist_dir="dist/tgt"

echo "==> Building release binary (cargo build --release -p tgt-app)"
cargo build --release -p tgt-app

bin_src="$target_dir/$bin_name"
if [[ ! -f "$bin_src" ]]; then
    echo "error: expected binary at $bin_src, not found" >&2
    exit 1
fi

# build.rs (crates/app/build.rs) copies the downloaded tdlib-rs dylibs next
# to the binary in target/<profile>/ so `cargo run`/`cargo test` resolve them
# via the @executable_path rpath. Per T04's empirical findings (documented in
# build.rs and architecture.md §9.2), the download contains BOTH an
# unversioned `libtdjson.dylib` and a versioned `libtdjson.<ver>.dylib`, and
# the dylib's own LC_ID_DYLIB — and therefore the linked binary's
# LC_LOAD_DYLIB — references the VERSIONED name. Both files must exist in
# dist/tgt/lib for the binary to resolve at runtime.
# (bash 3.2 on macOS has no mapfile)
dylibs=()
while IFS= read -r line; do
    dylibs+=("$line")
done < <(find "$target_dir" -maxdepth 1 -name 'libtdjson*.dylib' -type f)
if [[ ${#dylibs[@]} -eq 0 ]]; then
    echo "error: no libtdjson*.dylib found in $target_dir (did build.rs run?)" >&2
    exit 1
fi

versioned_dylib=""
unversioned_dylib=""
for d in "${dylibs[@]}"; do
    base="$(basename "$d")"
    if [[ "$base" == "libtdjson.dylib" ]]; then
        unversioned_dylib="$d"
    else
        versioned_dylib="$d"
    fi
done

if [[ -z "$versioned_dylib" ]]; then
    echo "error: no versioned libtdjson.<ver>.dylib found in $target_dir" >&2
    exit 1
fi

versioned_name="$(basename "$versioned_dylib")"
# Extract "<ver>" from "libtdjson.<ver>.dylib".
dylib_version="${versioned_name#libtdjson.}"
dylib_version="${dylib_version%.dylib}"

echo "==> Found tdjson dylib version: $dylib_version"
echo "    versioned:   $versioned_dylib"
echo "    unversioned: ${unversioned_dylib:-<none found, will symlink>}"

echo "==> Laying out $dist_dir"
rm -rf "$dist_dir"
mkdir -p "$dist_dir/bin" "$dist_dir/lib"

cp "$bin_src" "$dist_dir/bin/$bin_name"
chmod +x "$dist_dir/bin/$bin_name"

cp "$versioned_dylib" "$dist_dir/lib/$versioned_name"
# Ship the unversioned name too (as a symlink to the real, versioned file)
# for any tool/consumer that looks for the bare `libtdjson.dylib` name.
# The binary itself only needs the versioned name on its LC_LOAD_DYLIB, so
# this is belt-and-suspenders, not load-bearing for `tgt` itself.
ln -sf "$versioned_name" "$dist_dir/lib/libtdjson.dylib"

echo "==> Fixing install names with install_name_tool"
# The dylib's own LC_ID_DYLIB is derived from its build-time absolute path
# (or already @rpath-relative, depending on how tdlib-rs built it) — force
# it to the versioned @rpath name we're shipping under.
install_name_tool -id "@rpath/$versioned_name" "$dist_dir/lib/$versioned_name"

# If the binary's LC_LOAD_DYLIB for tdjson recorded an absolute build-machine
# path (rather than @rpath already), rewrite it to match.
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

echo "==> Relocation proof: running the packaged binary from a moved directory"
proof_dir="$(mktemp -d)"
trap 'rm -rf "$proof_dir"' EXIT
cp -R "$dist_dir" "$proof_dir/tgt"

version_output=""
if ! version_output="$("$proof_dir/tgt/bin/$bin_name" --version 2>&1)"; then
    echo "    unsigned execution failed, retrying after ad-hoc codesign"
    codesign --force --sign - "$proof_dir/tgt/bin/$bin_name" "$proof_dir/tgt/lib/$versioned_name"
    codesign --force --sign - "$dist_dir/bin/$bin_name" "$dist_dir/lib/$versioned_name"
    version_output="$("$proof_dir/tgt/bin/$bin_name" --version)"
fi
echo "    $bin_name --version => $version_output"
echo "$version_output" | grep -q "$bin_name" || {
    echo "error: --version output did not contain expected binary name" >&2
    exit 1
}

echo "==> Relocation proof passed"

pkg_version="$(grep -m1 '^version' Cargo.toml | sed -E 's/version = "([^"]+)"/\1/')"
target_triple="aarch64-apple-darwin"
tarball="dist/${bin_name}-${pkg_version}-${target_triple}.tar.gz"

echo "==> Creating tarball: $tarball"
tar -czf "$tarball" -C dist tgt

echo "==> Done"
echo "    dist dir: $dist_dir"
echo "    tarball:  $tarball"
