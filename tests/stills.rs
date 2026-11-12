//! Stills in the viewer: the formats that open as stills, EXIF and transparency, the kitty probe and image mode, `v`, the folder's other images, turns, zoom and pan, SVG detail, the look, `s`, `r`, `y`, and the keys a still leaves alone (inventory sections 5, 6 and 7; the still side of V16a and V17).

mod common;

use common::*;
use image::codecs::jpeg::JpegEncoder;
use image::codecs::png::PngEncoder;
use image::{DynamicImage, ImageEncoder, Rgb, RgbImage, Rgba, RgbaImage};
use std::collections::BTreeSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

const SHIFT_RIGHT: &[u8] = b"\x1b[1;2C";
const SHIFT_LEFT: &[u8] = b"\x1b[1;2D";
const SHIFT_UP: &[u8] = b"\x1b[1;2A";
const SHIFT_DOWN: &[u8] = b"\x1b[1;2B";

/// 100 x 30 cells of 9 x 20 px: the 900 x 600 px window of the old suite's still tests (`still` of tests/test_viewer.py:862 at tag python-final).
const HALF: Size = Size {
    rows: 30,
    cols: 100,
    xpx: 900,
    ypx: 600,
};

/// The toast of `3`: once it shows, the keys sent before it are done.
const DARKER: &str = "level 5% darker";

/// What a still's draw starts with: images deleted (row K5).
const STILL_DRAW: &[u8] = b"\x1b[?2026h\x1b[0m\x1b_Ga=d,d=A,q=2\x1b\\";

/// A picture whose pixels say where they are: red x / 8, green y / 8.
fn coded(w: u32, h: u32) -> RgbImage {
    RgbImage::from_fn(w, h, |x, y| Rgb([(x / 8) as u8, (y / 8) as u8, 0]))
}

/// White on the left half, black on the right.
fn halves(w: u32, h: u32) -> RgbImage {
    RgbImage::from_fn(w, h, |x, _| Rgb([if x < w / 2 { 255 } else { 0 }; 3]))
}

/// Dark grey with a red square of 100 px in the top left corner: a turn shows where it went.
fn marked(w: u32, h: u32) -> RgbImage {
    RgbImage::from_fn(w, h, |x, y| {
        Rgb(if x < 100 && y < 100 {
            [255, 0, 0]
        } else {
            [40, 40, 40]
        })
    })
}

/// The squares transparency shows over in image mode, as row K2 has them: 8 px, #666666 where x / 8 + y / 8 is even, #999999 where odd.
fn checkerboard(w: u32, h: u32) -> RgbImage {
    RgbImage::from_fn(w, h, |x, y| {
        Rgb([[0x66, 0x99][((x / 8 + y / 8) % 2) as usize]; 3])
    })
}

/// A picture with alpha saved at `path` (the format by its extension).
fn save_rgba(picture: &RgbaImage, path: &Path) -> PathBuf {
    picture.save(path).unwrap();
    path.to_path_buf()
}

/// An EXIF block holding the orientation alone (src/picture.rs tests).
fn exif(orientation: u8) -> Vec<u8> {
    let mut out = b"II*\0\x08\0\0\0\x01\0\x12\x01\x03\0\x01\0\0\0".to_vec();
    out.extend([orientation, 0, 0, 0, 0, 0, 0, 0]);
    out
}

/// A JPEG of `picture`, with an EXIF orientation when one is given.
fn jpeg(picture: &RgbImage, orientation: Option<u8>) -> Vec<u8> {
    let mut out = Vec::new();
    let mut encoder = JpegEncoder::new_with_quality(&mut out, 95);
    if let Some(n) = orientation {
        encoder.set_exif_metadata(exif(n)).unwrap();
    }
    picture.write_with_encoder(encoder).unwrap();
    out
}

/// An MPO that Pillow opens as two frames (JpegImagePlugin's jpeg_factory): the JPEG of `first` with an APP2 `MPF` segment right after its SOI, whose index (CIPA DC-007) lists it and the JPEG of `second` appended behind it.
fn mpo(first: &RgbImage, second: &RgbImage) -> Vec<u8> {
    let (head, tail) = (jpeg(first, None), jpeg(second, None));
    // the index is little-endian TIFF: its offsets count from its own start, 4 bytes into the segment's data
    let mut index = b"II*\0\x08\0\0\0\x03\0".to_vec();
    // MPFVersion `0100`; NumberOfImages 2; MPEntry, 2 entries of 16 bytes at offset 50, past the directory and its next-directory offset of 0
    for (tag, kind, count, value) in [
        (0xb000u16, 7u16, 4u32, u32::from_le_bytes(*b"0100")),
        (0xb001, 4, 1, 2),
        (0xb002, 7, 32, 50),
    ] {
        index.extend(tag.to_le_bytes());
        index.extend(kind.to_le_bytes());
        index.extend(count.to_le_bytes());
        index.extend(value.to_le_bytes());
    }
    index.extend(0u32.to_le_bytes());
    let segment = 2 + 4 + index.len() + 32;
    let start = 2 + 2 + 2 + 4; // the index's start in the file: past SOI, the marker, the length and `MPF\0`
    let second_at = (head.len() + 2 + segment - start) as u32;
    // each entry: attributes (a representative baseline primary, then a disparity image, both JPEG), size, offset, two dependent entries
    for (attributes, size, offset) in [
        (0x2003_0000u32, (head.len() + 2 + segment) as u32, 0u32),
        (0x0002_0002, tail.len() as u32, second_at),
    ] {
        index.extend(attributes.to_le_bytes());
        index.extend(size.to_le_bytes());
        index.extend(offset.to_le_bytes());
        index.extend([0; 4]);
    }
    let mut out = head[..2].to_vec();
    out.extend([0xff, 0xe2]);
    out.extend((segment as u16).to_be_bytes());
    out.extend(b"MPF\0");
    out.extend(index);
    out.extend(&head[2..]);
    out.extend(tail);
    out
}

/// A PNG of `picture` with an EXIF orientation (an eXIf chunk).
fn oriented_png(picture: &RgbImage, orientation: u8) -> Vec<u8> {
    let mut out = Vec::new();
    let mut encoder = PngEncoder::new(&mut out);
    encoder.set_exif_metadata(exif(orientation)).unwrap();
    picture.write_with_encoder(encoder).unwrap();
    out
}

/// A PNG file from its chunks: (type, data).
fn png_chunks(chunks: &[(&[u8; 4], Vec<u8>)]) -> Vec<u8> {
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    for (kind, data) in chunks {
        let mut crc = flate2::Crc::new();
        crc.update(*kind);
        crc.update(data);
        out.extend((data.len() as u32).to_be_bytes());
        out.extend(*kind);
        out.extend(data);
        out.extend(crc.sum().to_be_bytes());
    }
    out
}

/// A palette PNG `w` x `h`: index 0 transparent black on the left half, index 1 opaque white on the right.
fn palette_png(w: u32, h: u32) -> Vec<u8> {
    let mut rows = Vec::new();
    for _ in 0..h {
        rows.push(0); // no filter
        rows.extend((0..w).map(|x| (x >= w / 2) as u8));
    }
    let mut zlib = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
    zlib.write_all(&rows).unwrap();
    let header = [&w.to_be_bytes()[..], &h.to_be_bytes(), &[8, 3, 0, 0, 0]].concat();
    png_chunks(&[
        (b"IHDR", header),
        (b"PLTE", vec![0, 0, 0, 255, 255, 255]),
        (b"tRNS", vec![0]),
        (b"IDAT", zlib.finish().unwrap()),
        (b"IEND", vec![]),
    ])
}

/// A little-endian TIFF of 8-bit grey pages `w` x `h`, each one uncompressed strip.
fn tiff(w: u32, h: u32, pages: &[Vec<u8>]) -> Vec<u8> {
    let mut out = b"II*\0\x08\0\0\0".to_vec();
    for (i, page) in pages.iter().enumerate() {
        // a directory of 9 entries (2 + 9 * 12 + 4 bytes), then its pixels
        let pixels = out.len() as u32 + 114;
        let next = if i + 1 < pages.len() {
            pixels + page.len() as u32
        } else {
            0
        };
        // width, height, bits, no compression, black is zero, strip offset, samples, rows a strip, strip bytes: (tag, LONG 4 or SHORT 3, value)
        let entries: [(u16, u16, u32); 9] = [
            (256, 4, w),
            (257, 4, h),
            (258, 3, 8),
            (259, 3, 1),
            (262, 3, 1),
            (273, 4, pixels),
            (277, 3, 1),
            (278, 4, h),
            (279, 4, page.len() as u32),
        ];
        out.extend(9u16.to_le_bytes());
        for (tag, kind, value) in entries {
            out.extend(tag.to_le_bytes());
            out.extend(kind.to_le_bytes());
            out.extend(1u32.to_le_bytes());
            if kind == 3 {
                out.extend((value as u16).to_le_bytes());
                out.extend([0, 0]);
            } else {
                out.extend(value.to_le_bytes());
            }
        }
        out.extend(next.to_le_bytes());
        out.extend(page);
    }
    out
}

/// A sandbox whose runs say they are in kitty (`KITTY_WINDOW_ID`).
fn kitty_sandbox() -> Sandbox {
    let mut sandbox = Sandbox::new();
    sandbox.set("KITTY_WINDOW_ID", "1");
    sandbox
}

/// The viewer on `file` in a window of `size`, the kitty probe answered at once: image mode when the sandbox says kitty.
fn kitty_view(sandbox: &Sandbox, file: &Path, size: Size) -> Tty {
    sandbox
        .launch()
        .arg(file)
        .size(size)
        .kitty(KITTY_OK, Duration::ZERO)
        .start()
}

/// The viewer on `file` in a window of `size`, its first draw read to its end.
fn drawn(sandbox: &Sandbox, file: &Path, size: Size) -> Tty {
    let tty = sandbox.launch().arg(file).size(size).start();
    tty.expect(0, b"\x1b[?2026l");
    tty
}

/// Quit with `q`, exit 0; no file the run named for kitty is left in /dev/shm.
fn quit(tty: &mut Tty) {
    tty.send(b"q");
    assert_eq!(tty.exit(), 0);
    let left: Vec<PathBuf> = transfer_files(&tty.out())
        .into_iter()
        .filter(|file| file.exists())
        .collect();
    assert!(left.is_empty(), "left in /dev/shm: {left:?}");
}

/// Send `keys`, then wait for the toast `text`; the output's length before the keys.
fn says(tty: &mut Tty, keys: &[u8], text: &str) -> usize {
    let at = tty.send(keys);
    tty.expect(at, toast(text));
    at
}

/// The synchronized update around byte `at` of the output, once its end has come.
fn update_at(tty: &Tty, at: usize) -> Vec<u8> {
    let end = tty.expect(at, b"\x1b[?2026l");
    let out = tty.out();
    let start = out[..at]
        .windows(8)
        .rposition(|w| w == b"\x1b[?2026h")
        .expect("an update");
    out[start..end].to_vec()
}

/// Wait until the bottom row is blank and the draw that blanked it has ended: a draw erases that row before its picture rows.
fn toast_gone(tty: &mut Tty) {
    let rows = tty.size.rows as usize;
    tty.until("the toast gone", |screen| screen.row(rows).is_empty());
    let out = tty.out();
    if let Some(start) = out.windows(8).rposition(|w| w == b"\x1b[?2026h") {
        tty.expect(start, b"\x1b[?2026l");
    }
}

/// An image the viewer placed: its keys, the file it named, the pixels the file held, the bytes of its draw before it, and where its draw ended.
struct Placed {
    keys: String,
    file: PathBuf,
    rgb: Vec<u8>,
    before: Vec<u8>,
    end: usize,
}

impl Placed {
    /// `s` and `v`: the px size sent.
    fn size(&self) -> (u32, u32) {
        let key = |name: &str| -> u32 {
            let found = self.keys.split(',').find_map(|k| k.strip_prefix(name));
            found.expect("the key").parse().unwrap()
        };
        (key("s="), key("v="))
    }

    fn px(&self, x: u32, y: u32) -> [u8; 3] {
        let at = ((y * self.size().0 + x) * 3) as usize;
        [self.rgb[at], self.rgb[at + 1], self.rgb[at + 2]]
    }

    /// Where in a `coded` picture the pixel at (x, y) of the view lies, to the middle of its 8 px.
    fn spot(&self, x: u32, y: u32) -> (f64, f64) {
        let [r, g, _] = self.px(x, y);
        (r as f64 * 8.0 + 4.0, g as f64 * 8.0 + 4.0)
    }
}

/// The first image placed after byte `from`, once the draw that holds it has ended; its file is read at once, before a later draw can replace it.
fn placed(tty: &Tty, from: usize) -> Placed {
    let found = |out: &[u8]| -> Option<(usize, usize, usize)> {
        let start = find(out, b"\x1b_Ga=T,", 0)?;
        let close = find(out, b"\x1b\\", start)?;
        let end = find(out, b"\x1b[?2026l", close)?;
        Some((start, close, end))
    };
    if !tty.wait(from, PATIENCE, |out| found(out).is_some()) {
        tty.fail(&format!("no image placed after byte {from}"));
    }
    let out = tty.out();
    let (start, close, end) = found(&out[from..]).unwrap();
    let (start, close, end) = (from + start, from + close, from + end);
    let body = &out[start + 3..close];
    let split = body.iter().position(|&b| b == b';').unwrap();
    let file = placed_file(&body[split + 1..]);
    let rgb = std::fs::read(&file).unwrap_or_else(|e| tty.fail(&format!("{file:?}: {e}")));
    let draw = out[..start]
        .windows(8)
        .rposition(|w| w == b"\x1b[?2026h")
        .unwrap();
    let placed = Placed {
        keys: String::from_utf8_lossy(&body[..split]).into_owned(),
        file,
        rgb,
        before: out[draw..start].to_vec(),
        end: end + 8,
    };
    let (w, h) = placed.size();
    assert_eq!(placed.rgb.len(), (w * h * 3) as usize, "{}", placed.keys);
    placed
}

/// Whether a place found in a `coded` picture is within 8 px of where it should be.
fn near((x, y): (f64, f64), (left, top): (f64, f64)) -> bool {
    (x - left).abs() <= 8.0 && (y - top).abs() <= 8.0
}

/// The picture rows of the last synchronized update in `out` that writes any: each row's place and its bytes up to the closing SGR 0 (bold or a colour code first; the toast and the bottom row's erasing start otherwise).
fn written_rows(out: &[u8]) -> Vec<(usize, usize, Vec<u8>)> {
    let (mut last, mut at) = (Vec::new(), 0);
    while let Some(start) = find(out, b"\x1b[?2026h", at) {
        let Some(end) = find(out, b"\x1b[?2026l", start) else {
            break;
        };
        let update = &out[start..end];
        let (mut rows, mut from) = (Vec::new(), 0);
        while let Some(csi) = find(update, b"\x1b[", from) {
            from = csi + 2;
            let tail = &update[from..];
            let digits = tail
                .iter()
                .take_while(|b| b.is_ascii_digit() || **b == b';')
                .count();
            let place = String::from_utf8_lossy(&tail[..digits]).into_owned();
            let (Some(&b'H'), Some((row, col))) = (tail.get(digits), place.split_once(';')) else {
                continue;
            };
            let body = &tail[digits + 1..];
            let picture = [&b"\x1b[1m"[..], b"\x1b[3", b"\x1b[9"]
                .iter()
                .any(|start| body.starts_with(start));
            if let (true, Some(close)) = (picture, find(body, b"\x1b[0m", 0)) {
                rows.push((
                    row.parse().unwrap(),
                    col.parse().unwrap(),
                    body[..close].to_vec(),
                ));
            }
        }
        if !rows.is_empty() {
            last = rows;
        }
        at = end;
    }
    last
}

/// The SGR sequences in a row's bytes.
fn codes(row: &[u8]) -> Vec<String> {
    let (mut codes, mut at) = (Vec::new(), 0);
    while let Some(start) = find(row, b"\x1b[", at) {
        let end = start + row[start..].iter().position(|&b| b == b'm').unwrap() + 1;
        codes.push(String::from_utf8_lossy(&row[start..end]).into_owned());
        at = end;
    }
    codes
}

#[test]
fn the_decoders_formats_and_svg_open_as_stills() {
    // rows S1, V17, N7: the image crate's formats (AGENTS.md, 2026-11-10) and SVG by its suffix in any case (is_svg of lib/ascii_image.py:26 at tag python-final), an MPO (its MPF index lists two pictures: a still all the same, is_animation of lib/ascii_image.py:61) shown as its first picture, a multi-page TIFF's first page (N7: the old tools played the pages as an animation); each shown as ASCII, its white left half inked and its black right half blank, and each a still: `v` answers `no kitty graphics` (N13), which a video does not
    let sandbox = Sandbox::new();
    let picture = halves(160, 80);
    // the file, and whether its left half alone is white
    let mut files: Vec<(PathBuf, bool)> = Vec::new();
    for suffix in [
        "png", "jpg", "gif", "bmp", "tif", "webp", "tga", "qoi", "ppm",
    ] {
        files.push((save(&picture, &sandbox.path(format!("pic.{suffix}"))), true));
    }
    // an icon's picture needs an alpha band
    let icon = DynamicImage::ImageRgb8(picture.clone()).into_rgba8();
    files.push((save_rgba(&icon, &sandbox.path("pic.ico")), true));
    let float = DynamicImage::ImageRgb32F(DynamicImage::ImageRgb8(picture.clone()).into_rgb32f());
    for suffix in ["hdr", "exr"] {
        let path = sandbox.path(format!("pic.{suffix}"));
        float.save(&path).unwrap();
        files.push((path, true));
    }
    let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="160" height="80"><rect width="160" height="80"/><rect width="80" height="80" fill="#fff"/></svg>"##;
    for name in ["pic.svg", "upper.SVG"] {
        std::fs::write(sandbox.path(name), svg).unwrap();
        files.push((sandbox.path(name), true));
    }
    // its second picture the other way round: white on the right
    let mirrored = image::imageops::flip_horizontal(&picture);
    std::fs::write(sandbox.path("pic.mpo"), mpo(&picture, &mirrored)).unwrap();
    files.push((sandbox.path("pic.mpo"), true));
    let first = DynamicImage::ImageRgb8(picture.clone())
        .into_luma8()
        .into_raw();
    std::fs::write(
        sandbox.path("pages.tif"),
        tiff(160, 80, &[first, vec![255; 160 * 80]]),
    )
    .unwrap();
    files.push((sandbox.path("pages.tif"), true));
    // Pillow's pictures of four flat quarters, none black (the image crate writes neither format)
    for name in ["picture-quarters.avif", "picture-quarters.dds"] {
        std::fs::copy(
            Path::new(CRATE).join("tests/golden").join(name),
            sandbox.path(name),
        )
        .unwrap();
        files.push((sandbox.path(name), false));
    }
    let mut ttys: Vec<Tty> = files
        .iter()
        .map(|(file, _)| sandbox.launch().arg(file).size(SMALL).start())
        .collect();
    for (tty, (file, half)) in ttys.iter_mut().zip(&files) {
        let name = file.file_name().unwrap().to_string_lossy();
        // 80 x 18 cells from row 4: half of them inked, or all
        tty.until(&format!("{name}'s picture"), |screen| screen.inked() > 600);
        says(tty, b"v", "no kitty graphics");
        let screen = tty.screen();
        if *half {
            let inked: Vec<bool> = (1..=80).map(|col| screen.cell(12, col).ch != ' ').collect();
            assert!(
                inked[1..36].iter().all(|i| *i) && !inked[44..79].iter().any(|i| *i),
                "{name}: {:?}",
                screen.row(12)
            );
        }
        quit(tty);
    }
}

#[test]
fn an_animated_png_or_webp_plays_as_video() {
    // row V17: is_animation of lib/ascii_image.py:61 at tag python-final: a picture of several frames is no still but media, played with a status row to its end (an animated GIF: tests/playback.rs)
    let sandbox = Sandbox::new();
    let (apng, webp) = (sandbox.path("anim.png"), sandbox.path("anim.webp"));
    let source = "testsrc2=s=80x45:r=10:d=2";
    ffmpeg(
        &sandbox,
        &[
            "-f",
            "lavfi",
            "-i",
            source,
            "-f",
            "apng",
            apng.to_str().unwrap(),
        ],
    );
    ffmpeg(
        &sandbox,
        &[
            "-f",
            "lavfi",
            "-i",
            source,
            "-c:v",
            "libwebp_anim",
            "-loop",
            "0",
            webp.to_str().unwrap(),
        ],
    );
    let mut ttys = [&apng, &webp].map(|file| sandbox.launch().arg(file).size(SMALL).start());
    for tty in &mut ttys {
        assert_eq!(tty.exit(), 0);
        // the draws one by one: the screen at the end is the normal one again
        let played = replay(&tty.out(), SMALL, |screen| {
            screen.picture_rows().len() > 15 && screen.row(24).starts_with("> 00:0")
        });
        assert!(played.contains(&true), "no frame under a status row");
    }
}

#[test]
fn an_svg_that_does_not_draw_is_no_still() {
    // row S1: upright of lib/ascii_image.py:82 at tag python-final: an SVG resvg cannot draw is no still but goes the media way; run in this harness the old viewer then drew no picture and ended at once, exit 0, and so does the new one (mpv plays nothing of it); a still would wait for `q`
    let sandbox = Sandbox::new();
    let broken = sandbox.path("broken.svg");
    std::fs::write(&broken, "<svg").unwrap();
    let mut tty = sandbox.launch().arg(&broken).size(SMALL).start();
    assert_eq!(tty.exit(), 0);
    let inked = replay(&tty.out(), SMALL, |screen| screen.inked());
    assert!(
        inked.iter().all(|cells| *cells == 0),
        "a picture: {inked:?}"
    );
}

#[test]
fn exif_orientation_shows_the_picture_upright() {
    // row S2: ImageOps.exif_transpose of lib/ascii_image.py:93 at tag python-final (tests/test_image.py:81): stored 80 x 40 with red in the top left corner, orientation 6 shows it 40 x 80 with the red top right, orientation 8 bottom left; sent at its size in image mode
    let sandbox = kitty_sandbox();
    let stored = RgbImage::from_fn(80, 40, |x, y| {
        Rgb(if x < 20 && y < 20 {
            [255, 0, 0]
        } else {
            [0, 0, 0]
        })
    });
    let (cam, png) = (sandbox.path("cam.jpg"), sandbox.path("left.png"));
    std::fs::write(&cam, jpeg(&stored, Some(6))).unwrap();
    std::fs::write(&png, oriented_png(&stored, 8)).unwrap();
    for (file, red) in [(cam, (30, 5)), (png, (5, 74))] {
        let mut tty = kitty_view(&sandbox, &file, WIDE);
        let shown = placed(&tty, 0);
        assert_eq!(shown.size(), (40, 80), "{file:?}");
        let ([r, g, _], [dark, ..]) = (shown.px(red.0, red.1), shown.px(5, 40));
        assert!(r > 200 && g < 60 && dark < 60, "{file:?}: {r} {g} {dark}");
        quit(&mut tty);
    }
}

#[test]
fn transparency_shows_over_squares_that_stay_put() {
    // rows S3, K2: checkerboard and kitty_place of bin/ascii-viewer:202 and :2006 at tag python-final (tests/test_viewer.py:1146): a clear 900 x 600 picture in 900 x 600 px is sent as the 8 px squares, #666666 at the top left, the same at 141 % and after a pan; a palette picture's transparent index shows them too (tests/test_image.py:30)
    let sandbox = kitty_sandbox();
    let clear = save_rgba(&RgbaImage::new(900, 600), &sandbox.path("clear.png"));
    let board = checkerboard(900, 600);
    let mut tty = kitty_view(&sandbox, &clear, HALF);
    let fit = placed(&tty, 0);
    assert_eq!(fit.size(), (900, 600));
    assert_eq!(
        [fit.px(0, 0), fit.px(8, 0), fit.px(0, 8), fit.px(8, 8)],
        [[0x66; 3], [0x99; 3], [0x99; 3], [0x66; 3]]
    );
    assert!(fit.rgb == board.as_raw()[..], "the squares at the fit");
    for keys in [&b"+"[..], b"l"] {
        let at = tty.send(keys);
        let moved = placed(&tty, at);
        assert!(moved.rgb == board.as_raw()[..], "after {}", shown(keys));
    }
    quit(&mut tty);
    let palette = sandbox.path("palette.png");
    std::fs::write(&palette, palette_png(100, 60)).unwrap();
    let mut tty = kitty_view(&sandbox, &palette, HALF);
    let sent = placed(&tty, 0);
    assert_eq!(sent.size(), (100, 60));
    let board = checkerboard(100, 60);
    for (x, y) in [(0, 0), (8, 0), (49, 59), (50, 0), (99, 59)] {
        let want = if x < 50 {
            board.get_pixel(x, y).0
        } else {
            [255; 3]
        };
        assert_eq!(sent.px(x, y), want, "({x}, {y})");
    }
    quit(&mut tty);
}

#[test]
fn the_probe_offers_kitty_a_file_and_ok_gives_the_image() {
    // row S4: kitty_terminal of lib/ascii_term.py:51 and kitty_reads_files of bin/ascii-viewer:234 at tag python-final (tests/test_viewer_tty.py:130): with KITTY_WINDOW_ID set or TERM=xterm-kitty a 3-byte file in /dev/shm is offered, `\e_Gi=31,s=1,v=1,a=q,t=t,f=24;<base64 path>\e\\` then `\e[c`; kitty's `OK` gives the image itself; the file goes
    let picture = gradient(800, 400);
    let by_id = kitty_sandbox();
    let mut by_term = Sandbox::new();
    by_term.set("TERM", "xterm-kitty");
    for sandbox in [&by_id, &by_term] {
        let file = save(&picture, &sandbox.path("pic.png"));
        // answered here, once the file is read: the probe waits a second for it
        let mut tty = sandbox.launch().arg(&file).start();
        let start = tty.expect(0, b"\x1b_Gi=31,s=1,v=1,a=q,t=t,f=24;");
        let end = tty.expect(start, b"\x1b\\\x1b[c");
        let offered = placed_file(&tty.out()[start..end - 5]);
        let held = std::fs::read(&offered);
        tty.send(KITTY_OK);
        let name = offered.to_string_lossy().into_owned();
        assert!(
            name.starts_with("/dev/shm/tty-graphics-protocol-") && name.len() > 31,
            "{name}"
        );
        assert_eq!(held.unwrap(), [0, 0, 0]);
        assert_eq!(placed(&tty, end).size(), (800, 400));
        assert!(!offered.exists(), "the probe's file stays");
        quit(&mut tty);
    }
}

#[test]
fn without_kittys_ok_the_still_is_ascii_and_v_says_so() {
    // rows S4, N13: tests/test_viewer_tty.py:158 at tag python-final: an error for an answer, or none within 1 s, gives the ASCII picture; an empty KITTY_WINDOW_ID asks nothing (kitty_terminal of lib/ascii_term.py:51); a key typed while the probe waits is lost (kitty_reads_files of bin/ascii-viewer:234); N13: then `v` toasts `no kitty graphics` and the ASCII stays, drawn again without a clear (the old viewer turned to image mode and showed nothing, S6)
    let picture = gradient(800, 400);
    let (error, silent, mut empty) = (kitty_sandbox(), kitty_sandbox(), Sandbox::new());
    empty.set("KITTY_WINDOW_ID", "");
    let ebadf: &[u8] = b"\x1b_Gi=31;EBADF:could not read\x1b\\\x1b[?62;22c";
    let file = |sandbox: &Sandbox| save(&picture, &sandbox.path("pic.png"));
    let mut ttys = [
        error
            .launch()
            .arg(file(&error))
            .kitty(ebadf, Duration::ZERO)
            .start(),
        silent.launch().arg(file(&silent)).start(),
        empty.launch().arg(file(&empty)).start(),
    ];
    // no answer: `q` while the probe waits its second
    ttys[1].expect(0, b"\x1b_Gi=31,s=1,v=1,a=q,t=t,f=24;");
    ttys[1].send(b"q");
    for (i, tty) in ttys.iter_mut().enumerate() {
        tty.until("the ASCII picture", |screen| screen.inked() > 1000);
        let out = tty.out();
        assert_eq!(find(&out, b"\x1b_Gi=31,", 0).is_some(), i < 2, "probed");
        assert!(find(&out, b"a=T,", 0).is_none(), "an image placed");
        let at = tty.send(b"v");
        let end = tty.expect(at, toast("no kitty graphics"));
        let update = update_at(tty, end);
        assert!(
            find(&update, b"\x1b[2J", 0).is_none() && find(&update, b"a=T,", 0).is_none(),
            "{}",
            shown(&update[..update.len().min(200)])
        );
        assert!(!written_rows(&update).is_empty(), "the ASCII not drawn");
        says(tty, b"3", DARKER);
        assert!(tty.running(), "a key from the probe's wait was taken");
        quit(tty);
    }
}

#[test]
fn v_switches_between_the_image_and_its_ascii() {
    // rows S5, K3, K5, V16a: `v` of bin/ascii-viewer:2257 at tag python-final toasts `ascii` and draws the ASCII picture from a cleared screen, the kitty images deleted first; `v` again toasts `image` and sends the image again through a new file, the one before removed first
    let sandbox = kitty_sandbox();
    let picture = save(&gradient(800, 400), &sandbox.path("pic.png"));
    let mut tty = kitty_view(&sandbox, &picture, WIDE);
    let first = placed(&tty, 0);
    let at = tty.send(b"v");
    let end = tty.expect(at, toast("ascii"));
    let update = update_at(&tty, end);
    assert!(
        update.starts_with(&[STILL_DRAW, b"\x1b[2J"].concat()),
        "{}",
        shown(&update[..update.len().min(80)])
    );
    assert!(find(&update, b"a=T,", 0).is_none());
    assert_eq!(written_rows(&update).len(), 45, "the ASCII picture");
    let at = says(&mut tty, b"v", "image");
    let again = placed(&tty, at);
    assert_eq!(again.size(), (800, 400));
    assert!(again.before.starts_with(&[STILL_DRAW, b"\x1b[2J"].concat()));
    assert!(again.file != first.file && !first.file.exists());
    quit(&mut tty);
}

#[test]
fn h_and_l_step_through_the_folders_images_by_name() {
    // rows S7, S18, N7: sibling_images, step and settle_step of bin/ascii-viewer:271, :1888 and :1900 at tag python-final (tests/test_viewer.py:459 and :511): the files whose lower-cased suffix the decoders take, or .svg, by lower-cased name, wrapping, one that does not open passed over (also one whose header identifies, its pixels cut short), toast `<name>  <place>/<count>`; N7: a PCX file, which only mpv decodes, is no entry (the old listing held it: `B.png  2/7`); one over 179 megapixels is passed over with `; <name> too large`
    let sandbox = Sandbox::new();
    let pics = sandbox.folder("pics");
    let other = sandbox.folder("other");
    let small = gradient(80, 40);
    save(&small, &pics.join("a.png"));
    save(&small, &pics.join("B.png"));
    save(&small, &pics.join("c.jpg"));
    std::fs::rename(pics.join("c.jpg"), pics.join("c.JPG")).unwrap();
    std::fs::write(pics.join("d.txt"), "words").unwrap();
    ffmpeg(
        &sandbox,
        &[
            "-f",
            "lavfi",
            "-i",
            "testsrc2=s=80x40:d=1",
            "-frames:v",
            "1",
            pics.join("e.pcx").to_str().unwrap(),
        ],
    );
    std::fs::create_dir(pics.join("f.png")).unwrap();
    std::fs::write(
        pics.join("g.svg"),
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="80" height="40"><rect width="40" height="40"/></svg>"#,
    )
    .unwrap();
    save(&small, &other.join("z.png"));
    std::os::unix::fs::symlink("../other/z.png", pics.join("h.png")).unwrap();
    std::os::unix::fs::symlink("missing.png", pics.join("i.png")).unwrap();
    std::fs::write(pics.join("j.png"), "not a picture").unwrap();
    let big = sandbox.folder("big");
    save(&gradient(40, 30), &big.join("0.png"));
    png_header(&big.join("1.png"), 20_000, 10_000);
    save(&gradient(40, 30), &big.join("2.png"));
    // a header that identifies, its pixels cut short (tests/test_viewer.py:478)
    let whole = std::fs::read(big.join("2.png")).unwrap();
    std::fs::write(big.join("3.png"), &whole[..60]).unwrap();
    let mut tty = drawn(&sandbox, &pics.join("a.png"), SMALL);
    let mut over = drawn(&sandbox, &big.join("0.png"), SMALL);
    for (keys, text) in [
        (&b"l"[..], "B.png  2/6"),
        (b"l", "c.JPG  3/6"),
        (RIGHT, "g.svg  4/6"),
        (b"l", "h.png  5/6"),
        (b"l", "a.png  1/6"),
        (b"h", "h.png  5/6"),
        (LEFT, "g.svg  4/6"),
    ] {
        says(&mut tty, keys, text);
    }
    quit(&mut tty);
    for (keys, text) in [
        (b"l", "2.png  3/4; 1.png too large"),
        (b"l", "0.png  1/4"),
        (b"h", "2.png  3/4"),
        (b"h", "0.png  1/4; 1.png too large"),
    ] {
        says(&mut over, keys, text);
    }
    quit(&mut over);
}

#[test]
fn steps_at_the_edges_of_the_listing() {
    // row S7a: step and show_next of bin/ascii-viewer:1888 and :1850 at tag python-final: a still that is no entry (a PNG named .raw) counts from the first entry, so `l` shows the second and `h` the last; without entries nothing happens; a lone image opens again, `a.png  1/1`; when no entry opens, nothing changes and nothing is said
    let sandbox = Sandbox::new();
    let some = sandbox.folder("some");
    for name in ["a.png", "b.png", "c.png"] {
        save(&gradient(80, 40), &some.join(name));
    }
    save(&gradient(80, 40), &some.join("x.png"));
    std::fs::rename(some.join("x.png"), some.join("x.raw")).unwrap();
    let lone = sandbox.folder("lone");
    std::fs::copy(some.join("x.raw"), lone.join("x.raw")).unwrap();
    let solo = sandbox.folder("solo");
    save(&gradient(80, 40), &solo.join("a.png"));
    let dead = sandbox.folder("dead");
    save(&gradient(800, 400), &dead.join("a.png"));
    let mut ttys = [
        drawn(&sandbox, &some.join("x.raw"), SMALL),
        drawn(&sandbox, &some.join("x.raw"), SMALL),
        drawn(&sandbox, &lone.join("x.raw"), SMALL),
        drawn(&sandbox, &solo.join("a.png"), SMALL),
        drawn(&sandbox, &dead.join("a.png"), SMALL),
    ];
    says(&mut ttys[0], b"l", "b.png  2/3");
    says(&mut ttys[1], b"h", "c.png  3/3");
    says(&mut ttys[3], b"l", "a.png  1/1");
    says(&mut ttys[3], b"h", "a.png  1/1");
    // the picture on screen, and then no entry that opens
    std::fs::write(dead.join("a.png"), "not a picture").unwrap();
    std::fs::write(dead.join("b.png"), "not a picture").unwrap();
    for i in [2, 4] {
        let tty = &mut ttys[i];
        let at = tty.send(b"l3");
        let sentinel = toast(DARKER);
        let end = tty.expect(at, &sentinel);
        assert_eq!(
            find(&tty.out(), b"\x1b[7m", at),
            Some(end - sentinel.len()),
            "a toast of its own"
        );
        assert!(tty.screen().inked() > 600, "the picture went");
    }
    for tty in &mut ttys {
        quit(tty);
    }
}

#[test]
fn a_batch_of_steps_opens_the_image_presses_one_by_one_reach() {
    // row S8: settle_step of bin/ascii-viewer:1900 at tag python-final (tests/test_viewer.py:491): 0.png to 4.png, 1.png no picture, from 0.png: `ll` 3, `lll` 4, `llll` 0, `llh` 2, `h` 4, `hhh` 2, `lh` 0; the batch opens one image: in image mode `lll` sends 4.png, 44 x 30 here, and nothing else, with one toast; a key of another kind first settles the steps: `llk` turns 3.png (tests/test_viewer.py:460), 43 x 30 here
    let mut sandbox = Sandbox::new();
    let pics = sandbox.folder("pics");
    for i in [0, 2, 3, 4] {
        save(&gradient(40 + i, 30), &pics.join(format!("{i}.png")));
    }
    std::fs::write(pics.join("1.png"), "not a picture").unwrap();
    let batches: [(&[u8], u32); 7] = [
        (b"ll", 3),
        (b"lll", 4),
        (b"llll", 0),
        (b"llh", 2),
        (b"h", 4),
        (b"hhh", 2),
        (b"lh", 0),
    ];
    let first = pics.join("0.png");
    let mut ttys: Vec<Tty> = batches
        .iter()
        .map(|_| drawn(&sandbox, &first, SMALL))
        .collect();
    sandbox.set("KITTY_WINDOW_ID", "1");
    let mut turned = kitty_view(&sandbox, &first, SMALL);
    let mut stepped = kitty_view(&sandbox, &first, SMALL);
    for (tty, (keys, reached)) in ttys.iter_mut().zip(batches) {
        says(tty, keys, &format!("{reached}.png  {}/5", reached + 1));
        quit(tty);
    }
    assert_eq!(placed(&stepped, 0).size(), (40, 30));
    let at = stepped.send(b"lll");
    let toasted = stepped.expect(at, toast("4.png  5/5"));
    let end = stepped.expect(toasted, b"\x1b[?2026l");
    let out = stepped.out();
    let batch = &out[at..end];
    assert_eq!(
        (count(batch, b"\x1b_Ga=T,"), count(batch, b"\x1b[7m")),
        (1, 1),
        "{}",
        shown(batch)
    );
    assert_eq!(placed(&stepped, at).size(), (44, 30));
    quit(&mut stepped);
    assert_eq!(placed(&turned, 0).size(), (40, 30));
    let at = says(&mut turned, b"llk", "3.png  4/5");
    assert_eq!(placed(&turned, at).size(), (30, 43));
    quit(&mut turned);
}

#[test]
fn turns_are_quarter_turns_of_the_view_alone() {
    // rows S10, K3, V16a: rotate and turned of bin/ascii-viewer:1984 and :1696 at tag python-final (tests/test_viewer.py:988, :1752): `k` and Up turn a quarter right, `j` and Down a quarter left, each drawn from a cleared screen; 400 x 800 px goes at row 11, column 79 over 44 x 40 cells (kitty_pixels of bin/ascii-viewer:2027); the file is never written; the next image shows unturned, and the look and the ASCII mode go with it
    let sandbox = kitty_sandbox();
    let pics = sandbox.folder("pics");
    let a = save(&marked(800, 400), &pics.join("a.png"));
    save(&gradient(300, 200), &pics.join("b.png"));
    let stored = std::fs::read(&a).unwrap();
    let mut tty = kitty_view(&sandbox, &a, WIDE);
    let first = placed(&tty, 0);
    assert_eq!((first.size(), first.px(10, 10)), ((800, 400), [255, 0, 0]));
    // where the red corner goes: the size, and a pixel inside the corner
    let right = ((400, 800), (389, 10));
    let upside = ((800, 400), (789, 389));
    let left = ((400, 800), (10, 789));
    let upright = ((800, 400), (10, 10));
    for (keys, (size, (x, y))) in [
        (&b"k"[..], right),
        (b"k", upside),
        (UP, left),
        (DOWN, upside),
        (b"j", right),
        (b"j", upright),
        (b"j", left),
    ] {
        let at = tty.send(keys);
        let turned = placed(&tty, at);
        assert_eq!(turned.size(), size, "after {}", shown(keys));
        assert_eq!(turned.px(x, y), [255, 0, 0], "after {}", shown(keys));
        assert!(
            find(&turned.before, b"\x1b[2J", 0).is_some(),
            "not from a cleared screen"
        );
        if size == (400, 800) {
            assert!(
                turned.before.ends_with(b"\x1b[11;79H")
                    && turned.keys.ends_with(",c=44,r=40,C=1,z=-1,q=2"),
                "{} {}",
                shown(&turned.before),
                turned.keys
            );
        }
    }
    assert!(std::fs::read(&a).unwrap() == stored, "the file was written");
    let at = says(&mut tty, b"l", "b.png  2/2");
    assert_eq!(placed(&tty, at).size(), (300, 200));
    let at = says(&mut tty, b"h", "a.png  1/2");
    let back = placed(&tty, at);
    assert_eq!((back.size(), back.px(10, 10)), ((800, 400), [255, 0, 0]));
    says(&mut tty, b"v", "ascii");
    says(&mut tty, b"c", "mono");
    let at = tty.send(b"l");
    let end = tty.expect(at, toast("b.png  2/2"));
    let update = update_at(&tty, end);
    assert!(find(&update, b"a=T,", 0).is_none(), "ASCII mode went");
    let rows = written_rows(&update);
    assert!(!rows.is_empty());
    for (row, _, bytes) in &rows {
        assert_eq!(codes(bytes), ["\x1b[97m"], "row {row}: mono went");
    }
    quit(&mut tty);
}

#[test]
fn zoom_steps_go_by_root_2_from_the_fit_to_8_px_a_pixel() {
    // rows Z1, Z2, Z7, V16a: zoom_by and zoom_limit of bin/ascii-viewer:1769 and :1734 at tag python-final (tests/test_viewer.py:885 and :903): 1800 x 1200 in 900 x 600 px fits at 50 %; `+` `=` and `-` step by 2^0.5 between `fit` and `zoom 800%` about the middle, held at both ends with the toast and a cleared draw all the same; a 100 x 100 picture is never enlarged at the fit and stops at 800 % after 6 steps
    let sandbox = kitty_sandbox();
    let big = save(&coded(1800, 1200), &sandbox.path("big.png"));
    let small = save(&gradient(100, 100), &sandbox.path("small.png"));
    let mut tty = kitty_view(&sandbox, &big, HALF);
    let mut little = kitty_view(&sandbox, &small, HALF);
    let fit = placed(&tty, 0);
    assert_eq!(fit.size(), (900, 600));
    assert!(near(fit.spot(0, 0), (0.0, 0.0)) && near(fit.spot(899, 599), (1796.0, 1196.0)));
    let steps: [(&[u8], &str); 8] = [
        (b"-", "fit"),
        (b"+", "zoom 71%"),
        (b"=", "zoom 100%"),
        (&[b'+'; 20], "zoom 800%"),
        (b"+", "zoom 800%"),
        (b"-", "zoom 566%"),
        (&[b'-'; 20], "fit"),
        (b"+", "zoom 71%"),
    ];
    for (keys, text) in steps {
        let at = says(&mut tty, keys, text);
        let zoomed = placed(&tty, at);
        assert_eq!(zoomed.size(), (900, 600), "{text}");
        assert!(
            find(&zoomed.before, b"\x1b[2J", 0).is_some(),
            "{text}: not from a cleared screen"
        );
        assert!(
            near(zoomed.spot(450, 300), (900.0, 600.0)),
            "{text}: the middle at {:?}",
            zoomed.spot(450, 300)
        );
    }
    quit(&mut tty);
    assert_eq!(placed(&little, 0).size(), (100, 100));
    let at = says(&mut little, &[b'+'; 20], "zoom 800%");
    assert_eq!(placed(&little, at).size(), (800, 600));
    quit(&mut little);
}

#[test]
fn ascii_zoom_passes_over_steps_that_show_the_whole_picture() {
    // rows Z3, Z1: tests/test_viewer_tty.py:113 and :130 at tag python-final: 800 x 400 fits 1800 x 1200 px 2.25 times over and the cells are filled either way, so in ASCII the first `+` crops at `zoom 283%` and the way back passes the same steps to `fit`; in image mode the first `+` is `zoom 141%`
    let sandbox = kitty_sandbox();
    let picture = save(&gradient(800, 400), &sandbox.path("pic.png"));
    let plain = Sandbox::new();
    let mut ascii = drawn(&plain, &picture, WIDE);
    let mut image = kitty_view(&sandbox, &picture, WIDE);
    for (keys, text) in [
        (b"+", "zoom 283%"),
        (b"+", "zoom 400%"),
        (b"-", "zoom 283%"),
        (b"-", "fit"),
    ] {
        says(&mut ascii, keys, text);
    }
    let out = ascii.out();
    assert!(
        find(&out, b"zoom 141%", 0).is_none() && find(&out, b"zoom 200%", 0).is_none(),
        "a step that shows it all"
    );
    quit(&mut ascii);
    placed(&image, 0);
    says(&mut image, b"+", "zoom 141%");
    quit(&mut image);
}

#[test]
fn zoomed_in_the_move_keys_pan_and_stop_at_the_edges() {
    // rows Z4, Z7: pan_by, zoom_by and crop_box of bin/ascii-viewer:1784, :1769 and :1748 at tag python-final (tests/test_viewer.py:928, :948 and :969): at 100 % the 900 x 600 px view of an 1800 x 1200 picture moves a quarter of itself by h j k l and the arrows, a sixteenth by H J K L and Shift+arrows, held at the edges, without a toast; a zoom keeps the view's middle, and out again the view is kept inside the picture; the pixels say where the view is (red x / 8, green y / 8)
    let sandbox = kitty_sandbox();
    let big = save(&coded(1800, 1200), &sandbox.path("big.png"));
    let mut tty = kitty_view(&sandbox, &big, HALF);
    placed(&tty, 0);
    says(&mut tty, b"++", "zoom 100%");
    toast_gone(&mut tty);
    let from = tty.len();
    // the keys, and the view's left and top after them
    let moves: [(&[u8], f64, f64); 16] = [
        (b"l", 675.0, 300.0),
        (RIGHT, 900.0, 300.0),
        (b"l", 900.0, 300.0),
        (b"hhhhhhhhh", 0.0, 300.0),
        (b"k", 0.0, 150.0),
        (UP, 0.0, 0.0),
        (b"k", 0.0, 0.0),
        (b"jjjjjjjjj", 0.0, 600.0),
        (b"L", 56.25, 600.0),
        (SHIFT_RIGHT, 112.5, 600.0),
        (b"K", 112.5, 562.5),
        (SHIFT_UP, 112.5, 525.0),
        (b"H", 56.25, 525.0),
        (SHIFT_LEFT, 0.0, 525.0),
        (b"J", 0.0, 562.5),
        (SHIFT_DOWN, 0.0, 600.0),
    ];
    for (keys, left, top) in moves {
        let at = tty.send(keys);
        let view = placed(&tty, at);
        assert_eq!(view.size(), (900, 600), "turned by {}", shown(keys));
        assert!(
            near(view.spot(0, 0), (left, top)),
            "{}: the view at {:?}, not ({left}, {top})",
            shown(keys),
            view.spot(0, 0)
        );
    }
    let at = tty.send(b"3");
    let sentinel = toast(DARKER);
    let end = tty.expect(at, &sentinel);
    assert_eq!(
        find(&tty.out(), b"\x1b[7m", from),
        Some(end - sentinel.len()),
        "a pan toasted"
    );
    // the view 0..900 x 600..1200, its middle (450, 900): in to 141 % about it, back to 100 % there, out to 71 % pushed inside the picture
    let at = says(&mut tty, b"+", "zoom 141%");
    let view = placed(&tty, at);
    assert!(
        near(view.spot(450, 300), (450.0, 900.0)),
        "{:?}",
        view.spot(450, 300)
    );
    let at = says(&mut tty, b"-", "zoom 100%");
    assert!(near(placed(&tty, at).spot(0, 0), (0.0, 600.0)));
    let at = says(&mut tty, b"-", "zoom 71%");
    let view = placed(&tty, at);
    assert!(
        near(view.spot(450, 300), (636.4, 775.7)) && near(view.spot(0, 599), (4.0, 1196.0)),
        "{:?} {:?}",
        view.spot(450, 300),
        view.spot(0, 599)
    );
    quit(&mut tty);
}

#[test]
fn an_axis_the_picture_does_not_fill_shows_whole_and_does_not_pan() {
    // rows Z4, Z7: crop_box of bin/ascii-viewer:1748 at tag python-final (tests/test_viewer.py:923): 1000 x 200 in 400 x 600 px fits at 40 %; at 80 % the view is 500 px across and all 200 high, sent as 400 x 160 px at row 12; `k` and `j` change nothing, `l` moves it a quarter
    let sandbox = kitty_sandbox();
    let wide = save(&coded(1000, 200), &sandbox.path("wide.png"));
    let window = Size {
        rows: 30,
        cols: 40,
        xpx: 400,
        ypx: 600,
    };
    let mut tty = kitty_view(&sandbox, &wide, window);
    placed(&tty, 0);
    let at = says(&mut tty, b"++", "zoom 80%");
    let zoomed = placed(&tty, at);
    assert_eq!(zoomed.size(), (400, 160));
    assert!(
        zoomed.before.ends_with(b"\x1b[12;1H"),
        "{}",
        shown(&zoomed.before)
    );
    assert!(
        near(zoomed.spot(0, 0), (250.0, 0.0)),
        "{:?}",
        zoomed.spot(0, 0)
    );
    for keys in [&b"k"[..], b"j"] {
        let at = tty.send(keys);
        assert!(
            placed(&tty, at).rgb == zoomed.rgb,
            "{} moved it",
            shown(keys)
        );
    }
    let at = tty.send(b"l");
    assert!(near(placed(&tty, at).spot(0, 0), (375.0, 0.0)));
    quit(&mut tty);
}

#[test]
fn backspace_goes_back_to_the_fit_and_the_middle() {
    // row Z5: bin/ascii-viewer:2249 at tag python-final (tests/test_viewer.py:1102): Backspace, 0x7f or 0x08, zooms out to `fit` with the view centred again: zoomed in after it, the view is the picture's middle
    let sandbox = kitty_sandbox();
    let big = save(&coded(1800, 1200), &sandbox.path("big.png"));
    let mut tty = kitty_view(&sandbox, &big, HALF);
    placed(&tty, 0);
    for backspace in [BACKSPACE, b"\x08"] {
        says(&mut tty, b"++", "zoom 100%");
        let at = tty.send(b"l");
        assert!(near(placed(&tty, at).spot(0, 0), (675.0, 300.0)));
        let at = says(&mut tty, backspace, "fit");
        let fit = placed(&tty, at);
        assert_eq!(fit.size(), (900, 600));
        assert!(near(fit.spot(0, 0), (0.0, 0.0)));
        let at = says(&mut tty, b"++", "zoom 100%");
        assert!(near(placed(&tty, at).spot(0, 0), (450.0, 300.0)));
        says(&mut tty, backspace, "fit");
    }
    quit(&mut tty);
}

#[test]
fn a_zoom_kept_from_a_smaller_window_is_held_to_the_new_limit() {
    // rows Z1, V10: steps of bin/ascii-viewer:1739 at tag python-final (tests/test_viewer.py:1123): 3600 x 2400 in 900 x 600 px fits at 25 %, 10 steps to 800 %; the window grown to 1800 x 1200 px, the picture is drawn at once at the new size from a cleared screen (poll of bin/ascii-viewer:2340); there the fit is 50 % and the limit 8 steps: the 10 count as 8, so `-` gives 7 (`zoom 566%`, not 9's 1131 %) and `+` stops at 800 %
    let sandbox = kitty_sandbox();
    let big = sandbox.path("big.png");
    ffmpeg(
        &sandbox,
        &[
            "-f",
            "lavfi",
            "-i",
            "color=c=gray:s=3600x2400",
            "-frames:v",
            "1",
            big.to_str().unwrap(),
        ],
    );
    let mut tty = kitty_view(&sandbox, &big, HALF);
    placed(&tty, 0);
    says(&mut tty, &[b'+'; 12], "zoom 800%");
    let before = tty.len();
    tty.resize(WIDE);
    let resized = placed(&tty, before);
    assert_eq!(resized.size(), (1800, 1200));
    assert!(
        find(&resized.before, b"\x1b[2J", 0).is_some(),
        "not from a cleared screen: {}",
        shown(&resized.before)
    );
    for (keys, text) in [
        (b"-", "zoom 566%"),
        (b"+", "zoom 800%"),
        (b"+", "zoom 800%"),
    ] {
        says(&mut tty, keys, text);
    }
    quit(&mut tty);
}

#[test]
fn the_next_image_after_a_deletion_starts_at_its_fit() {
    // row Z6: show and reset_view of bin/ascii-viewer:1677 and :1691 at tag python-final (tests/test_viewer.py:1013): zoomed in and panned, a picture is deleted (`D` `y`) and the next shows at its own fit, its first step from there and about its middle; keys reach another image only at the fit (zoomed in, h and l pan) and turn only there, so a deletion is how one comes while zoomed
    let sandbox = kitty_sandbox();
    let pics = sandbox.folder("pics");
    let a = save(&coded(1800, 1200), &pics.join("a.png"));
    save(&coded(1000, 500), &pics.join("b.png"));
    let mut tty = kitty_view(&sandbox, &a, HALF);
    placed(&tty, 0);
    says(&mut tty, b"++", "zoom 100%");
    let at = tty.send(b"l");
    placed(&tty, at);
    let at = says(&mut tty, b"Dy", "deleted a.png");
    assert_eq!(placed(&tty, at).size(), (900, 450));
    assert!(!a.exists());
    let at = says(&mut tty, b"+", "zoom 127%");
    let view = placed(&tty, at);
    assert!(
        near(view.spot(450, 300), (500.0, 250.0)),
        "{:?}",
        view.spot(450, 300)
    );
    quit(&mut tty);
}

#[test]
fn the_image_goes_at_the_zooms_scale_as_far_as_the_window_holds_it() {
    // row K1: kitty_image and cut of bin/ascii-viewer:1989 and :209 at tag python-final (tests/test_viewer.py:1083 and :1092): 400 x 300 in 1800 x 1000 px is sent at 400x300, 566x424, 800x600, 1131x849, then 1600x1000 (as high as the window); enlarged by nearest neighbour (1 px stripes at 400 % as blocks of 4 px, 0 and 255 alone), reduced bicubic (flat blocks halved at the fit take values between); a side never under 1 px: 1 x 3000 in 1800 x 1200 px fits at 0.4 and goes as 1 x 1200 (cut of bin/ascii-viewer:218), one cell across at column 100
    let sandbox = kitty_sandbox();
    let window = Size {
        rows: 50,
        cols: 200,
        xpx: 1800,
        ypx: 1000,
    };
    let small = save(&gradient(400, 300), &sandbox.path("small.png"));
    let stripes = RgbImage::from_fn(40, 30, |x, y| Rgb([255 * ((x + y) % 2) as u8; 3]));
    let stripes = save(&stripes, &sandbox.path("stripes.png"));
    let blocks = RgbImage::from_fn(1800, 1200, |x, y| {
        Rgb([255 * ((x / 45 + y / 40) % 2) as u8; 3])
    });
    let blocks = save(&blocks, &sandbox.path("blocks.png"));
    let thin = save(&gradient(1, 3000), &sandbox.path("thin.png"));
    let mut ttys = [
        kitty_view(&sandbox, &small, window),
        kitty_view(&sandbox, &stripes, window),
        kitty_view(&sandbox, &blocks, HALF),
        kitty_view(&sandbox, &thin, WIDE),
    ];
    assert_eq!(placed(&ttys[0], 0).size(), (400, 300));
    for (size, text) in [
        ((566, 424), "zoom 141%"),
        ((800, 600), "zoom 200%"),
        ((1131, 849), "zoom 283%"),
        ((1600, 1000), "zoom 400%"),
    ] {
        let at = says(&mut ttys[0], b"+", text);
        assert_eq!(placed(&ttys[0], at).size(), size, "{text}");
    }
    placed(&ttys[1], 0);
    let at = says(&mut ttys[1], b"++++", "zoom 400%");
    let enlarged = placed(&ttys[1], at);
    assert_eq!(enlarged.size(), (160, 120));
    assert!(enlarged.rgb.iter().all(|&v| v == 0 || v == 255));
    let first: Vec<u8> = (0..40).map(|x| enlarged.px(x * 4, 0)[0]).collect();
    let stripe: Vec<u8> = (0..40).map(|x| 255 * (x % 2) as u8).collect();
    assert_eq!(first, stripe);
    let halved = placed(&ttys[2], 0);
    let values: BTreeSet<u8> = halved.rgb.iter().copied().collect();
    assert!(values.len() > 2, "{values:?}");
    let sliver = placed(&ttys[3], 0);
    assert_eq!(sliver.keys, "a=T,f=24,t=t,s=1,v=1200,c=1,r=60,C=1,z=-1,q=2");
    assert!(
        sliver.before.ends_with(b"\x1b[1;100H"),
        "{}",
        shown(&sliver.before)
    );
    for tty in &mut ttys {
        quit(tty);
    }
}

#[test]
fn the_placement_names_a_file_of_raw_rgb_in_the_middle_of_the_window() {
    // rows K3, K5: kitty_pixels and screen of bin/ascii-viewer:2027 and :2192 at tag python-final (tests/test_viewer_tty.py:133): 800 x 400 px in 200 x 60 cells of 9 x 20 px goes at row 21, column 56 over 89 x 20 cells, after the images were deleted and the screen cleared; its file in /dev/shm holds the 960,000 bytes of RGB; the next placement removes it first, and the exit the last
    let sandbox = kitty_sandbox();
    let picture = gradient(800, 400);
    let file = save(&picture, &sandbox.path("pic.png"));
    let mut tty = kitty_view(&sandbox, &file, WIDE);
    let first = placed(&tty, 0);
    assert!(
        first.before == [STILL_DRAW, b"\x1b[2J\x1b[21;56H"].concat(),
        "{}",
        shown(&first.before)
    );
    assert_eq!(
        first.keys,
        "a=T,f=24,t=t,s=800,v=400,c=89,r=20,C=1,z=-1,q=2"
    );
    let name = first.file.to_string_lossy().into_owned();
    assert!(
        name.starts_with("/dev/shm/tty-graphics-protocol-"),
        "{name}"
    );
    assert!(
        first.rgb == picture.as_raw()[..],
        "not the picture's pixels"
    );
    let at = tty.send(b"+");
    let next = placed(&tty, at);
    assert!(!first.file.exists() && next.file.exists());
    quit(&mut tty);
    assert!(!next.file.exists());
}

#[test]
fn a_redraw_of_the_bottom_row_leaves_the_image_where_it_is() {
    // rows K4, S11, V15, V16a: screen and status of bin/ascii-viewer:2192 and :2143 at tag python-final (tests/test_viewer.py:344): with the image on screen, a toast and its end write the bottom row alone, each in a synchronized update of its own, no screen cleared (`y` here: a copy changes nothing on screen); a still has no status row: between toasts the bottom row is blank
    let sandbox = kitty_sandbox();
    let file = save(&gradient(800, 400), &sandbox.path("pic.png"));
    let mut tty = kitty_view(&sandbox, &file, WIDE);
    let first = placed(&tty, 0);
    let at = says(&mut tty, b"y", "copying ...");
    tty.expect(at, toast("copied 800x400"));
    toast_gone(&mut tty);
    let out = tty.out();
    let after = &out[first.end..];
    assert!(
        find(after, b"a=T,", 0).is_none() && find(after, b"\x1b[2J", 0).is_none(),
        "{}",
        shown(after)
    );
    let draws = count(after, b"\x1b[?2026h");
    assert!(draws >= 2, "{}", shown(after));
    assert_eq!(
        count(after, b"\x1b[?2026h\x1b[0m\x1b[60;1H\x1b[2K"),
        draws,
        "{}",
        shown(after)
    );
    assert!(
        find(&out, b"\x1b[60;1H\x1b[0m", 0).is_none(),
        "a status row"
    );
    assert!(first.file.exists(), "the image's file went");
    quit(&mut tty);
}

#[test]
fn a_look_key_in_image_mode_only_toasts() {
    // rows K4, V16a: key and screen of bin/ascii-viewer:2284 and :2198 at tag python-final: a look key changes nothing of the image, so its draw is the bottom row with the toast, no image sent again, no cleared screen (tests/test_viewer.py:344 pins that draw); seen: `\e_Ga=d,d=A,q=2\e\\\e[2J` and the image placed again after each look key
    let sandbox = kitty_sandbox();
    let file = save(&gradient(800, 400), &sandbox.path("pic.png"));
    let mut tty = kitty_view(&sandbox, &file, WIDE);
    let first = placed(&tty, 0);
    for (key, text) in [
        ("3", DARKER),
        ("c", "mono"),
        ("b", "bold"),
        ("g", "glyphs: density"),
    ] {
        says(&mut tty, key.as_bytes(), text);
    }
    let out = tty.out();
    let after = &out[first.end..];
    assert!(
        find(after, b"a=T,", 0).is_none() && find(after, b"\x1b[2J", 0).is_none(),
        "{}",
        shown(&after[..after.len().min(300)])
    );
    quit(&mut tty);
}

#[test]
fn view_changes_draw_from_a_cleared_screen_and_the_rest_does_not() {
    // rows V16a, K5: redraw(clear=True) in zoom_by, pan_by, step, rotate and the `v` key of bin/ascii-viewer:1782, :1790, :1898, :1987 and :2260 at tag python-final: zoom, pan, a step, a turn and `v` draw from `\e[2J`; a look key, `y` and a toast's end do not; every ASCII draw of a still deletes the kitty images first (bin/ascii-viewer:2204)
    let sandbox = kitty_sandbox();
    let pics = sandbox.folder("pics");
    let a = save(&gradient(800, 400), &pics.join("a.png"));
    save(&gradient(400, 200), &pics.join("b.png"));
    let mut tty = kitty_view(&sandbox, &a, WIDE);
    placed(&tty, 0);
    let ascii = says(&mut tty, b"v", "ascii");
    // zoom 283 %, a pan, the fit, a turn and back, the next image
    for keys in [&b"+"[..], b"l", BACKSPACE, b"k", b"j", b"l"] {
        let at = tty.send(keys);
        tty.expect(at, b"\x1b[2J");
    }
    tty.expect(ascii, toast("b.png  2/2"));
    let at = says(&mut tty, b"3", DARKER);
    let copy = says(&mut tty, b"y", "copying ...");
    tty.expect(copy, toast("copied 400x200"));
    toast_gone(&mut tty);
    let out = tty.out();
    assert!(
        find(&out[at..], b"\x1b[2J", 0).is_none(),
        "{}",
        shown(&out[at..])
    );
    let (mut draws, mut from) = (0, ascii);
    while let Some(start) = find(&out, b"\x1b[?2026h", from) {
        let stop = find(&out, b"\x1b[?2026l", start).unwrap();
        assert!(
            out[start..stop].starts_with(STILL_DRAW),
            "{}",
            shown(&out[start..(start + 60).min(stop)])
        );
        (draws, from) = (draws + 1, stop);
    }
    assert!(draws >= 10, "{draws} draws");
    let at = says(&mut tty, b"v", "image");
    assert_eq!(placed(&tty, at).size(), (400, 200));
    quit(&mut tty);
}

#[test]
fn look_keys_act_on_a_still() {
    // rows S12, P17: the look keys of bin/ascii-viewer:2284 at tag python-final act on a still as on a frame: `3` draws it darker (other rows than before), `c` in one colour (bl1nk's brightest, `97`), `b` bold after each row's place, `g` steps the glyphs, `` ` `` takes level and contrast back; each a toast, none from a cleared screen
    let sandbox = Sandbox::new();
    let file = save(&gradient(800, 400), &sandbox.path("pic.png"));
    let mut tty = drawn(&sandbox, &file, SMALL);
    let before = written_rows(&tty.out());
    assert_eq!(before.len(), 18);
    let from = tty.len();
    let rows_after = |tty: &mut Tty, keys: &[u8], text: &str| {
        let at = tty.send(keys);
        let end = tty.expect(at, toast(text));
        written_rows(&update_at(tty, end))
    };
    let darker = rows_after(&mut tty, b"3", DARKER);
    assert_eq!(darker.len(), 18);
    assert!(darker != before, "the same rows");
    for (row, _, bytes) in rows_after(&mut tty, b"c", "mono") {
        assert_eq!(codes(&bytes), ["\x1b[97m"], "row {row}");
    }
    for (row, _, bytes) in rows_after(&mut tty, b"b", "bold") {
        assert!(bytes.starts_with(b"\x1b[1m\x1b[97m"), "row {row}");
    }
    says(&mut tty, b"g", "glyphs: density");
    says(&mut tty, b"`", "level 0, contrast 0");
    assert!(
        find(&tty.out()[from..], b"\x1b[2J", 0).is_none(),
        "a cleared draw"
    );
    quit(&mut tty);
}

/// A row's bytes without their SGR sequences: its glyphs.
fn glyphs(row: &[u8]) -> Vec<u8> {
    let (mut glyphs, mut at) = (Vec::new(), 0);
    while let Some(start) = find(row, b"\x1b[", at) {
        glyphs.extend(&row[at..start]);
        at = start + row[start..].iter().position(|&b| b == b'm').unwrap() + 1;
    }
    glyphs.extend(&row[at..]);
    glyphs
}

#[test]
fn bold_takes_the_ramp_of_the_bold_face() {
    // row G8: Term.render of lib/ascii_term.py:73 at tag python-final measures the density ramp in the face of font_path(bold), fc-match's answer to weight 210: in density mode every cell takes a glyph of the ramp and the slot of its own colour (lib/ascii_density.py:80), so `b` changes glyphs while each row keeps its colour codes, SGR 1 before them
    let sandbox = Sandbox::new();
    let file = save(&gradient(800, 400), &sandbox.path("pic.png"));
    let mut tty = drawn(&sandbox, &file, SMALL);
    let rows_after = |tty: &mut Tty, keys: &[u8], text: &str| {
        let at = tty.send(keys);
        let end = tty.expect(at, toast(text));
        written_rows(&update_at(tty, end))
    };
    let regular = rows_after(&mut tty, b"g", "glyphs: density");
    let bold = rows_after(&mut tty, b"b", "bold");
    assert_eq!(regular.len(), 18);
    assert_eq!(bold.len(), 18);
    let mut changed = 0;
    for ((row, _, plain), (_, _, heavy)) in regular.iter().zip(&bold) {
        let want = [vec!["\x1b[1m".to_string()], codes(plain)].concat();
        assert_eq!(codes(heavy), want, "row {row}");
        changed += (glyphs(heavy) != glyphs(plain)) as usize;
    }
    assert!(changed > 0, "the same glyphs in bold");
    quit(&mut tty);
}

#[test]
fn the_font_is_looked_up_once_a_weight() {
    // rows G10, G8: font_path of lib/ascii_term.py:21 at tag python-final asks `fc-match -f %{file}` once a weight in a process (functools.cache), JetBrainsMono Nerd Font Mono and, for bold, its weight 210: `b` three times on a still in ASCII, then the render page drawing its preview in bold and regular as Level and Bold change, ask each once
    let sandbox = kitty_sandbox();
    sandbox.program(
        "fc-match",
        "#!/bin/sh\n{ printf '%s\\n' \"$@\"; echo --; } >> \"$0.calls\"\nexec /usr/bin/fc-match \"$@\"\n",
    );
    let file = save(&gradient(800, 400), &sandbox.path("pic.png"));
    let mut tty = kitty_view(&sandbox, &file, HALF);
    placed(&tty, 0);
    for (key, text) in [
        ("v", "ascii"),
        ("b", "bold"),
        ("b", "regular"),
        ("b", "bold"),
    ] {
        says(&mut tty, key.as_bytes(), text);
    }
    // the end of the update that places the page's next preview (its format aside: N29)
    let previewed = |tty: &mut Tty, keys: &[u8]| {
        let at = tty.send(keys);
        let start = tty.expect(at, b"\x1b_Ga=T,");
        tty.expect(start, b"\x1b[?2026l");
    };
    previewed(&mut tty, b"r");
    tty.until("the page", |screen| screen.row(1).starts_with(" Render"));
    // Level up and down, then up to Bold (four rows round from Level), off and on: each a preview of its own
    for keys in [&b"l"[..], b"h", b"kkkkl", b"l"] {
        previewed(&mut tty, keys);
    }
    let at = tty.send(b"q");
    tty.expect(at, b"\x1b[2J");
    tty.until("the still", |screen| {
        !screen.row(1).starts_with(" Render") && screen.inked() > 600
    });
    quit(&mut tty);
    let noted = sandbox.noted("fc-match.calls");
    let asked: Vec<Vec<&str>> = noted
        .split_terminator("--\n")
        .map(|call| call.lines().collect())
        .collect();
    assert_eq!(
        asked,
        [
            ["-f", "%{file}", "JetBrainsMono Nerd Font Mono"],
            ["-f", "%{file}", "JetBrainsMono Nerd Font Mono:weight=210"]
        ],
        "{noted:?}"
    );
}

/// The pixels of the PNG at `file`.
fn picture_at(file: &Path) -> RgbImage {
    image::open(file)
        .unwrap_or_else(|e| panic!("{file:?}: {e}"))
        .to_rgb8()
}

#[test]
fn s_saves_the_whole_picture_as_turned_beside_the_file() {
    // row S12: screenshot of bin/ascii-viewer:1621 at tag python-final (tests/test_viewer_tty.py:101): toasts `screenshot ...` and `saved <stem>-ascii.png`, beside the file, one there before replaced; the whole picture as turned, whatever the zoom, which the old viewer handed ascii-convert as a PNG of its own (turned, or a transparent one over #808080): the file is what `asciier convert` makes of that PNG
    let sandbox = Sandbox::new();
    let marked = marked(800, 400);
    let pic = save(&marked, &sandbox.path("pic.png"));
    std::fs::write(sandbox.path("pic-ascii.png"), "not a picture").unwrap();
    let clear = RgbaImage::from_fn(160, 80, |x, _| {
        Rgba(if x < 80 {
            [255, 0, 0, 255]
        } else {
            [0, 0, 0, 0]
        })
    });
    let clear = save_rgba(&clear, &sandbox.path("clear.png"));
    let mut turned = drawn(&sandbox, &pic, SMALL);
    let mut grey = drawn(&sandbox, &clear, SMALL);
    let at = turned.send(b"k");
    turned.expect(at, b"\x1b[2J");
    says(&mut turned, b"+", "zoom 85%");
    let at = says(&mut turned, b"s", "screenshot ...");
    turned.expect(at, toast("saved pic-ascii.png"));
    says(&mut grey, b"s", "saved clear-ascii.png");
    quit(&mut turned);
    quit(&mut grey);
    // the PNGs the old viewer would have handed ascii-convert
    save(
        &image::imageops::rotate90(&marked),
        &sandbox.path("turned.png"),
    );
    let over = RgbImage::from_fn(160, 80, |x, _| {
        Rgb(if x < 80 { [255, 0, 0] } else { [128; 3] })
    });
    save(&over, &sandbox.path("over.png"));
    for (shot, source) in [
        ("pic-ascii.png", "turned.png"),
        ("clear-ascii.png", "over.png"),
    ] {
        let out = sandbox.path(format!("want-{source}"));
        let ran = sandbox.run(&[
            "convert".as_ref(),
            sandbox.path(source).as_os_str(),
            "-o".as_ref(),
            out.as_os_str(),
        ]);
        assert_eq!(ran.code, Some(0), "{ran:?}");
        let (got, want) = (picture_at(&sandbox.path(shot)), picture_at(&out));
        assert_eq!(got.dimensions(), want.dimensions(), "{shot}");
        assert!(
            got == want,
            "{shot}: not the converter's picture of {source}"
        );
    }
}

#[test]
fn s_draws_in_the_players_look_or_says_it_failed() {
    // row S12: screenshot of bin/ascii-viewer:1621 at tag python-final hands ascii-convert the player's look (`--level 95` here) and toasts `screenshot failed` when it fails: a ramp up to 200 at 95 % darker, where every value under 242 goes to 0 (tone_table of lib/ascii_theme.py:135), is bl1nk's background (#111111) alone, at level 0 it is not; a folder of the file's name is a file that cannot be written
    let sandbox = Sandbox::new();
    let ramp = RgbImage::from_fn(800, 400, |x, _| Rgb([(x * 200 / 799) as u8; 3]));
    let pic = save(&ramp, &sandbox.path("pic.png"));
    let shot = sandbox.path("pic-ascii.png");
    let mut tty = drawn(&sandbox, &pic, SMALL);
    let at = says(&mut tty, b"s", "screenshot ...");
    tty.expect(at, toast("saved pic-ascii.png"));
    let plain = picture_at(&shot);
    assert!(plain.pixels().any(|px| px.0 != [0x11; 3]), "no picture");
    says(&mut tty, &[b'3'; 19], "level 95% darker");
    let at = says(&mut tty, b"s", "screenshot ...");
    tty.expect(at, toast("saved pic-ascii.png"));
    let dark = picture_at(&shot);
    assert_eq!(dark.dimensions(), plain.dimensions());
    assert!(
        dark.pixels().all(|px| px.0 == [0x11; 3]),
        "not drawn at 95 % darker"
    );
    std::fs::remove_file(&shot).unwrap();
    std::fs::create_dir(&shot).unwrap();
    let at = says(&mut tty, b"s", "screenshot ...");
    tty.expect(at, toast("screenshot failed"));
    quit(&mut tty);
}

#[test]
fn r_opens_the_render_page_on_the_picture_as_turned() {
    // row S12: open_page of bin/ascii-viewer:2048 at tag python-final hands the page the picture in memory as turned: after `k` the crop editor shows 400x800 of an 800 x 400 still; `q` there and `q` on the menu come back to the still, turned as it was
    let sandbox = Sandbox::new();
    let file = save(&gradient(800, 400), &sandbox.path("pic.png"));
    let mut tty = drawn(&sandbox, &file, WIDE);
    let at = tty.send(b"k");
    tty.expect(at, b"\x1b[2J");
    tty.send(b"r");
    tty.until("the page", |screen| screen.row(1).starts_with(" Render"));
    tty.send(&b"j".repeat(8));
    tty.send(ENTER);
    tty.until("the crop editor on the turned picture", |screen| {
        screen.row(1).starts_with(" Crop") && screen.row(1).contains(" source · crop 400x800 ")
    });
    tty.send(b"q");
    tty.until("the menu", |screen| screen.row(1).starts_with(" Render"));
    tty.send(b"q");
    // 400 x 800 in 200 x 60 cells: 67 columns of 200
    tty.until("the still, turned", |screen| {
        !screen.row(1).starts_with(" Render")
            && screen.inked() > 1000
            && screen
                .picture_cols()
                .is_some_and(|(left, right)| right - left < 100)
    });
    quit(&mut tty);
}

#[test]
fn keys_without_a_use_on_a_still_do_nothing() {
    // row S13: key of bin/ascii-viewer:2264 at tag python-final (tests/test_viewer.py:969): `space p m [ ] { } \ 0 9 * / . ,`, keys it does not know, and at the fit `H J K L` and Shift+arrows: no draw, no toast, no file, no zoom; `3` after them shows when they are done
    let sandbox = Sandbox::new();
    let file = save(&gradient(800, 400), &sandbox.path("pic.png"));
    let mut tty = drawn(&sandbox, &file, SMALL);
    let keys = [
        &b" pm[]{}\\09*/.,xZn\t\x1bOQ\x1b[H"[..],
        b"HJKL",
        SHIFT_LEFT,
        SHIFT_RIGHT,
        SHIFT_UP,
        SHIFT_DOWN,
        b"\x1b\r3",
    ]
    .concat();
    let at = tty.send(&keys);
    let sentinel = toast(DARKER);
    let end = tty.expect(at, &sentinel);
    // the sentinel's draw ends right after its toast
    let stop = tty.expect(end, b"\x1b[?2026l") - 8;
    let out = tty.out();
    let draw = find(&out, b"\x1b[?2026h", at).unwrap();
    assert!(
        draw < end && find(&out, b"\x1b[?2026l", draw) == Some(stop),
        "a draw of their own: {}",
        shown(&out[at..end])
    );
    assert_eq!(find(&out, b"\x1b[7m", at), Some(end - sentinel.len()));
    let mut names: Vec<String> = std::fs::read_dir(&sandbox.dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .filter(|name| name.starts_with("pic"))
        .collect();
    names.sort();
    assert_eq!(names, ["pic.png"]);
    // 800 x 400 in 720 x 480 px fits at 90 % and its first step crops already: nothing above moved the zoom
    says(&mut tty, b"+", "zoom 127%");
    quit(&mut tty);
}

/// The PNG the wl-copy stub of `sandbox` took: its bit depth and colour type, by its header, and its picture.
fn copied(sandbox: &Sandbox) -> ((u8, u8), DynamicImage) {
    let bytes = std::fs::read(sandbox.path("bin/wl-copy.stdin")).unwrap();
    assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n");
    (
        (bytes[24], bytes[25]),
        image::load_from_memory(&bytes).unwrap(),
    )
}

#[test]
fn y_copies_the_file_itself_as_png() {
    // rows S14, N20: copy_image and original_png of bin/ascii-viewer:1657 and :258 at tag python-final (tests/test_viewer_tty.py:190, tests/test_viewer.py:766-791): `copying ...`, then `copied WxH`; `wl-copy --type image/png` takes the PNG on stdin: the file's own pixels at its size, turned as shown, upright by EXIF, an SVG at the size it declares, alpha kept; `copy failed` when wl-copy fails or is missing, `too large to copy` for an SVG that declares over 179 megapixels; N20: a palette picture comes as 8-bit RGBA (the old copy kept the palette)
    let picture = gradient(800, 400);
    let names = [
        "pic.png",
        "fails.png",
        "missing.png",
        "icon.svg",
        "vast.svg",
        "cam.jpg",
        "veiled.png",
        "palette.png",
    ];
    let mut sandboxes: Vec<Sandbox> = names.iter().map(|_| Sandbox::new()).collect();
    for (sandbox, name) in sandboxes.iter().zip(&names[..3]) {
        save(&picture, &sandbox.path(name));
    }
    sandboxes[1].stub_data("wl-copy.exit", "1");
    // a PATH of fc-match alone: no wl-copy, and nothing else of the system's
    let path = sandboxes[2].folder("path");
    std::os::unix::fs::symlink("/usr/bin/fc-match", path.join("fc-match")).unwrap();
    sandboxes[2].set("PATH", &path);
    std::fs::write(
        sandboxes[3].path("icon.svg"),
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="64" height="32"><rect width="64" height="32"/></svg>"#,
    )
    .unwrap();
    std::fs::write(
        sandboxes[4].path("vast.svg"),
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="20000" height="10000"><rect width="20000" height="10000" fill="#888"/></svg>"##,
    )
    .unwrap();
    std::fs::write(
        sandboxes[5].path("cam.jpg"),
        jpeg(&RgbImage::new(400, 200), Some(6)),
    )
    .unwrap();
    let mut veiled = RgbaImage::from_pixel(300, 100, Rgba([255, 0, 0, 128]));
    veiled.put_pixel(0, 0, Rgba([0, 0, 255, 255]));
    save_rgba(&veiled, &sandboxes[6].path("veiled.png"));
    std::fs::write(sandboxes[7].path("palette.png"), palette_png(10, 10)).unwrap();
    let mut ttys: Vec<Tty> = sandboxes
        .iter()
        .zip(names)
        .map(|(sandbox, name)| drawn(sandbox, &sandbox.path(name), SMALL))
        .collect();
    let said = [
        "copied 800x400",
        "copy failed",
        "copy failed",
        "copied 64x32",
        "too large to copy",
        "copied 200x400",
        "copied 300x100",
        "copied 10x10",
    ];
    for (tty, text) in ttys.iter_mut().zip(said) {
        let at = says(tty, b"y", "copying ...");
        tty.expect(at, toast(text));
    }
    assert_eq!(
        sandboxes[0].noted("wl-copy.calls"),
        "--type\nimage/png\n--\n"
    );
    assert!(
        copied(&sandboxes[0]).1.to_rgb8() == picture,
        "not the file's pixels"
    );
    assert_eq!(sandboxes[2].noted("wl-copy.calls"), "", "a wl-copy ran");
    let icon = copied(&sandboxes[3]).1;
    assert_eq!((icon.width(), icon.height()), (64, 32));
    assert!(
        copied(&sandboxes[6]).1 == DynamicImage::ImageRgba8(veiled),
        "the alpha went"
    );
    let ((depth, colour), palette) = copied(&sandboxes[7]);
    assert_eq!((depth, colour), (8, 6), "not 8-bit RGBA");
    let palette = palette.to_rgba8();
    assert_eq!(
        (palette.get_pixel(0, 0).0, palette.get_pixel(9, 9).0),
        ([0; 4], [255; 4])
    );
    // turned as shown
    let tty = &mut ttys[0];
    let at = tty.send(b"k");
    tty.expect(at, b"\x1b[2J");
    let at = says(tty, b"y", "copying ...");
    tty.expect(at, toast("copied 400x800"));
    assert!(
        copied(&sandboxes[0]).1.to_rgb8() == image::imageops::rotate90(&picture),
        "not turned as shown"
    );
    for tty in &mut ttys {
        quit(tty);
    }
}

#[test]
fn y_runs_wl_copy_in_a_session_of_its_own() {
    // row S14: copy_image of bin/ascii-viewer:1666 at tag python-final starts wl-copy with start_new_session: it serves the clipboard after the viewer is gone, and the terminal's hang-up does not reach it
    let sandbox = Sandbox::new();
    sandbox.program(
        "wl-copy",
        "#!/bin/sh\n{ cut -d' ' -f6 /proc/$$/stat; echo $$; } > \"$0.session\"\ncat > /dev/null\n",
    );
    let file = save(&gradient(80, 40), &sandbox.path("pic.png"));
    let mut tty = drawn(&sandbox, &file, SMALL);
    let at = says(&mut tty, b"y", "copying ...");
    tty.expect(at, toast("copied 80x40"));
    let noted = sandbox.noted("wl-copy.session");
    let lines: Vec<&str> = noted.lines().collect();
    assert!(
        lines.len() == 2 && lines[0] == lines[1],
        "session, pid: {noted:?}"
    );
    assert_ne!(lines[0], tty.pid.to_string());
    quit(&mut tty);
}

#[test]
fn the_image_on_screen_is_its_folder_and_its_own_name() {
    // row S15: entry and sibling_images of bin/ascii-viewer:1871 and :271 at tag python-final (tests/test_viewer.py:1339): a symlink into another folder stays itself: the steps count in its own folder and `s` names its file after it, beside it; a path through a linked folder counts in the folder it resolves to
    let sandbox = Sandbox::new();
    let (pics, other) = (sandbox.folder("pics"), sandbox.folder("other"));
    for (folder, name) in [
        (&pics, "a.png"),
        (&pics, "b.png"),
        (&other, "z.png"),
        (&other, "zz.png"),
    ] {
        save(&gradient(80, 40), &folder.join(name));
    }
    std::os::unix::fs::symlink("../other/z.png", pics.join("link.png")).unwrap();
    std::os::unix::fs::symlink("pics", sandbox.path("alias")).unwrap();
    let mut link = drawn(&sandbox, &pics.join("link.png"), SMALL);
    let mut alias = drawn(&sandbox, &sandbox.path("alias/b.png"), SMALL);
    says(&mut link, b"l", "a.png  1/3");
    says(&mut link, b"h", "link.png  3/3");
    says(&mut alias, b"l", "link.png  3/3");
    // the screenshot is a fourth entry of the folder: after the steps
    says(&mut link, b"s", "saved link-ascii.png");
    assert!(pics.join("link-ascii.png").is_file() && !other.join("z-ascii.png").exists());
    quit(&mut link);
    quit(&mut alias);
}

#[test]
fn ascii_shows_the_part_in_view_with_transparency_over_grey() {
    // rows S16, S3: thumbnail of bin/ascii-viewer:1836 at tag python-final (tests/test_viewer.py:878 and :1031): in ASCII transparency is drawn over #808080, so a picture clear on its right half is written as that picture over grey; zoomed in, the rows show the part in view: in the top left corner the red quarter's colour alone, in the bottom right the blue's
    let sandbox = Sandbox::new();
    let clear = RgbaImage::from_fn(160, 80, |x, _| Rgba(if x < 80 { [255; 4] } else { [0; 4] }));
    let clear = save_rgba(&clear, &sandbox.path("clear.png"));
    let over = RgbImage::from_fn(160, 80, |x, _| Rgb([if x < 80 { 255 } else { 128 }; 3]));
    let over = save(&over, &sandbox.path("over.png"));
    let quarters = RgbImage::from_fn(1800, 1200, |x, y| {
        Rgb(if x < 900 && y < 600 {
            [255, 0, 0]
        } else {
            [0, 0, 255]
        })
    });
    let quarters = save(&quarters, &sandbox.path("quarters.png"));
    let mut ttys = [
        drawn(&sandbox, &clear, SMALL),
        drawn(&sandbox, &over, SMALL),
        drawn(&sandbox, &quarters, WIDE),
    ];
    let rows: Vec<_> = ttys[..2]
        .iter()
        .map(|tty| written_rows(&tty.out()))
        .collect();
    assert_eq!(rows[0].len(), 18);
    assert!(rows[0] == rows[1], "not the picture over grey");
    // at 400 % the view is 450 x 300 px of the picture: a corner of it lies inside a quarter
    let tty = &mut ttys[2];
    says(tty, b"++++", "zoom 400%");
    let mut inks = Vec::new();
    for keys in [&b"hhhhhhhhhkkkkkkkkk"[..], b"llllllllljjjjjjjjj"] {
        // the end of the pan's draw, which clears: the zoom toast's end may draw before it
        let at = tty.send(keys);
        let cleared = tty.expect(at, b"\x1b[2J");
        tty.expect(cleared, b"\x1b[?2026l");
        let screen = tty.screen();
        let seen: BTreeSet<Option<u32>> = (1..60)
            .flat_map(|row| (1..=200).map(move |col| (row, col)))
            .map(|(row, col)| screen.cell(row, col))
            .filter(|cell| cell.ch != ' ')
            .map(|cell| cell.pen.fg)
            .collect();
        assert_eq!(seen.len(), 1, "after {}: {seen:?}", shown(keys));
        inks.extend(seen);
    }
    assert_ne!(inks[0], inks[1]);
    for tty in &mut ttys {
        quit(tty);
    }
}

/// The share of the columns of a placed image that are the column on their left over again.
fn repeated_columns(sent: &Placed) -> f64 {
    let (w, h) = sent.size();
    let column = |x: u32| (0..h).map(move |y| sent.px(x, y));
    let same = (1..w).filter(|&x| column(x).eq(column(x - 1))).count();
    same as f64 / (w - 1) as f64
}

/// The columns of a placed image that are #00ff00 from top to bottom.
fn green_columns(sent: &Placed) -> usize {
    let (w, h) = sent.size();
    (0..w)
        .filter(|&x| (0..h).all(|y| sent.px(x, y) == [0, 255, 0]))
        .count()
}

/// Wait until `test` holds for the number of what draws SVG parts for the viewer in the background: its threads named `svg part`, and its child processes (the Python tools ran resvg for a part); fails the test after `PATIENCE`. None left: each part asked for has been handed over and the loop woken, so the loop takes it before a key sent after.
fn await_parts(tty: &Tty, what: &str, test: impl Fn(usize) -> bool) {
    let read = |path: PathBuf| std::fs::read_to_string(path).unwrap_or_default();
    let listed = |dir: String| {
        let entries = std::fs::read_dir(dir).into_iter().flatten().flatten();
        entries.map(|entry| entry.path())
    };
    let parent = |stat: String| -> Option<u32> {
        let (_, rest) = stat.rsplit_once(')')?;
        rest.split_whitespace().nth(1)?.parse().ok()
    };
    let drawing = || {
        let threads = listed(format!("/proc/{}/task", tty.pid))
            .filter(|task| read(task.join("comm")) == "svg part\n")
            .count();
        let children = listed("/proc".into())
            .filter(|process| {
                let name = process.file_name().and_then(|name| name.to_str());
                name.is_some_and(|name| name.bytes().all(|b| b.is_ascii_digit()))
            })
            .filter(|process| parent(read(process.join("stat"))) == Some(tty.pid))
            .count();
        threads + children
    };
    let end = std::time::Instant::now() + PATIENCE;
    while !test(drawing()) {
        if std::time::Instant::now() > end {
            tty.fail(&format!("never {what}"));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn a_zoomed_svg_is_drawn_again_from_the_svg_at_the_size_shown() {
    // rows Z8, Z9: part, render_parts and zoom_by of bin/ascii-viewer:1799, :1817 and :1769 at tag python-final (tests/test_viewer.py:1220 and :1267): an SVG's zoom counts px of its 1600 px raster (100 x 50 units in 900 x 600 px: fit 56 %, the first step `zoom 80%`, not the 141 % of its own units), and the fit stays when finer parts were drawn; past 1 screen px a raster px the raster shows enlarged by nearest neighbour first (runs of equal columns at 636 %), then the part drawn alone at the size shown takes its place, the diagonal edge in every column
    let sandbox = kitty_sandbox();
    let svg = sandbox.path("edge.svg");
    // the noise in the middle costs the first raster little and the zoomed part much: drawing that outlasts the read of the raster's placement (0.7 to 1.6 s against 0.2 to 0.4 s without)
    std::fs::write(
        &svg,
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="50"><filter id="n" x="0" y="0" width="1" height="1"><feTurbulence baseFrequency="0.5" numOctaves="2"/></filter><rect width="100" height="50" fill="#fff"/><path d="M0 50 L100 0 L100 50 Z"/><rect x="46" y="23" width="8" height="4" filter="url(#n)"/></svg>"##,
    )
    .unwrap();
    let mut tty = kitty_view(&sandbox, &svg, HALF);
    assert_eq!(placed(&tty, 0).size(), (900, 450));
    says(&mut tty, b"+", "zoom 80%");
    let at = tty.send(&[b'+'; 6]);
    let toasted = tty.expect(at, toast("zoom 636%"));
    let out = tty.out();
    let update = out[..toasted]
        .windows(8)
        .rposition(|w| w == b"\x1b[?2026h")
        .unwrap();
    let raster = placed(&tty, update);
    assert_eq!(raster.size(), (900, 600));
    let part = placed(&tty, raster.end);
    assert_eq!(part.size(), (900, 600));
    let (blocky, fine) = (repeated_columns(&raster), repeated_columns(&part));
    assert!(
        blocky > 0.7 && fine < 0.2,
        "repeated columns: the raster's {blocky}, the part's {fine}"
    );
    says(&mut tty, BACKSPACE, "fit");
    says(&mut tty, b"+", "zoom 80%");
    quit(&mut tty);
}

#[test]
fn a_zoomed_svg_shows_the_part_of_the_view_on_screen_alone() {
    // row Z8: part, render_parts and thumbnail of bin/ascii-viewer:1799, :1817 and :1836 at tag python-final: in ASCII a zoomed SVG is drawn from a part too: 200 x 60 cells at 800 %, the rows of the part differ from those of the raster, drawn with the toast, along the diagonal edge alone; in image mode a part for a view that is gone when it lands never shows: `+` six times (450 % in 900 x 600 px), and once more (636 %) before that part lands; a green bar at x 44 to 45 of the 100 units lies in the 450 % view (x 43.75 to 56.25) and not in the 636 % one (x 45.58 to 54.42): placed then, the 636 % raster enlarged and the 636 % part alone, no bar in either, nothing more by the toast of a look key sent once neither part is drawing
    let plain = Sandbox::new();
    let edge = plain.path("edge.svg");
    std::fs::write(
        &edge,
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="50"><rect width="100" height="50" fill="#fff"/><path d="M0 50 L100 0 L100 50 Z"/></svg>"##,
    )
    .unwrap();
    let sandbox = kitty_sandbox();
    let noisy = sandbox.path("noisy.svg");
    // noise over every view from 450 % on: a part costs about 2 s, the next key comes in milliseconds; the bar over it, the views' heights through
    std::fs::write(
        &noisy,
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="50"><filter id="n" x="0" y="0" width="1" height="1"><feTurbulence baseFrequency="0.5" numOctaves="2"/></filter><rect width="100" height="50" fill="#fff"/><path d="M0 50 L100 0 L100 50 Z"/><rect x="40" y="18" width="20" height="14" filter="url(#n)"/><rect x="44" y="18" width="1" height="14" fill="#0f0"/></svg>"##,
    )
    .unwrap();
    let mut ascii = drawn(&plain, &edge, WIDE);
    let mut image = kitty_view(&sandbox, &noisy, HALF);
    // 225 x 150 px of the 1600 x 800 raster about its middle over 200 x 60 cells: the edge from (0, 800) to (1600, 0) passes column c at row 53.1 - 0.225 c
    let cells = |screen: &common::screen::Screen| -> Vec<(char, Option<u32>)> {
        (1..60)
            .flat_map(|row| (1..=200).map(move |col| (row, col)))
            .map(|(row, col)| (screen.cell(row, col).ch, screen.cell(row, col).pen.fg))
            .collect()
    };
    let at = ascii.send(&[b'+'; 8]);
    let toasted = ascii.expect(at, toast("zoom 800%"));
    let end = ascii.expect(toasted, b"\x1b[?2026l");
    let shown = replay(&ascii.out()[..end], WIDE, cells);
    let enlarged = shown.last().unwrap().clone();
    let drawn = shown.len();
    if !ascii.wait(0, PATIENCE, |out| {
        replay(out, WIDE, cells)[drawn..]
            .iter()
            .any(|s| *s != enlarged)
    }) {
        ascii.fail("no draw from the part");
    }
    let finer = replay(&ascii.out(), WIDE, cells)
        .into_iter()
        .skip(drawn)
        .find(|s| *s != enlarged)
        .unwrap();
    let off: Vec<(usize, usize)> = (0..enlarged.len())
        .filter(|&at| enlarged[at] != finer[at])
        .map(|at| (at / 200 + 1, at % 200 + 1))
        .collect();
    assert!(off.len() > 20, "{off:?}");
    for (row, col) in &off {
        let edge = 53.1 - 0.225 * *col as f64;
        assert!(
            (*row as f64 - edge).abs() <= 2.0,
            "row {row}, column {col}: not at the edge"
        );
    }
    quit(&mut ascii);
    placed(&image, 0);
    let at = image.send(&[b'+'; 6]);
    let toasted = image.expect(at, toast("zoom 450%"));
    let first = placed(&image, at);
    // the next key at once, the 450 % part still drawing: the scans below take seconds in a debug build under load
    let at = image.send(b"+");
    let next = image.expect(at, toast("zoom 636%"));
    let raster = placed(&image, at);
    assert_eq!(
        count(&image.out()[first.end..raster.end], b"\x1b_Ga=T,"),
        1,
        "the 450 % part landed before the next key"
    );
    // the parts draw on threads named `svg part` (the Python tools' in resvg children) for the waits here: without the name they would wait for nothing
    await_parts(&image, "a part drawing", |drawing| drawing > 0);
    let bars = green_columns(&first);
    assert!(
        first.end > toasted && repeated_columns(&first) > 0.7 && bars > 50,
        "the 450 % raster: {bars} green columns"
    );
    assert!(raster.end > next && repeated_columns(&raster) > 0.7);
    assert_eq!(green_columns(&raster), 0, "the bar in the 636 % view");
    let part = placed(&image, raster.end);
    assert!(repeated_columns(&part) < 0.2, "the raster again");
    assert_eq!(green_columns(&part), 0, "the 450 % part shown");
    // both parts handed over before a look key, which draws its toast alone: a part drawn at all is drawn by its end
    await_parts(&image, "the parts ended", |drawing| drawing == 0);
    let at = image.send(b"3");
    let toasted = image.expect(at, toast(DARKER));
    let end = image.expect(toasted, b"\x1b[?2026l");
    let late = count(&image.out()[part.end..end], b"\x1b_Ga=T,");
    assert_eq!(late, 0, "placed after the part of the view shown");
    quit(&mut image);
}
