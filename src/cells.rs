//! A picture as cells: libchafa's glyph shapes, each cell's colour one of the theme's slots, flat cells given glyphs by brightness; and the cells as terminal text.
//!
//! libchafa picks a cell's glyph by shape: right on an edge, but on a flat area or a smooth gradient a few glyphs serve many shades, and the picture shows bands.
//! There the glyph comes from the cell's brightness: the ramp runs over all 95 glyphs by ink coverage, dithered between neighbours, so a gradient steps by single cells.
//! Brightness is linear light: a glyph of coverage c in a colour of luminance L lights a cell about c x L, so darker theme colours draw denser glyphs.

use crate::font::Ramp;
use crate::look::{Glyphs, Skin};
use crate::picture;
use crate::theme::{Recolour, Rgb, Theme, luminance};
use crate::{Result, err};
use std::ffi::{c_int, c_void};
use std::sync::OnceLock;

/// A cell's colour slot for the theme's foreground: libchafa's default colour, and all of mono.
pub const FG: u8 = 16;
/// Px a cell side that libchafa samples: its symbols are 8 x 8.
pub const CELL_PX: u32 = 8;
/// Px a side libchafa 1.18 takes, of a picture and of its canvas: over it the process ends (SIGSEGV).
const CHAFA_PX: u32 = 65535;
/// Linear-light deviation within a cell under which `mixed` counts it as flat.
const FLAT: f64 = 0.045;
/// Ordered dither's thresholds in sixteenths, by (row % 4, column % 4).
const BAYER: [[u8; 4]; 4] = [[0, 8, 2, 10], [12, 4, 14, 6], [3, 11, 1, 9], [15, 7, 13, 5]];
/// A slot's colour code: 30-37, 90-97, 39 for the foreground.
const CODES: [&[u8; 5]; 17] = [
    b"\x1b[30m",
    b"\x1b[31m",
    b"\x1b[32m",
    b"\x1b[33m",
    b"\x1b[34m",
    b"\x1b[35m",
    b"\x1b[36m",
    b"\x1b[37m",
    b"\x1b[90m",
    b"\x1b[91m",
    b"\x1b[92m",
    b"\x1b[93m",
    b"\x1b[94m",
    b"\x1b[95m",
    b"\x1b[96m",
    b"\x1b[97m",
    b"\x1b[39m",
];

// libchafa 1.x's values, from its headers.
const PIXEL_RGB8: c_int = 8;
const CANVAS_TRUECOLOR: c_int = 0;
/// Two colours: glyph shapes only.
const CANVAS_FGBG: c_int = 5;
const SYMBOLS_ASCII_AND_SPACE: c_int = 1 << 14 | 1;

#[link(name = "chafa")]
unsafe extern "C" {
    fn chafa_symbol_map_new() -> *mut c_void;
    fn chafa_symbol_map_add_by_tags(map: *mut c_void, tags: c_int);
    fn chafa_symbol_map_unref(map: *mut c_void);
    fn chafa_canvas_config_new() -> *mut c_void;
    fn chafa_canvas_config_set_geometry(config: *mut c_void, cols: c_int, rows: c_int);
    fn chafa_canvas_config_set_canvas_mode(config: *mut c_void, mode: c_int);
    fn chafa_canvas_config_set_symbol_map(config: *mut c_void, map: *const c_void);
    fn chafa_canvas_config_set_fg_only_enabled(config: *mut c_void, on: c_int);
    fn chafa_canvas_config_set_preprocessing_enabled(config: *mut c_void, on: c_int);
    fn chafa_canvas_config_set_work_factor(config: *mut c_void, factor: f32);
    fn chafa_canvas_config_unref(config: *mut c_void);
    fn chafa_canvas_new(config: *const c_void) -> *mut c_void;
    fn chafa_canvas_draw_all_pixels(
        canvas: *mut c_void,
        kind: c_int,
        pixels: *const u8,
        w: c_int,
        h: c_int,
        stride: c_int,
    );
    fn chafa_canvas_get_char_at(canvas: *mut c_void, x: c_int, y: c_int) -> u32;
    fn chafa_canvas_get_raw_colors_at(
        canvas: *mut c_void,
        x: c_int,
        y: c_int,
        fg: *mut c_int,
        bg: *mut c_int,
    );
    fn chafa_canvas_unref(canvas: *mut c_void);
    fn chafa_calc_canvas_geometry(
        w: c_int,
        h: c_int,
        cols: *mut c_int,
        rows: *mut c_int,
        ratio: f32,
        zoom: c_int,
        stretch: c_int,
    );
    fn chafa_set_n_threads(count: c_int);
}

/// `chars`: each cell's glyph (ASCII); `slots`: its colour (0-15 palette, 16 foreground); both row by row.
#[derive(Clone, PartialEq, Debug)]
pub struct Grid {
    pub cols: usize,
    pub rows: usize,
    pub chars: Vec<u8>,
    pub slots: Vec<u8>,
}

/// A theme set up for drawing: the slot mapping, each slot's linear luminance (16 colours, then the foreground), mono's slot.
#[derive(Clone)]
pub struct Inks {
    pub recolour: Recolour,
    pub luma: [f64; 17],
    pub mono: u8,
}

impl Inks {
    pub fn new(theme: &Theme, saturation: i32, skin: Skin) -> Inks {
        Inks {
            recolour: Recolour::new(&theme.colors, saturation, skin),
            luma: std::array::from_fn(|slot| luminance(theme.ink(slot as u8))),
            mono: theme.mono_slot(),
        }
    }
}

/// The cell grid a `w` x `h` px picture takes within `cols` x `rows` (`None`: as many rows as the columns need) at cell ratio width / height: libchafa's geometry, zoom on, stretch off.
/// The picture fills the grid in one direction, aspect kept; (0, 0) without a picture or room.
pub fn geometry(w: u32, h: u32, cols: u32, rows: Option<u32>, ratio: f32) -> (u32, u32) {
    let (w, h, mut cols) = (w as c_int, h as c_int, cols as c_int);
    let mut rows = rows.filter(|&rows| rows > 0).unwrap_or(100_000) as c_int;
    // SAFETY: both sizes are live for the call, which keeps neither pointer.
    unsafe { chafa_calc_canvas_geometry(w, h, &mut cols, &mut rows, ratio, 1, 0) };
    (cols.max(0) as u32, rows.max(0) as u32)
}

/// A libchafa canvas with a picture drawn on it.
struct Canvas(*mut c_void);

impl Canvas {
    /// `rgb` (packed, `w` x `h`) on `cols` x `rows` cells: ASCII and space, foreground only, truecolour or two colours.
    /// Preprocessing is off: it levels every picture, which would undo the tone table.
    fn draw(rgb: &[u8], w: u32, h: u32, cols: u32, rows: u32, mono: bool, work: f32) -> Canvas {
        assert_eq!(rgb.len(), w as usize * h as usize * 3, "packed RGB");
        let (w, h) = (w as c_int, h as c_int);
        let mode = if mono { CANVAS_FGBG } else { CANVAS_TRUECOLOR };
        // SAFETY: every pointer comes from libchafa and is released once; the canvas copies its configuration, the configuration its symbol map; `rgb` holds the `h` rows of `w * 3` bytes that the draw reads (checked above).
        unsafe {
            let symbols = chafa_symbol_map_new();
            chafa_symbol_map_add_by_tags(symbols, SYMBOLS_ASCII_AND_SPACE);
            let config = chafa_canvas_config_new();
            chafa_canvas_config_set_geometry(config, cols as c_int, rows as c_int);
            chafa_canvas_config_set_canvas_mode(config, mode);
            chafa_canvas_config_set_symbol_map(config, symbols);
            chafa_canvas_config_set_fg_only_enabled(config, 1);
            chafa_canvas_config_set_preprocessing_enabled(config, 0);
            chafa_canvas_config_set_work_factor(config, work);
            let canvas = chafa_canvas_new(config);
            chafa_canvas_config_unref(config);
            chafa_symbol_map_unref(symbols);
            chafa_canvas_draw_all_pixels(canvas, PIXEL_RGB8, rgb.as_ptr(), w, h, w * 3);
            Canvas(canvas)
        }
    }

    /// A cell's glyph and its colour in truecolour (0xRRGGBB; negative: the default colour). Reading equals printing the canvas and parsing the text.
    fn cell(&self, x: u32, y: u32) -> (u8, i32) {
        let (mut fg, mut bg) = (0, 0);
        // SAFETY: the canvas lives as long as `self`; libchafa checks the cell against its size.
        let glyph = unsafe {
            chafa_canvas_get_raw_colors_at(self.0, x as c_int, y as c_int, &mut fg, &mut bg);
            chafa_canvas_get_char_at(self.0, x as c_int, y as c_int)
        };
        (glyph as u8, fg)
    }
}

impl Drop for Canvas {
    fn drop(&mut self) {
        // SAFETY: the one reference `draw` took.
        unsafe { chafa_canvas_unref(self.0) }
    }
}

/// The picture (`rgb`: packed RGB of `w` x `h`, tone table already applied) as a `Grid`: libchafa's glyphs, slots through `inks`, then the ramp on picked cells by `glyphs`; mono: two-colour shapes, every slot `FG`.
/// `mixed` picks the flat cells, `density` all, `shape` none; a picked cell takes its mean colour's slot and the ramp glyph for its luminance against that slot's (mono: against `inks.luma[inks.mono]`).
/// A picture over 65535 px a side is reduced to fit first; a grid over 8191 cells a side is an error: libchafa takes neither.
#[allow(clippy::too_many_arguments)]
pub fn grid(
    rgb: &[u8],
    w: u32,
    h: u32,
    cols: u32,
    rows: Option<u32>,
    ratio: f32,
    glyphs: Glyphs,
    mono: bool,
    inks: &mut Inks,
    ramp: &Ramp,
    work: f32,
) -> Result<Grid> {
    let reduced;
    let (rgb, w, h) = if w.max(h) > CHAFA_PX {
        let scale = CHAFA_PX as f64 / w.max(h) as f64;
        let side = |px: u32| ((px as f64 * scale) as u32).max(1);
        reduced = picture::box_filter(
            rgb,
            w as usize,
            h as usize,
            side(w) as usize,
            side(h) as usize,
        );
        (&reduced[..], side(w), side(h))
    } else {
        (rgb, w, h)
    };
    let (cols, rows) = geometry(w, h, cols, rows, ratio);
    if cols.max(rows) > CHAFA_PX / CELL_PX {
        err!(
            "{cols} x {rows} cells: libchafa takes {} a side at most",
            CHAFA_PX / CELL_PX
        );
    }
    let mut grid = Grid {
        cols: cols as usize,
        rows: rows as usize,
        chars: Vec::new(),
        slots: Vec::new(),
    };
    if cols == 0 || rows == 0 {
        return Ok(grid);
    }
    // the statistics beside libchafa's drawing: about half a frame's work each
    let (canvas, cells) = std::thread::scope(|scope| {
        let cells = (glyphs != Glyphs::Shape).then(|| {
            scope.spawn(|| light(rgb, w as usize, h as usize, cols as usize, rows as usize))
        });
        (
            Canvas::draw(rgb, w, h, cols, rows, mono, work),
            cells.map(|thread| thread.join().expect("the statistics")),
        )
    });
    for at in 0..cols * rows {
        let (glyph, colour) = canvas.cell(at % cols, at / cols);
        let ink = [colour >> 16, colour >> 8, colour].map(|v| v as u8);
        let default = mono || colour < 0;
        let slot = if default { FG } else { inks.recolour.slot(ink) };
        grid.chars.push(glyph);
        grid.slots.push(slot);
    }
    let Some(cells) = cells else { return Ok(grid) };
    for (at, &(colour, light, deviation)) in cells.iter().enumerate() {
        if glyphs == Glyphs::Mixed && deviation >= FLAT as f32 {
            continue;
        }
        if !mono {
            grid.slots[at] = inks.recolour.slot(colour);
        }
        let slot = if mono { inks.mono } else { grid.slots[at] };
        let want = (light as f64 / inks.luma[slot.min(FG) as usize].max(1e-3)).clamp(0.0, 1.0);
        grid.chars[at] = ramp_glyph(ramp, want, at / grid.cols, at % grid.cols);
    }
    Ok(grid)
}

/// Per cell of `cols` x `rows` over the picture: mean colour, mean linear luminance and its deviation over 4 x 4 box-filtered samples, in single precision as the Python tools computed them (so the same cells come out).
fn light(rgb: &[u8], w: usize, h: usize, cols: usize, rows: usize) -> Vec<(Rgb, f32, f32)> {
    static SHARES: OnceLock<[[f32; 256]; 3]> = OnceLock::new();
    let shares = SHARES.get_or_init(|| {
        [0.2126, 0.7152, 0.0722].map(|weight| {
            std::array::from_fn(|byte| (weight * crate::theme::linear()[byte]) as f32)
        })
    });
    let (fw, fh) = (cols * 4, rows * 4);
    let fine = picture::box_filter(rgb, w, h, fw, fh);
    let colours = picture::box_filter(&fine, fw, fh, cols, rows);
    let light = fine.as_chunks::<3>().0.iter();
    let light: Vec<f32> = light
        .map(|p| shares[0][p[0] as usize] + shares[1][p[1] as usize] + shares[2][p[2] as usize])
        .collect();
    // a 4 x 4 block mean of the samples and of their squares as Pillow's box filter makes it on floats: along one axis, then the other (picture::vertical_first), each sum in double precision stored single
    let quarter = |four: [f32; 4]| {
        (four[0] as f64 * 0.25
            + four[1] as f64 * 0.25
            + four[2] as f64 * 0.25
            + four[3] as f64 * 0.25) as f32
    };
    let first = |four: [f32; 4]| [quarter(four), quarter(four.map(|v| v * v))];
    let then = |four: [[f32; 2]; 4]| [0, 1].map(|m| quarter(four.map(|pair| pair[m])));
    let means: Vec<[f32; 2]> = if picture::vertical_first(fw, fh, cols, rows) {
        let down: Vec<[f32; 2]> = (0..rows * fw)
            .map(|i| first([0, 1, 2, 3].map(|k| light[(i / fw * 4 + k) * fw + i % fw])))
            .collect();
        (0..rows * cols)
            .map(|i| then([0, 1, 2, 3].map(|k| down[i / cols * fw + i % cols * 4 + k])))
            .collect()
    } else {
        let across: Vec<[f32; 2]> = (0..fh * cols)
            .map(|i| first(light[i * 4..i * 4 + 4].try_into().expect("four samples")))
            .collect();
        (0..rows * cols)
            .map(|i| then([0, 1, 2, 3].map(|k| across[(i / cols * 4 + k) * cols + i % cols])))
            .collect()
    };
    let colours = colours.as_chunks::<3>().0;
    let cell = |(&[mean, squares], &colour): (&[f32; 2], &Rgb)| {
        (colour, mean, (squares - mean * mean).max(0.0).sqrt())
    };
    means.iter().zip(colours).map(cell).collect()
}

/// The ramp glyph for coverage `want` (0..1) in the cell at `row`, `col`: of its two ramp neighbours the denser when the fraction between them exceeds the cell's Bayer threshold.
fn ramp_glyph(ramp: &Ramp, want: f64, row: usize, col: usize) -> u8 {
    let cover = &ramp.cover;
    let above = cover.partition_point(|&cover| cover < want).clamp(1, 94);
    let fraction = (want - cover[above - 1]) / (cover[above] - cover[above - 1]).max(1e-6);
    let denser = fraction > BAYER[row % 4][col % 4] as f64 / 16.0 + 1.0 / 32.0;
    ramp.glyphs[if denser { above } else { above - 1 }]
}

/// Terminal text, a row of bytes per grid row: a colour code where the slot changes (30-37, 90-97, 39 for the foreground), slot 0 blank, 39 at the row's end; `mono`: that slot's code once a row, nothing else.
/// Slot 0 is ANSI black: libchafa's glyphs for dark cells, left blank.
pub fn text(grid: &Grid, mono: Option<u8>) -> Vec<Vec<u8>> {
    let code = |slot: u8| CODES[slot.min(FG) as usize];
    let row = |row: usize| {
        let cells = row * grid.cols..(row + 1) * grid.cols;
        let (chars, slots) = (&grid.chars[cells.clone()], &grid.slots[cells]);
        if let Some(slot) = mono {
            return [&code(slot)[..], chars].concat();
        }
        let mut text = Vec::with_capacity(grid.cols * 2);
        for (at, (&glyph, &slot)) in chars.iter().zip(slots).enumerate() {
            if at == 0 || slot != slots[at - 1] {
                text.extend(code(slot));
            }
            text.push(if slot == 0 { b' ' } else { glyph });
        }
        text.extend(code(FG));
        text
    };
    (0..grid.rows).map(row).collect()
}

/// libchafa's own thread count (-1: its default, one a core).
pub fn threads(count: i32) {
    // SAFETY: sets a number that later draws read.
    unsafe { chafa_set_n_threads(count) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::luminance;
    use std::ffi::{c_char, c_int};

    fn bl1nk() -> Theme {
        Theme::from_text(include_str!("../tests/bl1nk.conf")).unwrap()
    }

    fn inks() -> Inks {
        Inks::new(&bl1nk(), 100, Skin::Grey)
    }

    /// A ramp that needs no font: glyph 32 + i covers i / 94.
    fn ramp() -> Ramp {
        Ramp {
            glyphs: std::array::from_fn(|i| 32 + i as u8),
            cover: std::array::from_fn(|i| i as f64 / 94.0),
        }
    }

    /// tests/support.py gradient: a left-to-right grey ramp, `np.linspace(0, 255, w).astype(np.uint8)`.
    fn gradient(w: usize, h: usize) -> Vec<u8> {
        let step = 255.0 / (w - 1) as f64;
        let grey = |x: usize| {
            if x == w - 1 {
                255
            } else {
                (x as f64 * step) as u8
            }
        };
        (0..w * h).flat_map(|at| [grey(at % w); 3]).collect()
    }

    /// tests/test_chafa.py colourful.
    fn colourful(w: usize, h: usize) -> Vec<u8> {
        let colour = |x: usize, y: usize| [x * 255 / w, y * 255 / h, (x + y) * 255 / (w + h)];
        (0..w * h)
            .flat_map(|at| colour(at % w, at / w).map(|v| v as u8))
            .collect()
    }

    /// Black, white from column `at` on.
    fn edge(w: usize, h: usize, at: usize) -> Vec<u8> {
        (0..w * h)
            .flat_map(|px| [if px % w < at { 0 } else { 255 }; 3])
            .collect()
    }

    fn noise(w: usize, h: usize) -> Vec<u8> {
        let value = |x: u32, y: u32, c: u32| {
            let v = (x
                .wrapping_mul(1973)
                .wrapping_add(y.wrapping_mul(9277))
                .wrapping_add(c * 26699))
                ^ 0x5BD1_E995;
            (v.wrapping_mul(1_103_515_245).wrapping_add(12345) >> 16) as u8
        };
        (0..w * h)
            .flat_map(|at| [0, 1, 2].map(|c| value((at % w) as u32, (at / w) as u32, c)))
            .collect()
    }

    /// Cells of one colour each, `px` a side: greys in the upper half, colours below.
    fn flat(cols: usize, rows: usize, px: usize) -> Vec<u8> {
        let colour = |cx: usize, cy: usize| {
            if cy < rows / 2 {
                [(cy * cols + cx) * 255 / (cols * rows / 2 - 1); 3]
            } else {
                let r = cx * 255 / (cols - 1);
                [r, (cy - rows / 2) * 255 / (rows - rows / 2 - 1), 255 - r]
            }
        };
        (0..cols * px * rows * px)
            .flat_map(|at| colour(at % (cols * px) / px, at / (cols * px) / px).map(|v| v as u8))
            .collect()
    }

    #[allow(clippy::too_many_arguments)]
    fn cells(
        rgb: &[u8],
        w: u32,
        h: u32,
        cols: u32,
        rows: Option<u32>,
        ratio: f32,
        glyphs: Glyphs,
        mono: bool,
    ) -> Grid {
        let (mut inks, ramp) = (inks(), ramp());
        grid(
            rgb, w, h, cols, rows, ratio, glyphs, mono, &mut inks, &ramp, 0.2,
        )
        .unwrap()
    }

    fn lines(text: &[u8]) -> Vec<Vec<u8>> {
        text.split(|&b| b == b'\n').map(<[u8]>::to_vec).collect()
    }

    /// A text row without its colour codes.
    fn plain(row: &[u8]) -> Vec<u8> {
        let (mut out, mut code) = (Vec::new(), false);
        for &byte in row {
            match (code, byte) {
                (false, 0x1b) => code = true,
                (false, _) => out.push(byte),
                (true, _) => code = byte != b'm',
            }
        }
        out
    }

    #[test]
    fn libchafas_limits_are_kept() {
        // measured with libchafa 1.18.3: a picture 64 x 65536 px, or a canvas of 8 x 8193 cells, ends the process
        let tall = vec![200; 60 * 70_000 * 3];
        let art = cells(&tall, 60, 70_000, 8, None, 0.5, Glyphs::Mixed, false);
        // reduced to 56 x 65535 px first
        assert_eq!((art.cols, art.rows), (8, 4682));
        let mut inks = inks();
        let grid = |cols, ratio, inks: &mut Inks| {
            let narrow = vec![200; 64 * 600 * 3];
            let ramp = ramp();
            grid(
                &narrow,
                64,
                600,
                cols,
                None,
                ratio,
                Glyphs::Mixed,
                false,
                inks,
                &ramp,
                0.2,
            )
        };
        assert_eq!(grid(8, 0.5, &mut inks).map(|art| art.rows).ok(), Some(38));
        // the tallest canvas: 8 x 8191 cells, 65528 px
        let tallest = vec![200; 64 * 65528 * 3];
        let art = cells(&tallest, 64, 65528, 8, None, 1.0, Glyphs::Mixed, false);
        assert_eq!((art.cols, art.rows), (8, 8191));
        let refused = grid(2000, 0.5, &mut inks).err().map(|e| e.0);
        assert_eq!(
            refused.as_deref(),
            Some("2000 x 9375 cells: libchafa takes 8191 a side at most")
        );
        assert!(grid(1000, 2.0, &mut inks).is_err());
    }

    #[test]
    fn geometry_follows_the_cell_ratio() {
        // tests/test_chafa.py AsciiArtTest, tests/test_term.py test_font_ratio_sets_rows, row G1; every pair is lib/ascii_chafa.py's for the same arguments.
        assert_eq!(geometry(100, 100, 40, None, 0.5), (40, 20));
        assert_eq!(geometry(100, 100, 40, None, 9.0 / 20.0), (40, 18));
        assert_eq!(geometry(1920, 1080, 160, None, 12.0 / 27.0), (160, 41));
        assert_eq!(geometry(400, 300, 160, None, 12.0 / 27.0), (160, 54));
        assert_eq!(geometry(100, 400, 80, Some(30), 0.5), (15, 30));
        assert_eq!(geometry(400, 400, 60, Some(100), 0.5), (60, 30));
        assert_eq!(geometry(400, 400, 60, Some(100), 9.0 / 20.0), (60, 27));
        assert_eq!(geometry(400, 400, 60, Some(50), 9.0 / 20.0), (60, 27));
        assert_eq!(geometry(333, 211, 37, Some(11), 0.5), (35, 11));
        assert_eq!(geometry(1, 1000, 80, Some(24), 0.45), (1, 24));
        assert_eq!(geometry(1000, 1, 80, Some(24), 0.45), (80, 1));
        // zoom: a small picture fills the grid too
        assert_eq!(geometry(16, 9, 160, None, 12.0 / 27.0), (160, 41));
        assert_eq!(geometry(10, 10, 40, None, 0.5), (40, 20));
        // no picture or no room: no cells
        assert_eq!(geometry(0, 100, 40, None, 0.5), (0, 0));
        assert_eq!(geometry(100, 100, 0, None, 0.5), (0, 0));
        // 0 rows count as None, as Python's `rows or 100000`
        assert_eq!(geometry(100, 100, 40, Some(0), 0.5), (40, 20));
    }

    #[test]
    fn only_ascii_glyphs() {
        // tests/test_chafa.py test_only_ascii_glyphs
        let picture = colourful(240, 160);
        let art = cells(&picture, 240, 160, 60, None, 0.5, Glyphs::Shape, false);
        assert_eq!((art.cols, art.rows), (60, 20));
        assert_eq!((art.chars.len(), art.slots.len()), (1200, 1200));
        assert!(art.chars.iter().all(|c| (32..127).contains(c)));
        assert!(art.slots.iter().all(|&slot| slot <= FG));
    }

    #[test]
    fn libchafa_is_set_as_the_python_tools_set_it() {
        // rows G1, G3: the canvas's own record of lib/ascii_chafa.py:63-68 (cells, mode, foreground only, preprocessing, work factor).
        let flat = |v: u8| vec![v; 160 * 40 * 3];
        for (mono, work) in [(false, 0.2), (true, 0.5)] {
            let canvas = Canvas::draw(&flat(9), 160, 40, 20, 3, mono, work);
            let (mut cols, mut rows) = (0, 0);
            // SAFETY: the configuration belongs to the canvas, alive here; the getters only read it.
            let set = unsafe {
                let config = chafa_canvas_peek_config(canvas.0);
                chafa_canvas_config_get_geometry(config, &mut cols, &mut rows);
                (
                    chafa_canvas_config_get_canvas_mode(config),
                    chafa_canvas_config_get_fg_only_enabled(config),
                    chafa_canvas_config_get_preprocessing_enabled(config),
                    chafa_canvas_config_get_work_factor(config),
                )
            };
            assert_eq!(
                (cols, rows, set),
                (20, 3, (if mono { 5 } else { 0 }, 1, 0, work))
            );
        }
        // And by what each setting shows: the Python call with the setting flipped gave the other answer every time (2026-11-10).
        let shape =
            |rgb: &[u8], mono: bool| cells(rgb, 160, 40, 20, None, 0.5, Glyphs::Shape, mono);
        // foreground only: white is ink in white's slot, not a blank on a white ground
        let white = shape(&flat(255), false);
        assert!(white.chars.iter().all(|&c| c != b' ') && white.slots == [15; 60]);
        // truecolour: a cell's colour is the picture's, through Recolour
        for (colour, slot) in [([255, 0, 0], 9), ([0, 0, 255], 12), ([255, 128, 0], 3)] {
            assert_eq!(
                shape(&colour.repeat(160 * 40), false).slots,
                [slot; 60],
                "{colour:?}"
            );
        }
        // black is ink in slot 0, which the text leaves blank: dots, as lib/ascii_term.py grid() draws it (run 2026-11-12)
        let black = shape(&flat(0), false);
        assert_eq!(black.chars, [b'.'; 60]);
        assert_eq!(black.slots, [0; 60]);
        assert!(
            text(&black, None)
                .iter()
                .all(|row| plain(row) == [b' '; 20])
        );
        // two colours: glyphs for light, blanks for dark
        assert!(shape(&flat(255), true).chars.iter().all(|&c| c != b' '));
        assert_eq!(shape(&flat(0), true).chars, [b' '; 60]);
        // preprocessing off: two near greys are not pulled apart into blank and ink
        let pair: Vec<u8> = (0..160 * 40)
            .flat_map(|at| [if at % 160 < 80 { 100 } else { 120 }; 3])
            .collect();
        let pair = shape(&pair, true);
        assert!(pair.chars.iter().all(|&c| c == pair.chars[0]));
    }

    #[test]
    fn the_work_factor_changes_the_glyphs() {
        // rows G1: lib/ascii_term.py grid() on this noise at 60 x 20 cells, bl1nk, run 2026-11-12: libchafa's glyphs at work 0.2 (the viewer's, lib/ascii_term.py:29) and 0.5 (the converter's, bin/ascii-convert:206) differ in 2 of the 1200 cells in truecolour and in 432 in two colours
        let picture = noise(96, 64);
        let glyphs = |mono: bool, work: f32| {
            let (mut inks, ramp) = (inks(), ramp());
            let art = grid(
                &picture,
                96,
                64,
                60,
                None,
                0.5,
                Glyphs::Shape,
                mono,
                &mut inks,
                &ramp,
                work,
            );
            art.unwrap().chars
        };
        let differ = |mono: bool| {
            let (live, drawn) = (glyphs(mono, 0.2), glyphs(mono, 0.5));
            assert_eq!((live.len(), drawn.len()), (1200, 1200));
            live.iter().zip(&drawn).filter(|(a, b)| a != b).count()
        };
        assert_eq!((differ(false), differ(true)), (2, 432));
    }

    #[test]
    fn the_picture_lands_the_right_way_round() {
        // lib/ascii_chafa.py's cells for four flat quarters, white and red over blue and black: 20 x 5 cells, the middle row holding both halves.
        let colour = |x: usize, y: usize| match (x < 80, y < 40) {
            (true, true) => [255, 255, 255],
            (false, true) => [255, 0, 0],
            (true, false) => [0, 0, 255],
            (false, false) => [0, 0, 0],
        };
        let picture: Vec<u8> = (0..160 * 80)
            .flat_map(|at| colour(at % 160, at / 160))
            .collect();
        let art = cells(&picture, 160, 80, 20, None, 0.5, Glyphs::Shape, false);
        let slots = |row: usize| -> String {
            art.slots[row * 20..][..20]
                .iter()
                .map(|slot| format!("{slot:x}"))
                .collect()
        };
        assert_eq!((art.cols, art.rows), (20, 5));
        assert_eq!([slots(0), slots(1)], ["ffffffffff9999999999"; 2]);
        assert_eq!([slots(3), slots(4)], ["cccccccccc0000000000"; 2]);
    }

    #[test]
    fn mono_is_all_foreground() {
        // tests/test_chafa.py test_mono_is_all_foreground: colourful in grey, by Pillow's L.
        let grey: Vec<u8> = colourful(240, 160)
            .chunks(3)
            .flat_map(|p| {
                [((19595 * p[0] as u32 + 38470 * p[1] as u32 + 7471 * p[2] as u32 + 0x8000) >> 16)
                    as u8; 3]
            })
            .collect();
        for glyphs in Glyphs::ALL {
            let art = cells(&grey, 240, 160, 50, None, 0.5, glyphs, true);
            assert_eq!((art.cols, art.slots.len()), (50, 50 * art.rows));
            assert!(art.slots.iter().all(|&slot| slot == FG), "{glyphs:?}");
        }
    }

    #[repr(C)]
    struct GString {
        text: *mut c_char,
        len: usize,
        allocated: usize,
    }

    #[link(name = "glib-2.0")]
    unsafe extern "C" {
        fn g_string_free(string: *mut GString, free_text: c_int) -> *mut c_char;
    }

    #[link(name = "chafa")]
    unsafe extern "C" {
        fn chafa_term_db_get_default() -> *mut c_void;
        fn chafa_term_db_detect(db: *mut c_void, env: *mut *mut c_char) -> *mut c_void;
        fn chafa_term_info_unref(info: *mut c_void);
        fn chafa_canvas_print(canvas: *mut c_void, info: *mut c_void) -> *mut GString;
        fn chafa_canvas_peek_config(canvas: *mut c_void) -> *const c_void;
        fn chafa_canvas_config_get_geometry(
            config: *const c_void,
            cols: *mut c_int,
            rows: *mut c_int,
        );
        fn chafa_canvas_config_get_canvas_mode(config: *const c_void) -> c_int;
        fn chafa_canvas_config_get_fg_only_enabled(config: *const c_void) -> c_int;
        fn chafa_canvas_config_get_preprocessing_enabled(config: *const c_void) -> c_int;
        fn chafa_canvas_config_get_work_factor(config: *const c_void) -> f32;
    }

    /// The canvas as libchafa prints it for a truecolour xterm, read back as lib/ascii_chafa.py read it: each glyph with the colour of the last escape before it (-1: the default colour, after `39`, `0` or no number).
    fn printed(canvas: &Canvas) -> Vec<(u8, i32)> {
        let mut env = [
            c"TERM=xterm-256color".as_ptr().cast_mut(),
            c"COLORTERM=truecolor".as_ptr().cast_mut(),
            std::ptr::null_mut(),
        ];
        // SAFETY: the environment is a null-terminated array of C strings that libchafa only reads; the string it returns is copied before it is freed.
        let text = unsafe {
            let info = chafa_term_db_detect(chafa_term_db_get_default(), env.as_mut_ptr());
            let string = chafa_canvas_print(canvas.0, info);
            let text =
                std::slice::from_raw_parts((*string).text.cast::<u8>(), (*string).len).to_vec();
            g_string_free(string, 1);
            chafa_term_info_unref(info);
            text
        };
        let (mut cells, mut colour, mut at) = (Vec::new(), -1, 0);
        while at < text.len() {
            match text[at] {
                0x1b => {
                    let end = at + text[at..].iter().position(|&b| b == b'm').unwrap();
                    let code = std::str::from_utf8(&text[at + 2..end]).unwrap();
                    let numbers: Vec<i32> =
                        code.split(';').filter_map(|n| n.parse().ok()).collect();
                    match numbers[..] {
                        [38, 2, r, g, b] => colour = r << 16 | g << 8 | b,
                        [39] | [0] | [] => colour = -1,
                        _ => {}
                    }
                    at = end;
                }
                b'\n' => {}
                glyph => cells.push((glyph, colour)),
            }
            at += 1;
        }
        cells
    }

    #[test]
    fn accessors_give_the_printed_cells() {
        // tests/test_chafa.py test_matches_reference_parser, and row G12: reading the cells equals printing and parsing them.
        for (picture, w, h) in [
            (colourful(240, 160), 240, 160),
            (noise(96, 64), 96, 64),
            (edge(320, 160, 162), 320, 160),
        ] {
            for work in [0.2, 0.5] {
                let canvas = Canvas::draw(&picture, w, h, 60, 20, false, work);
                let read: Vec<(u8, i32)> =
                    (0..1200).map(|at| canvas.cell(at % 60, at / 60)).collect();
                assert!(read == printed(&canvas), "{w} x {h} at {work}");
                // in two colours nothing is printed but glyphs, whatever the cells' colours hold
                let canvas = Canvas::draw(&picture, w, h, 60, 20, true, work);
                let read: Vec<(u8, i32)> = (0..1200)
                    .map(|at| (canvas.cell(at % 60, at / 60).0, -1))
                    .collect();
                assert!(read == printed(&canvas), "{w} x {h} at {work}, mono");
            }
        }
    }

    #[test]
    fn cell_statistics_match_the_reference() {
        // rows G7: tests/test_density.py CellLightTest: its pictures and grids; the reference works each cell out from its 16 samples in turn.
        for picture in [noise(480, 270), gradient(480, 270)] {
            for (cols, rows) in [(120, 34), (37, 11)] {
                let fine = picture::box_filter(&picture, 480, 270, cols * 4, rows * 4);
                let got = light(&picture, 480, 270, cols, rows);
                assert_eq!(got.len(), cols * rows);
                for (cell, &(colour, mean, deviation)) in got.iter().enumerate() {
                    let block: Vec<&[u8]> = (0..16)
                        .map(|k| {
                            ((cell / cols * 4 + k / 4) * cols * 4 + cell % cols * 4 + k % 4) * 3
                        })
                        .map(|at| &fine[at..at + 3])
                        .collect();
                    let lights: Vec<f64> = block
                        .iter()
                        .map(|s| luminance([s[0], s[1], s[2]]))
                        .collect();
                    let want = lights.iter().sum::<f64>() / 16.0;
                    let spread =
                        (lights.iter().map(|l| (l - want).powi(2)).sum::<f64>() / 16.0).sqrt();
                    for c in 0..3 {
                        let channel = block.iter().map(|s| s[c] as f64).sum::<f64>() / 16.0;
                        assert!(
                            (colour[c] as f64 - channel).abs() <= 1.0,
                            "colour of cell {cell}"
                        );
                    }
                    // single precision: the deviation is a root of a difference of near numbers
                    assert!(
                        (mean as f64 - want).abs() < 1e-6,
                        "mean of cell {cell}: {mean} {want}"
                    );
                    assert!(
                        (deviation as f64 - spread).abs() < 2e-3,
                        "deviation of cell {cell}: {deviation} {spread}"
                    );
                }
            }
        }
    }

    #[test]
    fn cell_statistics_are_the_python_tools_bit_for_bit() {
        // rows G7, L6: lib/ascii_density.py cell_light under Pillow 13, as the Python tools ran: FNV-1a over each cell's colour and the bits of its mean and deviation; the tall grids go along y first at both box filters (picture::vertical_first), the wide one along x
        let fnv = |bytes: &[u8]| {
            bytes.iter().fold(0xcbf2_9ce4_8422_2325u64, |hash, &byte| {
                (hash ^ byte as u64).wrapping_mul(0x0100_0000_01b3)
            })
        };
        for ((w, h, cols, rows), hash) in [
            ((48, 400, 6, 40), 0x055b_29d3_f2d7_efee),
            ((30, 333, 5, 41), 0x8457_d139_fedb_1bac),
            ((480, 270, 37, 11), 0x7dd8_2a4d_cd6e_ace1),
        ] {
            let bytes: Vec<u8> = light(&noise(w, h), w, h, cols, rows)
                .iter()
                .flat_map(|(colour, mean, deviation)| {
                    [&colour[..], &mean.to_le_bytes(), &deviation.to_le_bytes()].concat()
                })
                .collect();
            assert_eq!(fnv(&bytes), hash, "{w} x {h} in {cols} x {rows}");
        }
    }

    #[test]
    fn shape_leaves_libchafas_cells() {
        // rows G2, G7: tests/test_density.py test_shape_leaves_grid: each cell keeps libchafa's glyph, and its colour goes to the slot Recolour gives
        let picture = gradient(320, 160);
        let art = cells(&picture, 320, 160, 60, None, 0.5, Glyphs::Shape, false);
        let (canvas, mut inks) = (Canvas::draw(&picture, 320, 160, 60, 15, false, 0.2), inks());
        assert_eq!((art.cols, art.rows), (60, 15));
        for at in 0..900 {
            let (glyph, colour) = canvas.cell(at % 60, at / 60);
            let slot =
                inks.recolour
                    .slot([(colour >> 16) as u8, (colour >> 8) as u8, colour as u8]);
            assert_eq!(
                (art.chars[at as usize], art.slots[at as usize]),
                (glyph, slot)
            );
        }
    }

    #[test]
    fn density_follows_brightness() {
        // tests/test_density.py test_density_follows_brightness, with the installed font's ramp as there.
        let (picture, ramp, mut inks) = (
            gradient(640, 160),
            crate::font::ramp(false).unwrap(),
            inks(),
        );
        let (size, mode) = ((640, 160), (Glyphs::Density, false));
        let art = grid(
            &picture, size.0, size.1, 80, None, 0.5, mode.0, mode.1, &mut inks, ramp, 0.2,
        )
        .unwrap();
        let stats = light(&picture, 640, 160, art.cols, art.rows);
        // a cell lights about coverage x its colour's luminance: brighter greys take sparser glyphs
        let lit = |at: usize| {
            let place = ramp
                .glyphs
                .iter()
                .position(|&g| g == art.chars[at])
                .unwrap();
            let shown = if art.slots[at] == 0 {
                0.0
            } else {
                inks.luma[art.slots[at] as usize]
            };
            ramp.cover[place] * shown
        };
        // by column: the picture's light and the drawn light, then their correlation
        let column = |value: &dyn Fn(usize) -> f64| -> Vec<f64> {
            let sum = |col: usize| {
                (0..art.rows)
                    .map(|row| value(row * art.cols + col))
                    .sum::<f64>()
            };
            (0..art.cols)
                .map(|col| sum(col) / art.rows as f64)
                .collect()
        };
        let (want, got) = (column(&|at| stats[at].1 as f64), column(&lit));
        let mean = |v: &[f64]| v.iter().sum::<f64>() / v.len() as f64;
        let (mw, mg) = (mean(&want), mean(&got));
        let cross: f64 = want
            .iter()
            .zip(&got)
            .map(|(w, g)| (w - mw) * (g - mg))
            .sum();
        let spread = |v: &[f64], m: f64| v.iter().map(|x| (x - m).powi(2)).sum::<f64>().sqrt();
        assert!(cross / (spread(&want, mw) * spread(&got, mg)) > 0.98);
    }

    #[test]
    fn mixed_keeps_edges() {
        // rows G7: tests/test_density.py test_mixed_keeps_edges: an edge inside a cell (8 px a cell at 40 columns).
        let picture = edge(320, 160, 162);
        let shape = cells(&picture, 320, 160, 40, None, 0.5, Glyphs::Shape, false);
        let mixed = cells(&picture, 320, 160, 40, None, 0.5, Glyphs::Mixed, false);
        let column = |art: &Grid, col: usize| -> Vec<u8> {
            (0..art.rows).map(|row| art.chars[row * 40 + col]).collect()
        };
        assert_eq!(column(&mixed, 20), column(&shape, 20));
        // beside the edge the cells are flat: white on the brightest slot takes the densest glyph
        assert_eq!(column(&mixed, 30), vec![b'~'; shape.rows]);
    }

    #[test]
    fn flat_cells_take_the_python_tools_glyphs() {
        // rows G3-G7: lib/ascii_term.py grid() on the same picture (16 x 8 cells of 8 px, ratio 1) with the same ramp, bl1nk, work 0.2, 2026-11-10.
        // Density draws every cell from the ramp and mixed every flat one: no glyph here is libchafa's.
        let picture = flat(16, 8, 8);
        let chars = " *4>KZl~~~~~~GMSZbirx~~~~~~~~VY^bflpuz~~~~~~gjnquy}~~~~~lprvy}~~KEA988:558GMW`lxqlgdXXYMH[aiNTZa~~~~~~wxz}LMPR~~~~~~~~~~~~~~~~~~";
        let slots = "00000000000006666666888888888eeeeeeeeeeeeeee777777777777ffffffffccc5555d11999999cccc555d19993333444444aaaabbbb334aaa2222222222bb";
        let slots: Vec<u8> = slots
            .chars()
            .map(|c| c.to_digit(16).unwrap() as u8)
            .collect();
        let mono = "  ! !!!!\"\"\"\"##$$%&&'()*+,-./023567:;=>@BDFIJMORTVY[^`dfjlprvy}~~(&&%%%%&'(*+.035.-,,+,+,-//2368<DCCBBBBCDDGHJLPRonmmlmmnnpqsuxz}";
        for glyphs in [Glyphs::Density, Glyphs::Mixed] {
            let art = cells(&picture, 128, 64, 16, None, 1.0, glyphs, false);
            assert_eq!((art.cols, art.rows), (16, 8));
            assert_eq!(String::from_utf8_lossy(&art.chars), chars, "{glyphs:?}");
            assert_eq!(art.slots, slots, "{glyphs:?}");
            let art = cells(&picture, 128, 64, 16, None, 1.0, glyphs, true);
            assert_eq!(
                String::from_utf8_lossy(&art.chars),
                mono,
                "{glyphs:?}, mono"
            );
            assert_eq!(art.slots, [FG; 128], "{glyphs:?}, mono");
        }
        // lib/ascii_term.py Term.text() of the two grids: the 13 cells of slot 0, 12 of them inked, print as spaces
        let art = cells(&picture, 128, 64, 16, None, 1.0, Glyphs::Density, false);
        let want = b"\x1b[30m             \x1b[36mGMS\x1b[39m\n\x1b[36mZbir\x1b[90mx~~~~~~~~\x1b[96mVY^\x1b[39m\n\x1b[96mbflpuz~~~~~~\x1b[37mgjnq\x1b[39m\n\x1b[37muy}~~~~~\x1b[97mlprvy}~~\x1b[39m\n\x1b[94mKEA\x1b[35m988:\x1b[95m5\x1b[31m58\x1b[91mGMW`lx\x1b[39m\n\x1b[94mqlgd\x1b[35mXXY\x1b[95mM\x1b[31mH\x1b[91m[ai\x1b[33mNTZa\x1b[39m\n\x1b[34m~~~~~~\x1b[92mwxz}\x1b[93mLMPR\x1b[33m~~\x1b[39m\n\x1b[34m~\x1b[92m~~~\x1b[32m~~~~~~~~~~\x1b[93m~~\x1b[39m";
        assert_eq!(text(&art, None), lines(want));
        let art = cells(&picture, 128, 64, 16, None, 1.0, Glyphs::Density, true);
        let want = b"\x1b[97m  ! !!!!\"\"\"\"##$$\n\x1b[97m%&&'()*+,-./0235\n\x1b[97m67:;=>@BDFIJMORT\n\x1b[97mVY[^`dfjlprvy}~~\n\x1b[97m(&&%%%%&'(*+.035\n\x1b[97m.-,,+,+,-//2368<\n\x1b[97mDCCBBBBCDDGHJLPR\n\x1b[97monmmlmmnnpqsuxz}";
        assert_eq!(text(&art, Some(inks().mono)), lines(want));
    }

    #[test]
    fn picked_cells_take_their_mean_colours_slot() {
        // rows G6, G7: lib/ascii_term.py grid() on red and blue stripes of 4 px (8 x 4 cells of 8 px, each half red, half blue), same ramp, 2026-11-10.
        let picture: Vec<u8> = (0..64 * 32)
            .flat_map(|at| if at % 8 < 4 { [255, 0, 0] } else { [0, 0, 255] })
            .collect();
        let density = cells(&picture, 64, 32, 8, None, 1.0, Glyphs::Density, false);
        assert_eq!(
            density.chars,
            ["UUUTUUUT", "TUTUTUTU", "UTUTUTUT", "TUTUTUTU"]
                .concat()
                .as_bytes()
        );
        assert_eq!(density.slots, [13; 32]);
        let mono = cells(&picture, 64, 32, 8, None, 1.0, Glyphs::Density, true);
        assert_eq!(
            mono.chars,
            ["/./././.", "./.../..", "/./././.", "........"]
                .concat()
                .as_bytes()
        );
        // no cell is flat: mixed keeps libchafa's glyph and colour
        let shape = cells(&picture, 64, 32, 8, None, 1.0, Glyphs::Shape, false);
        assert_eq!(
            cells(&picture, 64, 32, 8, None, 1.0, Glyphs::Mixed, false),
            shape
        );
    }

    #[test]
    fn mixed_takes_cells_under_the_flat_bound() {
        // rows G7: lib/ascii_term.py grid() on two cells of 8 px, grey 112 left of 137, then left of 138 (deviation 0.04406 and 0.04606 in linear light), the same ramp, bl1nk, work 0.2, run 2026-11-12: mixed draws the first from the ramp, as density does, and keeps libchafa's glyph for the second, as shape does (FLAT 0.045, lib/ascii_density.py:26, :76)
        let picture: Vec<u8> = (0..16 * 8)
            .flat_map(|at| [[112, 137, 112, 138][at % 16 / 4]; 3])
            .collect();
        let deviation: Vec<f32> = light(&picture, 16, 8, 2, 1)
            .iter()
            .map(|cell| cell.2)
            .collect();
        assert!(
            (deviation[0] - 0.044_064).abs() < 1e-6 && (deviation[1] - 0.046_061).abs() < 1e-6,
            "{deviation:?}"
        );
        // mixed, density, shape
        for (mono, want, slot) in [
            (false, ["^@", "^_", "@@"], 14),
            (true, ["54", "55", "44"], FG),
        ] {
            for (glyphs, want) in [Glyphs::Mixed, Glyphs::Density, Glyphs::Shape]
                .iter()
                .zip(want)
            {
                let art = cells(&picture, 16, 8, 2, None, 1.0, *glyphs, mono);
                assert_eq!(
                    (String::from_utf8_lossy(&art.chars), art.slots.as_slice()),
                    (want.into(), [slot; 2].as_slice()),
                    "{glyphs:?}, mono {mono}"
                );
            }
        }
    }

    #[test]
    fn mono_measures_against_its_own_slot() {
        // rows G6, G7: the luminance a mono cell is measured against is mono's slot's, as lib/ascii_term.py:68 luma_mono gives it: mono's cells count as the foreground slot and shine as mono's colour, so a brighter slot draws sparser glyphs; in mono a picked cell keeps the foreground slot, not its mean colour's.
        let (picture, mut inks, mode) = (flat(16, 8, 8), inks(), Glyphs::Density);
        let mut draw = |mono: u8| {
            inks.mono = mono;
            grid(
                &picture,
                128,
                64,
                16,
                None,
                1.0,
                mode,
                true,
                &mut inks,
                &ramp(),
                0.2,
            )
            .unwrap()
        };
        let (bright, dim) = (draw(15), draw(8));
        assert!(bright.chars.iter().zip(&dim.chars).all(|(b, d)| b <= d));
        assert_ne!(bright.chars, dim.chars);
        assert_eq!(dim.slots, [FG; 128]);
    }

    #[test]
    fn a_slot_under_a_thousandth_counts_as_a_thousandth() {
        // rows G6: lib/ascii_term.py grid() on eight flat cells of grey 0 to 7 (linear luminance 0 to 0.0021), the same ramp, bl1nk with color0 #000000, #010101, #020202 or #030303 (luminance 0 to 0.00091), work 0.2, run 2026-11-12: the greys take slot 0 and coverage min(Y / 0.001, 1) whatever slot 0's luminance under 0.001 (lib/ascii_density.py:81)
        let picture: Vec<u8> = (0..64 * 8)
            .flat_map(|at| [(at % 64 / 8) as u8; 3])
            .collect();
        for black in 0..4 {
            let mut theme = bl1nk();
            theme.colors[0] = [black; 3];
            let mut inks = Inks::new(&theme, 100, Skin::Grey);
            assert!(inks.luma[0] < 1e-3, "{black}");
            for glyphs in [Glyphs::Density, Glyphs::Mixed] {
                let art = grid(
                    &picture,
                    64,
                    8,
                    8,
                    None,
                    1.0,
                    glyphs,
                    false,
                    &mut inks,
                    &ramp(),
                    0.2,
                )
                .unwrap();
                assert_eq!(
                    (String::from_utf8_lossy(&art.chars), art.slots.as_slice()),
                    (" =Yu~~~~".into(), [0; 8].as_slice()),
                    "color0 {black}, {glyphs:?}"
                );
            }
        }
    }

    #[test]
    fn ramp_glyphs_dither_between_neighbours() {
        // Row G6: the denser of two ramp neighbours when the fraction between them exceeds the Bayer threshold.
        let ramp = ramp();
        assert_eq!(ramp_glyph(&ramp, 0.0, 0, 0), b' ');
        assert_eq!(ramp_glyph(&ramp, 1.0, 3, 0), b'~');
        assert_eq!(ramp_glyph(&ramp, 10.0 / 94.0, 0, 0), 32 + 10);
        assert_eq!(ramp_glyph(&ramp, 10.0 / 94.0, 3, 0), 32 + 10);
        // half way between glyphs 10 and 11: the denser where the threshold k / 16 + 1 / 32 is under 0.5
        let tile: Vec<u8> = (0..16)
            .map(|at| ramp_glyph(&ramp, 10.5 / 94.0, at / 4, at % 4) - 42)
            .collect();
        assert_eq!(tile, [1, 0, 1, 0, 0, 1, 0, 1, 1, 0, 1, 0, 0, 1, 0, 1]);
        // the pattern repeats every four cells each way
        assert_eq!(
            ramp_glyph(&ramp, 10.5 / 94.0, 5, 6),
            ramp_glyph(&ramp, 10.5 / 94.0, 1, 2)
        );
        // a sixteenth past glyph 10: only where the threshold is 1 / 32
        let dense: Vec<usize> = (0..16)
            .filter(|at| ramp_glyph(&ramp, (10.0 + 1.0 / 16.0) / 94.0, at / 4, at % 4) == 43)
            .collect();
        assert_eq!(dense, [0]);
        // two glyphs of one coverage, met exactly: the first, as numpy's searchsorted finds it
        let mut twins = self::ramp();
        twins.cover[11] = twins.cover[10];
        assert_eq!(
            [0, 3, 5, 12].map(|at| ramp_glyph(&twins, 10.0 / 94.0, at / 4, at % 4)),
            [42; 4]
        );
    }

    #[test]
    fn text_codes_where_the_slot_changes() {
        // tests/test_term.py TextTest
        let art = Grid {
            cols: 4,
            rows: 1,
            chars: b"ABCD".to_vec(),
            slots: vec![1, 1, 9, 16],
        };
        assert_eq!(
            text(&art, None),
            [b"\x1b[31mAB\x1b[91mC\x1b[39mD\x1b[39m".to_vec()]
        );
        let art = Grid {
            cols: 2,
            rows: 1,
            chars: b"AB".to_vec(),
            slots: vec![0, 2],
        };
        assert_eq!(text(&art, None), [b"\x1b[30m \x1b[32mB\x1b[39m".to_vec()]);
        let art = Grid {
            cols: 1,
            rows: 2,
            chars: b"AB".to_vec(),
            slots: vec![0, 0],
        };
        assert_eq!(
            text(&art, Some(inks().mono)),
            [b"\x1b[97mA".to_vec(), b"\x1b[97mB".to_vec()]
        );
        // every slot's code: lib/ascii_term.py CODES
        let art = Grid {
            cols: 17,
            rows: 1,
            chars: vec![b'x'; 17],
            slots: (0..17).collect(),
        };
        let codes: String = [
            30, 31, 32, 33, 34, 35, 36, 37, 90, 91, 92, 93, 94, 95, 96, 97, 39,
        ]
        .iter()
        .enumerate()
        .map(|(slot, code)| format!("\x1b[{code}m{}", if slot == 0 { ' ' } else { 'x' }))
        .collect();
        assert_eq!(text(&art, None), [format!("{codes}\x1b[39m").into_bytes()]);
    }

    #[test]
    fn every_mode_fills_the_grid() {
        // tests/test_term.py RenderTest: 60 x 27 cells is Term.render's answer for the same call.
        let picture = gradient(400, 400);
        for glyphs in Glyphs::ALL {
            for mono in [false, true] {
                let art = cells(&picture, 400, 400, 60, Some(50), 9.0 / 20.0, glyphs, mono);
                assert_eq!((art.cols, art.rows), (60, 27), "{glyphs:?} {mono}");
                let rows = text(&art, mono.then_some(15));
                assert_eq!(rows.len(), 27);
                assert!(
                    rows.iter().all(|row| plain(row).len() == 60),
                    "{glyphs:?} {mono}"
                );
            }
        }
        let rows = |ratio: f32| {
            cells(
                &picture,
                400,
                400,
                60,
                Some(100),
                ratio,
                Glyphs::Mixed,
                false,
            )
            .rows
        };
        assert_eq!((rows(0.5), rows(9.0 / 20.0)), (30, 27));
    }

    #[test]
    fn inks_of_a_theme() {
        // Term(THEME).luma rounded to four places and .mono, lib/ascii_term.py on tests/bl1nk.conf.
        let inks = inks();
        let luma = [
            0.0114, 0.3038, 0.654, 0.4006, 0.2022, 0.1899, 0.0831, 0.5944, 0.0853, 0.2266, 0.3695,
            0.827, 0.1596, 0.2548, 0.3125, 0.935, 0.3515,
        ];
        for (got, want) in inks.luma.iter().zip(luma) {
            assert!((got - want).abs() < 0.00005, "{got} {want}");
        }
        assert_eq!(inks.mono, 15);
    }

    #[test]
    fn nothing_to_draw_is_an_empty_grid() {
        let art = cells(&[], 0, 0, 40, None, 0.5, Glyphs::Mixed, false);
        assert_eq!(
            (art.cols, art.rows, art.chars.len(), art.slots.len()),
            (0, 0, 0, 0)
        );
        assert!(text(&art, None).is_empty());
    }

    #[test]
    fn the_thread_count_changes_no_cell() {
        let picture = colourful(240, 160);
        let many = cells(&picture, 240, 160, 60, None, 0.5, Glyphs::Mixed, false);
        threads(1);
        let one = cells(&picture, 240, 160, 60, None, 0.5, Glyphs::Mixed, false);
        threads(-1);
        assert_eq!(one, many);
    }
}
