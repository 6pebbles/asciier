//! Cells to pixels: columns and rows, the font's fit, the output size, glyph tiles pasted on the theme's background.

use crate::Result;
use crate::cells::{self, CELL_PX, Grid, Inks};
use crate::convert::{Fit, Options, SIZE};
use crate::font::{self, Font, Ramp};
use crate::look::{self, Look};
use crate::picture;
use crate::theme::{Rgb, Theme};
use image::RgbImage;
use std::borrow::Cow;

/// What finds a picture's cells: columns, look, the theme's inks, the font's ramp and cell ratio. A copy works on another thread.
#[derive(Clone)]
pub struct Art {
    pub cols: u32,
    look: Look,
    inks: Inks,
    ramp: &'static Ramp,
    /// The font's cell ratio (width / height) at 20 px, for libchafa's geometry.
    ratio: f32,
}

impl Art {
    /// The picture's cells: reduced to 8 px a column by a box filter, tone table, cells at work factor 0.5 (row L6).
    pub fn cells(&mut self, picture: &RgbImage) -> Result<Grid> {
        let width = self.cols * CELL_PX;
        let mut picture = Cow::Borrowed(picture);
        if picture.width() > width {
            let height = picture.height() as f64 * width as f64 / picture.width() as f64;
            let height = (height.round_ties_even() as u32).max(1);
            picture = Cow::Owned(picture::resize_box(&picture, width, height));
        }
        if let Some(table) = self.look.tone() {
            look::apply(&table, picture.to_mut());
        }
        let (w, h, look) = (picture.width(), picture.height(), self.look);
        cells::grid(
            &picture,
            w,
            h,
            self.cols,
            None,
            self.ratio,
            look.glyphs,
            look.mono,
            &mut self.inks,
            self.ramp,
            0.5,
        )
    }
}

/// Everything a picture needs to be drawn: its cells' finder, theme, font, layout. Built once for a look, theme and size.
pub struct Style {
    pub art: Art,
    pub rows: Option<u32>,
    options: Options,
    theme: Theme,
    font: Font,
    /// The font size in px the tiles are drawn at.
    size: f64,
    cell_w: f64,
    cell_h: f64,
    /// Glyph tiles by glyph and slot: ceil(cell) px of RGB, drawn on first use.
    tiles: Vec<Option<Vec<u8>>>,
}

impl Style {
    /// For `options` and the colours to draw in, a source picture of `source` px (rows L1, L2): the columns, the font at 20 px.
    pub fn new(options: &Options, theme: Theme, source: (u32, u32)) -> Result<Style> {
        let look = options.look;
        let font = Font::open(font::path(look.bold)?)?;
        let cell = font.cell(SIZE);
        let (iw, ih) = (source.0 as f64, source.1 as f64);
        let cols = match (options.columns, options.width, options.height) {
            (Some(columns), _, _) => columns,
            (None, None, None) => 160,
            (None, width, height) => {
                let across = width.map_or(0.0, |w| (w as f64 / cell.w).ceil());
                let down = height.map_or(0.0, |h| (h as f64 * iw / (ih * cell.w)).ceil());
                across.max(down) as u32
            }
        };
        let art = Art {
            cols,
            look,
            inks: Inks::new(&theme, look.saturation, look.skin),
            ramp: font::ramp(look.bold)?,
            ratio: (cell.w / cell.h as f64) as f32,
        };
        Ok(Style {
            art,
            rows: None,
            options: options.clone(),
            theme,
            font,
            size: SIZE,
            cell_w: cell.w,
            cell_h: cell.h as f64,
            tiles: vec![None; 256 * 17],
        })
    }

    fn set_font(&mut self, size: f64) {
        let cell = self.font.cell(size);
        (self.size, self.cell_w, self.cell_h) = (size, cell.w, cell.h as f64);
        self.tiles.fill(None);
    }

    /// Take the row count; with columns and a size both given, the font size that fits them (row L3).
    pub fn layout(&mut self, rows: u32) {
        self.rows = Some(rows);
        let (width, height) = (self.options.width, self.options.height);
        if self.options.columns.is_some() && (width.is_some() || height.is_some()) {
            let across = width.map(|w| w as f64 / (self.art.cols as f64 * self.cell_w));
            let down = height.map(|h| h as f64 / (rows as f64 * self.cell_h));
            let fits = across.into_iter().chain(down);
            let fit = match self.options.fit {
                Fit::Cover => fits.fold(f64::MIN, f64::max),
                Fit::Contain => fits.fold(f64::MAX, f64::min),
            };
            self.set_font(SIZE * fit);
        }
    }

    /// The output size in px (row L4); `even`: rounded up to even numbers.
    pub fn size(&self, even: bool) -> (u32, u32) {
        let rows = self.rows.unwrap_or(0) as f64;
        let w = self
            .options
            .width
            .unwrap_or((self.art.cols as f64 * self.cell_w).round_ties_even() as u32);
        let h = self
            .options
            .height
            .unwrap_or((rows * self.cell_h).round_ties_even() as u32);
        if even { (w + w % 2, h + h % 2) } else { (w, h) }
    }

    fn ink(&self, slot: u8) -> Rgb {
        self.theme.ink(slot)
    }

    /// A cell that shows nothing: a space, or a glyph in color0's colour.
    fn blank(&self, glyph: u8, slot: u8) -> bool {
        glyph == b' ' || self.ink(slot) == self.theme.colors[0]
    }

    fn tile(&mut self, glyph: u8, slot: u8, tw: usize, th: usize) -> &[u8] {
        let (ink, ground) = (self.ink(slot), self.theme.background);
        let (font, size) = (&self.font, self.size);
        self.tiles[glyph as usize * 17 + slot.min(16) as usize].get_or_insert_with(|| {
            // Pillow's blend of a fill through an 8-bit mask
            let blend = |m: u32, c: usize| {
                let t = ground[c] as u32 * (255 - m) + ink[c] as u32 * m + 128;
                (((t >> 8) + t) >> 8) as u8
            };
            let mask = font.mask(glyph, size, tw, th);
            mask.into_iter()
                .flat_map(|m| [0, 1, 2].map(|c| blend(m as u32, c)))
                .collect()
        })
    }

    /// The cells drawn in the theme, cropped or banded to the output size (row L7): each tile pasted at its rounded position in cell order, blank cells left alone.
    pub fn paint(&mut self, grid: &Grid, even: bool) -> RgbImage {
        let (cols, rows) = (grid.cols, grid.rows);
        let (tw, th) = (self.cell_w.ceil() as usize, self.cell_h.ceil() as usize);
        let (cell_w, cell_h) = (self.cell_w, self.cell_h);
        let left = |c: usize| (c as f64 * cell_w).round_ties_even() as usize;
        let top = |r: usize| (r as f64 * cell_h).round_ties_even() as usize;
        // the grid's own picture: a tile may reach one px into the next cell, which that cell's tile then covers
        let (fw, fh) = (left(cols).max(1), top(rows).max(1));
        let ground = self.theme.background;
        let mut full = ground.repeat(fw * fh);
        for r in 0..rows {
            let (y0, y1) = (top(r), top(r + 1).min(fh));
            for c in 0..cols {
                let (glyph, slot) = (grid.chars[r * cols + c], grid.slots[r * cols + c]);
                if self.blank(glyph, slot) {
                    continue;
                }
                let x0 = left(c);
                let wide = tw.min(fw - x0);
                let tile = self.tile(glyph, slot, tw, th);
                for (ty, y) in (y0..y1).enumerate() {
                    full[(y * fw + x0) * 3..(y * fw + x0 + wide) * 3]
                        .copy_from_slice(&tile[ty * tw * 3..(ty * tw + wide) * 3]);
                }
            }
        }
        // centred crop to the exact size; a short picture gets background bands
        let (w, h) = self.size(even);
        let (w, h) = (w as usize, h as usize);
        if (w, h) == (fw, fh) {
            return RgbImage::from_raw(fw as u32, fh as u32, full).expect("sized buffer");
        }
        let mut out = ground.repeat(w * h);
        let (x0, y0) = (
            (w as i64 - fw as i64).div_euclid(2),
            (h as i64 - fh as i64).div_euclid(2),
        );
        let (sx, sy, dx, dy) = (
            (-x0).max(0) as usize,
            (-y0).max(0) as usize,
            x0.max(0) as usize,
            y0.max(0) as usize,
        );
        let (cw, ch) = ((fw - sx).min(w - dx), (fh - sy).min(h - dy));
        for y in 0..ch {
            let (from, to) = (((sy + y) * fw + sx) * 3, ((dy + y) * w + dx) * 3);
            out[to..to + cw * 3].copy_from_slice(&full[from..from + cw * 3]);
        }
        RgbImage::from_raw(w as u32, h as u32, out).expect("sized buffer")
    }

    /// A copy that draws the same cells `scale` times the size, font and output alike: the render page's preview.
    pub fn scaled(&self, scale: f64) -> Result<Style> {
        let (w, h) = self.size(false);
        let scaled = |n: u32| ((n as f64 * scale).round_ties_even() as u32).max(1);
        let mut options = self.options.clone();
        (options.width, options.height) = (Some(scaled(w)), Some(scaled(h)));
        // the cells keep the full size's layout, scaled, fractions and all: the font's whole-px line height would crop rows the file keeps
        Ok(Style {
            art: self.art.clone(),
            rows: self.rows,
            font: Font::open(font::path(options.look.bold)?)?,
            options,
            theme: self.theme.clone(),
            size: self.size * scale,
            cell_w: self.cell_w * scale,
            cell_h: self.cell_h * scale,
            tiles: vec![None; 256 * 17],
        })
    }
}
