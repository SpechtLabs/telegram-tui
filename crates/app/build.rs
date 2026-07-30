//! macOS `@rpath` mechanism for `tdlib-rs`'s `download-tdlib` output (architecture.md §9.2).
//!
//! `download-tdlib` builds a dynamic `libtdjson.dylib` under `tdlib-rs`'s own `OUT_DIR`
//! and its build script bakes in an absolute `-rpath` to that directory, which is not
//! relocatable. This script adds the two `@executable_path`-relative rpath entries every
//! binary of this crate needs, then, for dev builds, copies the downloaded dylib next to
//! the binary so the first of those entries actually resolves at dev runtime. Packaged
//! builds are handled separately by `scripts/package.sh` (T56).

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

fn main() {
    // Emitted unconditionally: bakes the packaged-layout rpath into every binary built
    // from this crate (dev and release), independent of whether the dylib copy below
    // finds anything to copy this run.
    println!("cargo:rustc-link-arg=-Wl,-rpath,@executable_path");
    println!("cargo:rustc-link-arg=-Wl,-rpath,@executable_path/../lib");

    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("cargo sets OUT_DIR for build scripts"));
    let profile_dir = match profile_dir(&out_dir) {
        Some(dir) => dir,
        None => return,
    };

    let dylibs = find_tdjson_dylibs(&profile_dir);
    if dylibs.is_empty() {
        // Nothing downloaded yet for this invocation (e.g. tdlib-rs hasn't run its own
        // build script). The absolute rpath tdlib-rs emits still covers `cargo run`/
        // `cargo test` from this same target directory.
        return;
    }

    for dest_dir in [profile_dir.clone(), profile_dir.join("deps")] {
        for dylib in &dylibs {
            copy_if_needed(dylib, &dest_dir);
        }
    }
}

/// `OUT_DIR` has the shape `target/<profile>/build/tgt-app-<hash>/out`; walk up to
/// `target/<profile>`, the directory dev binaries and `deps/` share.
fn profile_dir(out_dir: &Path) -> Option<PathBuf> {
    out_dir.parent()?.parent()?.parent().map(Path::to_path_buf)
}

/// Find every `libtdjson*.dylib` under `target/<profile>/build/tdlib-rs-*/out/`.
///
/// `tdlib-rs`'s downloaded archive contains both `libtdjson.dylib` and a versioned
/// `libtdjson.<ver>.dylib`; the dylib's own install name (`LC_ID_DYLIB`) is the versioned
/// one, so both must land next to the binary. Stale `tdlib-rs-*` build directories from
/// earlier invocations can coexist; when the same filename is found more than once, the
/// most recently modified copy wins.
fn find_tdjson_dylibs(profile_dir: &Path) -> Vec<PathBuf> {
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
        collect_dylibs(&path.join("out"), &mut found);
    }

    dedupe_by_filename_newest(found)
}

fn collect_dylibs(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_dylibs(&path, out);
        } else if is_tdjson_dylib(&path) {
            out.push(path);
        }
    }
}

fn is_tdjson_dylib(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with("libtdjson") && name.ends_with(".dylib"))
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
