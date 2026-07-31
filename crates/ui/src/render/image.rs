//! Inline image rendering for already-downloaded media (spec §8.3), backed
//! by `ratatui-image`.
//!
//! `tgt-ui` never inspects the environment or queries the terminal — that is
//! `tgt-app`'s job (`crates/app/src/graphics.rs`, T38's other half). This
//! module only ever learns "does a graphics protocol exist, and which one"
//! through [`Capability`], a value handed in from the outside. Everything
//! below it is pure: decode bytes, fit them to a bounded cell area, encode
//! for the given protocol, render.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use image::DynamicImage;
use ratatui::Frame;
use ratatui::layout::{Rect, Size};
use ratatui_image::protocol::Protocol;
use ratatui_image::protocol::iterm2::Iterm2;
use ratatui_image::protocol::kitty::Kitty;
use ratatui_image::protocol::sixel::Sixel;
use ratatui_image::{FontSize, Image, Resize};

/// A terminal graphics protocol `tgt-app`'s startup probe found available.
/// Mirrors (without depending on) `tgt_app::graphics::GraphicsProtocol`'s
/// `Kitty`/`Iterm2`/`Sixel` variants; that enum's fourth variant, `None`,
/// has no counterpart here on purpose — "no protocol" is `Option::None` at
/// the call site ([`ImageArea::new`]), not a variant an image renderer would
/// ever have to match on and do nothing for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Capability {
    Kitty,
    Iterm2,
    Sixel,
}

/// Hard cap on inline image height, in terminal rows (spec §8.3: "bounded
/// height"). Keeps a single tall photo from dominating the viewport.
pub const MAX_IMAGE_ROWS: u16 = 15;

/// Cell aspect ratio used to size images when no queried terminal font size
/// is available. `tgt-ui` must not query the terminal itself (see module
/// docs), so this is a fixed, conservative stand-in — the same fallback
/// value `ratatui-image`'s own `Picker` falls back to when its terminal
/// query comes up empty. A future task may thread the real queried
/// `FontSize` through from `tgt-app` if the approximation proves visibly
/// wrong; nothing here changes shape to accommodate that later.
const FALLBACK_FONT_SIZE: FontSize = FontSize::new(10, 20);

/// Monotonic per-process id source for Kitty protocol image placements.
/// Kitty identifies each transmitted image by a client-chosen id; any
/// value that doesn't collide with a still-live id in the same session
/// works; a per-process counter is enough (no persistence, no env, no
/// randomness dependency needed just for this).
static NEXT_KITTY_ID: AtomicU32 = AtomicU32::new(1);

fn next_kitty_id() -> u32 {
    NEXT_KITTY_ID.fetch_add(1, Ordering::Relaxed)
}

struct CachedImage {
    path: PathBuf,
    /// The `render()` input area (after the `MAX_IMAGE_ROWS` cap) this
    /// protocol was encoded for. Part of the cache key: a resized pane
    /// invalidates the cache exactly like an explicit `invalidate()` would.
    bounded: Rect,
    /// The actual image footprint inside `bounded` (aspect-fit, so usually
    /// smaller).
    rect: Rect,
    protocol: Protocol,
}

/// A per-message inline-image slot.
///
/// One instance is meant to live alongside each rendered message that
/// carries a photo — no global registry, no shared cache. That keeps
/// per-message invalidation trivial: dropping or invalidating one
/// `ImageArea` can never affect another message's already-encoded protocol.
pub struct ImageArea {
    capability: Option<Capability>,
    cached: Option<CachedImage>,
}

impl ImageArea {
    /// `capability` is `None` when `tgt-app`'s probe found no usable
    /// terminal graphics protocol (or protocol support hasn't been wired up
    /// yet by the caller). Every [`ImageArea::render`] call then returns
    /// `false` immediately, telling the caller to fall back to the T37
    /// placeholder card — the "placeholder fallback always available"
    /// half of spec §8.3.
    pub fn new(capability: Option<Capability>) -> Self {
        Self {
            capability,
            cached: None,
        }
    }

    /// Clears any cached encoded protocol state.
    ///
    /// Graphics-protocol cells (especially Kitty's, which are addressed by
    /// image id and persist server-side until told otherwise) must be
    /// invalidated whenever the region they were drawn into scrolls out
    /// from under them, or stale pixels can bleed through the next frame
    /// ("ghosting", spec §8.3). This type has no notion of "the viewport
    /// scrolled" — that is a `view::conversation` concern (wired in T40) —
    /// so the caller decides when to call this; `ImageArea` only guarantees
    /// that after the call, the next `render()` re-decodes and re-encodes
    /// from scratch rather than reusing anything.
    ///
    /// MANUAL GATE CHECK (documented per plan.md T38 — ghosting cannot be
    /// asserted from a `TestBackend` buffer, since it's a property of the
    /// real terminal's graphics protocol state, not of what ratatui thinks
    /// it drew): scroll a conversation containing an inline image up and
    /// down several screens in a Kitty- or iTerm2-capable terminal and
    /// confirm no stale image fragment remains rendered outside the
    /// message's current on-screen position.
    pub fn invalidate(&mut self) {
        self.cached = None;
    }

    /// Renders the image at `path` into `area`, bounded to
    /// [`MAX_IMAGE_ROWS`]. Returns `true` if an image was drawn; `false`
    /// means "draw the placeholder card instead" and covers every failure
    /// mode uniformly:
    /// - no graphics protocol available (`capability` is `None`),
    /// - `area` has no room,
    /// - `path` can't be read, or
    /// - the bytes don't decode as an image.
    pub fn render(&mut self, area: Rect, path: &Path, f: &mut Frame) -> bool {
        let Some(capability) = self.capability else {
            return false;
        };
        if area.width == 0 || area.height == 0 {
            return false;
        }
        let bounded = Rect {
            height: area.height.min(MAX_IMAGE_ROWS),
            ..area
        };

        if let Some(cached) = &self.cached
            && cached.path == path
            && cached.bounded == bounded
        {
            f.render_widget(Image::new(&cached.protocol), cached.rect);
            return true;
        }

        let Ok(bytes) = std::fs::read(path) else {
            return false;
        };
        let Ok(dyn_img) = image::load_from_memory(&bytes) else {
            return false;
        };

        let target = Size::new(bounded.width, bounded.height);
        let (image, size) = fit(dyn_img, target);
        if size.width == 0 || size.height == 0 {
            return false;
        }

        let built = match capability {
            Capability::Kitty => {
                Kitty::new(image, size, next_kitty_id(), false).map(Protocol::Kitty)
            }
            Capability::Iterm2 => Iterm2::new(image, size, false).map(Protocol::ITerm2),
            Capability::Sixel => Sixel::new(image, size, false).map(Protocol::Sixel),
        };
        let Ok(protocol) = built else {
            return false;
        };

        let rect = Rect {
            x: bounded.x,
            y: bounded.y,
            width: size.width,
            height: size.height,
        };
        f.render_widget(Image::new(&protocol), rect);
        self.cached = Some(CachedImage {
            path: path.to_path_buf(),
            bounded,
            rect,
            protocol,
        });
        true
    }
}

/// Fits `image` into `target` (a cell-grid bound) at [`FALLBACK_FONT_SIZE`],
/// preserving aspect ratio, resizing pixel data only when the image's
/// natural cell size would exceed `target`. Mirrors what
/// `ratatui_image::picker::Picker::new_protocol` does internally, minus the
/// parts of its API this crate isn't allowed to call (its resize-decision
/// helper is private to `ratatui-image`, and its `Picker` constructors all
/// read the environment, which this crate must not do — see module docs).
fn fit(image: DynamicImage, target: Size) -> (DynamicImage, Size) {
    let natural = Resize::natural_size(&image, FALLBACK_FONT_SIZE);
    if natural.width <= target.width && natural.height <= target.height {
        return (image, natural);
    }
    let resize = Resize::Fit(None);
    let size = resize.size_for(&image, FALLBACK_FONT_SIZE, target);
    let resized = resize.resize(&image, FALLBACK_FONT_SIZE, size, None);
    (resized, size)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};

    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use super::*;

    static TEST_FILE_COUNTER: AtomicU64 = AtomicU64::new(0);

    /// A fresh path under the OS temp dir. `tgt-ui` carries no `tempfile`
    /// dev-dependency (only `tgt-app` does; see `crates/ui/Cargo.toml`), so
    /// tests write directly under `std::env::temp_dir()` with a
    /// counter-suffixed name to avoid collisions between tests running in
    /// parallel.
    fn scratch_path(name: &str) -> PathBuf {
        let n = TEST_FILE_COUNTER.fetch_add(1, AtomicOrdering::Relaxed);
        std::env::temp_dir().join(format!("tgt-ui-image-test-{n}-{name}"))
    }

    /// Writes a synthetic PNG, `width` x `height`, generated in-memory with
    /// the `image` crate (a normal dependency, not a test-only one).
    fn write_png(path: &Path, width: u32, height: u32) {
        let img = image::RgbImage::from_fn(width, height, |x, y| {
            image::Rgb([(x % 256) as u8, (y % 256) as u8, 128])
        });
        image::DynamicImage::ImageRgb8(img)
            .save(path)
            .expect("write synthetic test PNG");
    }

    fn render_once(area: ImageArea, area_rect: Rect, path: &Path) -> bool {
        let mut area = area;
        let mut terminal = Terminal::new(TestBackend::new(area_rect.width, area_rect.height))
            .expect("test backend");
        let mut drawn = false;
        terminal
            .draw(|f| drawn = area.render(area_rect, path, f))
            .expect("draw");
        drawn
    }

    #[test]
    fn no_protocol_falls_back_to_placeholder() {
        let path = scratch_path("no-protocol.png");
        write_png(&path, 40, 40);

        let image_area = ImageArea::new(None);
        let drawn = render_once(image_area, Rect::new(0, 0, 20, 20), &path);

        assert!(!drawn, "no capability must never render an image cell");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn image_height_bounded() {
        // Deliberately much taller (in rows, at the fallback cell aspect)
        // than MAX_IMAGE_ROWS: 10px-wide font cells, 20px-tall, so a
        // 100x2000 image is 10 cols x 100 rows unbounded.
        let path = scratch_path("tall.png");
        write_png(&path, 100, 2000);

        let mut image_area = ImageArea::new(Some(Capability::Kitty));
        let area_rect = Rect::new(0, 0, 40, 40);
        let mut terminal =
            Terminal::new(TestBackend::new(area_rect.width, area_rect.height)).expect("backend");
        let mut drawn = false;
        terminal
            .draw(|f| drawn = image_area.render(area_rect, &path, f))
            .expect("draw");

        assert!(drawn, "a valid PNG with Kitty capability must render");
        let cached = image_area.cached.as_ref().expect("caches after render");
        assert!(
            cached.rect.height <= MAX_IMAGE_ROWS,
            "rendered image height {} exceeds MAX_IMAGE_ROWS {}",
            cached.rect.height,
            MAX_IMAGE_ROWS
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn decode_failure_falls_back_to_placeholder() {
        let path = scratch_path("garbage.png");
        std::fs::write(&path, b"not actually an image").expect("write garbage bytes");

        let image_area = ImageArea::new(Some(Capability::Kitty));
        let drawn = render_once(image_area, Rect::new(0, 0, 20, 20), &path);

        assert!(!drawn, "undecodable bytes must never render an image cell");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn unreadable_path_falls_back_to_placeholder() {
        let path = scratch_path("does-not-exist.png");

        let image_area = ImageArea::new(Some(Capability::Sixel));
        let drawn = render_once(image_area, Rect::new(0, 0, 20, 20), &path);

        assert!(!drawn, "a missing file must never render an image cell");
    }

    #[test]
    fn zero_area_falls_back_to_placeholder() {
        let path = scratch_path("zero-area.png");
        write_png(&path, 10, 10);

        let image_area = ImageArea::new(Some(Capability::Iterm2));
        let drawn = render_once(image_area, Rect::new(0, 0, 0, 0), &path);

        assert!(!drawn);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn invalidate_clears_the_cache_so_the_next_render_redecodes() {
        let path = scratch_path("cache-then-invalidate.png");
        write_png(&path, 20, 20);

        let mut image_area = ImageArea::new(Some(Capability::Kitty));
        let area_rect = Rect::new(0, 0, 20, 20);
        let mut terminal =
            Terminal::new(TestBackend::new(area_rect.width, area_rect.height)).expect("backend");
        terminal
            .draw(|f| {
                image_area.render(area_rect, &path, f);
            })
            .expect("draw");
        assert!(image_area.cached.is_some());

        image_area.invalidate();
        assert!(
            image_area.cached.is_none(),
            "invalidate must drop the cached protocol"
        );

        terminal
            .draw(|f| {
                image_area.render(area_rect, &path, f);
            })
            .expect("draw");
        assert!(image_area.cached.is_some(), "re-renders after invalidate");
        let _ = std::fs::remove_file(&path);
    }
}
