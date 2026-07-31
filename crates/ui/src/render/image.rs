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

/// The cell footprint [`ImageArea::plan`] expects an image to occupy, so the
/// caller can reserve exactly that many rows in its own layout before
/// anything is decoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Footprint {
    pub cols: u16,
    pub rows: u16,
}

/// The terminal's cell size in pixels, measured by `tgt-app`
/// (`graphics::cell_size`) and handed in like [`Capability`] is — `tgt-ui`
/// must not measure the terminal itself (see module docs).
///
/// This is not a cosmetic detail. Kitty and iTerm2 both place an image by
/// *pixel* extent, and both derive the cells it covers by dividing that
/// extent by the terminal's real cell size. Encode a picture at
/// `cols * width` x `rows * height` pixels with the wrong `width`/`height`
/// and the terminal spreads it over more (or fewer) cells than the layout
/// reserved: with a too-large assumption the picture spills past the rows and
/// columns we marked, out of the conversation pane, and stays there — nothing
/// ever rewrites cells our own model believes are unchanged blanks. That is
/// the oversized-and-smearing photo this type exists to prevent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CellSize {
    pub width: u16,
    pub height: u16,
}

impl CellSize {
    /// Zero in either axis is not a cell size, and would divide by zero
    /// downstream; it falls back to [`CellSize::FALLBACK`]'s corresponding
    /// axis rather than being rejected, so a terminal that reports only one
    /// of the two still contributes the one it knows.
    pub fn new(width: u16, height: u16) -> Self {
        CellSize {
            width: if width == 0 {
                Self::FALLBACK.width
            } else {
                width
            },
            height: if height == 0 {
                Self::FALLBACK.height
            } else {
                height
            },
        }
    }

    /// What to use when the terminal reports no pixel dimensions at all. The
    /// same 10x20 `ratatui-image`'s own `Picker` falls back to — a plausible
    /// 1:2 cell, and nothing more than that. Every protocol this module
    /// speaks sizes by pixels, so a session that lands here can still place
    /// images somewhat wrong; it is a floor, not a target.
    pub const FALLBACK: CellSize = CellSize {
        width: 10,
        height: 20,
    };

    fn font_size(self) -> FontSize {
        FontSize::new(self.width, self.height)
    }
}

impl Default for CellSize {
    fn default() -> Self {
        Self::FALLBACK
    }
}

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
    cell: CellSize,
    cached: Option<CachedImage>,
    /// Pixel dimensions read from `path`'s header by [`ImageArea::plan`],
    /// which the caller runs once per frame while it is laying rows out.
    /// Memoized so that repeated frames of a stationary photo don't re-open
    /// the file just to ask how tall it is.
    dimensions: Option<(PathBuf, (u32, u32))>,
    /// A path whose last [`ImageArea::render`] returned `false`. Planning
    /// declines it from then on, so a file that reads far enough to report
    /// its dimensions but not far enough to decode (a truncated download)
    /// costs one frame of reserved rows rather than every frame's.
    failed: Option<PathBuf>,
}

impl ImageArea {
    /// `capability` is `None` when `tgt-app`'s probe found no usable
    /// terminal graphics protocol (or protocol support hasn't been wired up
    /// yet by the caller). Every [`ImageArea::render`] call then returns
    /// `false` immediately, telling the caller to fall back to the T37
    /// placeholder card — the "placeholder fallback always available"
    /// half of spec §8.3.
    pub fn new(capability: Option<Capability>, cell: CellSize) -> Self {
        Self {
            capability,
            cell,
            cached: None,
            dimensions: None,
            failed: None,
        }
    }

    /// How many cells this image would take inside `max_cols` x `max_rows`
    /// (itself capped to [`MAX_IMAGE_ROWS`]), or `None` when no image can be
    /// placed at all — no protocol, no room, a path whose header won't read,
    /// or one whose full decode already failed once.
    ///
    /// The point of this call is that it is *cheap*: `image_dimensions`
    /// parses the file's header and stops, where [`ImageArea::render`]
    /// decodes every pixel. A view has to know how many rows to reserve
    /// while it is still building lines, long before it has a `Frame` to
    /// draw into, and it must not pay for a full decode per frame to find
    /// out.
    ///
    /// Both passes reach their answer through [`bound`] and [`footprint`],
    /// from the same cell size, so "the rows planning reserved" and "the
    /// cells rendering fills" are the same arithmetic on the same inputs
    /// rather than two derivations that happen to agree.
    pub fn plan(&mut self, path: &Path, max_cols: u16, max_rows: u16) -> Option<Footprint> {
        self.capability?;
        if self.failed.as_deref() == Some(path) {
            return None;
        }
        let bounds = bound(Size::new(max_cols, max_rows));
        if bounds.width == 0 || bounds.height == 0 {
            return None;
        }
        let (width_px, height_px) = self.dimensions_of(path)?;
        Some(footprint(width_px, height_px, bounds, self.cell))
    }

    fn dimensions_of(&mut self, path: &Path) -> Option<(u32, u32)> {
        if let Some((cached_path, size)) = &self.dimensions
            && cached_path == path
        {
            return Some(*size);
        }
        let size = image::image_dimensions(path).ok()?;
        self.dimensions = Some((path.to_path_buf(), size));
        Some(size)
    }

    /// Clears any cached encoded protocol state.
    ///
    /// Graphics-protocol cells (especially Kitty's, which are addressed by
    /// image id and persist server-side until told otherwise) must be
    /// invalidated whenever the region they were drawn into scrolls out
    /// from under them, or stale pixels can bleed through the next frame
    /// ("ghosting", spec §8.3). This type has no notion of "the viewport
    /// scrolled" — that is `render::state::RenderState`'s job, which owns
    /// every live `ImageArea` and invalidates the lot whenever the frame's
    /// content moves under them — so the caller decides when to call this;
    /// `ImageArea` only guarantees that after the call, the next `render()`
    /// re-decodes and re-encodes from scratch rather than reusing anything.
    ///
    /// The memoized header dimensions and the "this path failed to decode"
    /// note go with it: both are judgments about a file that may since have
    /// finished downloading, and re-reading a header is the cheap half of
    /// what this call already throws away.
    ///
    /// This is only half of what "the image must go away" needs, and the
    /// smaller half: dropping the encoded protocol stops *us* from drawing
    /// it, and does nothing about the pixels the terminal was already told
    /// to draw. Those are reclaimed when the cells they cover are written
    /// again, which is `RenderState::take_repaint_request`'s job — see that
    /// module's "Erasing, as opposed to forgetting".
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
        self.dimensions = None;
        self.failed = None;
    }

    /// Renders the image at `path` into `area`, bounded to
    /// [`MAX_IMAGE_ROWS`]. Returns `true` if an image was drawn; `false`
    /// means "draw the placeholder card instead" and covers every failure
    /// mode uniformly:
    /// - no graphics protocol available (`capability` is `None`),
    /// - `area` has no room,
    /// - `path` can't be read, or
    /// - the bytes don't decode as an image.
    ///
    /// Never draws a cell outside `area`. That is a stronger promise than it
    /// looks: a `Cell` write past the buffer is dropped, but a protocol
    /// placement is pixels the terminal owns, and one that overhangs its
    /// area lands somewhere nothing in this pane will ever rewrite.
    pub fn render(&mut self, area: Rect, path: &Path, f: &mut Frame) -> bool {
        let Some(capability) = self.capability else {
            return false;
        };
        // The caller has already clipped `area` to its pane; clipping again
        // to the frame is what makes "never a cell outside" hold even if a
        // future caller forgets, since the protocol's pixels do not stop at
        // the buffer's edge the way a `Cell` write does.
        let area = area.intersection(f.area());
        let capped = bound(Size::new(area.width, area.height));
        if capped.width == 0 || capped.height == 0 {
            return false;
        }
        let bounded = Rect {
            x: area.x,
            y: area.y,
            width: capped.width,
            height: capped.height,
        };

        if let Some(cached) = &self.cached
            && cached.path == path
            && cached.bounded == bounded
        {
            f.render_widget(Image::new(&cached.protocol), cached.rect);
            return true;
        }

        // Every failure from here on is a property of this file rather than
        // of the area it was asked to fill, so it is worth remembering:
        // `plan` declines the path afterwards and the caller keeps drawing
        // the placeholder card instead of reserving rows nothing can fill.
        let Some((protocol, size)) = self.encode(capability, bounded, path) else {
            self.failed = Some(path.to_path_buf());
            return false;
        };

        // `fit` cannot exceed `bounded` — but if it ever did, `Image` would
        // silently draw nothing while this returned `true`, leaving blank
        // reserved rows and no card. Falling back is the honest answer, and
        // unlike the failures above it says nothing about the file, so it is
        // not remembered.
        if size.width > bounded.width || size.height > bounded.height {
            return false;
        }
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

    /// Reads, decodes, fits and protocol-encodes `path` for `bounded`.
    /// `None` is the single "this file cannot be drawn" answer its caller
    /// records; the individual reasons (unreadable, undecodable, fits in no
    /// cells, protocol encoder refused it) are not distinguished because
    /// nothing downstream would treat them differently — all four mean "the
    /// placeholder card stands in".
    fn encode(
        &self,
        capability: Capability,
        bounded: Rect,
        path: &Path,
    ) -> Option<(Protocol, Size)> {
        let bytes = std::fs::read(path).ok()?;
        let dyn_img = image::load_from_memory(&bytes).ok()?;

        let target = Size::new(bounded.width, bounded.height);
        let (image, size) = fit(dyn_img, target, self.cell);
        if size.width == 0 || size.height == 0 {
            return None;
        }

        let built = match capability {
            Capability::Kitty => {
                Kitty::new(image, size, next_kitty_id(), false).map(Protocol::Kitty)
            }
            Capability::Iterm2 => Iterm2::new(image, size, false).map(Protocol::ITerm2),
            Capability::Sixel => Sixel::new(image, size, false).map(Protocol::Sixel),
        };
        Some((built.ok()?, size))
    }
}

/// The single place a caller's "you may have this much room" becomes the box
/// an image is actually fitted into: [`MAX_IMAGE_ROWS`] applied, nothing
/// else. Both [`ImageArea::plan`] and [`ImageArea::render`] go through it, so
/// the rows one reserves and the rows the other fills cannot drift apart by
/// one pass capping and the other not.
fn bound(available: Size) -> Size {
    Size::new(available.width, available.height.min(MAX_IMAGE_ROWS))
}

/// The cell footprint an image of `width_px` x `height_px` gets inside
/// `target`, without a decoded image in hand. Mirrors [`fit`] — natural size
/// when it already fits, otherwise `Resize::Fit`'s proportional shrink — in
/// the pixel arithmetic `ratatui-image` performs internally, so the rows
/// [`ImageArea::plan`] reserves and the rows [`ImageArea::render`] fills are
/// the same rows. (`Resize::size_for` would answer this directly but wants a
/// `DynamicImage`, i.e. the full decode this exists to avoid.)
fn footprint(width_px: u32, height_px: u32, target: Size, cell: CellSize) -> Footprint {
    let natural = round_to_cells(width_px, height_px, cell);
    if natural.cols <= target.width && natural.rows <= target.height {
        return natural;
    }
    let available_px = (
        u32::from(target.width) * u32::from(cell.width),
        u32::from(target.height) * u32::from(cell.height),
    );
    let ratio = f64::from(available_px.0.min(width_px)) / f64::from(width_px);
    let ratio = ratio.min(f64::from(available_px.1.min(height_px)) / f64::from(height_px));
    round_to_cells(
        ((f64::from(width_px) * ratio).round() as u32).max(1),
        ((f64::from(height_px) * ratio).round() as u32).max(1),
        cell,
    )
}

fn round_to_cells(width_px: u32, height_px: u32, cell: CellSize) -> Footprint {
    Footprint {
        cols: (width_px as f32 / f32::from(cell.width)).ceil() as u16,
        rows: (height_px as f32 / f32::from(cell.height)).ceil() as u16,
    }
}

/// Fits `image` into `target` (a cell-grid bound) at the terminal's real
/// cell size, preserving aspect ratio, resizing pixel data only when the
/// image's natural cell size would exceed `target`. Mirrors what
/// `ratatui_image::picker::Picker::new_protocol` does internally, minus the
/// parts of its API this crate isn't allowed to call (its resize-decision
/// helper is private to `ratatui-image`, and its `Picker` constructors all
/// read the environment, which this crate must not do — see module docs).
fn fit(image: DynamicImage, target: Size, cell: CellSize) -> (DynamicImage, Size) {
    let font_size = cell.font_size();
    let natural = Resize::natural_size(&image, font_size);
    if natural.width <= target.width && natural.height <= target.height {
        return (image, natural);
    }
    let resize = Resize::Fit(None);
    let size = resize.size_for(&image, font_size, target);
    let resized = resize.resize(&image, font_size, size, None);
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

        let image_area = ImageArea::new(None, CellSize::FALLBACK);
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

        let mut image_area = ImageArea::new(Some(Capability::Kitty), CellSize::FALLBACK);
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

        let image_area = ImageArea::new(Some(Capability::Kitty), CellSize::FALLBACK);
        let drawn = render_once(image_area, Rect::new(0, 0, 20, 20), &path);

        assert!(!drawn, "undecodable bytes must never render an image cell");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn unreadable_path_falls_back_to_placeholder() {
        let path = scratch_path("does-not-exist.png");

        let image_area = ImageArea::new(Some(Capability::Sixel), CellSize::FALLBACK);
        let drawn = render_once(image_area, Rect::new(0, 0, 20, 20), &path);

        assert!(!drawn, "a missing file must never render an image cell");
    }

    #[test]
    fn zero_area_falls_back_to_placeholder() {
        let path = scratch_path("zero-area.png");
        write_png(&path, 10, 10);

        let image_area = ImageArea::new(Some(Capability::Iterm2), CellSize::FALLBACK);
        let drawn = render_once(image_area, Rect::new(0, 0, 0, 0), &path);

        assert!(!drawn);
        let _ = std::fs::remove_file(&path);
    }

    /// Wide, tall, square, and one that fits without any resizing at all.
    const SHAPES: [(u32, u32); 4] = [(400, 100), (100, 2000), (300, 300), (30, 40)];

    /// The contract the conversation view depends on: the rows `plan`
    /// reserves are the rows `render` fills. A mismatch is not a crash, but
    /// it is a visible gap (reserved too many) or a clipped image (too few),
    /// and the two derive their answer from different inputs — a file header
    /// vs. a decoded image — so nothing but a test keeps them agreeing.
    ///
    /// Run at several cell sizes, because the cell size is now measured from
    /// the terminal rather than assumed: every one of them has to hold, not
    /// just the 10x20 both passes used to hard-code.
    #[test]
    fn plan_reserves_the_rows_render_actually_fills() {
        for cell in [
            CellSize::FALLBACK,
            CellSize::new(7, 15),
            CellSize::new(9, 18),
            CellSize::new(20, 40),
        ] {
            for (i, (w, h)) in SHAPES.into_iter().enumerate() {
                let path = scratch_path(&format!("plan-{}-{}-{i}.png", cell.width, cell.height));
                write_png(&path, w, h);

                let mut image_area = ImageArea::new(Some(Capability::Kitty), cell);
                let planned = image_area
                    .plan(&path, 30, 40)
                    .expect("a readable PNG with a capability plans a footprint");
                assert!(
                    planned.rows <= MAX_IMAGE_ROWS && planned.cols <= 30,
                    "{w}x{h} at {cell:?}: plan {planned:?} escaped its bounds"
                );

                let area_rect = Rect::new(0, 0, 30, planned.rows);
                let mut terminal = Terminal::new(TestBackend::new(40, 40)).expect("backend");
                let mut drawn = false;
                terminal
                    .draw(|f| drawn = image_area.render(area_rect, &path, f))
                    .expect("draw");
                assert!(drawn, "{w}x{h} at {cell:?}: should have rendered");

                let rect = image_area.cached.as_ref().expect("cached").rect;
                assert_eq!(
                    (rect.width, rect.height),
                    (planned.cols, planned.rows),
                    "{w}x{h} at {cell:?}: plan disagrees with the rendered footprint"
                );
                let _ = std::fs::remove_file(&path);
            }
        }
    }

    /// The property the leaked-fragment bug violated: whatever the file's
    /// aspect ratio and whatever the caller asks for, the cells drawn stay
    /// inside the area handed in. A protocol placement that runs one column
    /// past its area is pixels in a region nothing in the conversation pane
    /// will ever rewrite.
    #[test]
    fn a_rendered_image_never_exceeds_the_area_it_was_given() {
        // Deliberately awkward areas, including ones far taller than
        // MAX_IMAGE_ROWS and ones a single cell wide.
        for area_rect in [
            Rect::new(0, 0, 30, 40),
            Rect::new(3, 2, 12, 4),
            Rect::new(0, 0, 1, 30),
            Rect::new(10, 10, 25, 1),
        ] {
            for (i, (w, h)) in SHAPES.into_iter().enumerate() {
                let path = scratch_path(&format!(
                    "bounds-{}-{}-{i}.png",
                    area_rect.x, area_rect.width
                ));
                write_png(&path, w, h);

                let mut image_area = ImageArea::new(Some(Capability::Kitty), CellSize::new(7, 15));
                let mut terminal = Terminal::new(TestBackend::new(60, 60)).expect("backend");
                terminal
                    .draw(|f| {
                        image_area.render(area_rect, &path, f);
                    })
                    .expect("draw");

                if let Some(cached) = image_area.cached.as_ref() {
                    let drawn = cached.rect;
                    assert!(
                        drawn.right() <= area_rect.right()
                            && drawn.bottom() <= area_rect.bottom()
                            && drawn.x >= area_rect.x
                            && drawn.y >= area_rect.y,
                        "{w}x{h}: drew {drawn:?} outside {area_rect:?}"
                    );
                    assert!(
                        drawn.height <= MAX_IMAGE_ROWS,
                        "{w}x{h}: drew {} rows, over the {MAX_IMAGE_ROWS}-row cap",
                        drawn.height
                    );
                }
                let _ = std::fs::remove_file(&path);
            }
        }
    }

    /// A rect that reaches past the frame is clipped rather than trusted:
    /// `Frame`'s buffer stops writes at its edge, but a graphics protocol's
    /// pixels do not, so the area has to be narrowed before it is encoded.
    #[test]
    fn an_area_reaching_past_the_frame_is_clipped_to_it() {
        let path = scratch_path("past-the-frame.png");
        write_png(&path, 300, 300);

        let mut image_area = ImageArea::new(Some(Capability::Kitty), CellSize::FALLBACK);
        let mut terminal = Terminal::new(TestBackend::new(20, 20)).expect("backend");
        terminal
            .draw(|f| {
                image_area.render(Rect::new(15, 15, 40, 40), &path, f);
            })
            .expect("draw");

        let drawn = image_area.cached.as_ref().expect("cached").rect;
        assert!(
            drawn.right() <= 20 && drawn.bottom() <= 20,
            "drew {drawn:?} outside the 20x20 frame"
        );
        let _ = std::fs::remove_file(&path);
    }

    /// The same picture in the same box is the same size whatever the
    /// terminal reports — but a terminal with smaller cells fits *more* of
    /// them under one image, which is exactly the arithmetic that decides
    /// how far the picture spreads on screen. A hard-coded cell size gets
    /// this wrong in whichever direction the real terminal differs.
    #[test]
    fn the_measured_cell_size_decides_the_footprint() {
        let path = scratch_path("cell-size.png");
        // 210x150 px: 21x7.5 cells at 10x20, 30x10 cells at 7x15.
        write_png(&path, 210, 150);

        let footprint_at = |cell: CellSize| {
            ImageArea::new(Some(Capability::Kitty), cell)
                .plan(&path, 40, 40)
                .expect("plans")
        };

        assert_eq!(
            footprint_at(CellSize::FALLBACK),
            Footprint { cols: 21, rows: 8 }
        );
        assert_eq!(
            footprint_at(CellSize::new(7, 15)),
            Footprint { cols: 30, rows: 10 }
        );
        let _ = std::fs::remove_file(&path);
    }

    /// A cell size can arrive from an ioctl that filled in one axis and not
    /// the other; a zero there would divide by zero in `round_to_cells`.
    #[test]
    fn a_zero_axis_falls_back_rather_than_dividing_by_it() {
        assert_eq!(CellSize::new(0, 0), CellSize::FALLBACK);
        assert_eq!(
            CellSize::new(0, 15),
            CellSize::new(CellSize::FALLBACK.width, 15)
        );
        assert_eq!(
            CellSize::new(7, 0),
            CellSize::new(7, CellSize::FALLBACK.height)
        );
        assert_eq!(CellSize::default(), CellSize::FALLBACK);
    }

    #[test]
    fn plan_declines_without_capability_or_room_or_a_readable_file() {
        let path = scratch_path("plan-declines.png");
        write_png(&path, 40, 40);

        assert!(
            ImageArea::new(None, CellSize::FALLBACK)
                .plan(&path, 20, 20)
                .is_none(),
            "no capability plans nothing"
        );
        let mut with_capability = ImageArea::new(Some(Capability::Kitty), CellSize::FALLBACK);
        assert!(with_capability.plan(&path, 0, 20).is_none(), "no columns");
        assert!(with_capability.plan(&path, 20, 0).is_none(), "no rows");
        assert!(
            with_capability
                .plan(&scratch_path("plan-missing.png"), 20, 20)
                .is_none(),
            "a missing file plans nothing"
        );
        let _ = std::fs::remove_file(&path);
    }

    /// Bytes that read far enough to report dimensions but not far enough to
    /// decode: `plan` can't see that coming, so `render` reports it once and
    /// `plan` declines from then on rather than reserving rows every frame
    /// that nothing will ever fill.
    #[test]
    fn a_failed_render_stops_the_path_from_being_planned_again() {
        let path = scratch_path("truncated.png");
        write_png(&path, 40, 40);
        let bytes = std::fs::read(&path).expect("read the valid PNG");
        std::fs::write(&path, &bytes[..bytes.len() / 2]).expect("truncate it");

        let mut image_area = ImageArea::new(Some(Capability::Kitty), CellSize::FALLBACK);
        assert!(
            image_area.plan(&path, 20, 20).is_some(),
            "the header alone still parses, so planning cannot tell yet"
        );

        let mut terminal = Terminal::new(TestBackend::new(20, 20)).expect("backend");
        let mut drawn = true;
        terminal
            .draw(|f| drawn = image_area.render(Rect::new(0, 0, 20, 20), &path, f))
            .expect("draw");
        assert!(!drawn, "truncated bytes must not render");
        assert!(
            image_area.plan(&path, 20, 20).is_none(),
            "planning must decline a path whose decode already failed"
        );

        // …until something invalidates, which is also how a file that was
        // still downloading gets a second chance.
        image_area.invalidate();
        assert!(image_area.plan(&path, 20, 20).is_some());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn invalidate_clears_the_cache_so_the_next_render_redecodes() {
        let path = scratch_path("cache-then-invalidate.png");
        write_png(&path, 20, 20);

        let mut image_area = ImageArea::new(Some(Capability::Kitty), CellSize::FALLBACK);
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
