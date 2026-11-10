//! A picture as cells: libchafa's glyph shapes, each cell's colour one of the theme's slots, flat cells given glyphs by brightness; and the cells as terminal text.

use crate::font::Ramp;
use crate::look::{Glyphs, Skin};
use crate::theme::{Recolour, Rgb, Theme};

/// A cell's colour slot for the theme's foreground: libchafa's default colour, and all of mono.
pub const FG: u8 = 16;
/// Px a cell side that libchafa samples: its symbols are 8 x 8.
pub const CELL_PX: u32 = 8;

/// `chars`: each cell's glyph (ASCII); `slots`: its colour (0-15 palette, 16 foreground); both row by row.
#[derive(Clone, PartialEq, Debug)]
pub struct Grid {
    pub cols: usize,
    pub rows: usize,
    pub chars: Vec<u8>,
    pub slots: Vec<u8>,
}

/// A theme set up for drawing: the slot mapping, each slot's linear luminance (16 colours, then the foreground), mono's slot.
pub struct Inks {
    pub palette: [Rgb; 16],
    pub recolour: Recolour,
    pub luma: [f64; 17],
    pub mono: u8,
}

impl Inks {
    pub fn new(theme: &Theme, saturation: i32, skin: Skin) -> Inks {
        todo!("{theme:?} {saturation} {skin:?}")
    }
}

/// The cell grid a `w` x `h` px picture takes within `cols` x `rows` (`None`: as many rows as the columns need) at cell ratio width / height: libchafa's geometry, zoom on, stretch off.
pub fn geometry(w: u32, h: u32, cols: u32, rows: Option<u32>, ratio: f32) -> (u32, u32) {
    todo!("{w} {h} {cols} {rows:?} {ratio}")
}

/// The picture (`rgb`: packed RGB of `w` x `h`, tone table already applied) as a `Grid`: libchafa's glyphs, slots through `inks`, then the ramp on picked cells by `glyphs`; mono: two-colour shapes, every slot `FG`.
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
) -> Grid {
    todo!(
        "{} {w} {h} {cols} {rows:?} {ratio} {glyphs:?} {mono} {} {} {work}",
        rgb.len(),
        inks.mono,
        ramp.glyphs[0]
    )
}

/// Terminal text, a row of bytes per grid row: a colour code where the slot changes (30-37, 90-97, 39 for the foreground), slot 0 blank, 39 at the row's end; `mono`: that slot's code once a row, nothing else.
pub fn text(grid: &Grid, mono: Option<u8>) -> Vec<Vec<u8>> {
    todo!("{} {mono:?}", grid.cols)
}

/// libchafa's own thread count (-1: its default, one a core).
pub fn threads(count: i32) {
    todo!("{count}")
}
