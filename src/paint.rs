//! Cells to pixels: columns and rows, the font's fit, the output size, glyph tiles pasted on the theme's background.

use crate::Result;
use crate::cells::Grid;
use crate::convert::Options;
use crate::theme::Theme;
use image::RgbImage;

/// Everything a picture needs to be drawn: theme, font, layout. Built once for a look, theme and size.
pub struct Style {
    pub cols: u32,
    pub rows: Option<u32>,
}

impl Style {
    /// For `options` and the colours to draw in, a source picture of `source` px (rows L1, L2): the columns, the font at 20 px.
    pub fn new(options: &Options, theme: Theme, source: (u32, u32)) -> Result<Style> {
        todo!("{options:?} {theme:?} {source:?}")
    }

    /// The font's cell ratio (width / height) at 20 px, for libchafa's geometry.
    pub fn ratio(&self) -> f32 {
        todo!()
    }

    /// The picture's cells: reduced to 8 px a column by a box filter, tone table, cells at work factor 0.5 (row L6).
    pub fn art(&mut self, picture: &RgbImage) -> Grid {
        todo!("{:?}", picture.dimensions())
    }

    /// Take the row count; with columns and a size both given, the font size that fits them (row L3).
    pub fn layout(&mut self, rows: u32) {
        todo!("{rows}")
    }

    /// The output size in px (row L4); `even`: rounded up to even numbers.
    pub fn size(&self, even: bool) -> (u32, u32) {
        todo!("{even}")
    }

    /// The cells drawn in the theme, cropped or banded to the output size (row L7).
    pub fn paint(&mut self, grid: &Grid, even: bool) -> RgbImage {
        todo!("{} {even}", grid.cols)
    }

    /// A copy that draws the same cells `scale` times the size, font and output alike: the render page's preview.
    pub fn scaled(&self, scale: f64) -> Result<Style> {
        todo!("{scale}")
    }
}
