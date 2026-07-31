//! Path/extension → `OutgoingFileKind` (spec §10: "MIME type determines
//! whether TDLib is asked to send a photo, video, audio, or document"), plus
//! the impure half of the send-file purity split documented on
//! `tgt_core::state::composer::looks_like_path`.
//!
//! `tgt-core` is pure: it cannot expand `~` (needs `$HOME`) and cannot check
//! whether a path exists (needs the filesystem) or sniff a file's real type
//! (needs its bytes). Those three things live here instead, in the one crate
//! allowed to touch the outside world. `crates/core/src/state/modal.rs`
//! always sends `OutgoingFileKind::Document` for exactly this reason;
//! `dispatch.rs`'s `resolve_outgoing_file` calls both functions below on the
//! confirmed path before the `SendMessageFile` request reaches TDLib —
//! upgrading the kind, and turning a path that isn't there into a failed
//! send rather than a request.

use std::env;
use std::path::{Path, PathBuf};

use tgt_core::td::request::OutgoingFileKind;

/// Extension → `OutgoingFileKind`, case-insensitive. No extension, or one
/// not on any of the three recognized lists, sends as a plain `Document` —
/// the fallback TDLib accepts for any file (spec §10's table: jpg→Photo,
/// mp4→Video, mp3→Audio, pdf→Document, unknown→Document).
pub fn kind_for(path: &Path) -> OutgoingFileKind {
    let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
        return OutgoingFileKind::Document;
    };
    match ext.to_ascii_lowercase().as_str() {
        "jpg" | "jpeg" | "png" | "gif" | "webp" | "heic" => OutgoingFileKind::Photo,
        "mp4" | "mov" | "mkv" | "avi" | "webm" => OutgoingFileKind::Video,
        "mp3" | "m4a" | "flac" | "ogg" | "wav" | "aac" => OutgoingFileKind::Audio,
        _ => OutgoingFileKind::Document,
    }
}

/// The impure half of the paste/`/send` purity split: tilde-expands a
/// leading `~/` against `$HOME` and returns the expanded path only if it
/// exists on disk. Every failure mode — no `$HOME` set, the path doesn't
/// exist, or any other `fs` error swallowed by `Path::exists` — collapses to
/// `None`; callers only need "usable to hand to TDLib" or not, not the
/// reason it isn't.
pub fn existing_path(s: &str) -> Option<PathBuf> {
    let expanded = match s.strip_prefix("~/") {
        Some(rest) => PathBuf::from(env::var_os("HOME")?).join(rest),
        None => PathBuf::from(s),
    };
    expanded.exists().then_some(expanded)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn media_kind_from_extension() {
        assert_eq!(kind_for(Path::new("photo.jpg")), OutgoingFileKind::Photo);
        assert_eq!(kind_for(Path::new("clip.mp4")), OutgoingFileKind::Video);
        assert_eq!(kind_for(Path::new("track.mp3")), OutgoingFileKind::Audio);
        assert_eq!(
            kind_for(Path::new("report.pdf")),
            OutgoingFileKind::Document
        );
        assert_eq!(
            kind_for(Path::new("archive.tar.gz")),
            OutgoingFileKind::Document
        );
        assert_eq!(
            kind_for(Path::new("no_extension")),
            OutgoingFileKind::Document
        );

        // Case-insensitive.
        assert_eq!(kind_for(Path::new("PHOTO.JPG")), OutgoingFileKind::Photo);
        assert_eq!(kind_for(Path::new("Clip.MOV")), OutgoingFileKind::Video);
        assert_eq!(kind_for(Path::new("Track.WAV")), OutgoingFileKind::Audio);
    }

    /// The app-layer half of the plan's
    /// `send_command_parses_path_and_validates_existence` — see the core
    /// half of the same-named test in `tgt-core`'s
    /// `state/composer.rs::tests` for the parse-only side and the doc
    /// comment on `looks_like_path` there for why the split exists at all.
    #[test]
    fn send_command_parses_path_and_validates_existence() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file_path = dir.path().join("photo.jpg");
        std::fs::File::create(&file_path)
            .and_then(|mut f| f.write_all(b"fake"))
            .expect("write temp file");

        assert_eq!(
            existing_path(file_path.to_str().unwrap()),
            Some(file_path.clone())
        );

        let missing = dir.path().join("does-not-exist.jpg");
        assert_eq!(existing_path(missing.to_str().unwrap()), None);
    }

    #[test]
    fn existing_path_expands_leading_tilde() {
        let home = env::var_os("HOME").expect("HOME set in test environment");
        let home = PathBuf::from(home);
        // Something virtually guaranteed to exist under $HOME in CI/dev:
        // $HOME itself. Use a relative marker that always resolves back to
        // it so the test needs no fixture file.
        let resolved = existing_path("~/.").expect("~/. should resolve to an existing path");
        assert_eq!(resolved, home.join("."));
    }

    #[test]
    fn existing_path_rejects_missing_files() {
        assert_eq!(existing_path("/definitely/not/a/real/path/xyz"), None);
    }
}
