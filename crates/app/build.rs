//! Dynamic-loader plumbing for `tdlib-rs`'s `download-tdlib` output (architecture.md §9.2).
//!
//! `download-tdlib` unpacks a prebuilt shared TDLib under `tdlib-rs`'s own `OUT_DIR`
//! and its build script bakes in an absolute `-rpath` to that directory, which is not
//! relocatable. This script gives each platform the relocatable equivalent, then, for
//! dev builds, copies the downloaded library next to the binary so that equivalent
//! actually resolves at dev runtime. Packaged builds are handled separately by
//! `scripts/package.sh` (T56).

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

fn main() {
    // The *target* OS, not the host: a build script runs on the host, but every
    // decision below is about the binary being produced.
    let target_os =
        env::var("CARGO_CFG_TARGET_OS").expect("cargo sets CARGO_CFG_TARGET_OS for build scripts");

    emit_rpath_link_args(&target_os);

    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("cargo sets OUT_DIR for build scripts"));
    let profile_dir = match profile_dir(&out_dir) {
        Some(dir) => dir,
        None => return,
    };

    let libs = find_tdjson_libs(&profile_dir, &target_os);
    if libs.is_empty() {
        // Nothing downloaded yet for this invocation (e.g. tdlib-rs hasn't run its own
        // build script). On macOS and Linux the absolute rpath tdlib-rs emits still
        // covers `cargo run`/`cargo test` from this same target directory; on Windows
        // the DLL is reachable at dev time only if it was copied on an earlier run.
        return;
    }

    for dest_dir in [profile_dir.clone(), profile_dir.join("deps")] {
        for lib in &libs {
            copy_if_needed(lib, &dest_dir);
        }
    }
}

/// Bakes the packaged-layout search path into every binary built from this crate (dev
/// and release), independent of whether the copy above finds anything to copy this run.
///
/// macOS and Linux spell the same idea differently — `@executable_path` against
/// `$ORIGIN` — but both are resolved by the loader relative to the binary itself, which
/// is what makes the packaged tree relocatable: `bin/tgt` finds TDLib whether it sits
/// beside the binary or in a sibling `lib/`. `$ORIGIN` reaches the linker without a
/// shell in between, so it is passed through literally and must not be escaped.
///
/// Windows has no rpath. Its loader searches the directory of the executable image and
/// `PATH`, neither of which the binary can carry, so `tdjson.dll` sitting next to the
/// `.exe` is the entire mechanism — the copy above at dev time, and the packaging
/// script for a release tree.
fn emit_rpath_link_args(target_os: &str) {
    let origin = match target_os {
        "macos" => "@executable_path",
        "linux" => "$ORIGIN",
        _ => return,
    };
    println!("cargo:rustc-link-arg=-Wl,-rpath,{origin}");
    println!("cargo:rustc-link-arg=-Wl,-rpath,{origin}/../lib");
}

/// `OUT_DIR` has the shape `target/<profile>/build/tgt-app-<hash>/out`; walk up to
/// `target/<profile>`, the directory dev binaries and `deps/` share.
fn profile_dir(out_dir: &Path) -> Option<PathBuf> {
    out_dir.parent()?.parent()?.parent().map(Path::to_path_buf)
}

/// Find every shared TDLib under `target/<profile>/build/tdlib-rs-*/out/`.
///
/// Stale `tdlib-rs-*` build directories from earlier invocations can coexist; when the
/// same filename is found more than once, the most recently modified copy wins.
fn find_tdjson_libs(profile_dir: &Path, target_os: &str) -> Vec<PathBuf> {
    let build_dir = profile_dir.join("build");
    let Ok(entries) = fs::read_dir(&build_dir) else {
        return Vec::new();
    };

    let mut found: Vec<PathBuf> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let is_tdlib_rs_dir = entry
            .file_name()
            .to_str()
            .is_some_and(|name| name.starts_with("tdlib-rs-"));
        if !is_tdlib_rs_dir || !path.is_dir() {
            continue;
        }
        collect_shared_libs(&path.join("out"), target_os, &mut found);
    }

    dedupe_by_filename_newest(found)
}

fn collect_shared_libs(dir: &Path, target_os: &str, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_shared_libs(&path, target_os, out);
        } else if is_tdjson_shared_lib(&path, target_os) {
            out.push(path);
        }
    }
}

/// Whether `path` is the shared TDLib for `target_os`, matching how `tdlib-rs` 1.4.0's
/// build script names it.
///
/// The macOS and Linux archives each contain both an unversioned name and a versioned
/// one (`libtdjson.<ver>.dylib`, `libtdjson.so.<ver>`), and it is the versioned name
/// that is recorded in the library's own metadata — its `LC_ID_DYLIB` install name on
/// macOS, its `SONAME` on Linux — and therefore the one the loader actually asks for.
/// Both have to land next to the binary, so both patterns match.
///
/// Windows has the single `tdjson.dll`, and it is the one artifact that lives under
/// `bin/` rather than `lib/` — which the recursive walk above already covers. The
/// neighbouring `tdjson.lib` is deliberately not matched: an import library is consumed
/// at link time from a search path tdlib-rs already emits, and is dead weight at
/// runtime.
fn is_tdjson_shared_lib(path: &Path, target_os: &str) -> bool {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    match target_os {
        "macos" => name.starts_with("libtdjson") && name.ends_with(".dylib"),
        "linux" => name.starts_with("libtdjson.so"),
        "windows" => name.eq_ignore_ascii_case("tdjson.dll"),
        _ => false,
    }
}

fn modified(path: &Path) -> SystemTime {
    fs::metadata(path)
        .and_then(|meta| meta.modified())
        .unwrap_or(SystemTime::UNIX_EPOCH)
}

/// When multiple `tdlib-rs-*` build directories contain a file of the same name, keep
/// only the most recently modified one so a stale directory never shadows the live build.
fn dedupe_by_filename_newest(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut by_name: std::collections::HashMap<std::ffi::OsString, PathBuf> =
        std::collections::HashMap::new();
    for path in paths {
        let Some(name) = path.file_name() else {
            continue;
        };
        match by_name.get(name) {
            Some(existing) if modified(existing) >= modified(&path) => {}
            _ => {
                by_name.insert(name.to_owned(), path);
            }
        }
    }
    by_name.into_values().collect()
}

/// Copy `src` into `dest_dir` under its own filename, skipping the copy when a
/// same-size, same-mtime file already exists there.
fn copy_if_needed(src: &Path, dest_dir: &Path) {
    if fs::create_dir_all(dest_dir).is_err() {
        return;
    }
    let Some(file_name) = src.file_name() else {
        return;
    };
    let dest = dest_dir.join(file_name);

    if let (Ok(src_meta), Ok(dest_meta)) = (fs::metadata(src), fs::metadata(&dest))
        && src_meta.len() == dest_meta.len()
        && src_meta.modified().ok() == dest_meta.modified().ok()
    {
        return;
    }

    if let Err(err) = fs::copy(src, &dest) {
        println!(
            "cargo:warning=failed to copy {} to {}: {err}",
            src.display(),
            dest.display()
        );
    }
}
