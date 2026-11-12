//! The picture to cells as the viewer draws it, against the Python tools of tag python-final (inventory section 11): what the unit tests of src/cells.rs cannot reach behind the window.

mod common;

use common::*;

/// The noise of the unit tests in src/cells.rs: each byte a hash of its column, row and channel.
fn noise(w: u32, h: u32) -> image::RgbImage {
    let value = |x: u32, y: u32, c: u32| {
        let v = (x
            .wrapping_mul(1973)
            .wrapping_add(y.wrapping_mul(9277))
            .wrapping_add(c * 26699))
            ^ 0x5BD1_E995;
        (v.wrapping_mul(1_103_515_245).wrapping_add(12345) >> 16) as u8
    };
    image::RgbImage::from_fn(w, h, |x, y| image::Rgb([0, 1, 2].map(|c| value(x, y, c))))
}

/// lib/ascii_term.py grid() at tag python-final on noise(320, 96) in 40 x 13 cells of ratio 1, mixed, truecolour, the regular face, bl1nk, at its default work factor 0.2 (lib/ascii_term.py:29), as Term.text() prints it, escapes dropped, run 2026-11-12: 40 x 12 cells, none in slot 0. At 0.5, the converter's (bin/ascii-convert:206), 33 of these 480 glyphs differ.
const OLD_ROWS: [&str; 12] = [
    "@@@@$@@@@@@@@@@@@@$@$@@@@@@$@$@@@$@@@@@@",
    "@@@@@@@@@@$@@@$@@$$g@@@@@@@@$@$@$@@@@@@@",
    "@$@@@@@@@@@@@@@@$@@@@3$@$@@$@@@$@@@@@@@@",
    "@@@@0@@@@@@@@@@@@@@@@@@@@@@@@$@@@@@@@@@@",
    "@@#$@@@@@@@@@@@@@@$0$@@@@@$@@@@@@@@@@@@@",
    "@@@@@@@@@$@$$@@@$@@@$@@@R@@@@@$@@@$@@@@@",
    "@@@@$@@@@@0@@@@@@@@@@@$$@@@@@$@@@@$@@@@@",
    "@@@@@@@@@@@0@B@@@$@@@@@@@@@@@@@@@@@@@$@@",
    "@@@@@$$@@@@@@@@@@W@@$@@@@$@@@@@@B@@@0@@@",
    "@@B@@@@@$@@@@@@$@@$@B@@@$$@@g@@$0@$@@@@@",
    "@@@@@$@@@@@@@@$@$@@$@@@$@@@@@@@@@@@@@@@@",
    "$@@@@@@@@@@@@@@$@@@@@@@@@@B@@@B@@@@@$@@@",
];

#[test]
fn the_viewer_draws_at_work_0_2() {
    // rows G1: the viewer's live picture takes libchafa's glyphs at work factor 0.2: a still this small is shown at its own size, 8 px a cell (thumbnail of bin/ascii-viewer:1842 at tag python-final, MAX_W and MAX_H at :153; src/still.rs), so its rows are grid()'s at 0.2; the old viewer draws the same rows
    let sandbox = Sandbox::new();
    let file = save(&noise(320, 96), &sandbox.path("noise.png"));
    // 10 x 10 px cells: ratio 1
    let size = Size {
        rows: 13,
        cols: 40,
        xpx: 400,
        ypx: 130,
    };
    let mut tty = sandbox.launch().arg(&file).size(size).start();
    // the first draw, read to its end
    tty.expect(0, b"\x1b[?2026l");
    let screen = tty.screen();
    let rows: Vec<String> = (1..=12).map(|row| screen.row(row)).collect();
    assert_eq!(rows, OLD_ROWS);
    tty.send(b"q");
    assert_eq!(tty.exit(), 0);
}
