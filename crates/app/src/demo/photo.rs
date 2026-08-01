//! The one media file `--demo` ships: a synthetic pixel-art cat face, since
//! inline image rendering (architecture §4.9.1, `tgt_ui::render::image`)
//! resolves a real file on disk via `MessageContent::Photo`'s file id joined
//! against `FileSnapshot.local_path` — there is no way to show a picture in
//! the conversation pane without one.
//!
//! We were explicitly told not to invent a way to fetch a real photo from the
//! network (`--demo` is offline by construction — see the parent module
//! docs), and there is no real "picture of the cat" to ship in this
//! repository. So this draws one: a small, deliberately cartoonish 16x16
//! pixel-art face, scaled up and written out as a PNG. It reads as "yes,
//! obviously a placeholder" rather than attempting (and failing) to pass for
//! a real photo.
//!
//! # Swapping in a real photo
//!
//! Set `TGT_DEMO_PHOTO` to the path of a real image before running
//! `tgt --demo`, e.g.:
//!
//! ```sh
//! TGT_DEMO_PHOTO=~/Pictures/ferris.jpg tgt --demo
//! ```
//!
//! [`resolve`] uses it as-is (after decoding it once, to size the message's
//! declared width/height) instead of generating the placeholder. No rebuild
//! needed.

use std::path::{Path, PathBuf};

use color_eyre::eyre::{self, Context};

/// `TGT_DEMO_PHOTO` env var: an override path to a real image, so recording
/// a demo with an actual photo needs no rebuild. See the module docs.
const OVERRIDE_ENV: &str = "TGT_DEMO_PHOTO";

/// Pixel-art grid for the placeholder: a simple cat face. `.` background,
/// `F` fur, `W` white (inner ear/muzzle patch), `K` outline/eyes/nose, `P`
/// pink (nose/inner ear).
const GRID: [&str; 16] = [
    "................",
    ".K............K.",
    "KFK..........KFK",
    ".KFK........KFK.",
    "..KFFK....KFFK..",
    "..KFFFFKKKFFFFK.",
    ".KFFFFFFFFFFFFFK",
    "KFFWWFFFFFFWWFFK",
    "KFFWKFFFFFFKWFFK",
    "KFFWWFFFFFFWWFFK",
    "KFFFFFFPPFFFFFFK",
    ".KFFFFFPPPFFFFK.",
    ".KFFFWWWWWWFFFK.",
    "..KFFFFFFFFFFK..",
    "...KKFFFFFFKK...",
    "....KKKKKKKK....",
];

/// How many real pixels each `GRID` cell becomes. 320x320 total — small
/// enough to write instantly, large enough to look deliberate rather than
/// blocky at typical terminal cell sizes.
const SCALE: u32 = 20;

const BACKGROUND: [u8; 3] = [235, 235, 250]; // lavender
const FUR: [u8; 3] = [222, 142, 62]; // orange
const WHITE: [u8; 3] = [255, 255, 255];
const OUTLINE: [u8; 3] = [40, 36, 34]; // near-black
const NOSE: [u8; 3] = [255, 173, 186]; // pink

/// The photo to use for the demo's one media message: either the real file
/// named by `TGT_DEMO_PHOTO`, or a freshly drawn placeholder written into
/// `scratch_dir`. Either way, returns the path plus the pixel dimensions to
/// declare on `MessageContent::Photo` (rendering itself decodes the file
/// directly — see `tgt_ui::render::image` — these are only the message's
/// stated metadata).
pub fn resolve(scratch_dir: &Path) -> eyre::Result<(PathBuf, u32, u32)> {
    if let Ok(path) = std::env::var(OVERRIDE_ENV) {
        let path = PathBuf::from(path);
        let (width, height) = image::image_dimensions(&path).with_context(|| {
            format!(
                "{OVERRIDE_ENV} names {}, which could not be read as an image",
                path.display()
            )
        })?;
        return Ok((path, width, height));
    }

    let path = scratch_dir.join("demo-cat.png");
    let (width, height) = write_placeholder(&path)?;
    Ok((path, width, height))
}

/// `pub(super)` rather than private: `runtime.rs`'s tests reuse this to build
/// a throwaway photo file without going through `resolve`'s env-var check.
pub(super) fn write_placeholder(path: &Path) -> eyre::Result<(u32, u32)> {
    let side = GRID.len() as u32 * SCALE;
    let img = image::RgbImage::from_fn(side, side, |x, y| {
        let row = (y / SCALE) as usize;
        let col = (x / SCALE) as usize;
        let cell = GRID[row].as_bytes()[col];
        let rgb = match cell {
            b'F' => FUR,
            b'W' => WHITE,
            b'K' => OUTLINE,
            b'P' => NOSE,
            _ => BACKGROUND,
        };
        image::Rgb(rgb)
    });
    img.save(path).with_context(|| {
        format!(
            "failed to write the demo placeholder photo to {}",
            path.display()
        )
    })?;
    Ok((side, side))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // `TGT_DEMO_PHOTO` is process-wide; serialize the two tests below so a
    // parallel `cargo test` run can't have one observe the other's value.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn grid_rows_are_all_the_same_width() {
        let width = GRID[0].len();
        assert!(
            GRID.iter().all(|row| row.len() == width),
            "every row of the pixel-art grid must be the same length"
        );
    }

    #[test]
    fn placeholder_writes_a_decodable_square_png() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cat.png");

        let (width, height) = write_placeholder(&path).expect("placeholder should render");
        assert_eq!(width, height, "the placeholder is drawn as a square");
        assert_eq!(width, GRID.len() as u32 * SCALE);

        let decoded = image::open(&path).expect("the written file must be a valid image");
        assert_eq!(decoded.width(), width);
        assert_eq!(decoded.height(), height);
    }

    #[test]
    fn resolve_without_the_override_writes_into_scratch_dir() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        // SAFETY: serialized by ENV_LOCK.
        unsafe {
            std::env::remove_var(OVERRIDE_ENV);
        }
        let dir = tempfile::tempdir().unwrap();

        let (path, width, height) = resolve(dir.path()).expect("resolve should succeed");
        assert!(path.starts_with(dir.path()));
        assert!(path.exists());
        assert_eq!(width, height);
    }

    #[test]
    fn resolve_prefers_the_override_env_var() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real.png");
        write_placeholder(&real).unwrap();

        // SAFETY: serialized by ENV_LOCK.
        unsafe {
            std::env::set_var(OVERRIDE_ENV, &real);
        }
        let (path, ..) = resolve(dir.path()).expect("resolve should succeed");
        // SAFETY: serialized by ENV_LOCK.
        unsafe {
            std::env::remove_var(OVERRIDE_ENV);
        }

        assert_eq!(path, real);
    }
}
