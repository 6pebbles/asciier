//! Theme colours: kitty theme text, the slot each colour maps to, mono's colour, kitty's theme collection.

use crate::Result;
use crate::look::Skin;

pub type Rgb = [u8; 3];

/// A kitty theme's colours: foreground, background, color0-15; `extra` holds color16 and up (they only join mono's pool).
#[derive(Clone, PartialEq, Debug)]
pub struct Theme {
    pub foreground: Rgb,
    pub background: Rgb,
    pub colors: [Rgb; 16],
    pub extra: Vec<Rgb>,
}

/// The colour lines of a kitty theme text, in order: a key at column 0 (`foreground`, `background`, `color<N>`), whitespace, `#` and six hex digits.
pub fn parse(text: &str) -> Vec<(String, Rgb)> {
    todo!("{text}")
}

impl Theme {
    /// `~/.config/kitty/current-theme.conf`; an `Error` naming the file or the missing key.
    pub fn current() -> Result<Theme> {
        todo!()
    }

    /// A complete theme from text; an `Error` naming the first missing key.
    pub fn from_text(text: &str) -> Result<Theme> {
        todo!("{text}")
    }

    /// The colours `text` defines replace this theme's (`--theme`).
    pub fn overlay(&mut self, text: &str) {
        todo!("{text}")
    }

    /// The colour of a slot: 0-15 the palette, 16 the foreground.
    pub fn ink(&self, slot: u8) -> Rgb {
        todo!("{slot}")
    }

    /// Mono's slot among color0-15 and the foreground (16): of those within 0.05 Oklab lightness of the lightest, the least colourful (ties: the lighter); the first slot holding that colour.
    pub fn mono_slot(&self) -> u8 {
        todo!()
    }

    /// (darkest by linear luminance, brightest by mono's rule) over background, foreground and every colour.
    pub fn extremes(&self) -> (Rgb, Rgb) {
        todo!()
    }
}

/// Linear luminance of an sRGB colour, 0..1.
pub fn luminance(colour: Rgb) -> f64 {
    todo!("{colour:?}")
}

/// Oklab (L, a, b) of an sRGB colour.
pub fn oklab(colour: Rgb) -> [f64; 3] {
    todo!("{colour:?}")
}

/// Each colour to the palette slot (0-15) of the theme colour it maps to: hue first, greys by lightness; by 6 bits a channel, each bucket mapped once from its centre.
pub struct Recolour {
    table: Vec<i8>,
}

impl Recolour {
    pub fn new(palette: &[Rgb; 16], saturation: i32, skin: Skin) -> Recolour {
        todo!("{palette:?} {saturation} {skin:?}")
    }

    pub fn slot(&mut self, colour: Rgb) -> u8 {
        todo!("{colour:?}")
    }
}

/// kitty's theme collection from `~/.cache/kitty/kitty-themes.zip`, in its order: (name, is_dark, theme text); empty when the cache is missing or odd.
pub fn collection() -> Vec<(String, bool, String)> {
    todo!()
}

/// A theme's text by name: the cache entry of exactly that name, else `kitten themes --dump-theme NAME`; an `Error` `no kitty theme '<name>': <reason>`.
pub fn by_name(name: &str) -> Result<String> {
    todo!("{name}")
}
