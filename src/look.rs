//! The look: seven settings that the viewer's keys, the render page and the converter share.

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Glyphs {
    Mixed,
    Density,
    Shape,
}

impl Glyphs {
    pub const ALL: [Glyphs; 3] = [Glyphs::Mixed, Glyphs::Density, Glyphs::Shape];

    pub fn name(self) -> &'static str {
        match self {
            Glyphs::Mixed => "mixed",
            Glyphs::Density => "density",
            Glyphs::Shape => "shape",
        }
    }

    pub fn parse(text: &str) -> Option<Glyphs> {
        Self::ALL.into_iter().find(|g| g.name() == text)
    }

    /// `by` places on, wrapping round (the `g` key, the page's `h` `l`).
    pub fn step(self, by: i32) -> Glyphs {
        let at = Self::ALL.iter().position(|g| *g == self).unwrap() as i32;
        Self::ALL[(at + by).rem_euclid(3) as usize]
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Skin {
    Grey,
    Theme,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Look {
    pub level: i32,
    pub contrast: i32,
    pub saturation: i32,
    pub skin: Skin,
    pub glyphs: Glyphs,
    pub mono: bool,
    pub bold: bool,
}

impl Default for Look {
    fn default() -> Self {
        Look {
            level: 0,
            contrast: 0,
            saturation: 100,
            skin: Skin::Grey,
            glyphs: Glyphs::Mixed,
            mono: false,
            bold: false,
        }
    }
}

pub const LEVEL: (i32, i32) = (-95, 95);
pub const CONTRAST: (i32, i32) = (-95, 95);
pub const SATURATION: (i32, i32) = (0, 300);

pub fn clamp(value: i32, (low, high): (i32, i32)) -> i32 {
    value.clamp(low, high)
}

impl Look {
    pub fn tone(&self) -> Option<[u8; 256]> {
        tone_table(self.level, self.contrast)
    }
}

/// Byte map of level and contrast (percent, -95..95), `None` when both are 0.
/// Level up raises the black point, down lowers the white point; contrast scales about mid grey.
pub fn tone_table(level: i32, contrast: i32) -> Option<[u8; 256]> {
    if level == 0 && contrast == 0 {
        return None;
    }
    let gain = 1.0 + contrast as f64 / 100.0;
    let mut table = [0u8; 256];
    for (v, out) in table.iter_mut().enumerate() {
        let v = v as f64;
        let tone = if level > 0 {
            let cut = 255.0 * level as f64 / 100.0;
            if v < cut {
                0.0
            } else {
                (v - cut) * 255.0 / (255.0 - cut)
            }
        } else if level < 0 {
            let top = 255.0 * (1.0 + level as f64 / 100.0);
            if v >= top { 255.0 } else { v * 255.0 / top }
        } else {
            v
        };
        *out = (127.5 + (tone - 127.5) * gain)
            .round_ties_even()
            .clamp(0.0, 255.0) as u8;
    }
    Some(table)
}

/// Apply a tone table to packed pixels in place.
pub fn apply(table: &[u8; 256], pixels: &mut [u8]) {
    for p in pixels {
        *p = table[*p as usize];
    }
}

/// Level in words: darker above 0, brighter below.
pub fn level_label(level: i32) -> String {
    match level {
        0 => "0".into(),
        l if l > 0 => format!("{l}% darker"),
        l => format!("{}% brighter", -l),
    }
}

pub fn contrast_label(contrast: i32) -> String {
    if contrast == 0 {
        "0".into()
    } else {
        format!("{contrast:+}%")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn neutral_is_none() {
        assert!(tone_table(0, 0).is_none());
    }

    #[test]
    fn level_up_raises_the_black_point() {
        let t = tone_table(40, 0).unwrap();
        assert!(t[..101].iter().all(|&v| v == 0));
        assert_eq!((t[102], t[103], t[255]), (0, 2, 255));
        assert!(t.windows(2).all(|w| w[0] <= w[1]));
    }

    #[test]
    fn level_down_lowers_the_white_point() {
        let t = tone_table(-40, 0).unwrap();
        assert_eq!((t[0], t[152] < 255, t[153]), (0, true, 255));
        assert!(t[154..].iter().all(|&v| v == 255));
    }

    #[test]
    fn contrast_spreads_from_mid_grey() {
        let (up, down) = (tone_table(0, 50).unwrap(), tone_table(0, -50).unwrap());
        assert_eq!((up[64], up[192], down[64], down[192]), (32, 224, 96, 160));
    }

    #[test]
    fn labels() {
        assert_eq!(
            (level_label(0), level_label(15), level_label(-5)),
            ("0".into(), "15% darker".into(), "5% brighter".into())
        );
        assert_eq!(
            (contrast_label(0), contrast_label(5), contrast_label(-10)),
            ("0".into(), "+5%".into(), "-10%".into())
        );
    }

    #[test]
    fn glyphs_cycle() {
        assert_eq!(Glyphs::Mixed.step(1), Glyphs::Density);
        assert_eq!(Glyphs::Mixed.step(-1), Glyphs::Shape);
        assert_eq!(Glyphs::parse("shape"), Some(Glyphs::Shape));
    }
}
