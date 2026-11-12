//! The font: file lookup, cell metrics, glyph masks and the density ramp, by FreeType as Pillow drives it.

use crate::Result;
use freetype::face::LoadFlag;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

pub const FAMILY: &str = "JetBrainsMono Nerd Font Mono";

/// A cell at a font size: advance of `M` (26.6 precision), ascent + descent in whole px, and the ascent.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Cell {
    pub w: f64,
    pub h: i32,
    pub ascent: i32,
}

/// The printable ASCII glyphs sorted by ink coverage, coverage scaled to 0 (space) .. 1 (densest).
pub struct Ramp {
    pub glyphs: [u8; 95],
    pub cover: [f64; 95],
}

pub struct Font {
    face: freetype::Face,
}

/// The font file (`fc-match -f %{file}`), the heaviest face at or under weight 210 for bold; once a weight per process.
pub fn path(bold: bool) -> Result<&'static Path> {
    static FILES: [OnceLock<Option<PathBuf>>; 2] = [OnceLock::new(), OnceLock::new()];
    let found = FILES[bold as usize].get_or_init(|| {
        let pattern = format!("{FAMILY}{}", if bold { ":weight=210" } else { "" });
        let out = std::process::Command::new("fc-match")
            .args(["-f", "%{file}", &pattern])
            .output()
            .ok()?;
        let file = String::from_utf8(out.stdout).ok()?;
        (!file.is_empty()).then(|| PathBuf::from(file))
    });
    match found {
        Some(file) => Ok(file),
        None => crate::err!("no font file from fc-match for {FAMILY}"),
    }
}

/// The ramp of the regular or bold face, measured once.
pub fn ramp(bold: bool) -> Result<&'static Ramp> {
    static RAMPS: [OnceLock<Ramp>; 2] = [OnceLock::new(), OnceLock::new()];
    if let Some(ramp) = RAMPS[bold as usize].get() {
        return Ok(ramp);
    }
    let ramp = Font::open(path(bold)?)?.ramp();
    Ok(RAMPS[bold as usize].get_or_init(|| ramp))
}

impl Font {
    pub fn open(file: &Path) -> Result<Font> {
        let library = freetype::Library::init()?;
        match library.new_face(file, 0) {
            Ok(face) => Ok(Font { face }),
            Err(e) => crate::err!("cannot open font {}: {e}", file.display()),
        }
    }

    fn size(&self, size: f64) {
        // Pillow hands FreeType the size truncated to 26.6
        self.face
            .set_char_size(0, (size * 64.0) as isize, 0, 0)
            .expect("scalable font");
    }

    pub fn cell(&self, size: f64) -> Cell {
        self.size(size);
        let metrics = self.face.size_metrics().expect("sized font");
        self.face
            .load_char('M' as usize, LoadFlag::DEFAULT | LoadFlag::NO_HINTING)
            .expect("glyph M");
        let (ascent, descent) = ((metrics.ascender + 32) >> 6, (-metrics.descender + 32) >> 6);
        Cell {
            w: self.face.glyph().advance().x as f64 / 64.0,
            h: (ascent + descent) as i32,
            ascent: ascent as i32,
        }
    }

    /// Coverage (0..255) of `glyph` drawn at the top left of a `w` x `h` cell at `size`, as Pillow's `text((0, 0), ...)` puts it.
    pub fn mask(&self, glyph: u8, size: f64, w: usize, h: usize) -> Vec<u8> {
        let ascent = self.cell(size).ascent as i64;
        let mut cell = vec![0u8; w * h];
        if self
            .face
            .load_char(glyph as usize, LoadFlag::RENDER)
            .is_err()
        {
            return cell;
        }
        let slot = self.face.glyph();
        let bitmap = slot.bitmap();
        let (left, top) = (slot.bitmap_left() as i64, ascent - slot.bitmap_top() as i64);
        for y in 0..bitmap.rows() as i64 {
            for x in 0..bitmap.width() as i64 {
                let (cx, cy) = (x + left, y + top);
                if cx >= 0 && cy >= 0 && (cx as usize) < w && (cy as usize) < h {
                    cell[cy as usize * w + cx as usize] =
                        bitmap.buffer()[(y * bitmap.pitch() as i64 + x) as usize];
                }
            }
        }
        cell
    }

    pub fn ramp(&self) -> Ramp {
        let cell = self.cell(64.0);
        let (w, h) = (cell.w.round_ties_even() as usize, cell.h as usize);
        let mut cover: Vec<(u8, f64)> = (32u8..127)
            .map(|glyph| {
                (
                    glyph,
                    self.mask(glyph, 64.0, w, h)
                        .iter()
                        .map(|&v| v as f64)
                        .sum::<f64>()
                        / (w * h) as f64
                        / 255.0,
                )
            })
            .collect();
        cover.sort_by(|a, b| a.1.total_cmp(&b.1)); // stable, as numpy's argsort(kind="stable")
        let top = cover[94].1;
        Ramp {
            glyphs: std::array::from_fn(|i| cover[i].0),
            cover: std::array::from_fn(|i| cover[i].1 / top),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fnv(bytes: impl IntoIterator<Item = u8>) -> u64 {
        bytes.into_iter().fold(0xcbf2_9ce4_8422_2325, |h, b| {
            (h ^ b as u64).wrapping_mul(0x0100_0000_01b3)
        })
    }

    #[test]
    fn cell_is_pillows() {
        let font = Font::open(path(false).unwrap()).unwrap();
        assert_eq!(
            font.cell(20.0),
            Cell {
                w: 12.0,
                h: 27,
                ascent: 21
            }
        );
        assert_eq!(
            font.cell(23.53),
            Cell {
                w: 14.109375,
                h: 32,
                ascent: 24
            }
        );
        assert_eq!(
            font.cell(32.0),
            Cell {
                w: 19.203125,
                h: 43,
                ascent: 33
            }
        );
    }

    #[test]
    fn the_faces_are_fc_matchs_answers() {
        // rows G8, G10: the files lib/ascii_term.py:21-26 font_path() takes from fc-match, for the family and for the family at weight 210: the regular face, and the heaviest at or under 210, ExtraBold here
        for (bold, weight, style) in [(false, "", "Regular"), (true, ":weight=210", "ExtraBold")] {
            let query = format!("JetBrainsMono Nerd Font Mono{weight}");
            let asked = std::process::Command::new("fc-match")
                .args(["-f", "%{file}", &query])
                .output()
                .unwrap();
            let file = String::from_utf8(asked.stdout).unwrap();
            assert_eq!(path(bold).unwrap(), Path::new(&file), "{query}");
            let font = Font::open(path(bold).unwrap()).unwrap();
            assert_eq!(font.face.style_name().as_deref(), Some(style), "{file}");
        }
    }

    #[test]
    fn ramps_are_the_python_tools_ramps() {
        // rows G6, G8: lib/ascii_density.py:31-46 ramp() of the two faces lib/ascii_term.py:21-26 names, run 2026-11-12: the glyph order (the regular one is row G6's, after the space) and FNV-1a 64 of the coverages as little-endian f64 bytes; the bold face's ramp is its own
        let regular = b" `.-',:_;~\"^!=+><r|?/\\LJlv*T()cxYzuit[]yf{7n}41IjsoFCZVehk23X6a9P5SwEUmAqpbd#KOHGgD0%R8&QBMN$W@";
        let extra_bold = b" `.'-,:_;~\"!^<>+=*r?|\\/LJlv()TcxzYiuftyeno4s7][1I}{jCaZFVw#63hk29%XPm5ASEqpdbUGOKgH&08DRNM$QWB@";
        for (bold, glyphs, covers) in [
            (false, regular, 0xfe95_a742_87cf_2e0b),
            (true, extra_bold, 0xcedd_bda7_369e_87f5),
        ] {
            let ramp = ramp(bold).unwrap();
            assert_eq!(&ramp.glyphs, glyphs, "bold {bold}");
            let bytes = ramp.cover.iter().flat_map(|cover| cover.to_le_bytes());
            assert_eq!(fnv(bytes), covers, "bold {bold}");
        }
        assert_ne!(ramp(true).unwrap().glyphs, ramp(false).unwrap().glyphs);
    }

    #[test]
    fn ramp_runs_from_space_to_the_densest() {
        let ramp = ramp(false).unwrap();
        assert_eq!(
            (ramp.glyphs[0], ramp.cover[0], ramp.cover[94]),
            (b' ', 0.0, 1.0)
        );
        assert!(ramp.cover.windows(2).all(|w| w[0] <= w[1]));
        let mut sorted = ramp.glyphs;
        sorted.sort();
        assert!(sorted.iter().copied().eq(32u8..127));
    }
}
