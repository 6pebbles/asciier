//! Still images: opened upright with their transparency, SVG through resvg, cut and scaled for the screen, encoded as PNG.

use crate::Result;
use crate::theme::Rgb;
use image::{RgbImage, RgbaImage};
use std::path::Path;
use std::sync::Arc;

/// A still's pixels: RGBA when anything in it is transparent, else RGB.
#[derive(Clone)]
pub enum Pic {
    Rgb(RgbImage),
    Rgba(RgbaImage),
}

impl Pic {
    pub fn size(&self) -> (u32, u32) {
        match self {
            Pic::Rgb(i) => i.dimensions(),
            Pic::Rgba(i) => i.dimensions(),
        }
    }
}

/// An opened still: upright pixels (an SVG's first raster, `SVG_WIDTH` px wide) and the SVG behind them.
pub struct Still {
    pub pic: Pic,
    pub svg: Option<Arc<Svg>>,
}

pub enum Opened {
    Still(Still),
    /// Several frames to play (GIF, APNG, WebP): the media way.
    Animation,
    /// Over `MAX_PIXELS`; the message `<name> is too large to open (over 179 megapixels)`.
    TooLarge(String),
    NotImage,
}

/// Px an image may have: twice Pillow's MAX_IMAGE_PIXELS.
pub const MAX_PIXELS: u64 = 178_956_970;
pub const SVG_WIDTH: u32 = 1600;
/// Px on an SVG raster's long side, at most, when the converter rasterizes again.
pub const SVG_MAX: u32 = 8192;

/// Open the still at `path` (rows S1-S3, S18); a decoder panic counts as `NotImage`.
pub fn open(path: &Path) -> Opened {
    todo!("{path:?}")
}

/// Whether `path` holds a still by its header alone (an SVG by its suffix; an image over the limit counts).
pub fn identifies(path: &Path) -> bool {
    todo!("{path:?}")
}

pub fn is_svg(path: &Path) -> bool {
    path.extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("svg"))
}

/// Whether the folder listing takes `path` by its extension (the built-in decoders' extensions and `svg`).
pub fn listed(path: &Path) -> bool {
    todo!("{path:?}")
}

/// The picture as RGB, its transparency over `background`.
pub fn flatten(pic: &Pic, background: Rgb) -> RgbImage {
    todo!("{background:?} {:?}", pic.size())
}

/// The picture turned `quarter_turns` clockwise.
pub fn turned(pic: &Pic, quarter_turns: u8) -> Pic {
    todo!("{quarter_turns} {:?}", pic.size())
}

/// What shows under transparency.
pub enum Ground {
    Colour(Rgb),
    /// 8 px squares of #666666 and #999999, anchored at the top left.
    Checker,
}

/// The box (left, top, right, bottom in the picture's px, fractions allowed) scaled to `size`: nearest neighbour when `nearest`, bicubic otherwise; transparency over `ground`.
pub fn cut(pic: &Pic, bbox: [f64; 4], size: (u32, u32), nearest: bool, ground: Ground) -> RgbImage {
    todo!(
        "{bbox:?} {size:?} {nearest} {:?} {}",
        pic.size(),
        matches!(ground, Ground::Checker)
    )
}

pub fn checkerboard(w: u32, h: u32) -> RgbImage {
    todo!("{w} {h}")
}

/// The picture reduced by a box filter to `w` x `h`.
pub fn resize_box(picture: &RgbImage, w: u32, h: u32) -> RgbImage {
    todo!("{w} {h} {:?}", picture.dimensions())
}

/// The width to rasterize an SVG of `size` at for `want` px across: `want`, as long as its long side stays within `SVG_MAX`.
pub fn raster_width(want: u32, size: (u32, u32)) -> u32 {
    want.min((SVG_MAX as u64 * size.0 as u64 / size.0.max(size.1) as u64).max(1) as u32)
}

/// A parsed SVG, rasterized on demand.
pub struct Svg {
    tree: resvg::usvg::Tree,
}

impl Svg {
    pub fn load(path: &Path) -> Result<Svg> {
        todo!("{path:?}")
    }

    /// The size the SVG declares, rounded up to whole px.
    pub fn size(&self) -> (u32, u32) {
        todo!()
    }

    /// The whole SVG `width` px wide (`None`: at its declared size).
    pub fn raster(&self, width: Option<u32>) -> Result<RgbaImage> {
        todo!("{width:?}")
    }

    /// The box (left, top, right, bottom) of the SVG drawn `logical` px and turned `turns` quarters clockwise, rasterized alone at `size`.
    pub fn part(
        &self,
        logical: (u32, u32),
        turns: u8,
        bbox: [f64; 4],
        size: (u32, u32),
    ) -> Result<RgbaImage> {
        todo!("{logical:?} {turns} {bbox:?} {size:?}")
    }
}

/// Why `y` copied nothing.
pub enum NoCopy {
    TooLarge,
    Failed,
}

/// The still at `path` read again, at its own size (an SVG at the size it declares), upright and turned: PNG bytes, transparency kept, and its (width, height) (row S14).
pub fn original_png(path: &Path, turns: u8) -> std::result::Result<(Vec<u8>, (u32, u32)), NoCopy> {
    todo!("{path:?} {turns}")
}

/// PNG bytes of an RGB picture, fast compression.
pub fn png(picture: &RgbImage) -> Result<Vec<u8>> {
    todo!("{:?}", picture.dimensions())
}
