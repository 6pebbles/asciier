//! Theme colours: kitty theme text, the slot each colour maps to, mono's colour, kitty's theme collection.
//!
//! libchafa's own 16-colour mode picks against xterm's stock palette, and the terminal then shows that slot in the theme's colour: a green it calls yellow comes out as the theme's orange.
//! So libchafa is asked for truecolour and each cell's colour goes to one of the theme's own 16 slots here.
//! Hue first, because the glyph already carries the brightness: plain nearest-colour would send a dull green field to the theme's slate grey.

use crate::look::Skin;
use crate::{Error, Result};
use std::cmp::Ordering;
use std::f64::consts::PI;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

pub type Rgb = [u8; 3];

/// Oklab chroma under which a colour counts as grey (cells and theme slots alike), at saturation 100.
const CHROMA: f64 = 0.035;
/// Weight of a lightness gap against a hue gap (radians), between coloured slots.
const LIGHTNESS: f64 = 1.5;
/// Skin's hue band in degrees: there a colour takes a hue only from `WARM_CHROMA` up, else skin would come out as the theme's orange or red.
const WARM: (f64, f64) = (20.0, 75.0);
const WARM_CHROMA: f64 = 0.10;
/// Oklab lightness under the lightest colour that mono still takes, for a less colourful one.
const MONO_SPAN: f64 = 0.05;

/// A kitty theme's colours: foreground, background, color0-15; `extra` holds color16 and up (they only join mono's pool).
#[derive(Clone, PartialEq, Debug)]
pub struct Theme {
    pub foreground: Rgb,
    pub background: Rgb,
    pub colors: [Rgb; 16],
    pub extra: Vec<Rgb>,
}

/// The colour lines of a kitty theme text, in order: a key at column 0 (`foreground`, `background`, `color<N>`), whitespace, `#` and six hex digits.
/// One entry a key, at its first line's place with its last line's colour.
/// A line ends at `\n` or `\r`, as Python reads the theme file.
pub fn parse(text: &str) -> Vec<(String, Rgb)> {
    let mut found: Vec<(String, Rgb)> = Vec::new();
    let mut line = Some(text);
    while let Some(rest) = line {
        if let Some((key, colour)) = colour_at(rest) {
            match found.iter_mut().find(|(k, _)| k == key) {
                Some(entry) => entry.1 = colour,
                None => found.push((key.to_string(), colour)),
            }
        }
        line = rest.find(['\n', '\r']).map(|end| &rest[end + 1..]);
    }
    found
}

/// The key and colour `text` starts with: Python's `(foreground|background|color\d+)\s+(#[0-9A-Fa-f]{6})`, whose whitespace may run over line ends.
fn colour_at(text: &str) -> Option<(&str, Rgb)> {
    let digits = |d: &str| d.len() - d.trim_start_matches(|c: char| c.is_ascii_digit()).len();
    let key = match text.strip_prefix("color").map(digits) {
        Some(digits) if digits > 0 => 5 + digits,
        _ => ["foreground", "background"]
            .into_iter()
            .find(|key| text.starts_with(key))?
            .len(),
    };
    // Python's \s also takes the four separators below the space
    let space = |c: char| c.is_whitespace() || ('\x1c'..='\x1f').contains(&c);
    let value = text[key..].trim_start_matches(space);
    let spaced = value.len() < text.len() - key;
    let hex = value.strip_prefix('#').filter(|_| spaced)?.get(..6)?;
    let mut colour = [0; 3];
    for (channel, pair) in colour.iter_mut().zip(hex.as_bytes().chunks(2)) {
        let digit = |byte: u8| (byte as char).to_digit(16);
        *channel = (digit(pair[0])? * 16 + digit(pair[1])?) as u8;
    }
    Some((&text[..key], colour))
}

fn home() -> Result<PathBuf> {
    match std::env::var_os("HOME") {
        Some(home) => Ok(PathBuf::from(home)),
        None => crate::err!("HOME is not set"),
    }
}

/// The theme in `file`; bytes that are no UTF-8 cannot be part of a colour line.
fn read(file: &Path) -> Result<Theme> {
    let text = match std::fs::read(file) {
        Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        Err(e) => crate::err!("cannot read the kitty theme {}: {e}", file.display()),
    };
    Theme::from_text(&text).map_err(|e| Error(format!("{} {}", e.0, file.display())))
}

impl Theme {
    /// `~/.config/kitty/current-theme.conf`; an `Error` naming the file or the missing key.
    pub fn current() -> Result<Theme> {
        read(&home()?.join(".config/kitty/current-theme.conf"))
    }

    /// A complete theme from text; an `Error` naming the first missing key.
    pub fn from_text(text: &str) -> Result<Theme> {
        let found = parse(text);
        let mut keys = vec!["foreground".to_string(), "background".to_string()];
        keys.extend((0..16).map(|slot| format!("color{slot}")));
        if let Some(key) = keys.iter().find(|key| found.iter().all(|(k, _)| k != *key)) {
            crate::err!("no {key} in the kitty theme");
        }
        let mut theme = Theme {
            foreground: [0; 3],
            background: [0; 3],
            colors: [[0; 3]; 16],
            extra: Vec::new(),
        };
        theme.overlay(text);
        Ok(theme)
    }

    /// The colours `text` defines replace this theme's (`--theme`).
    /// When it defines any colour past color15, those replace all of `extra`.
    pub fn overlay(&mut self, text: &str) {
        let mut extra = Vec::new();
        for (key, colour) in parse(text) {
            let slot = (0..16).find(|slot| key == format!("color{slot}"));
            match (key.as_str(), slot) {
                ("foreground", _) => self.foreground = colour,
                ("background", _) => self.background = colour,
                (_, Some(slot)) => self.colors[slot] = colour,
                _ => extra.push(colour),
            }
        }
        if !extra.is_empty() {
            self.extra = extra;
        }
    }

    /// The colour of a slot: 0-15 the palette, 16 the foreground.
    pub fn ink(&self, slot: u8) -> Rgb {
        *self.colors.get(slot as usize).unwrap_or(&self.foreground)
    }

    /// Mono's slot among color0-15 and the foreground (16): of those within 0.05 Oklab lightness of the lightest, the least colourful (ties: the lighter); the first slot holding that colour.
    pub fn mono_slot(&self) -> u8 {
        let mut pool = self.colors.to_vec();
        pool.push(self.foreground);
        brightest(&pool) as u8
    }

    /// (darkest by linear luminance, brightest by mono's rule) over background, foreground and every colour.
    pub fn extremes(&self) -> (Rgb, Rgb) {
        let mut pool = vec![self.background, self.foreground];
        pool.extend(self.colors);
        pool.extend(&self.extra);
        let mut darkest = pool[0];
        for &colour in &pool {
            if luminance(colour) < luminance(darkest) {
                darkest = colour;
            }
        }
        (darkest, pool[brightest(&pool)])
    }
}

/// The place in `pool` of mono's colour: it keeps nearly the full range, and takes the near white just under a vivid yellow, never a theme's dark grey.
fn brightest(pool: &[Rgb]) -> usize {
    let lab: Vec<[f64; 3]> = pool.iter().map(|&colour| oklab(colour)).collect();
    let top = lab.iter().fold(f64::MIN, |top, lab| top.max(lab[0]));
    // of those near the lightest: the least colourful, then the lighter, then the first
    let key = |lab: &[f64; 3]| (milli(lab[1].hypot(lab[2])), -lab[0]);
    lab.iter()
        .enumerate()
        .filter(|(_, lab)| lab[0] >= top - MONO_SPAN)
        .min_by(|(_, a), (_, b)| key(a).partial_cmp(&key(b)).unwrap_or(Ordering::Equal))
        .map_or(0, |(place, _)| place)
}

/// Chroma in thousandths, rounded as numpy rounds to three places.
fn milli(chroma: f64) -> f64 {
    (chroma * 1000.0).round_ties_even()
}

/// sRGB byte to linear light.
pub fn linear() -> &'static [f64; 256] {
    static TABLE: OnceLock<[f64; 256]> = OnceLock::new();
    TABLE.get_or_init(|| {
        std::array::from_fn(|byte| {
            let v = byte as f64 / 255.0;
            // sRGB's straight segment near black
            if byte < 11 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        })
    })
}

/// Linear luminance of an sRGB colour, 0..1.
pub fn luminance(colour: Rgb) -> f64 {
    let [r, g, b] = colour.map(|v| linear()[v as usize]);
    0.2126 * r + 0.7152 * g + 0.0722 * b
}

/// Oklab (L, a, b) of an sRGB colour.
pub fn oklab(colour: Rgb) -> [f64; 3] {
    const LMS: [[f64; 3]; 3] = [
        [0.4122214708, 0.2119034982, 0.0883024619],
        [0.5363325363, 0.6806995451, 0.2817188376],
        [0.0514459929, 0.1073969566, 0.6299787005],
    ];
    const LAB: [[f64; 3]; 3] = [
        [0.2104542553, 1.9779984951, 0.0259040371],
        [0.7936177850, -2.4285922050, 0.7827717662],
        [-0.0040720468, 0.4505937099, -0.8086757660],
    ];
    let through = |v: [f64; 3], m: &[[f64; 3]; 3]| {
        [0, 1, 2].map(|to| v[0] * m[0][to] + v[1] * m[1][to] + v[2] * m[2][to])
    };
    let lms = through(colour.map(|v| linear()[v as usize]), &LMS);
    through(lms.map(|v| v.powf(1.0 / 3.0)), &LAB)
}

/// Each colour to the palette slot (0-15) of the theme colour it maps to: hue first, greys by lightness; by 6 bits a channel, each bucket mapped once from its centre.
/// So a slot depends on the colour alone, not on what came before it.
#[derive(Clone)]
pub struct Recolour {
    table: Vec<i8>,
    lab: [[f64; 3]; 16],
    hue: [f64; 16],
    greys: Vec<usize>,
    hues: Vec<usize>,
    /// Chroma a cell needs to take a hue, in the skin band and elsewhere.
    warm: f64,
    chroma: f64,
}

impl Recolour {
    /// `saturation` (percent) scales the chroma a colour needs to take a hue: twice `CHROMA` at 50, a third at 300, at 0 everything is grey.
    /// `Skin::Grey` keeps the skin band grey below `WARM_CHROMA`, scaled the same way.
    pub fn new(palette: &[Rgb; 16], saturation: i32, skin: Skin) -> Recolour {
        let need = if saturation > 0 {
            100.0 / saturation as f64
        } else {
            f64::INFINITY
        };
        let lab = palette.map(oklab);
        // without a grey slot all 16 serve as greys, without a coloured one all 16 as hues
        let slots = |grey: bool| -> Vec<usize> {
            let is = |slot: &usize| (lab[*slot][1].hypot(lab[*slot][2]) < CHROMA) == grey;
            let none = !(0..16).any(|slot| is(&slot));
            (0..16).filter(|slot| none || is(slot)).collect()
        };
        let warm = match skin {
            Skin::Grey => WARM_CHROMA,
            Skin::Theme => CHROMA,
        };
        Recolour {
            table: vec![-1; 1 << 18],
            lab,
            hue: lab.map(|lab| lab[2].atan2(lab[1])),
            greys: slots(true),
            hues: slots(false),
            warm: warm * need,
            chroma: CHROMA * need,
        }
    }

    pub fn slot(&mut self, colour: Rgb) -> u8 {
        let [r, g, b] = colour.map(|v| v as usize >> 2);
        let key = r << 12 | g << 6 | b;
        if self.table[key] < 0 {
            self.table[key] = self.pick(colour.map(|v| v & 0xFC | 2)) as i8;
        }
        self.table[key] as u8
    }

    /// A near-grey colour takes the grey slot nearest in lightness, any other the slot nearest in hue, the lighter or darker of a pair by lightness; ties go to the lower slot.
    fn pick(&self, colour: Rgb) -> usize {
        let [l, a, b] = oklab(colour);
        let hue = b.atan2(a);
        let warm = (WARM.0..=WARM.1).contains(&hue.to_degrees().rem_euclid(360.0));
        let grey = a.hypot(b) < if warm { self.warm } else { self.chroma };
        let cost = |slot: usize| {
            let light = (self.lab[slot][0] - l).powi(2);
            let gap = (self.hue[slot] - hue).abs();
            let far = gap.min(2.0 * PI - gap).powi(2) + LIGHTNESS * light;
            if grey { light } else { far }
        };
        let slots = if grey { &self.greys } else { &self.hues };
        let mut best = slots[0];
        for &slot in slots {
            if cost(slot) < cost(best) {
                best = slot;
            }
        }
        best
    }
}

/// kitty's theme collection from `~/.cache/kitty/kitty-themes.zip`, in its order: (name, is_dark, theme text); empty when the cache is missing or odd.
pub fn collection() -> Vec<(String, bool, String)> {
    home()
        .map(|home| read_cache(&home.join(".cache/kitty/kitty-themes.zip")))
        .unwrap_or_default()
}

fn read_cache(file: &Path) -> Vec<(String, bool, String)> {
    std::fs::read(file)
        .ok()
        .and_then(|zip| themes(&zip))
        .unwrap_or_default()
}

/// The themes a kitty-themes.zip lists in its `themes.json` (the first member of that name in a folder); `None` for an odd archive.
/// As the Python tools read it: an entry without a name or a file, or whose file the archive lacks, is passed over; a dark theme that cannot be read makes the archive odd.
fn themes(zip: &[u8]) -> Option<Vec<(String, bool, String)>> {
    let members = members(zip)?;
    // the last member of a name counts, as in Python's zipfile
    let member = |name: &[u8]| members.iter().rfind(|member| member.name == name);
    let index = members
        .iter()
        .find(|member| member.name.ends_with(b"/themes.json"))?
        .name;
    let root = &index[..index.len() - "themes.json".len()];
    let entries: serde_json::Value = serde_json::from_slice(&unpack(zip, member(index)?)?).ok()?;
    let mut themes = Vec::new();
    for entry in entries.as_array()? {
        let dark = entry["is_dark"].as_bool().unwrap_or(false);
        let (Some(name), Some(file)) = (entry["name"].as_str(), entry["file"].as_str()) else {
            continue;
        };
        let Some(file) = member(&[root, file.as_bytes()].concat()) else {
            continue;
        };
        match unpack(zip, file).and_then(|text| String::from_utf8(text).ok()) {
            Some(text) => themes.push((name.to_string(), dark, text)),
            None if dark => return None,
            None => {}
        }
    }
    Some(themes)
}

/// A zip archive's member, from its central directory; `at`: its local header.
struct Member<'a> {
    name: &'a [u8],
    method: usize,
    crc: usize,
    packed: usize,
    size: usize,
    at: usize,
}

/// The little-endian number in `len` bytes at `at`.
fn number(data: &[u8], at: usize, len: usize) -> Option<usize> {
    let bytes = data.get(at..at.checked_add(len)?)?;
    Some(bytes.iter().rfold(0, |n, &byte| n << 8 | byte as usize))
}

/// The members of a zip archive in its directory's order (no zip64).
fn members(zip: &[u8]) -> Option<Vec<Member<'_>>> {
    // the end record lies before the archive's comment, 65535 bytes at most
    let last = zip.len().checked_sub(22)?;
    let end = (0..=last)
        .rev()
        .take(1 << 16)
        .find(|&at| zip[at..].starts_with(b"PK\x05\x06"))?;
    let mut at = end.checked_sub(number(zip, end + 12, 4)?)?;
    // bytes before the archive move every offset, as Python's zipfile reads it
    let moved = at.checked_sub(number(zip, end + 16, 4)?)?;
    let mut members = Vec::new();
    while at < end {
        if !zip[at..].starts_with(b"PK\x01\x02") {
            return None;
        }
        let [name, extra, comment] = [28, 30, 32].map(|field| number(zip, at + field, 2));
        members.push(Member {
            name: zip.get(at + 46..at + 46 + name?)?,
            method: number(zip, at + 10, 2)?,
            crc: number(zip, at + 16, 4)?,
            packed: number(zip, at + 20, 4)?,
            size: number(zip, at + 24, 4)?,
            at: number(zip, at + 42, 4)? + moved,
        });
        at += 46 + name? + extra? + comment?;
    }
    Some(members)
}

/// A member's bytes, stored or deflated, checked against its size and checksum.
fn unpack(zip: &[u8], member: &Member) -> Option<Vec<u8>> {
    let at = member.at;
    if !zip.get(at..)?.starts_with(b"PK\x03\x04") {
        return None;
    }
    let start = at + 30 + number(zip, at + 26, 2)? + number(zip, at + 28, 2)?;
    let packed = zip.get(start..start.checked_add(member.packed)?)?;
    let mut data = Vec::new();
    match member.method {
        0 => data.extend_from_slice(packed),
        8 => {
            let limit = member.size as u64 + 1;
            let inflate = flate2::read::DeflateDecoder::new(packed);
            inflate.take(limit).read_to_end(&mut data).ok()?;
        }
        _ => return None,
    }
    let mut crc = flate2::Crc::new();
    crc.update(&data);
    (data.len() == member.size && crc.sum() as usize == member.crc).then_some(data)
}

/// A theme's text by name: the cache entry of exactly that name, else `kitten themes --dump-theme NAME`; an `Error` `no kitty theme '<name>': <reason>`.
pub fn by_name(name: &str) -> Result<String> {
    let mut kitten = Command::new("kitten");
    // the viewer's name for its own audio stream goes to no child
    kitten.env_remove("PIPEWIRE_PROPS");
    named(&collection(), kitten, name)
}

/// `by_name` over a collection and the command that stands for `kitten`.
fn named(cache: &[(String, bool, String)], mut kitten: Command, name: &str) -> Result<String> {
    if let Some((_, _, text)) = cache.iter().find(|(entry, _, _)| entry == name) {
        return Ok(text.clone());
    }
    let reason = match kitten.args(["themes", "--dump-theme", name]).output() {
        Ok(out) => {
            let text = String::from_utf8_lossy(&out.stdout);
            if out.status.success() && !parse(&text).is_empty() {
                return Ok(text.into_owned());
            }
            match String::from_utf8_lossy(&out.stderr).trim() {
                "" => "no colours in it".to_string(),
                said => said.to_string(),
            }
        }
        Err(e) => format!("cannot run kitten: {e}"),
    };
    crate::err!("no kitty theme '{name}': {reason}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::PI;
    use std::path::PathBuf;

    const BL1NK: &str = include_str!("../tests/bl1nk.conf");
    /// Written by Python's zipfile in kitty's layout: a root folder, themes.json after other entries, deflated and stored members, an archive comment.
    const CACHE: &[u8] = include_bytes!("../tests/golden/theme-collection.zip");

    fn bl1nk() -> Theme {
        Theme::from_text(BL1NK).unwrap()
    }

    fn rgb(hex: &str) -> Rgb {
        std::array::from_fn(|i| u8::from_str_radix(&hex[1 + 2 * i..3 + 2 * i], 16).unwrap())
    }

    fn pairs(list: &[(&str, &str)]) -> Vec<(String, Rgb)> {
        list.iter().map(|(k, v)| (k.to_string(), rgb(v))).collect()
    }

    /// A directory of a test's own, removed when the test ends.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Scratch {
            let name = format!("asciier-theme-{}-{name}", std::process::id());
            let dir = std::env::temp_dir().join(name);
            std::fs::create_dir_all(&dir).unwrap();
            Scratch(dir)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn fnv(bytes: impl IntoIterator<Item = u8>) -> u64 {
        bytes.into_iter().fold(0xcbf2_9ce4_8422_2325, |h, b| {
            (h ^ b as u64).wrapping_mul(0x0100_0000_01b3)
        })
    }

    #[test]
    fn parses_the_fixture() {
        // tests/test_theme.py test_parses_kitty_theme
        let found = parse(BL1NK);
        assert_eq!(found.len(), 18);
        assert!(found.contains(&("color0".into(), [0x1A, 0x1C, 0x1D])));
        assert_eq!(
            parse("color3 #abcdef\nfoo #000000\n"),
            pairs(&[("color3", "#abcdef")])
        );
    }

    #[test]
    fn syntax_is_the_python_pattern() {
        // Expected: ascii_theme.theme() on the same texts (COLOURS, re.M), run 2026-11-10.
        let cases: [(&str, &[(&str, &str)]); 22] = [
            (" color1 #111111\ncolor2 #222222", &[("color2", "#222222")]),
            ("color1\t#0a10Ff\n", &[("color1", "#0a10Ff")]),
            ("color1 #abcdef12\n", &[("color1", "#abcdef")]),
            ("color1 #abcde\n", &[]),
            ("color1 abcdef\n", &[]),
            ("color1 #111111\ncolor1 #222222\n", &[("color1", "#222222")]),
            (
                "color16 #010203\ncolor255 #040506\ncolor007 #070809\n",
                &[
                    ("color16", "#010203"),
                    ("color255", "#040506"),
                    ("color007", "#070809"),
                ],
            ),
            (
                "color #111111\nforeground_x #222222\nforeground #333333\nbackground\t \t#444444\n",
                &[("foreground", "#333333"), ("background", "#444444")],
            ),
            (
                "url_color #0087bd\nselection_foreground #111111\nxcolor1 #222222\n",
                &[],
            ),
            (
                "color1 #111111\r\ncolor2 #222222\r\n",
                &[("color1", "#111111"), ("color2", "#222222")],
            ),
            ("color1\n#111111\n", &[("color1", "#111111")]),
            (
                "color1 \n\n  #111111\ncolor2 #222222",
                &[("color1", "#111111"), ("color2", "#222222")],
            ),
            ("color1#111111\n", &[]),
            ("color1 # 111111\n", &[]),
            ("color1 #11111g\n", &[]),
            (
                "color1 #111111 trailing\ncolor2 #222222# comment\n",
                &[("color1", "#111111"), ("color2", "#222222")],
            ),
            ("FOREGROUND #111111\nColor1 #222222\n", &[]),
            (
                "color1\u{a0}#111111\ncolor2\u{1f}#222222\ncolor3\u{3000}#333333\n",
                &[
                    ("color1", "#111111"),
                    ("color2", "#222222"),
                    ("color3", "#333333"),
                ],
            ),
            ("color1 #111111", &[("color1", "#111111")]),
            ("", &[]),
            ("\ncolor5 #555555", &[("color5", "#555555")]),
            (
                "color0 #1A1C1D\ncolor8 #1a1c1d\n",
                &[("color0", "#1A1C1D"), ("color8", "#1a1c1d")],
            ),
        ];
        for (text, want) in cases {
            assert_eq!(parse(text), pairs(want), "{text:?}");
        }
    }

    #[test]
    fn a_lone_carriage_return_ends_a_line() {
        // As Python reads the theme file (universal newlines); ascii_theme.theme() on a string kept the first line only.
        assert_eq!(
            parse("color1 #111111\rcolor2 #222222\r"),
            pairs(&[("color1", "#111111"), ("color2", "#222222")])
        );
    }

    #[test]
    fn a_later_line_keeps_the_key_s_first_place() {
        assert_eq!(
            parse("color1 #111111\ncolor2 #222222\ncolor1 #333333\n"),
            pairs(&[("color1", "#333333"), ("color2", "#222222")])
        );
    }

    #[test]
    fn theme_from_text() {
        let theme = bl1nk();
        assert_eq!(
            (theme.foreground, theme.background),
            (rgb("#A0A0A0"), rgb("#111111"))
        );
        assert_eq!(
            (theme.colors[0], theme.colors[8], theme.colors[15]),
            (rgb("#1A1C1D"), rgb("#505354"), rgb("#F8F8F2"))
        );
        assert!(theme.extra.is_empty());
        let more = Theme::from_text(&format!(
            "{BL1NK}\ncolor16 #010203\ncolor007 #040506\ncolor16 #070809\n"
        ))
        .unwrap();
        assert_eq!(more.extra, [rgb("#070809"), rgb("#040506")]);
        assert_eq!(more.colors, theme.colors);
    }

    #[test]
    fn a_missing_key_is_named() {
        let without = |key: &str| {
            let text: String = BL1NK
                .lines()
                .filter(|l| l.split_whitespace().next() != Some(key))
                .map(|l| format!("{l}\n"))
                .collect();
            Theme::from_text(&text).unwrap_err().0
        };
        assert_eq!(without("color7"), "no color7 in the kitty theme");
        assert_eq!(without("foreground"), "no foreground in the kitty theme");
        assert_eq!(without("background"), "no background in the kitty theme");
        assert_eq!(
            Theme::from_text("").unwrap_err().0,
            "no foreground in the kitty theme"
        );
    }

    #[test]
    fn the_theme_file() {
        let dir = Scratch::new("file");
        let file = dir.0.join("current-theme.conf");
        let gone = read(&file).unwrap_err().0;
        assert!(
            gone.starts_with(&format!("cannot read the kitty theme {}: ", file.display())),
            "{gone}"
        );
        std::fs::write(&file, BL1NK.replace('\n', "\r\n")).unwrap();
        assert_eq!(read(&file).unwrap(), bl1nk());
        std::fs::write(&file, "foreground #000000\nbackground #000000\n").unwrap();
        assert_eq!(
            read(&file).unwrap_err().0,
            format!("no color0 in the kitty theme {}", file.display())
        );
    }

    #[test]
    fn overlay_replaces_what_the_text_defines() {
        let mut theme = bl1nk();
        theme.overlay("color1 #010203\nforeground #040506\nurl_color #ffffff\n");
        let mut want = bl1nk();
        (want.colors[1], want.foreground) = ([1, 2, 3], [4, 5, 6]);
        assert_eq!(theme, want);
        theme.overlay("color16 #070809\ncolor17 #0a0b0c\n");
        assert_eq!(theme.extra, [[7, 8, 9], [10, 11, 12]]);
        theme.overlay("background #000000\n");
        assert_eq!(theme.extra, [[7, 8, 9], [10, 11, 12]]);
        theme.overlay("color16 #0d0e0f\n");
        assert_eq!(theme.extra, [[13, 14, 15]]);
    }

    #[test]
    fn ink_by_slot() {
        let theme = bl1nk();
        assert_eq!(
            (theme.ink(0), theme.ink(9), theme.ink(15), theme.ink(16)),
            (
                rgb("#1A1C1D"),
                rgb("#F92571"),
                rgb("#F8F8F2"),
                rgb("#A0A0A0")
            )
        );
    }

    #[test]
    fn extremes_and_mono_slot() {
        // tests/test_theme.py test_extremes_and_brightest
        let theme = bl1nk();
        assert_eq!(theme.extremes(), (rgb("#111111"), rgb("#F8F8F2")));
        assert_eq!(theme.mono_slot(), 15);
        // yellow is brighter, but mono stays grey
        let mut vivid = bl1nk();
        (vivid.colors[11], vivid.colors[15]) = (rgb("#ffff00"), rgb("#eeeeee"));
        assert_eq!(
            (vivid.mono_slot(), vivid.extremes().1),
            (15, rgb("#eeeeee"))
        );
        // nothing as light: the lightest
        let mut no_grey = bl1nk();
        no_grey.colors = [rgb("#3355ff"); 16];
        (no_grey.foreground, no_grey.background) = (rgb("#ffcc00"), rgb("#0000aa"));
        assert_eq!(
            (no_grey.mono_slot(), no_grey.extremes()),
            (16, (rgb("#0000aa"), rgb("#ffcc00")))
        );
        // its only grey is black: mono must not take it
        let mut dark_grey = no_grey.clone();
        dark_grey.colors[0] = rgb("#000000");
        assert_eq!(
            (dark_grey.mono_slot(), dark_grey.extremes()),
            (16, (rgb("#000000"), rgb("#ffcc00")))
        );
        // lib/ascii_theme.py on two more: whites alike in chroma to three places, the lighter wins; a tint of a thousandth loses to the grey
        let mut whites = bl1nk();
        (whites.colors[15], whites.foreground) = (rgb("#fafafa"), rgb("#ffffff"));
        assert_eq!(whites.mono_slot(), 16);
        assert_eq!(whites.extremes(), (rgb("#111111"), rgb("#ffffff")));
        whites.foreground = rgb("#fffffe");
        assert_eq!(whites.mono_slot(), 15);
        assert_eq!(whites.extremes(), (rgb("#111111"), rgb("#fafafa")));
        // and on a colour held twice: the first slot holding it
        let mut twice = bl1nk();
        twice.colors[7] = twice.colors[15];
        assert_eq!(twice.mono_slot(), 7);
        twice = bl1nk();
        twice.foreground = twice.colors[15];
        assert_eq!(twice.mono_slot(), 15);
        let mut white = bl1nk();
        white.foreground = rgb("#FFFFFF");
        assert_eq!(
            (white.mono_slot(), white.extremes().1),
            (16, rgb("#FFFFFF"))
        );
    }

    #[test]
    fn extremes_count_the_extra_colours() {
        // ascii_theme.extremes and brightest_slot on bl1nk with color16 #000000 and color17 #ffffff
        let mut theme = bl1nk();
        theme.extra = vec![rgb("#000000"), rgb("#ffffff")];
        assert_eq!(theme.extremes(), (rgb("#000000"), rgb("#ffffff")));
        assert_eq!(theme.mono_slot(), 15);
    }

    #[test]
    fn mono_rounds_chroma_as_numpy() {
        // round(np.float64(x), 3) is rint(x * 1000) / 1000: 0.0005 -> 0.0 and 0.0025 -> 0.002 (Python's own round gives 0.001 and 0.003).
        assert_eq!(
            (milli(0.0005), milli(0.0015), milli(0.0025)),
            (0.0, 2.0, 2.0)
        );
    }

    #[test]
    fn luminance_and_oklab_are_the_python_values() {
        // ascii_theme.luminance and ascii_theme.oklab, printed with float.hex() 2026-11-10.
        let near = |got: f64, want: f64| (got - want).abs() < 1e-12;
        let luma = [
            (rgb("#000000"), 0.0),
            (rgb("#ffffff"), 1.0),
            (rgb("#1A1C1D"), 0.011388282588020838),
            (rgb("#FF5894"), 0.3037756538274988),
            ([10, 16, 255], 0.07655111911253724),
            ([128, 128, 128], 0.21586050011389923),
            ([1, 2, 3], 0.0005644387786074181),
            ([200, 30, 30], 0.13301644130157075),
            ([224, 172, 140], 0.4724579095856184),
        ];
        for (colour, want) in luma {
            assert!(near(luminance(colour), want), "{colour:?}");
        }
        let lab = [
            ([0, 0, 0], [0.0, 0.0, 0.0]),
            (
                [255, 255, 255],
                [
                    0.9999999934735462,
                    8.095285553011422e-11,
                    3.727390762708893e-08,
                ],
            ),
            (
                [26, 28, 29],
                [
                    0.22472185591973742,
                    -0.0023738747538346316,
                    -0.0027276299408584004,
                ],
            ),
            (
                [255, 88, 148],
                [
                    0.6992144587896104,
                    0.20704430961600273,
                    0.007361210934989315,
                ],
            ),
            (
                [10, 16, 255],
                [
                    0.45882407829944677,
                    -0.02812720584547651,
                    -0.3071611292166987,
                ],
            ),
            (
                [128, 128, 128],
                [
                    0.5998708017071177,
                    4.8561543675162966e-11,
                    2.2359528895066205e-08,
                ],
            ),
            (
                [1, 2, 3],
                [
                    0.08230913712662,
                    -0.003926063627585227,
                    -0.007010752932764555,
                ],
            ),
            (
                [200, 30, 30],
                [0.5349152190107048, 0.17955772290750124, 0.09390931229282318],
            ),
            (
                [224, 172, 140],
                [0.7844224218757583, 0.04534403362037981, 0.05999497177335911],
            ),
        ];
        for (colour, want) in lab {
            let got = oklab(colour);
            assert!((0..3).all(|i| near(got[i], want[i])), "{colour:?} {got:?}");
        }
    }

    #[test]
    fn recolour_by_hue_then_lightness() {
        // tests/test_theme.py RecolourTest; the slots are the Python tools' (bl1nk: default, skin off, saturation 0).
        let palette = bl1nk().colors;
        let mut default = Recolour::new(&palette, 100, Skin::Grey);
        let mut no_skin = Recolour::new(&palette, 100, Skin::Theme);
        let mut no_colour = Recolour::new(&palette, 0, Skin::Grey);
        let cases = [
            ([20, 20, 20], 0, 0, 0),
            ([90, 90, 90], 8, 8, 8),
            ([160, 160, 160], 14, 14, 14),
            ([250, 250, 250], 15, 15, 15),
            ([0, 200, 0], 10, 10, 14),
            ([40, 60, 220], 12, 12, 8),
            ([60, 110, 40], 10, 10, 8),
            ([224, 172, 140], 7, 3, 7),
            ([255, 0, 0], 9, 9, 14),
            ([0, 255, 0], 2, 2, 7),
            ([0, 0, 255], 12, 12, 8),
        ];
        for (colour, a, b, c) in cases {
            assert_eq!(
                (
                    default.slot(colour),
                    no_skin.slot(colour),
                    no_colour.slot(colour)
                ),
                (a, b, c),
                "{colour:?}"
            );
        }
    }

    #[test]
    fn ties_go_to_the_lower_slot() {
        // lib/ascii_theme.py Recolour on bl1nk with color15 as color7 and color9 as color1, and on sixteen times #3355ff.
        let mut twins = bl1nk().colors;
        (twins[15], twins[9]) = (twins[7], twins[1]);
        let probe = [
            [255, 0, 0],
            [10, 10, 10],
            [200, 200, 200],
            [250, 250, 250],
            [0, 200, 0],
            [40, 60, 220],
            [224, 172, 140],
            [128, 128, 128],
        ];
        let mut recolour = Recolour::new(&twins, 100, Skin::Grey);
        assert_eq!(
            probe.map(|colour| recolour.slot(colour)),
            [1, 0, 7, 7, 10, 12, 7, 14]
        );
        let mut recolour = Recolour::new(&[rgb("#3355ff"); 16], 100, Skin::Grey);
        assert_eq!(probe.map(|colour| recolour.slot(colour)), [0; 8]);
    }

    /// Recolour's rule for one colour, written apart from the table: tests/test_theme.py reference_slots.
    fn reference_slot(palette: &[Rgb; 16], saturation: i32, skin: Skin, colour: Rgb) -> u8 {
        let lab = palette.map(oklab);
        let grey: Vec<usize> = (0..16)
            .filter(|&i| lab[i][1].hypot(lab[i][2]) < 0.035)
            .collect();
        let hued: Vec<usize> = (0..16).filter(|i| !grey.contains(i)).collect();
        let every: Vec<usize> = (0..16).collect();
        let (greys, hues) = (
            if grey.is_empty() { &every } else { &grey },
            if hued.is_empty() { &every } else { &hued },
        );
        let need = if saturation > 0 {
            100.0 / saturation as f64
        } else {
            f64::INFINITY
        };
        let [l, a, b] = oklab(colour);
        let hue = b.atan2(a);
        let warm = (20.0..=75.0).contains(&hue.to_degrees().rem_euclid(360.0));
        let chroma = if warm && skin == Skin::Grey {
            0.10 * need
        } else {
            0.035 * need
        };
        let cost = |i: usize| {
            if a.hypot(b) < chroma {
                (lab[i][0] - l).powi(2)
            } else {
                let gap = (lab[i][2].atan2(lab[i][1]) - hue).abs();
                gap.min(2.0 * PI - gap).powi(2) + 1.5 * (lab[i][0] - l).powi(2)
            }
        };
        let slots = if a.hypot(b) < chroma { greys } else { hues };
        let mut costs: Vec<(f64, usize)> = slots.iter().map(|&i| (cost(i), i)).collect();
        costs.sort_by(|x, y| x.0.total_cmp(&y.0).then(x.1.cmp(&y.1)));
        costs[0].1 as u8
    }

    fn centre(key: usize) -> Rgb {
        [key >> 12, key >> 6, key].map(|v| ((v & 63) * 4 + 2) as u8)
    }

    #[test]
    fn every_bucket_follows_the_rule_from_its_centre() {
        // tests/test_theme.py test_every_key_follows_the_rule_from_its_centre: all 262,144 keys, three settings.
        let palette = bl1nk().colors;
        for (saturation, skin) in [(100, Skin::Grey), (40, Skin::Theme), (300, Skin::Grey)] {
            let mut recolour = Recolour::new(&palette, saturation, skin);
            for key in 0..1 << 18 {
                let colour = centre(key);
                assert_eq!(
                    recolour.slot(colour),
                    reference_slot(&palette, saturation, skin, colour),
                    "{colour:?} at {saturation} {skin:?}"
                );
            }
        }
    }

    #[test]
    fn tables_are_the_python_tables() {
        // FNV-1a 64 of Recolour(palette, saturation, skin).slots(centres of all 262,144 keys) as bytes, from lib/ascii_theme.py 2026-11-10.
        let hues = [
            "#ff0000", "#00ff00", "#0000ff", "#ffff00", "#ff00ff", "#00ffff", "#ff8800", "#8800ff",
            "#880000", "#008800", "#000088", "#888800", "#880088", "#008888", "#884400", "#440088",
        ]
        .map(rgb);
        let dracula = [
            "#21222c", "#ff5555", "#50fa7b", "#f1fa8c", "#bd93f9", "#ff79c6", "#8be9fd", "#f8f8f2",
            "#6272a4", "#ff6e6e", "#69ff94", "#ffffa5", "#d6acff", "#ff92df", "#a4ffff", "#ffffff",
        ]
        .map(rgb);
        let bl1nk = bl1nk().colors;
        // one colour twice or sixteen times, and no coloured slot at all
        let (mut twins, one) = (bl1nk, [rgb("#3355ff"); 16]);
        (twins[15], twins[9]) = (twins[7], twins[1]);
        let greys: [Rgb; 16] = std::array::from_fn(|slot| [8 + 16 * slot as u8; 3]);
        let cases = [
            (bl1nk, 100, Skin::Grey, 0xa80d18f9747be337u64),
            (bl1nk, 40, Skin::Theme, 0x2f9513f15f98a8d1),
            (bl1nk, 300, Skin::Grey, 0xff0ba3c4485e9669),
            (bl1nk, 0, Skin::Grey, 0xc6356f910e25f5b7),
            (bl1nk, 100, Skin::Theme, 0xf73beaddf9081f1c),
            (bl1nk, 55, Skin::Grey, 0x7e8c6ecb8a3db9c3),
            (bl1nk, 300, Skin::Theme, 0x8ba8e32fc99fba2a),
            (bl1nk, 7, Skin::Grey, 0xc6356f910e25f5b7),
            (hues, 100, Skin::Grey, 0xa83a973e910c7869),
            (dracula, 100, Skin::Grey, 0xc22257c3004ca50d),
            (dracula, 250, Skin::Theme, 0xb7309a5becebbb41),
            (twins, 100, Skin::Grey, 0x44e35adfd5a55701),
            (one, 100, Skin::Grey, 0x9c735bed0a722325),
            (greys, 100, Skin::Grey, 0x3c6fb75924604641),
        ];
        for (palette, saturation, skin, want) in cases {
            let mut recolour = Recolour::new(&palette, saturation, skin);
            let table = (0..1 << 18).map(|key| recolour.slot(centre(key)));
            assert_eq!(fnv(table), want, "{:?} {saturation} {skin:?}", palette[1]);
        }
    }

    #[test]
    fn slots_do_not_depend_on_order() {
        // tests/test_theme.py test_slots_do_not_depend_on_order: 5000 colours and a twin of each in its 6-bit group.
        let mut seed = 3u32;
        let mut random = || {
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            (seed >> 24) as u8
        };
        let mut both: Vec<Rgb> = (0..5000).map(|_| [random(), random(), random()]).collect();
        let twins: Vec<Rgb> = both
            .iter()
            .map(|c| c.map(|v| v & 0xFC | random() & 3))
            .collect();
        both.extend(twins);
        let palette = bl1nk().colors;
        let fresh = || Recolour::new(&palette, 100, Skin::Grey);
        let (mut forward, mut backward, mut centres) = (fresh(), fresh(), fresh());
        for &colour in both.iter().rev() {
            backward.slot(colour);
        }
        for &colour in &both {
            let slot = forward.slot(colour);
            assert_eq!(slot, backward.slot(colour));
            assert_eq!(slot, centres.slot(colour.map(|v| v & 0xFC | 2)));
        }
    }

    #[test]
    fn collection_from_the_cache() {
        // The dark names in order are what bin/ascii-viewer kitty_themes() lists from this file: Night, Stored, Twin.
        let found = themes(CACHE).unwrap();
        let names: Vec<(&str, bool)> = found
            .iter()
            .map(|(n, dark, _)| (n.as_str(), *dark))
            .collect();
        assert_eq!(
            names,
            [
                ("Night", true),
                ("Day", false),
                ("Stored", true),
                ("Twin", true),
                ("Twin", false)
            ]
        );
        let night = &found[0].2;
        assert!(night.starts_with("## name: Night\n") && night.ends_with("color16 #ff9040\n"));
        assert_eq!((night.len(), parse(night).len()), (400, 19));
        assert_eq!(
            found[1].2,
            "# partial: three colours\nbackground #fafafa\nforeground #202020\ncolor1 #c02030\n"
        );
        assert_eq!(parse(&found[2].2).len(), 18);
        assert_eq!(
            (found[3].2.as_str(), found[4].2.as_str()),
            ("color2 #00aa00\n", "color2 #00bb00\n")
        );
    }

    /// A zip of stored members.
    fn zip(members: &[(&str, &[u8])]) -> Vec<u8> {
        let (mut out, mut directory) = (Vec::new(), Vec::new());
        for (name, data) in members {
            let mut crc = flate2::Crc::new();
            crc.update(data);
            let mut sizes = crc.sum().to_le_bytes().to_vec();
            sizes.extend(
                [data.len() as u32, data.len() as u32]
                    .map(u32::to_le_bytes)
                    .concat(),
            );
            sizes.extend((name.len() as u16).to_le_bytes());
            directory.extend(b"PK\x01\x02\x14\0\x14\0\0\0\0\0\0\0\0\0");
            directory.extend(&sizes);
            directory.extend([0; 12]);
            directory.extend((out.len() as u32).to_le_bytes());
            directory.extend(name.bytes());
            out.extend(b"PK\x03\x04\x14\0\0\0\0\0\0\0\0\0");
            out.extend(&sizes);
            out.extend([0; 2]);
            out.extend(name.bytes());
            out.extend(*data);
        }
        let count = (members.len() as u16).to_le_bytes();
        let (size, offset) = (directory.len() as u32, out.len() as u32);
        out.extend(directory);
        out.extend(b"PK\x05\x06\0\0\0\0");
        out.extend([count, count].concat());
        out.extend([size, offset].map(u32::to_le_bytes).concat());
        out.extend([0; 2]);
        out
    }

    #[test]
    fn odd_caches() {
        // bin/ascii-viewer kitty_themes() on four of these archives (2026-11-10): A from the plain one, from the one with an unreadable light theme and from the one with a member twice; nothing from the one whose dark theme is no text.
        let index = br#"[{"name": "A", "file": "a.conf", "is_dark": true}]"#;
        let good = zip(&[("r/themes.json", index), ("r/a.conf", b"color1 #010203\n")]);
        let one = vec![("A".to_string(), true, "color1 #010203\n".to_string())];
        assert_eq!(themes(&good), Some(one.clone()));
        // data before the archive, as Python's zipfile allows
        assert_eq!(
            themes(&[b"junk".as_slice(), &good].concat()),
            Some(one.clone())
        );
        // no archive, a cut archive, no index, an index outside a folder, an index that is no list
        assert_eq!(themes(b""), None);
        assert_eq!(
            themes(b"not a zip at all, but long enough to hold a directory"),
            None
        );
        assert_eq!(themes(&good[..good.len() - 30]), None);
        assert_eq!(themes(&zip(&[("r/a.conf", b"color1 #010203\n")])), None);
        assert_eq!(
            themes(&zip(&[("themes.json", index), ("a.conf", b"")])),
            None
        );
        assert_eq!(themes(&zip(&[("r/themes.json", b"{}")])), None);
        assert_eq!(themes(&zip(&[("r/themes.json", b"[")])), None);
        // entries that are no themes are passed over
        let mixed = br#"[1, "x", null, {"name": 2, "file": "a.conf"}, {"name": "B", "file": 3}, {"name": "A", "file": "a.conf", "is_dark": true}]"#;
        assert_eq!(
            themes(&zip(&[
                ("r/themes.json", mixed),
                ("r/a.conf", b"color1 #010203\n")
            ])),
            Some(one.clone())
        );
        // a dark theme whose bytes are not its checksum's, or no text: an odd archive
        let mut bad = good.clone();
        let at = bad.windows(6).position(|w| w == b"color1").unwrap();
        bad[at] = b'C';
        assert_eq!(themes(&bad), None);
        assert_eq!(
            themes(&zip(&[("r/themes.json", index), ("r/a.conf", b"\xff\xfe")])),
            None
        );
        // the Python tools never read the other themes: such a one is passed over
        let light = br#"[{"name": "B", "file": "b.conf"}, {"name": "A", "file": "a.conf", "is_dark": true}]"#;
        let members: [(&str, &[u8]); 3] = [
            ("r/themes.json", light),
            ("r/b.conf", b"\xff\xfe"),
            ("r/a.conf", b"color1 #010203\n"),
        ];
        assert_eq!(themes(&zip(&members)), Some(one.clone()));
        // the last member of a name is the one read
        assert_eq!(
            themes(&zip(&[
                ("r/themes.json", index),
                ("r/a.conf", b"old"),
                ("r/a.conf", b"color1 #010203\n")
            ])),
            Some(one)
        );
    }

    #[test]
    fn the_cache_file() {
        let dir = Scratch::new("cache");
        let file = dir.0.join("kitty-themes.zip");
        assert!(read_cache(&file).is_empty());
        std::fs::write(&file, CACHE).unwrap();
        assert_eq!(read_cache(&file).len(), 5);
        std::fs::write(&file, &CACHE[..CACHE.len() / 2]).unwrap();
        assert!(read_cache(&file).is_empty());
    }

    #[test]
    fn files_under_home() {
        // HOME cannot change under the other tests: this test runs itself again as a child with a HOME of its own.
        if let Some(home) = std::env::var_os("ASCIIER_TEST_HOME") {
            let theme = Path::new(&home).join(".config/kitty/current-theme.conf");
            if theme.exists() {
                assert_eq!(Theme::current().unwrap(), bl1nk());
                assert_eq!(collection(), themes(CACHE).unwrap());
                assert_eq!(by_name("Twin").unwrap(), "color2 #00aa00\n");
            } else {
                let failure = Theme::current().unwrap_err().0;
                let start = format!("cannot read the kitty theme {}: ", theme.display());
                assert!(failure.starts_with(&start), "{failure}");
                assert!(collection().is_empty());
            }
            return;
        }
        let child = |home: &Path| {
            let test = ["--exact", "theme::tests::files_under_home"];
            let out = Command::new(std::env::current_exe().unwrap())
                .args(test)
                .env("HOME", home)
                .env("ASCIIER_TEST_HOME", home)
                // row G11: the theme's place does not follow XDG_CONFIG_HOME
                .env("XDG_CONFIG_HOME", home.join("elsewhere"))
                .output()
                .unwrap();
            let said = String::from_utf8_lossy(&out.stdout);
            assert!(out.status.success() && said.contains("1 passed"), "{said}");
        };
        let dir = Scratch::new("home");
        child(&dir.0);
        for folder in [".config/kitty", ".cache/kitty"] {
            std::fs::create_dir_all(dir.0.join(folder)).unwrap();
        }
        std::fs::write(dir.0.join(".config/kitty/current-theme.conf"), BL1NK).unwrap();
        std::fs::write(dir.0.join(".cache/kitty/kitty-themes.zip"), CACHE).unwrap();
        child(&dir.0);
    }

    #[test]
    fn theme_by_name() {
        let dir = Scratch::new("kitten");
        let (script, calls) = (dir.0.join("kitten"), dir.0.join("kitten.calls"));
        // a stand-in for kitten: it notes each call, knows one theme and fails in three ways
        let stand_in = r#"echo "$@" >> "$0.calls"
case "$3" in
Known) printf 'background #000000\r\ncolor1 #aa0000\r\n';;
Empty) echo nothing;;
Quiet) exit 3;;
Half) echo 'color1 #aa0000'; exit 2;;
*) echo " Error: No theme named: $3 " >&2; exit 1;;
esac
"#;
        std::fs::write(&script, stand_in).unwrap();
        let kitten = || {
            let mut sh = Command::new("sh");
            sh.arg(&script);
            sh
        };
        let cache = themes(CACHE).unwrap();
        // the cache entry of exactly that name, the first of two; kitten is not asked
        assert_eq!(named(&cache, kitten(), "Twin").unwrap(), "color2 #00aa00\n");
        assert_eq!(named(&cache, kitten(), "Day").unwrap().len(), 78);
        assert!(!calls.exists());
        // else kitten's text
        assert_eq!(
            named(&cache, kitten(), "Known").unwrap(),
            "background #000000\r\ncolor1 #aa0000\r\n"
        );
        assert_eq!(
            std::fs::read_to_string(calls).unwrap(),
            "themes --dump-theme Known\n"
        );
        // row C7: kitten's stderr, or "no colours in it"
        let failure = |name: &str| named(&cache, kitten(), name).unwrap_err().0;
        assert_eq!(
            failure("night"),
            "no kitty theme 'night': Error: No theme named: night"
        );
        assert_eq!(failure("Empty"), "no kitty theme 'Empty': no colours in it");
        assert_eq!(failure("Quiet"), "no kitty theme 'Quiet': no colours in it");
        assert_eq!(failure("Half"), "no kitty theme 'Half': no colours in it");
        let none = named(&[], Command::new(dir.0.join("no-kitten")), "X")
            .unwrap_err()
            .0;
        assert!(
            none.starts_with("no kitty theme 'X': cannot run kitten: "),
            "{none}"
        );
    }
}
