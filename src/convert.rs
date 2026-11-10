//! `asciier convert`: an image or a video drawn as the viewer draws it, written as PNG or MP4.

use crate::Result;
use crate::look::Look;
use crate::theme::Theme;
use image::RgbImage;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Fit {
    Contain,
    Cover,
}

/// The converter's settings: what its options say, what the render page edits, what a screenshot uses.
#[derive(Clone, PartialEq, Debug)]
pub struct Options {
    pub look: Look,
    /// `--theme`: a kitty theme by name, or a file.
    pub theme: Option<String>,
    /// `--crop`: X, Y, W, H in percent of the picture.
    pub crop: [f64; 4],
    pub output: Option<PathBuf>,
    pub columns: Option<u32>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub fit: Fit,
    /// `--at`: a video's frame at that time, as a PNG.
    pub at: Option<f64>,
    pub input: PathBuf,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            look: Look::default(),
            theme: None,
            crop: FULL,
            output: None,
            columns: None,
            width: None,
            height: None,
            fit: Fit::Contain,
            at: None,
            input: PathBuf::new(),
        }
    }
}

/// `--crop`'s default: the whole picture.
pub const FULL: [f64; 4] = [0.0, 0.0, 100.0, 100.0];
/// Limits of `--columns`, and of `--width` and `--height`.
pub const COLUMNS: (u32, u32) = (8, 2000);
pub const PX: (u32, u32) = (16, 16384);
/// The font size in px a column is one cell of.
pub const SIZE: f64 = 20.0;

/// Run `asciier convert` with its arguments; the exit code (0, 1, 2, 130).
pub fn main(args: impl Iterator<Item = OsString>) -> u8 {
    let _ = args;
    todo!()
}

/// The options from the arguments; the `Err` is the usage error's text (exit 2), or the help text for `--help` (exit 0).
pub fn parse(args: impl Iterator<Item = OsString>) -> std::result::Result<Options, (u8, String)> {
    let _ = args;
    todo!()
}

/// The output file beside `source`: `<stem>-ascii.png`, `<stem>-ascii.mp4` for a video, `<stem>-ascii-<at>s.png` for a frame (row C3).
pub fn out_name(source: &Path, at: Option<f64>, video: bool) -> PathBuf {
    todo!("{source:?} {at:?} {video}")
}

/// 1920 px on the long side in the frame's shape, even numbers (row L5).
pub fn web_size(size: (u32, u32)) -> (u32, u32) {
    todo!("{size:?}")
}

/// `--crop` in px: left, top, right, bottom (row L9).
pub fn crop_rect(size: (u32, u32), spec: [f64; 4]) -> [i64; 4] {
    todo!("{size:?} {spec:?}")
}

/// The picture through `--crop`; outside it is black.
pub fn crop(picture: &RgbImage, spec: [f64; 4]) -> RgbImage {
    todo!("{:?} {spec:?}", picture.dimensions())
}

/// The colours to draw in: the current kitty theme, `--theme` over it, mono's two (rows C7, C8).
pub fn colours(options: &Options) -> Result<Theme> {
    todo!("{options:?}")
}

/// A still picture (already cropped, turned and flattened by the caller) drawn at full size: what a screenshot and the converter's still share.
pub fn draw(picture: &RgbImage, options: &Options) -> Result<RgbImage> {
    todo!("{:?} {options:?}", picture.dimensions())
}
