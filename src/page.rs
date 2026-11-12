//! The render page (`r`): the picture as the converter would save it, set interactively and exported.
//!
//! Settings down the left, then Export and Back; on the right the picture as the file will hold it: the converter's own drawing, made at the size it is shown.
//! Density is a character's size in the output, in px of the font: the columns follow from it and the output's width, and a typed column count sets the density that gives it.

use crate::convert::{self, COLUMNS, FULL, Fit, Options, PX, SIZE};
use crate::font::{self, Font};
use crate::look::{self, Glyphs, Skin};
use crate::paint::Style;
use crate::term::Geometry;
use crate::term::kitty::{self, Transfer};
use crate::theme::{self, Theme};
use crate::{Result, desktop, picture};
use image::{Rgb, RgbImage};
use std::cell::OnceCell;
use std::path::PathBuf;

/// What the viewer does after a key.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum After {
    Stay,
    Close,
}

/// Cells of the menu column.
const WIDTH: usize = 46;
const DIM: &str = "\x1b[90m";
const REVERSE: &str = "\x1b[7m";
const OFF: &str = "\x1b[0m";
/// The size presets: px on the output's long side, in the cropped picture's shape; 0 stands for the focused monitor's size.
const SIZES: [(&str, u32); 4] = [
    ("1920 long side", 1920),
    ("1080 long side", 1080),
    ("3840 long side", 3840),
    ("screen", 0),
];
/// Font px: the range of the Density row.
const DENSITY: (f64, f64) = (4.0, 120.0);
/// The crop editor's ratios, width to height: the first leaves the shape free, the second is the picture's own.
const RATIOS: [(&str, f64); 12] = [
    ("free", 0.0),
    ("original", 0.0),
    ("1:1", 1.0),
    ("4:5", 4.0 / 5.0),
    ("5:4", 5.0 / 4.0),
    ("3:4", 3.0 / 4.0),
    ("4:3", 4.0 / 3.0),
    ("2:3", 2.0 / 3.0),
    ("3:2", 3.0 / 2.0),
    ("9:16", 9.0 / 16.0),
    ("16:9", 16.0 / 9.0),
    ("21:9", 21.0 / 9.0),
];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Row {
    Level,
    Contrast,
    Saturation,
    Skin,
    Glyphs,
    Density,
    Columns,
    Fit,
    Crop,
    CropW,
    CropH,
    CropX,
    CropY,
    Size,
    Theme,
    Bold,
    Mono,
    Export,
    Back,
}

/// The menu from the top: each row at the place of its discriminant, its label, and what a value typed for it may be.
const ROWS: [(Row, &str, &str); 19] = [
    (Row::Level, "Level", "-95..95, up darker"),
    (Row::Contrast, "Contrast", "-95..95"),
    (Row::Saturation, "Saturation", "percent, 0..300"),
    (Row::Skin, "Skin", ""),
    (Row::Glyphs, "Glyphs", ""),
    (Row::Density, "Density", "px 4..120"),
    (Row::Columns, "Columns", "8..2000"),
    (Row::Fit, "Fit", ""),
    (Row::Crop, "Crop", ""),
    (Row::CropW, "Crop W", "% of picture width"),
    (Row::CropH, "Crop H", "% of picture height"),
    (Row::CropX, "Crop X", "% from the left edge"),
    (Row::CropY, "Crop Y", "% from the top edge"),
    (Row::Size, "Size", "WxH, each 16..16384"),
    (Row::Theme, "Theme", ""),
    (Row::Bold, "Bold", ""),
    (Row::Mono, "Mono", ""),
    (Row::Export, "Export", ""),
    (Row::Back, "Back", ""),
];

/// What the menu column shows and the keys act on.
enum State {
    Menu,
    /// A value being typed for a row.
    Prompt(Row, String),
    List(List),
    Crop,
}

/// An open list: the sizes, or the themes with their search.
struct List {
    themes: bool,
    /// The cursor, a place among the rows the search leaves.
    at: usize,
    query: String,
    searching: bool,
    /// The theme under the cursor, a place in `Page::themes`: the preview shows it.
    theme: usize,
}

pub struct Page {
    picture: RgbImage,
    out: PathBuf,
    /// Look, crop and fit as the rows set them; `settings` adds theme, columns and size.
    options: Options,
    /// Font px of a character in the output.
    density: f64,
    /// The size preset, a place in `SIZES`; a custom size comes before it.
    size: usize,
    custom: Option<(u32, u32)>,
    /// The focused monitor's size, asked once.
    screen: OnceCell<Option<(u32, u32)>>,
    /// The crop editor's ratio, a place in `RATIOS`.
    ratio: usize,
    /// The chosen theme, a place in `themes`.
    theme: usize,
    /// `current`, then kitty's dark themes: name and strip of colours.
    themes: Vec<(String, String)>,
    /// Cell width per px of font size.
    cell: f64,
    /// The cursor, a place in `ROWS`.
    at: usize,
    state: State,
    message: String,
    /// The settings and room last rendered, their drawing unless it failed, and what the last draw left on screen.
    tried: String,
    preview: Option<RgbImage>,
    shown: String,
}

fn cut(text: &str, cells: usize) -> String {
    text.chars().take(cells).collect()
}

fn dim(text: &str) -> String {
    format!(" {DIM}{text}{OFF}")
}

/// A row with its mark, cut and padded to `cells`: in reverse video and marked `>` under the cursor.
fn entry(at: bool, text: &str, cells: usize) -> String {
    let row = cut(&format!(" {} {text}", if at { '>' } else { ' ' }), cells);
    let (on, off) = if at { (REVERSE, OFF) } else { ("", "") };
    format!("{on}{row:<cells$}{off}")
}

/// A theme's colours as a strip of blocks.
fn strip(colours: impl Iterator<Item = [u8; 3]>) -> String {
    let blocks = colours.map(|[r, g, b]| format!("\x1b[38;2;{r};{g};{b}m█"));
    blocks.collect::<String>() + "\x1b[39m"
}

/// To two decimals as Python's `round(x, 2)`: by the decimal expansion, not by x * 100 (2.675 gives 2.67); -0 as 0.
fn round2(x: f64) -> f64 {
    format!("{x:.2}").parse().unwrap_or(x) + 0.0
}

/// `lines` down the menu column from the top row, each padded to its width, the rows under them blank: every row is written in full, so what was there goes.
fn column(lines: &[String], rows: usize) -> Vec<u8> {
    let mut out = String::new();
    for row in 0..rows {
        let line = lines.get(row).map_or("", String::as_str);
        // a colour code takes no cell
        let codes = line.split('\x1b').skip(1).filter_map(|code| code.find('m'));
        let cells = line.chars().count() - codes.map(|end| end + 2).sum::<usize>();
        let pad = " ".repeat(WIDTH.saturating_sub(cells));
        out += &format!("\x1b[{};1H{line}{pad}", row + 1);
    }
    out.into_bytes()
}

/// The frame right of the menu column, `wide` cells inside, over all `rows`, `title` centred in its top edge.
fn frame(title: &str, wide: usize, rows: usize) -> Vec<u8> {
    let (left, right) = (WIDTH + 1, WIDTH + 2 + wide);
    let pad = wide.saturating_sub(title.chars().count());
    // an odd remainder goes left when the width is odd too, as Python's str.center puts it
    let before = pad / 2 + (pad & wide & 1);
    let top = format!("{}{title}{}", "─".repeat(before), "─".repeat(pad - before));
    let mut out = format!("\x1b[1;{left}H{DIM}┌{}┐{OFF}", cut(&top, wide));
    for row in 2..rows {
        out += &format!("\x1b[{row};{left}H{DIM}│\x1b[{row};{right}H│{OFF}");
    }
    out += &format!("\x1b[{rows};{left}H{DIM}└{}┘{OFF}", "─".repeat(wide));
    out.into_bytes()
}

impl Page {
    /// The page for `picture` (the still as turned, or a video's frame), to be exported as `out`, opening on the look of `options`.
    pub fn new(picture: RgbImage, out: PathBuf, options: Options) -> Result<Page> {
        let current = strip(Theme::current()?.colors.into_iter());
        let mut themes = vec![("current".to_string(), current)];
        for (name, _, text) in theme::collection().into_iter().filter(|theme| theme.1) {
            // the colours its own text sets
            let found = theme::parse(&text);
            let set = |slot: u8| found.iter().find(|(key, _)| *key == format!("color{slot}"));
            let colours = (0..16).filter_map(set).map(|(_, colour)| *colour);
            themes.push((name, strip(colours)));
        }
        let look = options.look;
        Ok(Page {
            picture,
            out,
            options: Options {
                look,
                ..Options::default()
            },
            density: SIZE,
            size: 0,
            custom: None,
            screen: OnceCell::new(),
            ratio: 0,
            theme: 0,
            themes,
            cell: Font::open(font::path(false)?)?.cell(100.0).w / 100.0,
            at: 0,
            state: State::Menu,
            message: String::new(),
            tried: String::new(),
            preview: None,
            shown: String::new(),
        })
    }

    fn extent(&self) -> (f64, f64) {
        (self.picture.width() as f64, self.picture.height() as f64)
    }

    fn name(&self) -> String {
        let name = self.out.file_name().unwrap_or_default();
        name.to_string_lossy().into_owned()
    }

    /// The output size in px: the custom one, the screen's, or the preset's long side in the cropped picture's shape, even numbers.
    fn target(&self) -> (u32, u32) {
        if let Some(custom) = self.custom {
            return custom;
        }
        let mut long = SIZES[self.size].1;
        if long == 0 {
            match self.screen.get_or_init(desktop::screen_size) {
                Some(screen) => return *screen,
                None => long = SIZES[0].1,
            }
        }
        let [left, top, right, bottom] =
            convert::crop_rect(self.picture.dimensions(), self.options.crop);
        let (w, h) = convert::web_size(((right - left) as u32, (bottom - top) as u32));
        let side = |px: u32| ((px * long) as f64 / 1920.0 / 2.0).round_ties_even() as u32 * 2;
        (side(w), side(h))
    }

    /// The columns the density gives across the output.
    fn columns(&self) -> u32 {
        let columns = self.target().0 as f64 / (self.density * self.cell);
        (columns.round_ties_even() as u32).clamp(COLUMNS.0, COLUMNS.1)
    }

    /// The density that gives `columns` across the output, outside the Density row's range too.
    fn set_columns(&mut self, columns: i64) {
        let columns = columns.clamp(COLUMNS.0 as i64, COLUMNS.1 as i64);
        self.density = self.target().0 as f64 / (columns as f64 * self.cell);
    }

    fn set_density(&mut self, px: f64) {
        self.density = px.clamp(DENSITY.0, DENSITY.1);
    }

    /// The settings as converter options, drawn in the theme at `theme` of the list.
    fn settings(&self, theme: usize) -> Options {
        let (w, h) = self.target();
        Options {
            theme: (theme > 0).then(|| self.themes[theme].0.clone()),
            columns: Some(self.columns()),
            width: Some(w),
            height: Some(h),
            ..self.options.clone()
        }
    }

    /// A row that holds a whole number: its value, the step of `h` `l`, the least, the most, the default.
    fn number(&self, row: Row) -> Option<[f64; 5]> {
        let (look, [x, y, w, h]) = (self.options.look, self.options.crop);
        let whole = |value: i32, step: f64, (low, high): (i32, i32), default: f64| {
            [value as f64, step, low as f64, high as f64, default]
        };
        Some(match row {
            Row::Level => whole(look.level, 5.0, look::LEVEL, 0.0),
            Row::Contrast => whole(look.contrast, 5.0, look::CONTRAST, 0.0),
            Row::Saturation => whole(look.saturation, 10.0, look::SATURATION, 100.0),
            Row::CropW => [w, 5.0, 5.0, 400.0, 100.0],
            Row::CropH => [h, 5.0, 5.0, 400.0, 100.0],
            Row::CropX => [x, 5.0, -300.0, 300.0, 0.0],
            Row::CropY => [y, 5.0, -300.0, 300.0, 0.0],
            _ => return None,
        })
    }

    fn set_number(&mut self, row: Row, value: f64) {
        let (look, crop) = (&mut self.options.look, &mut self.options.crop);
        match row {
            Row::Level => look.level = value as i32,
            Row::Contrast => look.contrast = value as i32,
            Row::Saturation => look.saturation = value as i32,
            Row::CropX => crop[0] = value,
            Row::CropY => crop[1] = value,
            Row::CropW => crop[2] = value,
            Row::CropH => crop[3] = value,
            _ => {}
        }
    }

    /// `h` `l` on the menu: a step of the row's value in place.
    fn nudge(&mut self, row: Row, by: i32) {
        if let Some([value, step, low, high, _]) = self.number(row) {
            return self.set_number(row, round2(value + step * by as f64).clamp(low, high));
        }
        let look = &mut self.options.look;
        let around = |at: usize, len: usize| (at as i32 + by).rem_euclid(len as i32) as usize;
        match row {
            Row::Skin if look.skin == Skin::Grey => look.skin = Skin::Theme,
            Row::Skin => look.skin = Skin::Grey,
            Row::Fit if self.options.fit == Fit::Cover => self.options.fit = Fit::Contain,
            Row::Fit => self.options.fit = Fit::Cover,
            Row::Glyphs => look.glyphs = look.glyphs.step(by),
            Row::Bold => look.bold = !look.bold,
            Row::Mono => look.mono = !look.mono,
            Row::Density => self.set_density((self.density + by as f64).round_ties_even()),
            Row::Columns => self.set_columns(self.columns() as i64 + 10 * by as i64),
            Row::Size => (self.size, self.custom) = (around(self.size, SIZES.len()), None),
            Row::Theme => self.theme = around(self.theme, self.themes.len()),
            _ => {}
        }
    }

    /// `` ` ``: the row's default.
    fn reset(&mut self, row: Row) {
        if let Some([.., default]) = self.number(row) {
            return self.set_number(row, default);
        }
        let look = &mut self.options.look;
        match row {
            Row::Skin => look.skin = Skin::Grey,
            Row::Glyphs => look.glyphs = Glyphs::Mixed,
            Row::Bold => look.bold = false,
            Row::Mono => look.mono = false,
            Row::Fit => self.options.fit = Fit::Contain,
            Row::Density | Row::Columns => self.density = SIZE,
            Row::Crop => (self.options.crop, self.ratio) = (FULL, 0),
            Row::Size => (self.size, self.custom) = (0, None),
            Row::Theme => self.theme = 0,
            _ => {}
        }
    }

    /// A typed value for `row`, taken when it parses: whole numbers and the density held to their range, a size refused outside its own and made even.
    fn commit(&mut self, row: Row, text: &str) -> Option<()> {
        let text = text.trim();
        match row {
            Row::Density => self.set_density(text.parse().ok().filter(|px: &f64| !px.is_nan())?),
            Row::Columns => self.set_columns(text.parse().ok()?),
            Row::Size => {
                let side = |px: &str| px.parse().ok().filter(|px| (PX.0..=PX.1).contains(px));
                let sides = text.to_lowercase().replace(' ', "");
                let (w, h) = sides.split_once('x')?;
                let (w, h): (u32, u32) = (side(w)?, side(h)?);
                self.custom = Some((w + w % 2, h + h % 2));
            }
            _ => {
                let [_, _, low, high, _] = self.number(row)?;
                self.set_number(row, (text.parse::<i64>().ok()? as f64).clamp(low, high));
            }
        }
        Some(())
    }

    /// The picture as the converter draws it for the settings, in the theme at `theme` of the list, `scale` times the size: the full size's cells and layout, so a preview is the file's own drawing.
    fn drawing(&self, theme: usize, scale: f64) -> Result<RgbImage> {
        let options = self.settings(theme);
        let source = convert::crop(&self.picture, options.crop)?;
        let mut style = Style::new(&options, convert::colours(&options)?, source.dimensions())?;
        let grid = style.art.cells(&source)?;
        style.layout(grid.rows as u32);
        if scale != 1.0 {
            style = style.scaled(scale)?;
        }
        Ok(style.paint(&grid, false))
    }

    /// `s`: the drawing at full size from the settings as they are, written as PNG over the output path.
    fn export(&mut self) {
        let save = || -> Result<()> {
            let png = picture::png(&self.drawing(self.theme, 1.0)?)?;
            Ok(std::fs::write(&self.out, png)?)
        };
        self.message = match save() {
            Ok(()) => format!("exported {}", self.name()),
            Err(e) => format!("not exported: {}", e.0),
        };
    }

    /// The preview for the settings and the theme under the cursor in `room` px, drawn when any of them changed since the last render; a failure is the message, and nothing shows.
    fn render(&mut self, room: (u32, u32)) {
        let theme = match &self.state {
            State::List(list) => list.theme,
            _ => self.theme,
        };
        let ((w, h), key) = (
            self.target(),
            format!("{:?} {room:?}", self.settings(theme)),
        );
        if key == self.tried {
            return;
        }
        // never enlarged
        let fit = |room: u32, px: u32| (room as f64 / px as f64).min(1.0);
        let drawn = self.drawing(theme, fit(room.0, w).min(fit(room.1, h)));
        if let Err(e) = &drawn {
            self.message = format!("render failed: {}", e.0);
        }
        (self.tried, self.preview) = (key, drawn.ok());
    }

    /// A key for the menu, or for the prompt, list or crop editor that is open; the message goes with any.
    pub fn key(&mut self, key: &[u8]) -> After {
        self.message.clear();
        self.state = match std::mem::replace(&mut self.state, State::Menu) {
            State::Menu => return self.menu_key(key),
            State::Prompt(row, text) => self.prompt_key(row, text, key),
            State::List(list) => self.list_key(list, key),
            State::Crop => self.crop_key(key),
        };
        After::Stay
    }

    fn menu_key(&mut self, key: &[u8]) -> After {
        let (row, key): (Row, &[u8]) = (ROWS[self.at].0, if key == b"\r" { b"\n" } else { key });
        match (key, row) {
            (b"q" | b"Q" | b"r" | b"\x1b", _) | (b"\n", Row::Back) => return After::Close,
            (b"j" | b"\x1b[B", _) => self.at = (self.at + 1) % ROWS.len(),
            (b"k" | b"\x1b[A", _) => self.at = (self.at + ROWS.len() - 1) % ROWS.len(),
            (b"h" | b"\x1b[D", _) => self.nudge(row, -1),
            (b"l" | b"\x1b[C", _) => self.nudge(row, 1),
            (b"`", _) => self.reset(row),
            (b"s", _) | (b"\n", Row::Export) => self.export(),
            (b"/", _) => self.state = self.list(true, true),
            (b"\n", Row::Size | Row::Theme) => self.state = self.list(row == Row::Theme, false),
            (b"\n", Row::Crop) => self.state = State::Crop,
            (b"\n", Row::Bold | Row::Mono | Row::Skin | Row::Fit | Row::Glyphs) => {
                self.nudge(row, 1)
            }
            (b"\n", _) => self.state = State::Prompt(row, String::new()),
            _ => {}
        }
        After::Stay
    }

    fn prompt_key(&mut self, row: Row, mut text: String, key: &[u8]) -> State {
        match key {
            b"\x1b" => return State::Menu,
            b"\n" | b"\r" => {
                if self.commit(row, &text).is_none() {
                    self.message = format!("not a value: {text}");
                }
                return State::Menu;
            }
            b"\x7f" | b"\x08" => _ = text.pop(),
            &[byte @ 32..=126] => text.push(byte as char),
            _ => {}
        }
        State::Prompt(row, text)
    }

    /// The size list or the theme list, the cursor on what is chosen.
    fn list(&self, themes: bool, searching: bool) -> State {
        State::List(List {
            themes,
            at: if themes { self.theme } else { self.size },
            query: String::new(),
            searching,
            theme: self.theme,
        })
    }

    /// The open list's rows, as places in its choices: the sizes and `custom WxH`, or the themes a search leaves.
    fn choices(&self, list: &List) -> Vec<usize> {
        let query = list.query.to_lowercase();
        let named = |at: &usize| self.themes[*at].0.to_lowercase().contains(&query);
        match list.themes {
            true => (0..self.themes.len()).filter(named).collect(),
            false => (0..=SIZES.len()).collect(),
        }
    }

    fn list_key(&mut self, mut list: List, key: &[u8]) -> State {
        let moves = matches!(key, b"\x1b[B" | b"\x1b[A" | b"\t");
        if list.searching {
            match key {
                b"\x7f" | b"\x08" => _ = list.query.pop(),
                &[byte @ 32..=126] => list.query.push(byte as char),
                b"\n" | b"\r" | b"\x1b" => list.searching = false,
                _ if moves => list.searching = false,
                _ => {}
            }
            // a search that changed starts from its first row; the cursor stays on a row it leaves, and the preview follows it
            if matches!(key, b"\x7f" | b"\x08" | [32..=126]) {
                list.at = 0;
            }
            let found = self.choices(&list);
            list.at = list.at.min(found.len().saturating_sub(1));
            list.theme = found.get(list.at).copied().unwrap_or(list.theme);
            if list.searching || !moves {
                return State::List(list);
            }
        }
        let (found, up) = (self.choices(&list), matches!(key, b"k" | b"\x1b[A"));
        match key {
            b"q" | b"\x1b" | b"h" | b"\x1b[D" => return State::Menu,
            b"/" if list.themes => list.searching = true,
            b"j" | b"\x1b[B" | b"\t" | b"k" | b"\x1b[A" if !found.is_empty() => {
                list.at = (list.at + if up { found.len() - 1 } else { 1 }) % found.len();
                if list.themes {
                    list.theme = found[list.at];
                }
            }
            b"\n" | b"\r" | b"l" | b"\x1b[C" if !found.is_empty() => {
                match found[list.at] {
                    choice if list.themes => self.theme = choice,
                    choice if choice < SIZES.len() => (self.size, self.custom) = (choice, None),
                    _ => return State::Prompt(Row::Size, String::new()),
                }
                return State::Menu;
            }
            _ => {}
        }
        State::List(list)
    }

    /// The crop window in the picture's px (x, y, w, h), maybe past its borders.
    fn crop_px(&self) -> [f64; 4] {
        let ((w, h), [x, y, cw, ch]) = (self.extent(), self.options.crop);
        [w * x / 100.0, h * y / 100.0, w * cw / 100.0, h * ch / 100.0]
    }

    /// The crop window reshaped to the chosen ratio about its centre: the largest window of that shape inside it.
    fn shaped(&self) -> [f64; 4] {
        let ([x, y, w, h], (pw, ph)) = (self.crop_px(), self.extent());
        let ratio = match self.ratio {
            0 => return [x, y, w, h],
            1 => pw / ph,
            at => RATIOS[at].1,
        };
        let (cx, cy) = (x + w / 2.0, y + h / 2.0);
        let wide = w / h > ratio;
        let (w, h) = if wide { (h * ratio, h) } else { (w, w / ratio) };
        [cx - w / 2.0, cy - h / 2.0, w, h]
    }

    fn crop_key(&mut self, key: &[u8]) -> State {
        let ([x, y, w, h], (pw, ph), free) = (self.crop_px(), self.extent(), self.ratio == 0);
        // a twentieth of the picture, the capitals a hundredth; `-` `<` `{` shrink, their partners grow by as much
        let (fine, shrink) = (
            matches!(key, b"H" | b"J" | b"K" | b"L"),
            matches!(key, b"-" | b"<" | b"{"),
        );
        let (part, f) = (
            if fine { 0.01 } else { 0.05 },
            if shrink { 0.95 } else { 1.0 / 0.95 },
        );
        let (dx, dy) = (w * (1.0 - f) / 2.0, h * (1.0 - f) / 2.0);
        let window = match key {
            b"\n" | b"\r" | b"q" | b"\x1b" => return State::Menu,
            b"h" | b"H" | b"\x1b[D" => [x - part * pw, y, w, h],
            b"l" | b"L" | b"\x1b[C" => [x + part * pw, y, w, h],
            b"k" | b"K" | b"\x1b[A" => [x, y - part * ph, w, h],
            b"j" | b"J" | b"\x1b[B" => [x, y + part * ph, w, h],
            b"-" | b"=" | b"+" => [x + dx, y + dy, w * f, h * f],
            b"<" | b">" if free => [x + dx, y, w * f, h],
            b"{" | b"}" if free => [x, y + dy, w, h * f],
            b"c" => [(pw - w) / 2.0, (ph - h) / 2.0, w, h],
            b"[" | b"]" => {
                let by = if key == b"]" { 1 } else { RATIOS.len() - 1 };
                self.ratio = (self.ratio + by) % RATIOS.len();
                self.shaped()
            }
            b"0" => {
                self.ratio = 1;
                [0.0, 0.0, pw, ph]
            }
            _ => return State::Crop,
        };
        // at least 2 % of the picture a side, rounded to 0.01 %
        let [x, y, w, h] = window;
        let sides = [x / pw, y / ph, w.max(pw * 0.02) / pw, h.max(ph * 0.02) / ph];
        self.options.crop = sides.map(|part| round2(part * 100.0));
        State::Crop
    }

    /// The crop editor's picture for `room` px: the source dimmed outside the crop window, the window's thirds and its edge on top.
    /// The view takes in the picture with a quarter of its size free on every side, and the window wherever it is: it can be moved past the borders.
    fn crop_canvas(&self, room: (u32, u32)) -> RgbImage {
        let ([x, y, w, h], (pw, ph)) = (self.crop_px(), self.extent());
        let (left, right) = (
            (-pw * 0.25).min(x - pw * 0.05),
            (pw * 1.25).max(x + w + pw * 0.05),
        );
        let (top, bottom) = (
            (-ph * 0.25).min(y - ph * 0.05),
            (ph * 1.25).max(y + h + ph * 0.05),
        );
        let scale = (room.0 as f64 / (right - left)).min(room.1 as f64 / (bottom - top));
        let px = |at: f64| (at * scale).round_ties_even() as i64;
        let size = |px: i64| px.max(1) as u32;
        let source = picture::resize_box(&self.picture, size(px(pw)), size(px(ph)));
        let [x0, y0, x1, y1] = [px(x - left), px(y - top), px(x + w - left), px(y + h - top)];
        let (at_x, at_y) = (px(-left), px(-top));
        let third = |from: i64, to: i64, at: i64| (1..=2).any(|n| at == from + (to - from) * n / 3);
        RgbImage::from_fn(size(px(right - left)), size(px(bottom - top)), |cx, cy| {
            let (cx, cy) = (cx as i64, cy as i64);
            // how far inside the window, its far edge included
            let inside = (cx - x0).min(x1 - cx).min(cy - y0).min(y1 - cy);
            match source.get_pixel_checked((cx - at_x) as u32, (cy - at_y) as u32) {
                // the edge, 2 px, over the thirds
                _ if (0..2).contains(&inside) => Rgb([248, 248, 242]),
                _ if inside >= 0 && (third(x0, x1, cx) || third(y0, y1, cy)) => Rgb([150; 3]),
                Some(pixel) if inside >= 0 && cx < x1 && cy < y1 => *pixel,
                Some(pixel) => Rgb(pixel.0.map(|c| (c as u16 * 2 / 5) as u8)),
                None => Rgb([24; 3]),
            }
        })
    }

    /// A row's value as the menu shows it.
    fn value(&self, row: Row) -> String {
        let (look, (w, h)) = (self.options.look, self.target());
        let on = |on: bool| if on { "on" } else { "off" };
        let percent = |[value, ..]: [f64; 5]| format!("{value}%");
        match row {
            Row::Level => look::level_label(look.level),
            Row::Contrast => look::contrast_label(look.contrast),
            Row::Skin if look.skin == Skin::Grey => "grey (calm)".into(),
            Row::Skin => "theme colour".into(),
            Row::Glyphs if look.glyphs == Glyphs::Mixed => "mixed (shapes, dithered flats)".into(),
            Row::Glyphs if look.glyphs == Glyphs::Density => "density (dithered)".into(),
            Row::Glyphs => "shape (chafa)".into(),
            Row::Density => format!("{:.1} px", self.density),
            Row::Columns => self.columns().to_string(),
            Row::Fit if self.options.fit == Fit::Cover => "cover (edges cut)".into(),
            Row::Fit => "contain (margins)".into(),
            Row::Crop => format!("{}, editor  (enter)", RATIOS[self.ratio].0),
            Row::Size if self.custom.is_some() => format!("{w}x{h}"),
            Row::Size => format!("{w}x{h}  ({})", SIZES[self.size].0),
            Row::Theme => self.themes[self.theme].0.clone(),
            Row::Bold => on(look.bold).into(),
            Row::Mono => on(look.mono).into(),
            // saturation and the crop's parts in percent; nothing for Export and Back
            _ => self.number(row).map(percent).unwrap_or_default(),
        }
    }

    /// The menu column's lines for the state: the menu, a list or the crop editor, then the prompt and the message.
    fn lines(&self, rows: usize) -> Vec<String> {
        let mut lines = match &self.state {
            State::List(list) => self.list_lines(list, rows),
            State::Crop => self.crop_lines(),
            _ => self.menu_lines(),
        };
        let mut last = Vec::new();
        if let State::Prompt(row, text) = &self.state {
            let (_, label, hint) = ROWS[*row as usize];
            last.push(format!(" {label} ({hint}): {text}_"));
        }
        if !self.message.is_empty() {
            last.push(format!(" {}", self.message));
        }
        // each under a blank line; a window too short for that gives them its last rows instead of losing them
        let short = lines.len() + 2 * last.len() > rows;
        if short {
            lines.resize(rows.saturating_sub(last.len()), String::new());
        }
        for line in last {
            lines.extend((!short).then(String::new));
            lines.push(cut(&line, WIDTH - 1));
        }
        lines
    }

    fn menu_lines(&self) -> Vec<String> {
        let name = dim(&cut(&self.name(), WIDTH - 2));
        let mut lines = vec![" Render".into(), name, String::new()];
        for (at, (row, label, _)) in ROWS.iter().enumerate() {
            if *row == Row::Export {
                lines.extend([String::new(), dim(&"─".repeat(WIDTH - 3))]);
            }
            let text = format!("{label:<10} {}", self.value(*row));
            lines.push(entry(at == self.at, &text, WIDTH - 1));
        }
        lines.push(String::new());
        lines.push(dim("j k move   h l nudge   enter set   ` default"));
        lines.push(dim("s export   / theme search   q back"));
        lines
    }

    fn list_lines(&self, list: &List, rows: usize) -> Vec<String> {
        let found = self.choices(list);
        // the cursor near the middle of the rows shown
        let room = rows.saturating_sub(8).max(3);
        let first = (list.at.saturating_sub(room / 2)).min(found.len().saturating_sub(room));
        let title = [" Size", " Theme  (dark, kitty's)"][list.themes as usize];
        let mut lines = vec![title.to_string(), String::new()];
        for (at, &choice) in found.iter().enumerate().skip(first).take(room) {
            let size = SIZES.get(choice).map_or("custom WxH", |size| size.0);
            lines.push(match self.themes.get(choice).filter(|_| list.themes) {
                // reverse video on the name only: it would hide the strip's blocks
                Some((name, strip)) => {
                    format!("{} {strip}", entry(at == list.at, name, WIDTH - 19))
                }
                None => entry(at == list.at, size, WIDTH - 1),
            });
        }
        lines.push(String::new());
        if !list.themes {
            lines.push(dim("j k move   enter pick   esc back"));
        } else if list.searching {
            lines.push(format!(" /{}_", cut(&list.query, WIDTH - 4)));
        } else {
            lines.push(dim("/ search   j k move   enter pick   esc back"));
        }
        lines.extend(list.themes.then(|| dim(&format!("{} themes", found.len()))));
        lines
    }

    fn crop_lines(&self) -> Vec<String> {
        let (crop, px) = (self.options.crop, self.crop_px());
        let part = |name: &str, at: usize| {
            let px = px[at].round_ties_even() as i64;
            format!("   {name:<10} {}%   ({px} px)", crop[at])
        };
        let mut lines = vec![" Crop".to_string()];
        lines.push(dim("the source; the bright frame is the crop"));
        lines.push(String::new());
        lines.push(format!("   Ratio      {}", RATIOS[self.ratio].0));
        lines.extend([part("X", 0), part("Y", 1), part("W", 2), part("H", 3)]);
        lines.push(String::new());
        lines.push(dim("h j k l    move (H J K L finer)"));
        lines.push(dim("- =        smaller, larger"));
        lines.push(dim("[ ]        ratio"));
        lines.push(dim("< > { }    width, height (free)"));
        lines.push(dim("c centre   0 whole picture"));
        lines.push(dim("enter q    back to the menu"));
        lines
    }

    /// The whole screen: menu, list, prompt or crop editor on the left, the preview in its frame on the right; `epoch` changes whenever what was on screen was covered or the window changed (the preview is then sent again).
    pub fn draw(&mut self, geometry: Geometry, epoch: u64, transfer: &mut Transfer) -> Vec<u8> {
        let (cols, rows) = (geometry.cols as usize, geometry.rows as usize);
        // the frame: every row, from the menu column to the last column but one; the picture's room lies inside it
        let (wide, high) = (cols.saturating_sub(WIDTH + 3), rows.saturating_sub(2));
        let px = |cells: usize, cell: f64| (cells as f64 * cell) as u32;
        let room = (px(wide, geometry.cell_w), px(high, geometry.cell_h));
        let framed = wide > 8 && room.0 > 0 && room.1 > 0;
        let cropping = matches!(self.state, State::Crop);
        if framed && !cropping {
            self.render(room);
        }
        // what the frame holds, by a name: the crop editor's picture, or the preview
        let held = match framed {
            true if cropping => Some(format!("{:?}", self.options.crop)),
            true => self.preview.as_ref().map(|_| self.tried.clone()),
            false => None,
        };
        let now = format!("{geometry:?} {epoch} {held:?}");
        let fresh = now != self.shown;
        let mut out = b"\x1b[?2026h".to_vec();
        if fresh {
            out.extend([kitty::CLEAR, b"\x1b[2J\x1b[0m"].concat());
        }
        out.extend(column(&self.lines(rows), rows));
        if held.is_some() {
            let ([.., w, h], (width, height)) =
                (self.crop_px().map(f64::round_ties_even), self.target());
            let title = match cropping {
                true => format!(" source · crop {w}x{h} "),
                false => format!(" {width}x{height} · {} columns ", self.columns()),
            };
            // the preview's frame at every draw, the crop editor's with its picture
            if fresh || !cropping {
                out.extend(frame(&title, wide, rows));
            }
            let canvas = (fresh && cropping).then(|| self.crop_canvas(room));
            if let Some(image) = canvas.as_ref().or(self.preview.as_ref()).filter(|_| fresh) {
                let ((w, h), col) = (image.dimensions(), WIDTH as u32 + 2);
                let (wide, high) = (wide as u32, high as u32);
                out.extend(transfer.place(w, h, image, 24, col, 2, wide, high, geometry));
            }
        }
        out.extend(b"\x1b[?2026l");
        self.shown = now;
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::look::Look;
    use crate::mpv::tests::{Scratch, Var, env};
    use std::io::Write;
    use std::sync::MutexGuard;

    const THEME: &str = include_str!("../tests/bl1nk.conf");
    /// kitty's theme collection as its cache holds it: the dark themes Night, Stored and Twin (which sets color2 alone) among light ones.
    const COLLECTION: &[u8] = include_bytes!("../tests/golden/theme-collection.zip");
    const ENTER: &[u8] = b"\r";
    const ESC: &[u8] = b"\x1b";
    const UP: &[u8] = b"\x1b[A";
    const DOWN: &[u8] = b"\x1b[B";
    const RIGHT: &[u8] = b"\x1b[C";
    const LEFT: &[u8] = b"\x1b[D";
    const BACKSPACE: &[u8] = b"\x7f";
    /// How a draw from a cleared screen starts: the update opens, kitty's images go, the screen is erased.
    const CLEARED: &[u8] = b"\x1b[?2026h\x1b_Ga=d,d=A,q=2\x1b\\\x1b[2J\x1b[0m";
    /// A picture's command for a room of 459 x 560 px (51 x 28 cells) that it fills across.
    const PLACED: &str = "a=T,f=24,t=t,s=459,v=258,c=51,r=13,C=1,z=-1,q=2";
    /// The rows from Level to Mono.
    const SETTINGS: usize = 17;

    // The screens: RenderPage.draw and draw_crop of bin/ascii-viewer:1096, :1169 at tag python-final in a window of 100 x 30 on a 1600 x 900 picture, run by python3; each row without its padding to the column's width.
    const MENU: [&str; 27] = [
        " Render",
        " \x1b[90mpic-ascii.png\x1b[0m",
        "",
        "\x1b[7m > Level      0                              \x1b[0m",
        "   Contrast   0",
        "   Saturation 100%",
        "   Skin       grey (calm)",
        "   Glyphs     mixed (shapes, dithered flats)",
        "   Density    20.0 px",
        "   Columns    160",
        "   Fit        contain (margins)",
        "   Crop       free, editor  (enter)",
        "   Crop W     100%",
        "   Crop H     100%",
        "   Crop X     0%",
        "   Crop Y     0%",
        "   Size       1920x1080  (1920 long side)",
        "   Theme      current",
        "   Bold       off",
        "   Mono       off",
        "",
        " \x1b[90m───────────────────────────────────────────\x1b[0m",
        "   Export",
        "   Back",
        "",
        " \x1b[90mj k move   h l nudge   enter set   ` default\x1b[0m",
        " \x1b[90ms export   / theme search   q back\x1b[0m",
    ];
    const SIZES_LIST: [&str; 9] = [
        " Size",
        "",
        "   1920 long side",
        "\x1b[7m > 1080 long side                            \x1b[0m",
        "   3840 long side",
        "   screen",
        "   custom WxH",
        "",
        " \x1b[90mj k move   enter pick   esc back\x1b[0m",
    ];
    /// The crop 12.5, -3.33, 47.37, 100 at the ratio 5:4.
    const CROP: [&str; 15] = [
        " Crop",
        " \x1b[90mthe source; the bright frame is the crop\x1b[0m",
        "",
        "   Ratio      5:4",
        "   X          12.5%   (200 px)",
        "   Y          -3.33%   (-30 px)",
        "   W          47.37%   (758 px)",
        "   H          100%   (900 px)",
        "",
        " \x1b[90mh j k l    move (H J K L finer)\x1b[0m",
        " \x1b[90m- =        smaller, larger\x1b[0m",
        " \x1b[90m[ ]        ratio\x1b[0m",
        " \x1b[90m< > { }    width, height (free)\x1b[0m",
        " \x1b[90mc centre   0 whole picture\x1b[0m",
        " \x1b[90menter q    back to the menu\x1b[0m",
    ];
    /// Each row's prompt with `12` typed.
    const PROMPTS: [(Row, &str); 10] = [
        (Row::Level, " Level (-95..95, up darker): 12_"),
        (Row::Contrast, " Contrast (-95..95): 12_"),
        (Row::Saturation, " Saturation (percent, 0..300): 12_"),
        (Row::Density, " Density (px 4..120): 12_"),
        (Row::Columns, " Columns (8..2000): 12_"),
        (Row::CropW, " Crop W (% of picture width): 12_"),
        (Row::CropH, " Crop H (% of picture height): 12_"),
        (Row::CropX, " Crop X (% from the left edge): 12_"),
        (Row::CropY, " Crop Y (% from the top edge): 12_"),
        (Row::Size, " Size (WxH, each 16..16384): 12_"),
    ];
    const THEMES_KEYS: &str = " \x1b[90m/ search   j k move   enter pick   esc back\x1b[0m";

    /// A page in a home of its own, which holds the kitty theme and kitty's theme collection; the screen's size is unknown. The environment is held while it lives.
    struct Stage {
        page: Page,
        transfer: Transfer,
        home: Scratch,
        _home: Var,
        _env: MutexGuard<'static, ()>,
    }

    /// Red grows to the right, green downwards; the bottom eighth is pure green, which bl1nk draws in color2.
    fn gradient(w: u32, h: u32) -> RgbImage {
        RgbImage::from_fn(w, h, |x, y| match y * 8 >= h * 7 {
            true => Rgb([0, 255, 0]),
            false => Rgb([(x * 255 / w) as u8, (y * 255 / h) as u8, 96]),
        })
    }

    /// A window of cells of 9 x 20 px.
    fn window(cols: u32, rows: u32) -> Geometry {
        let (cell_w, cell_h) = (9.0, 20.0);
        Geometry {
            cols,
            rows,
            cell_w,
            cell_h,
        }
    }

    impl Stage {
        /// On a picture of 1600 x 900 px.
        fn new() -> Stage {
            Stage::with(gradient(1600, 900))
        }

        /// The same shape at a fifth of the size: quicker to draw.
        fn small() -> Stage {
            Stage::with(gradient(320, 180))
        }

        fn with(picture: RgbImage) -> Stage {
            let _env = env();
            let home = Scratch::new();
            let files = [
                (".config/kitty/current-theme.conf", THEME.as_bytes()),
                (".cache/kitty/kitty-themes.zip", COLLECTION),
            ];
            for (file, data) in files {
                let file = home.0.join(file);
                std::fs::create_dir_all(file.parent().unwrap()).unwrap();
                std::fs::write(file, data).unwrap();
            }
            let _home = Var::set("HOME", &home.0);
            let out = home.0.join("pic-ascii.png");
            let mut page = Page::new(picture, out, Options::default()).unwrap();
            // no test asks Hyprland
            page.screen = OnceCell::from(None);
            let transfer = Transfer::default();
            Stage {
                page,
                transfer,
                home,
                _home,
                _env,
            }
        }

        fn theme_file(&self) -> PathBuf {
            self.home.0.join(".config/kitty/current-theme.conf")
        }

        /// The menu with the cursor on `row`.
        fn on(&mut self, row: Row) -> &mut Stage {
            (self.page.at, self.page.state) = (row as usize, State::Menu);
            self
        }

        /// Press `keys`; what the last one asks of the viewer.
        fn keys(&mut self, keys: &[&[u8]]) -> After {
            keys.iter().fold(After::Stay, |_, key| self.page.key(key))
        }

        /// Type `text` and press Enter.
        fn typed(&mut self, text: &str) {
            for byte in text.bytes() {
                self.page.key(&[byte]);
            }
            self.page.key(ENTER);
        }

        fn draw(&mut self, cols: u32, rows: u32) -> Vec<u8> {
            self.page.draw(window(cols, rows), 0, &mut self.transfer)
        }

        /// The menu column as drawn in a window of `rows` rows too narrow for the frame: no picture is drawn for it.
        fn column(&mut self, rows: u32) -> Vec<String> {
            column(&self.draw(50, rows), rows)
        }

        /// Everything the rows set.
        fn set(&self) -> (Options, f64, usize, Option<(u32, u32)>, usize, usize) {
            let (page, options) = (&self.page, self.page.options.clone());
            let sizes = (page.density, page.size, page.custom);
            (options, sizes.0, sizes.1, sizes.2, page.ratio, page.theme)
        }

        fn crop(&self) -> [f64; 4] {
            self.page.options.crop
        }

        fn list(&self) -> &List {
            match &self.page.state {
                State::List(list) => list,
                _ => panic!("no list is open"),
            }
        }

        fn prompt(&self) -> Option<(Row, &str)> {
            match &self.page.state {
                State::Prompt(row, text) => Some((*row, text)),
                _ => None,
            }
        }

        fn preview(&self) -> &RgbImage {
            self.page.preview.as_ref().expect("a preview")
        }

        fn saved(&self) -> RgbImage {
            image::open(&self.page.out).unwrap().into_rgb8()
        }
    }

    /// What a draw wrote at each cursor position: (row, column, text up to the next position).
    fn writes(out: &[u8]) -> Vec<(usize, usize, String)> {
        let text = std::str::from_utf8(out).unwrap();
        let text = text.strip_prefix("\x1b[?2026h").expect("an update");
        let text = text.strip_suffix("\x1b[?2026l").expect("its end");
        let mut found: Vec<(usize, usize, String)> = Vec::new();
        for part in text.split("\x1b[").skip(1) {
            let position = part.split_once('H').and_then(|(at, text)| {
                let (row, col) = at.split_once(';')?;
                Some((row.parse().ok()?, col.parse().ok()?, text.to_string()))
            });
            match (position, found.last_mut()) {
                (Some(write), _) => found.push(write),
                (None, Some(last)) => last.2 += &format!("\x1b[{part}"),
                (None, None) => {}
            }
        }
        found
    }

    /// `text` without its colour codes.
    fn plain(text: &str) -> String {
        let shown = |part: &str| {
            part.split_once('m')
                .map_or("", |(_, text)| text)
                .to_string()
        };
        let (first, coded) = text.split_once("\x1b[").unwrap_or((text, ""));
        first.to_string() + &coded.split("\x1b[").map(shown).collect::<String>()
    }

    /// The menu column of a draw in a window of `rows` rows, from the top: every row written once from column 1 and 46 cells wide; here without that padding.
    fn column(out: &[u8], rows: u32) -> Vec<String> {
        let mut written = writes(out);
        written.retain(|write| write.1 == 1);
        let places: Vec<usize> = written.iter().map(|write| write.0).collect();
        assert_eq!(places, (1..=rows as usize).collect::<Vec<_>>());
        for (row, _, text) in &written {
            assert_eq!(plain(text).chars().count(), WIDTH, "row {row}: {text:?}");
        }
        let unpadded = |write: &(usize, usize, String)| write.2.trim_end_matches(' ').to_string();
        written.iter().map(unpadded).collect()
    }

    fn unbase64(text: &str) -> String {
        const DIGITS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let six = |c: u8| DIGITS.iter().position(|digit| *digit == c);
        let (mut bytes, mut bits, mut held) = (Vec::new(), 0, 0);
        for six in text.bytes().filter_map(six) {
            (bits, held) = (bits << 6 | six, held + 6);
            if held >= 8 {
                held -= 8;
                bytes.push((bits >> held) as u8);
            }
        }
        String::from_utf8(bytes).unwrap()
    }

    /// The picture a draw sent to kitty: the cell it starts in, the command's keys, and the pixels in the file it names.
    fn placed(out: &[u8]) -> Option<((usize, usize), String, Vec<u8>)> {
        let picture = |write: &(usize, usize, String)| write.2.starts_with("\x1b_G");
        let (row, col, text) = writes(out).into_iter().find(picture)?;
        let command = text.strip_prefix("\x1b_G")?.strip_suffix("\x1b\\")?;
        let (keys, file) = command.split_once(';')?;
        let pixels = std::fs::read(unbase64(file)).unwrap();
        Some(((row, col), keys.to_string(), pixels))
    }

    fn has(out: &[u8], text: &str) -> bool {
        String::from_utf8_lossy(out).contains(text)
    }

    fn fnv(bytes: impl IntoIterator<Item = u8>) -> u64 {
        let step = |hash: u64, byte: u8| (hash ^ byte as u64).wrapping_mul(0x0100_0000_01b3);
        bytes.into_iter().fold(0xcbf2_9ce4_8422_2325, step)
    }

    /// The frame's top row in a window of 100 columns, the title between `around` dashes on each side of its 51 cells.
    fn top_edge(title: &str, around: usize) -> String {
        let dashes = "─".repeat(around);
        format!("\x1b[1;47H\x1b[90m┌{dashes}{title}{dashes}┐\x1b[0m")
    }

    #[test]
    fn opens_on_the_look_given_and_the_defaults_of_the_rest() {
        // row R3; the look: ConvertArgsTest of tests/test_viewer.py:707
        let _env = env();
        let home = Scratch::new();
        std::fs::create_dir_all(home.0.join(".config/kitty")).unwrap();
        std::fs::write(home.0.join(".config/kitty/current-theme.conf"), THEME).unwrap();
        let _home = Var::set("HOME", &home.0);
        let mut look = Look::default();
        (look.level, look.contrast, look.saturation) = (10, -5, 150);
        (look.glyphs, look.mono, look.bold) = (Glyphs::Shape, true, true);
        // of the options given the look alone counts
        let mut given = Options::default();
        (given.look, given.crop, given.fit) = (look, [1.0, 2.0, 3.0, 4.0], Fit::Cover);
        (given.columns, given.theme) = (Some(99), Some("Night".into()));
        let page = Page::new(gradient(64, 48), "pic-ascii.png".into(), given).unwrap();
        let fresh = Options {
            look,
            ..Options::default()
        };
        assert!(page.options == fresh && fresh.crop == FULL && fresh.fit == Fit::Contain);
        assert_eq!((page.density, page.size, page.custom), (20.0, 0, None));
        assert_eq!((page.ratio, page.theme, page.at, page.cell), (0, 0, 0, 0.6));
        assert!(matches!(page.state, State::Menu) && page.message.is_empty());
        // no theme collection: the current theme alone
        assert_eq!(page.themes.len(), 1);
    }

    #[test]
    fn the_themes_are_the_current_one_and_kitty_s_dark_ones() {
        // row R12
        let stage = Stage::new();
        let themes = &stage.page.themes;
        let names: Vec<&str> = themes.iter().map(|theme| theme.0.as_str()).collect();
        assert_eq!(names, ["current", "Night", "Stored", "Twin"]);
        // each with the colours its own text sets: sixteen, Twin's one
        let strip = |at: usize| themes[at].1.as_str();
        assert!(strip(0).starts_with("\x1b[38;2;26;28;29m█\x1b[38;2;255;88;148m█"));
        assert!(strip(0).ends_with("\x1b[38;2;248;248;242m█\x1b[39m"));
        assert!((0..3).all(|at| strip(at).matches('█').count() == 16));
        assert_eq!(strip(3), "\x1b[38;2;0;170;0m█\x1b[39m");
        // an odd cache: the current theme alone
        let page = || Page::new(gradient(8, 8), "x.png".into(), Options::default());
        let cache = stage.home.0.join(".cache/kitty/kitty-themes.zip");
        std::fs::write(cache, "no archive").unwrap();
        assert_eq!(page().ok().unwrap().themes.len(), 1);
        // no theme: no page, and the reason
        std::fs::remove_file(stage.theme_file()).unwrap();
        let failure = page().err().unwrap().0;
        let reason = "cannot read the kitty theme ";
        assert!(failure.starts_with(reason), "{failure}");
    }

    #[test]
    fn the_menu_and_the_preview_in_its_frame() {
        // RenderPageDrawTest.test_menu_and_preview of tests/test_viewer.py:298; MENU; rows R1, R2, R15, K3, K5
        let mut stage = Stage::small();
        let (first, second) = (stage.draw(100, 30), stage.draw(100, 30));
        // the frame: columns 47 to 99, 51 cells inside, every row
        let top = top_edge(" 1920x1080 · 160 columns ", 13);
        let side = "\x1b[15;47H\x1b[90m│\x1b[15;99H│\x1b[0m";
        let bottom = format!("\x1b[30;47H\x1b[90m└{}┘\x1b[0m", "─".repeat(51));
        for out in [&first, &second] {
            let rows = column(out, 30);
            assert_eq!(rows[..27], MENU);
            assert!(rows[27..].iter().all(String::is_empty));
            assert!(has(out, &top) && has(out, side) && has(out, &bottom));
        }
        // the picture: with the first draw, on a cleared screen, centred in the 51 x 28 cells from column 48, row 2
        assert!(first.starts_with(CLEARED));
        let (at, keys, pixels) = placed(&first).unwrap();
        assert_eq!((at, keys.as_str()), ((9, 48), PLACED));
        assert_eq!(pixels, *stage.preview().as_raw());
        // the same again: the text and the frame only
        assert!(!second.starts_with(CLEARED) && placed(&second).is_none());
        // a new window size, or a screen that was covered meanwhile: sent again
        assert!(placed(&stage.draw(80, 30)).is_some());
        assert!(placed(&stage.draw(80, 30)).is_none());
        let covered = stage.page.draw(window(80, 30), 1, &mut stage.transfer);
        assert!(covered.starts_with(CLEARED) && placed(&covered).is_some());
    }

    #[test]
    fn the_preview_is_the_file_s_drawing_never_enlarged() {
        // rows R1, R14
        let mut stage = Stage::small();
        stage.draw(100, 30);
        // 1920 x 1080 in a room of 459 x 560 px
        assert_eq!(stage.preview().dimensions(), (459, 258));
        // a file smaller than the room shows as it is
        stage.on(Row::Size).keys(&[ENTER, b"k", ENTER]);
        stage.typed("200x120");
        stage.draw(100, 30);
        assert_eq!(stage.preview().dimensions(), (200, 120));
        stage.keys(&[b"s"]);
        assert!(stage.saved() == *stage.preview());
    }

    #[test]
    fn the_frame_needs_room() {
        // row R1: a frame wider than 10 cells, a row at least for the picture
        let mut stage = Stage::small();
        for (cols, rows) in [(57, 30), (46, 30), (20, 30), (1, 1), (100, 2)] {
            let out = stage.draw(cols, rows);
            assert_eq!(column(&out, rows).len(), rows as usize);
            assert!(!has(&out, "┌") && placed(&out).is_none());
        }
        // and nothing was drawn for it
        assert!(stage.page.tried.is_empty() && stage.page.message.is_empty());
        // 9 x 2 cells are room; the title is cut to the edge
        let out = stage.draw(58, 4);
        assert!(has(&out, "\x1b[1;47H\x1b[90m┌ 1920x108┐\x1b[0m"));
        assert!(has(&out, "\x1b[4;47H\x1b[90m└─────────┘\x1b[0m"));
        let (at, keys, _) = placed(&out).unwrap();
        let fitted = "a=T,f=24,t=t,s=71,v=40,c=8,r=2,C=1,z=-1,q=2";
        assert_eq!((at, keys.as_str()), ((2, 48), fitted));
    }

    #[test]
    fn a_font_under_a_px_still_draws() {
        // row R16a: FreeType draws none smaller, nothing fails
        let mut stage = Stage::small();
        // one row of 20 px for a file of 1080: its 20 px font at 0.37 px
        let out = stage.draw(58, 3);
        assert!(stage.page.message.is_empty() && placed(&out).is_some());
        assert_eq!(stage.preview().dimensions(), (36, 20));
        // 160 columns across 16 px
        stage.on(Row::Size).keys(&[ENTER, b"k", ENTER]);
        stage.typed("16x16");
        stage.on(Row::Columns).keys(&[ENTER]);
        stage.typed("160");
        assert_eq!((stage.page.columns(), stage.page.target()), (160, (16, 16)));
        stage.keys(&[b"s"]);
        assert_eq!(stage.page.message, "exported pic-ascii.png");
        assert_eq!(stage.saved().dimensions(), (16, 16));
    }

    #[test]
    fn j_and_k_move_the_cursor_round_the_rows() {
        // row R4
        let mut stage = Stage::new();
        let moves: [(&[u8], usize); 9] = [
            (b"j", 1),
            (DOWN, 2),
            (b"j", 3),
            (b"k", 2),
            (UP, 1),
            (b"k", 0),
            (UP, 18),
            (b"k", 17),
            (b"j", 18),
        ];
        for (key, at) in moves {
            assert_eq!((stage.keys(&[key]), stage.page.at), (After::Stay, at));
        }
        assert_eq!((stage.keys(&[DOWN]), stage.page.at), (After::Stay, 0));
        // the row under the cursor is the marked one, in reverse video
        stage.keys(&[b"k", b"k"]);
        let rows = stage.column(30);
        assert_eq!(rows[3], "   Level      0");
        assert_eq!(rows[22], format!("\x1b[7m{:<45}\x1b[0m", " > Export"));
    }

    #[test]
    fn h_and_l_step_a_row_within_its_limits() {
        // rows R4, R18
        let mut stage = Stage::new();
        #[rustfmt::skip]
        let steps: [(Row, &[u8], usize, &str); 37] = [
            // (row, key, times, the value shown then)
            (Row::Level, b"l", 1, "5% darker"), (Row::Level, b"l", 30, "95% darker"), (Row::Level, b"h", 18, "5% darker"), (Row::Level, LEFT, 30, "95% brighter"),
            (Row::Contrast, RIGHT, 1, "+5%"), (Row::Contrast, b"l", 30, "+95%"), (Row::Contrast, b"h", 20, "-5%"), (Row::Contrast, b"h", 30, "-95%"),
            (Row::Saturation, b"h", 1, "90%"), (Row::Saturation, b"h", 20, "0%"), (Row::Saturation, b"l", 3, "30%"), (Row::Saturation, b"l", 40, "300%"),
            (Row::Skin, b"l", 1, "theme colour"), (Row::Skin, b"h", 1, "grey (calm)"), (Row::Fit, b"l", 1, "cover (edges cut)"), (Row::Fit, b"h", 1, "contain (margins)"),
            // GlyphsKeyTest.test_the_render_page_steps_them_both_ways of tests/test_viewer.py:743
            (Row::Glyphs, b"h", 1, "shape (chafa)"), (Row::Glyphs, b"l", 2, "density (dithered)"), (Row::Glyphs, b"h", 1, "mixed (shapes, dithered flats)"),
            (Row::Bold, b"l", 1, "on"), (Row::Bold, b"l", 1, "off"), (Row::Mono, b"h", 1, "on"), (Row::Mono, b"h", 1, "off"),
            (Row::CropW, b"h", 1, "95%"), (Row::CropW, b"h", 30, "5%"), (Row::CropW, b"l", 2, "15%"), (Row::CropW, b"l", 90, "400%"),
            (Row::CropH, b"h", 2, "90%"), (Row::CropH, b"l", 90, "400%"), (Row::CropH, b"h", 90, "5%"),
            (Row::CropX, b"l", 1, "5%"), (Row::CropX, b"l", 70, "300%"), (Row::CropX, b"h", 61, "-5%"), (Row::CropX, b"h", 70, "-300%"),
            (Row::CropY, b"h", 1, "-5%"), (Row::CropY, b"h", 70, "-300%"), (Row::CropY, b"l", 130, "300%"),
        ];
        for (row, key, times, value) in steps {
            assert_eq!(stage.on(row).keys(&vec![key; times]), After::Stay);
            assert_eq!(stage.page.value(row), value, "{row:?}: {key:?} x {times}");
        }
        assert_eq!(stage.crop(), [-300.0, 300.0, 400.0, 5.0]);
        // a part with decimals, as the crop editor leaves them, keeps them
        stage.page.options.crop[2] = 47.37;
        stage.on(Row::CropW).keys(&[b"l"]);
        assert_eq!(stage.page.value(Row::CropW), "52.37%");
    }

    #[test]
    fn density_and_columns_follow_each_other() {
        // rows R4, R11; RenderPage.nudge, columns and set_columns of bin/ascii-viewer:902, :687, :692 at tag python-final, run by python3
        let mut stage = Stage::new();
        type Step = (Row, Option<f64>, &'static [u8], u32, f64);
        #[rustfmt::skip]
        let steps: [Step; 14] = [
            // (row, the density set first, key) -> (columns, density)
            // ten columns a step: the density that gives them across 1920 px
            (Row::Columns, None, b"l", 170, 18.823529411764707), (Row::Columns, None, b"l", 180, 17.77777777777778), (Row::Columns, None, b"h", 170, 18.823529411764707),
            (Row::Columns, None, b"h", 160, 20.0), (Row::Columns, None, b"h", 150, 21.333333333333332), (Row::Columns, None, b"h", 140, 22.857142857142858),
            // a px a step, to whole px (a half to the even one), within 4..120
            (Row::Density, Some(20.5), b"l", 145, 22.0), (Row::Density, None, b"h", 152, 21.0), (Row::Density, None, b"h", 160, 20.0),
            (Row::Density, Some(4.0), b"h", 800, 4.0), (Row::Density, Some(120.0), b"l", 27, 120.0),
            // 8..2000 columns: their density lies outside the Density row's range
            (Row::Columns, Some(1.6), b"l", 2000, 1.6), (Row::Columns, Some(400.0), b"h", 8, 400.0), (Row::Columns, None, b"l", 18, 177.7777777777778),
        ];
        for (row, density, key, columns, stepped) in steps {
            stage.page.density = density.unwrap_or(stage.page.density);
            stage.on(row).keys(&[key]);
            assert_eq!(
                (stage.page.columns(), stage.page.density),
                (columns, stepped)
            );
        }
        assert_eq!(stage.page.value(Row::Density), "177.8 px");
        assert_eq!(stage.page.value(Row::Columns), "18");
    }

    #[test]
    fn h_and_l_cycle_the_sizes_and_the_themes_and_leave_the_rest() {
        // rows R4, R18
        let mut stage = Stage::new();
        #[rustfmt::skip]
        let steps: [(Row, &[u8], &str); 12] = [
            (Row::Size, b"l", "1080x608  (1080 long side)"), (Row::Size, b"l", "3840x2160  (3840 long side)"), (Row::Size, b"l", "1920x1080  (screen)"),
            (Row::Size, RIGHT, "1920x1080  (1920 long side)"), (Row::Size, b"h", "1920x1080  (screen)"), (Row::Size, LEFT, "3840x2160  (3840 long side)"),
            (Row::Theme, b"l", "Night"), (Row::Theme, b"l", "Stored"), (Row::Theme, b"l", "Twin"), (Row::Theme, b"l", "current"),
            (Row::Theme, b"h", "Twin"), (Row::Theme, b"h", "Stored"),
        ];
        for (row, key, value) in steps {
            stage.on(row).keys(&[key]);
            assert_eq!(stage.page.value(row), value);
        }
        // a step from a custom size leaves it for the next preset
        stage.page.custom = Some((640, 480));
        assert_eq!(stage.page.value(Row::Size), "640x480");
        stage.on(Row::Size).keys(&[RIGHT]);
        assert_eq!((stage.page.size, stage.page.custom), (3, None));
        // nothing to step on Crop, Export, Back
        let before = stage.set();
        for row in [Row::Crop, Row::Export, Row::Back] {
            stage.on(row).keys(&[b"h", b"l", LEFT, RIGHT]);
        }
        assert!(stage.set() == before && !stage.page.out.exists());
    }

    /// Every row away from its default.
    fn unsettle(page: &mut Page) {
        let look = &mut page.options.look;
        (look.level, look.contrast, look.saturation) = (10, -10, 50);
        (look.skin, look.glyphs, look.mono, look.bold) = (Skin::Theme, Glyphs::Shape, true, true);
        (page.options.fit, page.options.crop) = (Fit::Cover, [10.0, 20.0, 50.0, 40.0]);
        (page.density, page.size, page.custom) = (31.0, 2, Some((100, 100)));
        (page.theme, page.ratio) = (3, 3);
    }

    #[test]
    fn backtick_resets_every_row() {
        // RenderPageKeysTest.test_backtick_resets_every_row of tests/test_viewer.py:225; row R6
        let mut stage = Stage::new();
        let fresh = stage.set();
        unsettle(&mut stage.page);
        let unsettled = stage.set();
        // nothing to reset on Export and Back
        stage.on(Row::Export).keys(&[b"`"]);
        stage.on(Row::Back).keys(&[b"`"]);
        assert!(stage.set() == unsettled);
        // Columns resets the density, Size the preset and the custom size, Crop its four parts and the ratio
        stage.on(Row::Columns).keys(&[b"`"]);
        assert_eq!(stage.page.density, 20.0);
        stage.on(Row::Size).keys(&[b"`"]);
        assert_eq!((stage.page.size, stage.page.custom), (0, None));
        stage.on(Row::Crop).keys(&[b"`"]);
        assert_eq!((stage.crop(), stage.page.ratio), (FULL, 0));
        // a part of the crop resets alone
        unsettle(&mut stage.page);
        stage.on(Row::CropH).keys(&[b"`"]);
        assert_eq!(
            (stage.crop(), stage.page.ratio),
            ([10.0, 20.0, 50.0, 100.0], 3)
        );
        stage.on(Row::CropX).keys(&[b"`"]);
        assert_eq!(stage.crop(), [0.0, 20.0, 50.0, 100.0]);
        // every row in turn: all as the page opens
        unsettle(&mut stage.page);
        for (row, ..) in &ROWS[..SETTINGS] {
            stage.on(*row).keys(&[b"`"]);
        }
        assert!(stage.set() == fresh);
    }

    #[test]
    fn enter_opens_or_sets_what_the_row_holds() {
        // row R5
        let mut stage = Stage::new();
        #[rustfmt::skip]
        let set = [
            (Row::Bold, "on"), (Row::Mono, "on"), (Row::Skin, "theme colour"), (Row::Fit, "cover (edges cut)"),
            (Row::Glyphs, "density (dithered)"), (Row::Glyphs, "shape (chafa)"), (Row::Bold, "off"),
        ];
        for (row, value) in set {
            assert_eq!(stage.on(row).keys(&[ENTER]), After::Stay);
            assert_eq!(stage.page.value(row), value);
        }
        // a list for Size and Theme, the editor for Crop
        stage.on(Row::Size).keys(&[ENTER]);
        assert!(!stage.list().themes);
        stage.on(Row::Theme).keys(&[b"\n"]);
        assert!(stage.list().themes && !stage.list().searching);
        stage.on(Row::Crop).keys(&[ENTER]);
        assert!(matches!(stage.page.state, State::Crop));
        // a prompt for the rest
        let typed = [
            Row::Level,
            Row::Contrast,
            Row::Saturation,
            Row::Density,
            Row::Columns,
        ];
        for row in typed
            .into_iter()
            .chain([Row::CropW, Row::CropH, Row::CropX, Row::CropY])
        {
            stage.on(row).keys(&[b"\n"]);
            assert_eq!(stage.prompt(), Some((row, "")));
        }
        assert_eq!(stage.on(Row::Back).keys(&[ENTER]), After::Close);
        assert_eq!(stage.on(Row::Back).keys(&[b"\n"]), After::Close);
    }

    #[test]
    fn q_r_and_esc_close_from_the_menu_only() {
        // row R7
        let mut stage = Stage::new();
        let closing: [&[u8]; 4] = [b"q", b"Q", b"r", ESC];
        for key in closing {
            for row in [Row::Level, Row::Crop, Row::Export] {
                assert_eq!(stage.on(row).keys(&[key]), After::Close);
            }
            // typed in a prompt or dropping it; in a list and the crop editor back to the menu, or nothing
            for opened in [Row::Level, Row::Size, Row::Theme, Row::Crop] {
                assert_eq!(stage.on(opened).keys(&[ENTER, key]), After::Stay);
                let left = matches!(stage.page.state, State::Menu);
                let leaves = match opened {
                    Row::Level => key == ESC,
                    _ => key == b"q" || key == ESC,
                };
                assert_eq!(left, leaves, "{opened:?} {key:?}");
            }
        }
        // `/` from any row: the themes, in search
        stage.on(Row::Bold).keys(&[b"/"]);
        assert!(stage.list().themes && stage.list().searching);
        // other keys do nothing
        let before = stage.on(Row::Level).set();
        let idle: [&[u8]; 9] = [
            b"x",
            b"d",
            b" ",
            b"\t",
            b"1",
            b"H",
            b"\x1b[1;2C",
            b"\x1bOP",
            BACKSPACE,
        ];
        assert_eq!(stage.keys(&idle), After::Stay);
        assert!(stage.set() == before && matches!(stage.page.state, State::Menu));
        assert_eq!(stage.page.at, 0);
    }

    #[test]
    fn a_typed_value_is_held_to_the_row_s_range() {
        // rows R8, R11
        let mut stage = Stage::new();
        #[rustfmt::skip]
        let cases = [
            // (row, typed, the row's value then)
            (Row::Level, "12", "12% darker"), (Row::Level, "-200", "95% brighter"), (Row::Level, "200", "95% darker"),
            (Row::Level, "+5", "5% darker"), (Row::Level, " -7 ", "7% brighter"), (Row::Level, "0", "0"),
            (Row::Contrast, "-40", "-40%"), (Row::Contrast, "96", "+95%"), (Row::Contrast, "-96", "-95%"),
            (Row::Saturation, "250", "250%"), (Row::Saturation, "301", "300%"), (Row::Saturation, "-1", "0%"),
            (Row::Density, "12.5", "12.5 px"), (Row::Density, "1", "4.0 px"), (Row::Density, "500", "120.0 px"),
            (Row::Density, "inf", "120.0 px"), (Row::Density, "-inf", "4.0 px"), (Row::Density, "1e1", "10.0 px"),
            (Row::CropW, "50", "50%"), (Row::CropW, "1", "5%"), (Row::CropW, "999", "400%"), (Row::CropH, "150", "150%"),
            (Row::CropX, "-500", "-300%"), (Row::CropX, "25", "25%"), (Row::CropY, "301", "300%"),
            // columns: 8..2000
            (Row::Columns, "100", "100"), (Row::Columns, "5", "8"), (Row::Columns, "99999", "2000"),
        ];
        for (row, typed, value) in cases {
            stage.on(row).keys(&[ENTER]);
            stage.typed(typed);
            assert_eq!(stage.page.value(row), value, "{row:?} {typed:?}");
            assert!(matches!(stage.page.state, State::Menu) && stage.page.message.is_empty());
        }
        // and the density that gives the columns
        assert_eq!(stage.page.value(Row::Density), "1.6 px");
    }

    #[test]
    fn what_is_no_value_is_said_and_changes_nothing() {
        // rows R8, R8a, change N12: whole numbers only; `nan` is no density
        let mut stage = Stage::new();
        let before = stage.set();
        #[rustfmt::skip]
        let cases = [
            (Row::Level, "x"), (Row::Level, ""), (Row::Level, "1e2"), (Row::Level, "4.5"), (Row::Level, "1_0"), (Row::Level, "--1"),
            (Row::Contrast, "five"), (Row::Saturation, "50%"), (Row::Density, "nan"), (Row::Density, "NaN"), (Row::Density, "px"),
            (Row::Density, ""), (Row::Columns, "12.5"), (Row::Columns, "many"), (Row::CropW, "47.5"), (Row::CropX, "left"),
        ];
        for (row, typed) in cases {
            stage.on(row).keys(&[ENTER]);
            stage.typed(typed);
            assert_eq!(
                stage.page.message,
                format!("not a value: {typed}"),
                "{row:?}"
            );
            assert!(matches!(stage.page.state, State::Menu));
        }
        assert!(stage.set() == before);
    }

    #[test]
    fn the_prompt_takes_the_printable_keys() {
        // row R8; PROMPTS
        let mut stage = Stage::new();
        for (row, prompt) in PROMPTS {
            let opening: &[&[u8]] = match row {
                Row::Size => &[ENTER, b"k", ENTER],
                _ => &[ENTER],
            };
            stage.on(row).keys(opening);
            stage.keys(&[b"1", b"2"]);
            // under the menu's 27 rows and a blank one
            assert_eq!(stage.column(30)[26..], [MENU[26], "", prompt, ""]);
        }
        // 0x20 to 0x7E are typed, the menu's keys too; Backspace deletes; other keys are passed over
        let typed: [&[u8]; 9] = [b"q", b"s", b"`", b" ", b"~", UP, b"\t", b"\x1bOP", b"/"];
        stage.on(Row::Level).keys(&[ENTER]);
        assert_eq!(stage.keys(&typed), After::Stay);
        assert_eq!(stage.prompt(), Some((Row::Level, "qs` ~/")));
        stage.keys(&[BACKSPACE, b"\x08", b"\x80", b"j"]);
        assert_eq!(stage.prompt(), Some((Row::Level, "qs` j")));
        stage.keys(&[BACKSPACE; 9]);
        assert_eq!(stage.column(30)[28], " Level (-95..95, up darker): _");
        // Esc drops what was typed
        stage.keys(&[b"4", b"0", ESC]);
        assert!(matches!(stage.page.state, State::Menu) && stage.page.message.is_empty());
        assert_eq!((stage.page.options.look.level, stage.page.at), (0, 0));
        // a line longer than the column is cut
        stage.keys(&[ENTER]);
        stage.keys(&[&b"7"[..]; 60]);
        let cut = format!(" Level (-95..95, up darker): {}", "7".repeat(16));
        assert_eq!(stage.column(30)[28], cut);
    }

    #[test]
    fn the_size_list() {
        // rows R9, R4; SIZES_LIST
        let mut stage = Stage::new();
        stage.on(Row::Size).keys(&[ENTER, b"j"]);
        let rows = stage.column(30);
        assert_eq!(rows[..9], SIZES_LIST);
        assert!(rows[9..].iter().all(String::is_empty));
        // j, Down and Tab move down, k and Up up, round the ends; a search is the themes' alone
        let moves: [(&[u8], usize); 8] = [
            (DOWN, 2),
            (b"\t", 3),
            (b"j", 4),
            (b"j", 0),
            (b"k", 4),
            (UP, 3),
            (b"/", 3),
            (b"x", 3),
        ];
        for (key, at) in moves {
            stage.keys(&[key]);
            assert_eq!((stage.list().at, stage.list().searching), (at, false));
        }
        // the list opens on the preset in force
        stage.page.size = 2;
        stage.on(Row::Size).keys(&[ENTER]);
        assert_eq!(stage.list().at, 2);
        // Enter, l and Right pick: the preset, and no custom size; q, Esc, h and Left lead back with nothing picked
        let leaving: [(&[u8], usize); 8] = [
            (ENTER, 1),
            (b"\n", 1),
            (b"l", 1),
            (RIGHT, 1),
            (b"q", 0),
            (ESC, 0),
            (b"h", 0),
            (LEFT, 0),
        ];
        for (key, size) in leaving {
            (stage.page.size, stage.page.custom) = (0, Some((640, 480)));
            assert_eq!(stage.on(Row::Size).keys(&[ENTER, b"j", key]), After::Stay);
            let custom = (size == 0).then_some((640, 480));
            assert_eq!(
                (stage.page.size, stage.page.custom),
                (size, custom),
                "{key:?}"
            );
            assert!(matches!(stage.page.state, State::Menu));
        }
    }

    #[test]
    fn a_custom_size_is_typed() {
        // rows R8, R9, R18
        let mut stage = Stage::new();
        #[rustfmt::skip]
        let cases = [
            // (typed, the custom size then): spaces dropped, x or X, each side made even
            ("640x480", Some((640, 480))), (" 641 X 479 ", Some((642, 480))), ("16383x17", Some((16384, 18))), ("16x16384", Some((16, 16384))),
            // refused outside 16..16384 a side, and when it is no size
            ("15x100", None), ("100x15", None), ("16385x100", None), ("100x16385", None), ("100", None), ("1x2x3", None),
            ("axb", None), ("-16x16", None), ("640x", None), ("", None),
        ];
        for (typed, custom) in cases {
            // the last row of the list, whatever the size is
            stage.on(Row::Size).keys(&[ENTER, b"k"]);
            assert_eq!(stage.list().at, 4);
            stage.keys(&[ENTER]);
            assert_eq!(stage.prompt(), Some((Row::Size, "")));
            stage.typed(typed);
            let refusal = format!("not a value: {typed}");
            let message = if custom.is_some() { "" } else { &refusal };
            assert_eq!(stage.page.message, message);
            assert_eq!(stage.page.custom, custom.or(Some((16, 16384))), "{typed:?}");
        }
        assert_eq!(stage.page.value(Row::Size), "16x16384");
        assert_eq!(stage.page.target(), (16, 16384));
        // Esc at the prompt keeps what was
        stage.on(Row::Size).keys(&[ENTER, b"k", ENTER, b"9", ESC]);
        assert_eq!(stage.page.custom, Some((16, 16384)));
    }

    #[test]
    fn a_preset_takes_the_cropped_picture_s_shape() {
        // RenderPageTest.test_presets_follow_crop_shape of tests/test_viewer.py:217; row R10
        let mut stage = Stage::new();
        stage.page.size = 1;
        assert_eq!(stage.page.target(), (1080, 608));
        stage.page.options.crop[2] = 50.0;
        assert_eq!(stage.page.target(), (960, 1080));
        // (preset, crop) -> (size, columns): RenderPage.target and columns of bin/ascii-viewer:670, :687 at tag python-final on 1600 x 900, run by python3
        #[rustfmt::skip]
        let cases = [
            (0, [0.0, 0.0, 100.0, 100.0], (1920, 1080), 160), (0, [0.0, 0.0, 50.0, 100.0], (1706, 1920), 142),
            (0, [10.0, 20.0, 30.0, 90.0], (1138, 1920), 95), (0, [0.0, 0.0, 100.0, 5.0], (1920, 54), 160),
            (0, [-10.0, 0.0, 120.0, 100.0], (1920, 900), 160), (0, [0.0, 0.0, 400.0, 5.0], (1920, 14), 160),
            (0, [0.0, 0.0, 5.0, 400.0], (42, 1920), 8),
            (1, [10.0, 20.0, 30.0, 90.0], (640, 1080), 53), (1, [0.0, 0.0, 100.0, 5.0], (1080, 30), 90),
            (1, [-10.0, 0.0, 120.0, 100.0], (1080, 506), 90), (1, [0.0, 0.0, 400.0, 5.0], (1080, 8), 90),
            (1, [0.0, 0.0, 5.0, 400.0], (24, 1080), 8),
            (2, [0.0, 0.0, 100.0, 100.0], (3840, 2160), 320), (2, [0.0, 0.0, 50.0, 100.0], (3412, 3840), 284),
            (2, [10.0, 20.0, 30.0, 90.0], (2276, 3840), 190), (2, [0.0, 0.0, 5.0, 400.0], (84, 3840), 8),
        ];
        for (size, crop, target, columns) in cases {
            (stage.page.size, stage.page.options.crop) = (size, crop);
            assert_eq!(
                (stage.page.target(), stage.page.columns()),
                (target, columns),
                "{size} {crop:?}"
            );
        }
        // the shape is the cropped picture's in whole px, as the converter cuts it: 47.37 % by 61.03 % of 1600 x 900 is 758 x 549
        (stage.page.size, stage.page.options.crop) = (0, [12.5, 3.2, 47.37, 61.03]);
        assert_eq!(stage.page.target(), convert::web_size((758, 549)));
        assert_eq!(stage.page.target(), (1920, 1390));
    }

    #[test]
    fn the_screen_preset_is_the_focused_monitor_s_size() {
        // RenderPageTest.test_unknown_screen_falls_back of tests/test_viewer.py:212; row R9
        let mut stage = Stage::new();
        stage.page.size = 3;
        assert_eq!(stage.page.target(), (1920, 1080));
        assert_eq!(stage.page.value(Row::Size), "1920x1080  (screen)");
        stage.page.screen = OnceCell::from(Some((2560, 1440)));
        assert_eq!(
            (stage.page.target(), stage.page.columns()),
            ((2560, 1440), 213)
        );
        assert_eq!(stage.page.value(Row::Size), "2560x1440  (screen)");
        // whatever the crop; a custom size comes first
        stage.page.options.crop = [0.0, 0.0, 50.0, 100.0];
        assert_eq!(stage.page.target(), (2560, 1440));
        stage.page.custom = Some((320, 200));
        assert_eq!(stage.page.target(), (320, 200));
    }

    #[test]
    fn the_screen_is_asked_once_a_page() {
        // RenderPageTest.test_screen_size_asked_once of tests/test_viewer.py:204
        let mut stage = Stage::small();
        let program = stage.home.0.join("bin/hyprctl");
        std::fs::create_dir_all(program.parent().unwrap()).unwrap();
        // a stand-in for hyprctl that notes each call; written by a child: a file open for writing here would be busy for a child another test forks meanwhile
        let script = r#"#!/bin/sh
echo "$@" >> "$0.calls"
echo '[{"width": 2560, "height": 1440, "focused": true}]'
"#;
        let mut writer = std::process::Command::new("/bin/sh");
        writer
            .args(["-c", r#"cat > "$0" && chmod 755 "$0""#])
            .arg(&program);
        let mut writer = writer.stdin(std::process::Stdio::piped()).spawn().unwrap();
        writer
            .stdin
            .take()
            .unwrap()
            .write_all(script.as_bytes())
            .unwrap();
        assert!(writer.wait().unwrap().success());
        // first on the path
        let mut path = program.parent().unwrap().as_os_str().to_owned();
        path.push(":");
        path.push(std::env::var_os("PATH").unwrap());
        let _path = Var::set("PATH", path);
        (stage.page.screen, stage.page.size) = (OnceCell::new(), 3);
        for _ in 0..5 {
            assert_eq!(stage.page.target(), (2560, 1440));
            assert_eq!(stage.page.columns(), 213);
        }
        stage.draw(100, 30);
        assert_eq!(stage.preview().dimensions(), (459, 258));
        let calls = std::fs::read_to_string(program.with_extension("calls")).unwrap();
        assert_eq!(calls, "monitors -j\n");
    }

    #[test]
    fn the_theme_list() {
        // row R12; the rows: RenderPage.draw of bin/ascii-viewer:1096 at tag python-final with this collection, run by python3
        let mut stage = Stage::new();
        stage.on(Row::Theme).keys(&[ENTER, b"k"]);
        let rows = stage.column(30);
        // each name cut and padded to 24 cells, in reverse video under the cursor, then the strip
        assert_eq!(rows[..2], [" Theme  (dark, kitty's)", ""]);
        assert!(
            rows[2].starts_with(
                "   current                  \x1b[38;2;26;28;29m█\x1b[38;2;255;88;148m█"
            )
        );
        assert!(
            rows[3].starts_with(
                "   Night                    \x1b[38;2;24;24;28m█\x1b[38;2;224;96;112m█"
            )
        );
        assert!(
            rows[4]
                .starts_with("   Stored                   \x1b[38;2;0;0;255m█\x1b[38;2;16;8;239m█")
        );
        assert_eq!(
            rows[5],
            "\x1b[7m > Twin                    \x1b[0m \x1b[38;2;0;170;0m█\x1b[39m"
        );
        assert_eq!(rows[6..9], ["", THEMES_KEYS, " \x1b[90m4 themes\x1b[0m"]);
        assert!(rows[9..].iter().all(String::is_empty));
        // j, Down and Tab move down, k and Up up, round the ends; the theme under the cursor follows
        let moves: [(&[u8], usize); 8] = [
            (b"j", 0),
            (b"j", 1),
            (DOWN, 2),
            (b"\t", 3),
            (b"k", 2),
            (UP, 1),
            (b"k", 0),
            (b"k", 3),
        ];
        for (key, at) in moves {
            stage.keys(&[key]);
            assert_eq!((stage.list().at, stage.list().theme), (at, at));
        }
        // Enter, l and Right pick; q, Esc, h and Left lead back with nothing picked
        let leaving: [(&[u8], usize); 8] = [
            (ENTER, 2),
            (b"\n", 2),
            (b"l", 2),
            (RIGHT, 2),
            (b"q", 0),
            (ESC, 0),
            (b"h", 0),
            (LEFT, 0),
        ];
        for (key, theme) in leaving {
            stage.page.theme = 0;
            stage.on(Row::Theme).keys(&[ENTER, b"j", b"j", key]);
            assert!(matches!(stage.page.state, State::Menu));
            assert_eq!(stage.page.theme, theme, "{key:?}");
        }
        // the list opens on the chosen theme
        stage.page.theme = 3;
        stage.on(Row::Theme).keys(&[ENTER]);
        assert_eq!((stage.list().at, stage.list().theme), (3, 3));
    }

    #[test]
    fn a_search_filters_the_themes() {
        // row R12; RenderPage.list_key of bin/ascii-viewer:1044 at tag python-final with this collection, run by python3
        let mut stage = Stage::new();
        stage.on(Row::Theme).keys(&[ENTER, b"/"]);
        let (all, some): (&[usize], &[usize]) = (&[0, 1, 2, 3], &[1, 3]);
        type Step<'a> = (&'a [u8], &'a str, bool, usize, usize, &'a [usize]);
        #[rustfmt::skip]
        let steps: [Step; 21] = [
            // (key, the search, whether it goes on, the cursor, the theme under it, the themes left)
            (b"I", "I", true, 0, 1, some), (b"x", "Ix", true, 0, 1, &[]), (BACKSPACE, "I", true, 0, 1, some),
            (BACKSPACE, "", true, 0, 0, all), (b"\x08", "", true, 0, 0, all),
            (b"s", "s", true, 0, 2, &[2]), (b"t", "st", true, 0, 2, &[2]), (RIGHT, "st", true, 0, 2, &[2]),
            // Enter ends the search and keeps the filter
            (ENTER, "st", false, 0, 2, &[2]), (b"j", "st", false, 0, 2, &[2]), (b"/", "st", true, 0, 2, &[2]),
            (BACKSPACE, "s", true, 0, 2, &[2]), (BACKSPACE, "", true, 0, 0, all), (b"T", "T", true, 0, 0, all),
            // Tab, Up and Down end it and move
            (b"\t", "T", false, 1, 1, all), (b"/", "T", true, 1, 1, all), (b"w", "Tw", true, 0, 3, &[3]),
            (UP, "Tw", false, 0, 3, &[3]), (b"/", "Tw", true, 0, 3, &[3]), (b"q", "Twq", true, 0, 3, &[]),
            // so does Esc, and stays
            (ESC, "Twq", false, 0, 3, &[]),
        ];
        for (key, query, searching, at, theme, found) in steps {
            stage.keys(&[key]);
            let list = stage.list();
            let state = (list.query.as_str(), list.searching, list.at, list.theme);
            assert_eq!(state, (query, searching, at, theme), "after {key:?}");
            assert_eq!(stage.page.choices(list), found, "after {key:?}");
        }
        // nothing left to move to or to pick; q leaves, nothing chosen
        stage.keys(&[b"j", ENTER, b"l", DOWN]);
        assert_eq!((stage.list().at, stage.list().theme), (0, 3));
        stage.keys(&[b"q"]);
        assert!(matches!(stage.page.state, State::Menu) && stage.page.theme == 0);
        // Down ends a search as Tab does
        stage.keys(&[b"/", b"i", DOWN]);
        let list = stage.list();
        assert_eq!((list.searching, list.at, list.theme), (false, 1, 3));
    }

    #[test]
    fn the_search_shows_in_the_list() {
        // row R12; the rows: RenderPage.draw of bin/ascii-viewer:1096 at tag python-final with this collection, run by python3
        let mut stage = Stage::new();
        stage.keys(&[b"/", b"i"]);
        let rows = stage.column(30);
        assert!(
            rows[2].starts_with("\x1b[7m > Night                   \x1b[0m \x1b[38;2;24;24;28m█")
        );
        assert_eq!(
            rows[3],
            "   Twin                     \x1b[38;2;0;170;0m█\x1b[39m"
        );
        assert_eq!(rows[4..7], ["", " /i_", " \x1b[90m2 themes\x1b[0m"]);
        // RenderPageDrawTest.test_lists_prompts_and_messages_fill_the_column_too of tests/test_viewer.py:312: a message under the list
        stage.page.message = "render failed: no theme".into();
        assert_eq!(stage.column(30)[7..9], ["", " render failed: no theme"]);
        // once it has ended the keys show again; the filter stays
        stage.keys(&[ENTER]);
        assert_eq!(
            stage.column(30)[5..7],
            [THEMES_KEYS, " \x1b[90m2 themes\x1b[0m"]
        );
        // no theme of that name
        stage.keys(&[b"/", b"z"]);
        assert_eq!(
            stage.column(30)[2..5],
            ["", " /iz_", " \x1b[90m0 themes\x1b[0m"]
        );
        // a search longer than the column is cut
        stage.keys(&[&b"z"[..]; 60]);
        assert_eq!(stage.column(30)[3], format!(" /i{}_", "z".repeat(41)));
    }

    #[test]
    fn a_long_list_keeps_the_cursor_near_the_middle() {
        // row R12; (window rows, cursor) -> (first theme shown, how many): RenderPage.draw of bin/ascii-viewer:1096 at tag python-final on 30 themes, run by python3
        let mut stage = Stage::new();
        stage.page.themes = (0..30)
            .map(|n| (format!("theme {n:02}"), String::new()))
            .collect();
        #[rustfmt::skip]
        let cases = [
            (20, 0, 0, 12), (20, 6, 0, 12), (20, 7, 1, 12), (20, 20, 14, 12), (20, 29, 18, 12), (30, 15, 4, 22),
            (40, 15, 0, 30), (5, 10, 9, 3), (9, 29, 27, 3), (11, 0, 0, 3), (12, 3, 1, 4),
        ];
        for (rows, at, first, count) in cases {
            let (themes, query, searching, theme) = (true, String::new(), false, 0);
            let list = List {
                themes,
                at,
                query,
                searching,
                theme,
            };
            let lines = stage.page.list_lines(&list, rows);
            // the title, a blank row, the themes, a blank row, the keys, the count
            assert_eq!(lines.len(), count + 5, "{rows} rows, cursor {at}");
            let shown: Vec<String> = lines[2..2 + count].iter().map(|line| plain(line)).collect();
            let row =
                |n: usize| format!(" {} theme {n:02}{:17}", if n == at { '>' } else { ' ' }, "");
            assert_eq!(shown, (first..first + count).map(row).collect::<Vec<_>>());
        }
    }

    #[test]
    fn the_preview_follows_the_theme_under_the_cursor() {
        // rows R12, R15
        let mut stage = Stage::small();
        stage.page.custom = Some((640, 360));
        stage.draw(100, 30);
        let current = stage.preview().clone();
        stage.on(Row::Theme).keys(&[ENTER]);
        assert!(placed(&stage.draw(100, 30)).is_none());
        // Twin sets color2 alone: the drawing with another green
        stage.keys(&[b"k"]);
        let out = stage.draw(100, 30);
        let twin = stage.preview().clone();
        assert_eq!(placed(&out).unwrap().2, *twin.as_raw());
        assert!(twin != current && stage.page.tried.contains("theme: Some(\"Twin\")"));
        // back without a pick: the chosen theme's drawing again
        stage.keys(&[ESC]);
        let out = stage.draw(100, 30);
        assert!(placed(&out).is_some() && *stage.preview() == current);
        // picked: it stays, in the preview and in the file
        stage.on(Row::Theme).keys(&[ENTER, b"k", ENTER]);
        assert_eq!(stage.page.value(Row::Theme), "Twin");
        stage.draw(100, 30);
        assert!(*stage.preview() == twin);
        stage.keys(&[b"s"]);
        let mut named = stage.page.settings(0);
        named.theme = Some("Twin".to_string());
        let file = stage.saved();
        assert!(file == convert::draw(&stage.page.picture, &named, false).unwrap());
        // its green in the file, not the current theme's
        let inked = |ink: [u8; 3]| file.pixels().any(|pixel| pixel.0 == ink);
        assert!(inked([0, 170, 0]) && !inked([0xB5, 0xE3, 0x54]));
    }

    #[test]
    fn each_crop_key_moves_or_shapes_the_window() {
        // row R13; (key, crop, ratio) from the crop 10, 20, 50, 40 of 1600 x 900 at the free ratio: RenderPage.crop_key of bin/ascii-viewer:812 at tag python-final, run by python3
        let mut stage = Stage::new();
        #[rustfmt::skip]
        let cases: [(&[u8], [f64; 4], usize); 25] = [
            (b"h", [5.0, 20.0, 50.0, 40.0], 0), (b"l", [15.0, 20.0, 50.0, 40.0], 0), (b"k", [10.0, 15.0, 50.0, 40.0], 0), (b"j", [10.0, 25.0, 50.0, 40.0], 0),
            (b"H", [9.0, 20.0, 50.0, 40.0], 0), (b"L", [11.0, 20.0, 50.0, 40.0], 0), (b"K", [10.0, 19.0, 50.0, 40.0], 0), (b"J", [10.0, 21.0, 50.0, 40.0], 0),
            (LEFT, [5.0, 20.0, 50.0, 40.0], 0), (RIGHT, [15.0, 20.0, 50.0, 40.0], 0), (UP, [10.0, 15.0, 50.0, 40.0], 0), (DOWN, [10.0, 25.0, 50.0, 40.0], 0),
            (b"-", [11.25, 21.0, 47.5, 38.0], 0), (b"=", [8.68, 18.95, 52.63, 42.11], 0), (b"+", [8.68, 18.95, 52.63, 42.11], 0),
            (b"<", [11.25, 20.0, 47.5, 40.0], 0), (b">", [8.68, 20.0, 52.63, 40.0], 0), (b"{", [10.0, 21.0, 50.0, 38.0], 0), (b"}", [10.0, 18.95, 50.0, 42.11], 0),
            (b"c", [25.0, 30.0, 50.0, 40.0], 0), (b"0", [0.0, 0.0, 100.0, 100.0], 1), (b"[", [10.0, 20.95, 50.0, 38.1], 11), (b"]", [15.0, 20.0, 40.0, 40.0], 1),
            (b"x", [10.0, 20.0, 50.0, 40.0], 0), (b"\x1b[1;2C", [10.0, 20.0, 50.0, 40.0], 0),
        ];
        for (key, crop, ratio) in cases {
            (stage.page.options.crop, stage.page.ratio) = ([10.0, 20.0, 50.0, 40.0], 0);
            assert_eq!(stage.on(Row::Crop).keys(&[ENTER, key]), After::Stay);
            assert_eq!((stage.crop(), stage.page.ratio), (crop, ratio), "{key:?}");
            assert!(matches!(stage.page.state, State::Crop));
        }
        // width and height alone only at the free ratio
        stage.page.options.crop = FULL;
        stage.keys(&[b"]", b"<", b">", b"{", b"}"]);
        assert_eq!((stage.crop(), stage.page.ratio), (FULL, 1));
        // Enter, q and Esc lead back to the menu, the crop kept
        let leaving: [&[u8]; 4] = [ENTER, b"\n", b"q", ESC];
        for key in leaving {
            assert_eq!(stage.on(Row::Crop).keys(&[ENTER, b"H", key]), After::Stay);
            assert!(matches!(stage.page.state, State::Menu));
        }
        assert_eq!(stage.crop(), [-4.0, 0.0, 100.0, 100.0]);
        assert_eq!(stage.page.value(Row::Crop), "original, editor  (enter)");
        assert_eq!(stage.page.value(Row::CropX), "-4%");
    }

    #[test]
    fn the_ratios_in_turn() {
        // row R13; RenderPage.apply_ratio of bin/ascii-viewer:800 at tag python-final from the whole 1600 x 900 picture, run by python3: each the largest window of its shape inside the one before
        let mut stage = Stage::new();
        stage.on(Row::Crop).keys(&[ENTER]);
        #[rustfmt::skip]
        let turns = [
            ("original", [0.0, 0.0, 100.0, 100.0]), ("1:1", [21.88, 0.0, 56.25, 100.0]), ("4:5", [27.5, 0.0, 45.0, 100.0]),
            ("5:4", [27.5, 18.0, 45.0, 64.0]), ("3:4", [36.5, 18.0, 27.0, 64.0]), ("4:3", [36.5, 32.0, 27.0, 36.0]),
            ("2:3", [43.25, 32.0, 13.5, 36.0]), ("3:2", [43.25, 42.0, 13.5, 16.0]), ("9:16", [47.47, 42.0, 5.06, 16.0]),
            ("16:9", [47.47, 47.47, 5.06, 5.06]), ("21:9", [47.47, 48.07, 5.06, 3.86]), ("free", [47.47, 48.07, 5.06, 3.86]),
            ("original", [48.07, 48.07, 3.86, 3.86]),
        ];
        for (name, crop) in turns {
            stage.keys(&[b"]"]);
            assert_eq!((RATIOS[stage.page.ratio].0, stage.crop()), (name, crop));
        }
        // and back
        #[rustfmt::skip]
        let back = [(0, [48.07, 48.07, 3.86, 3.86]), (11, [48.07, 48.53, 3.86, 2.94]), (10, [48.53, 48.53, 2.94, 2.94])];
        for (ratio, crop) in back {
            stage.keys(&[b"["]);
            assert_eq!((stage.page.ratio, stage.crop()), (ratio, crop));
        }
    }

    #[test]
    fn the_window_keeps_2_percent_a_side_and_has_no_upper_bound() {
        // row R13; RenderPage.crop_key of bin/ascii-viewer:812 at tag python-final on 1600 x 900, run by python3
        let mut stage = Stage::new();
        type Case = ([f64; 4], &'static [u8], usize, [f64; 4]);
        #[rustfmt::skip]
        let cases: [Case; 7] = [
            // (the crop set first, key, times) -> the crop; an empty first crop: on from the one before
            (FULL, b"-", 100, [50.17, 50.17, 2.0, 2.0]), ([0.0; 4], b"=", 120, [-423.65, -423.65, 949.72, 949.72]),
            (FULL, b"<", 100, [50.17, 0.0, 2.0, 100.0]), ([0.0; 4], b"{", 100, [50.17, 50.17, 2.0, 2.0]),
            // past the picture's borders
            (FULL, b"h", 30, [-150.0, 0.0, 100.0, 100.0]), ([0.0; 4], b"K", 30, [-150.0, -30.0, 100.0, 100.0]),
            // a shape at the floor: 2 % of the width against 2 % of the height
            ([10.0, 5.0, 30.0, 90.0], b"]", 13, [24.25, 49.24, 2.0, 2.0]),
        ];
        for (start, key, times, crop) in cases {
            if start != [0.0; 4] {
                (stage.page.options.crop, stage.page.ratio) = (start, 0);
            }
            stage.on(Row::Crop).keys(&[ENTER]);
            stage.keys(&vec![key; times]);
            assert_eq!(stage.crop(), crop, "{key:?} x {times}");
        }
    }

    #[test]
    fn a_long_walk_of_crop_keys_ends_where_python_s_does() {
        // row R13; RenderPage.crop_key of bin/ascii-viewer:812 at tag python-final, run by python3: 400 keys drawn by random.Random(seed).choices; the crop after the 100th, the 250th and the last, the ratio then, and the FNV-1a 64 of every crop's repr on the way
        type Walk = ((u32, u32), &'static str, [[f64; 4]; 3], usize, u64);
        #[rustfmt::skip]
        let walks: [Walk; 3] = [
            ((1600, 900), "l{>jJK=>lh{K>hK<k[}hh-[LkLhkKJkkkKjh{-=k]{lH<<[L{=j-}{J-hj>Lk-<+HKJ>JLJhh<]-LkJ]>-{kJ[-Kj-]h>{}<{J-Lh}-kJJHH-==Khkk-{>>{j{+lhh>jl=HhlJkj<KjKhLLkl}Jk-{hhl<l<+-k]>Jk=L-j=hj]}j{j[<Ljh}h{]-k}]<JLHk+Kkl=jJH}}hkH]>Hk+{[H}+K]k<lk}k>-{HHj}-[}l-lhh}>{H=>L-klj}-cKj>{h=ll}hk]Llkk<l}L]}jjKl=hh]j-Kjh}]]lk=]-+=j-jjlj]K==[LjHj{}jH---jhjh-hh=j>J}lJ>h[k>]{jlJcj}l}hj}>}{<+kKl<=jh]{--{KLHjh=L-hHllj{LL=khJJ=K+<kJKkL-",
             [[-2.46, 10.76, 44.92, 32.51], [-1.63, 0.47, 3.2, 7.12], [46.89, 52.89, 2.22, 2.22]], 10, 0x9c0cec369cda0694),
            ((1000, 667), "][hl{<=j==-lLL<][-KjhhKjL}J-khHlJ0+k}><}>>H]]l><KJJcJ{H}}K-c<JkH+k}j}j]<JJ=-jkJ[=h{<}k<h=jk}lJ{jk}L<hHk=l[h<hj{lk+Lh]lhH=<lHhK><}>}<Kk=jlK}l-LJl]j=Lh-lhhll=J][]kKj-=><jLJhhLl<klkkkJKj=k}]<KJ-hLJkl>HJc=j]Hh+lj{=hKLJk-hjH[h>k-LK<Lllh{=]+h=><JHK>jJK[>[{jkJjL+c-{lH0lLhl}]=ljk=+KJl-[>lJ<j}K<L]>-lKh-}kJKL<[<K]H<=>{k=L=]=hK<}={h[<=}}l{>k<-k>l=Kj-Kk]hhJ{=>J+HjJhh>k<>L+>}lkLKjh-]HJLK}j=K-}h{j=>=L{l=LJ{>=jk",
             [[25.08, 12.67, 39.83, 74.65], [33.78, 56.57, 2.44, 4.87], [39.3, 21.98, 33.4, 40.05]], 4, 0x828308d6a3e67227),
            ((333, 1000), "k-H-=hh{jk]K{K=l=}J<=h>-jh}K<}<cL>K[}llk]K=jJLH--}+[{]=k{]}-<k{-jh{]l>Llj>}h=h<H}]J0jh-hkL=lh}j]}LKJ=--=[JK<kj]J-hL-h==h=K+H<<hh+]jK-jHjH-jL>h-<jk>kkK+ljH{K{kH=}KklJk>{kj>=>Hlj>jHLLLclh[}]K[ck<{=JjHkh-j{h}+c}}-h<kj=JL[=Hj}K>HkJ{k>c>{h=}hjjJLK>hhllh]{lJjjH=-HkHl-<LhkH->L>=KHJ<L+KjJ+hLL}[H}>jKl{=}>=<-l-hl>hll}kh{l{+{[->h>J<l<[hH-{kkj=<LHLHLlJ]L<l+>+JK=}ll<cJK<kjk-jk+[j<L{-jkhKLkHj>l]K-K{{-K<{L<]Kkk<",
             [[-8.0, 38.35, 100.0, 33.3], [31.18, 58.39, 9.65, 3.21], [50.98, 35.05, 2.0, 2.0]], 1, 0x16f58bcf5516a895),
        ];
        for (size, keys, crops, ratio, hash) in walks {
            let mut stage = Stage::with(gradient(size.0, size.1));
            stage.on(Row::Crop).keys(&[ENTER]);
            let mut walked = Vec::new();
            for key in keys.bytes() {
                stage.keys(&[&[key]]);
                walked.push(stage.crop());
            }
            assert_eq!([walked[99], walked[249], walked[399]], crops, "{size:?}");
            let reprs = walked
                .iter()
                .flatten()
                .flat_map(|part| format!("{part:?}").into_bytes());
            assert_eq!((stage.page.ratio, fnv(reprs)), (ratio, hash), "{size:?}");
        }
    }

    #[test]
    fn the_crop_editor_s_screen() {
        // RenderPageDrawTest.test_crop_editor of tests/test_viewer.py:320; CROP; rows R13, R15, K5, V15: the screen is one synchronized update
        let mut stage = Stage::new();
        stage.draw(100, 30);
        stage.on(Row::Crop).keys(&[ENTER]);
        (stage.page.options.crop, stage.page.ratio) = ([12.5, -3.33, 47.37, 100.0], 4);
        let out = stage.draw(100, 30);
        let rows = column(&out, 30);
        assert_eq!(rows[..15], CROP);
        assert!(rows[15..].iter().all(String::is_empty));
        // over the cleared preview: the frame titled with the window in px, and the source inside
        assert!(out.starts_with(CLEARED) && has(&out, &top_edge(" source · crop 758x900 ", 14)));
        let (at, keys, pixels) = placed(&out).unwrap();
        assert_eq!((at, keys.as_str()), ((9, 48), PLACED));
        assert_eq!(pixels, *stage.page.crop_canvas((459, 560)).as_raw());
        // the same crop again: the text alone
        let again = stage.draw(100, 30);
        assert_eq!(column(&again, 30), rows);
        assert!(placed(&again).is_none() && !has(&again, "┌"));
        // a key that leaves the window as it is sends nothing, one that moves it sends the picture again
        stage.keys(&[b"x"]);
        assert!(placed(&stage.draw(100, 30)).is_none());
        stage.keys(&[b"l"]);
        let moved = stage.draw(100, 30);
        assert!(moved.starts_with(CLEARED) && placed(&moved).is_some());
        assert!(has(&moved, " source · crop 758x900 "));
        assert_eq!(column(&moved, 30)[4], "   X          17.5%   (280 px)");
        // so does a new window size
        assert!(placed(&stage.draw(90, 30)).is_some());
        // back in the menu the preview goes up again, drawn for the new crop
        stage.keys(&[ENTER]);
        let menu = stage.draw(90, 30);
        assert!(menu.starts_with(CLEARED) && has(&menu, " 1618x1920 · 135 columns "));
        assert_eq!(placed(&menu).unwrap().2, *stage.preview().as_raw());
    }

    #[test]
    fn the_crop_editor_s_picture() {
        // row R13; (picture, crop, room) -> (size, FNV-1a 64 of the pixels): RenderPage.crop_canvas of bin/ascii-viewer:846 at tag python-final on pictures of the one colour 200, 100, 50, run by python3
        type Case = ((u32, u32), [f64; 4], (u32, u32), (u32, u32), u64);
        #[rustfmt::skip]
        let cases: [Case; 10] = [
            ((1600, 900), [0.0, 0.0, 100.0, 100.0], (459, 560), (459, 258), 0x29736a725f906dd3),
            ((1600, 900), [10.0, 0.0, 50.0, 100.0], (459, 560), (459, 258), 0x5080b9f439e7064d),
            ((1600, 900), [12.5, -3.33, 47.37, 61.03], (459, 560), (459, 258), 0xb7eaff04efb2358d),
            ((1600, 900), [-60.0, -40.0, 100.0, 100.0], (459, 560), (459, 231), 0x9c1ca215783c8747),
            ((1600, 900), [80.0, 90.0, 150.0, 30.0], (1000, 300), (924, 300), 0x6a6dbd9bcdf657f5),
            ((1600, 900), [49.0, 49.0, 2.0, 2.0], (459, 560), (459, 258), 0xa97fe8ff4840dddd),
            ((1600, 900), [0.0, 0.0, 100.0, 100.0], (81, 20), (36, 20), 0xa59c365d45faebab),
            ((333, 1000), [20.0, 10.0, 60.5, 33.3], (700, 900), (300, 900), 0x16178b4970cfb52d),
            ((40, 30), [0.0, 0.0, 100.0, 100.0], (1200, 800), (1067, 800), 0x925240867c079865),
            ((1000, 667), [-300.0, -300.0, 400.0, 400.0], (640, 480), (640, 427), 0x7a66eae0ebcce5c5),
        ];
        let flat = |(w, h): (u32, u32)| RgbImage::from_pixel(w, h, Rgb([200, 100, 50]));
        for (size, crop, room, canvas, hash) in cases {
            let mut stage = Stage::with(flat(size));
            stage.page.options.crop = crop;
            let drawn = stage.page.crop_canvas(room);
            let pixels = fnv(drawn.as_raw().iter().copied());
            assert_eq!(
                (drawn.dimensions(), pixels),
                (canvas, hash),
                "{size:?} {crop:?}"
            );
        }
        // the second by its parts along row 129: 2400 x 1350 px of the source's at 459 / 2400, the picture from column 76 to 381, the window from 107 to 260
        let mut stage = Stage::with(flat((1600, 900)));
        stage.page.options.crop = [10.0, 0.0, 50.0, 100.0];
        let drawn = stage.page.crop_canvas((459, 560));
        let (ground, dimmed, bright) = ([24; 3], [80, 40, 20], [200, 100, 50]);
        let (edge, third) = ([248, 248, 242], [150; 3]);
        #[rustfmt::skip]
        let along = [
            (0, ground), (75, ground), (76, dimmed), (106, dimmed), (107, edge), (108, edge), (109, bright), (157, bright), (158, third),
            (159, bright), (209, third), (258, bright), (259, edge), (260, edge), (261, dimmed), (381, dimmed), (382, ground), (458, ground),
        ];
        for (x, colour) in along {
            assert_eq!(drawn.get_pixel(x, 129).0, colour, "column {x}");
        }
        // down column 183: the picture and the window from row 43 to 215, the thirds on rows 100 and 157
        #[rustfmt::skip]
        let down = [(0, ground), (42, ground), (43, edge), (44, edge), (45, bright), (100, third), (157, third), (213, bright), (214, edge), (215, edge), (216, ground)];
        for (y, colour) in down {
            assert_eq!(drawn.get_pixel(183, y).0, colour, "row {y}");
        }
    }

    #[test]
    fn a_short_window_keeps_the_prompt_and_the_message_on_its_last_rows() {
        // row R2a, change N16: the menu takes 27 rows, the prompt belongs on the 29th
        let mut stage = Stage::new();
        stage.on(Row::Level).keys(&[ENTER, b"7"]);
        let prompt = " Level (-95..95, up darker): 7_";
        for (rows, at) in [
            (40, 29),
            (29, 29),
            (28, 28),
            (27, 27),
            (20, 20),
            (4, 4),
            (1, 1),
        ] {
            let column = stage.column(rows);
            assert_eq!(column[at - 1], prompt, "{rows} rows");
            assert_eq!(column.iter().filter(|row| *row == prompt).count(), 1);
            // the rows above stay the menu's
            let above = (at - 1).min(27);
            assert_eq!(column[..above], MENU[..above], "{rows} rows");
        }
        // a message likewise
        stage.typed("x");
        let message = " not a value: 7x";
        for (rows, at) in [(40, 29), (29, 29), (28, 28), (12, 12), (1, 1)] {
            let column = stage.column(rows);
            assert_eq!(column[at - 1], message, "{rows} rows");
            assert_eq!(column.iter().filter(|row| *row == message).count(), 1);
        }
        // both, when a render fails while a value is typed: the prompt above the message
        stage.keys(&[ENTER, b"7"]);
        let failure = " render failed: no theme";
        stage.page.message = failure.trim_start().to_string();
        for (rows, at, under) in [
            (40, 29, 31),
            (31, 29, 31),
            (30, 29, 30),
            (29, 28, 29),
            (10, 9, 10),
            (2, 1, 2),
        ] {
            let column = stage.column(rows);
            assert_eq!(
                (column[at - 1].as_str(), column[under - 1].as_str()),
                (prompt, failure),
                "{rows} rows"
            );
        }
        // under a list too
        stage.keys(&[ESC, b"/"]);
        stage.page.message = failure.trim_start().to_string();
        assert_eq!(stage.column(8)[5..], ["", " /_", failure]);
    }

    #[test]
    fn a_message_is_cut_to_the_column_and_cleared_by_the_next_key() {
        // row R17
        let mut stage = Stage::new();
        stage.on(Row::Level).keys(&[ENTER]);
        stage.typed(&"x".repeat(50));
        assert_eq!(
            stage.page.message,
            format!("not a value: {}", "x".repeat(50))
        );
        assert_eq!(
            stage.column(30)[28],
            format!(" not a value: {}", "x".repeat(31))
        );
        let any: [&[u8]; 4] = [b"j", b"x", ENTER, b"\x1bOP"];
        for key in any {
            stage.page.message = "exported pic-ascii.png".into();
            stage.on(Row::Level).keys(&[key]);
            assert!(stage.page.message.is_empty(), "{key:?}");
        }
        // a long file name is cut too
        stage.page.out = stage.home.0.join(format!("{}.png", "n".repeat(60)));
        let rows = stage.on(Row::Level).column(30);
        assert_eq!(rows[1], format!(" \x1b[90m{}\x1b[0m", "n".repeat(44)));
    }

    #[test]
    fn s_exports_the_png_from_the_settings_as_they_are() {
        // row R16, change N15
        let mut stage = Stage::small();
        std::fs::write(&stage.page.out, "what was there").unwrap();
        // no draw before it: the file needs no preview
        assert_eq!(stage.keys(&[b"s"]), After::Stay);
        assert_eq!(stage.page.message, "exported pic-ascii.png");
        assert_eq!(
            image::image_dimensions(&stage.page.out).unwrap(),
            (1920, 1080)
        );
        // Enter on Export, with other settings: 1080 on the long side of the left half, bold, 60 columns
        stage.on(Row::Size).keys(&[b"l"]);
        stage.on(Row::CropW).keys(&[ENTER]);
        stage.typed("50");
        stage.on(Row::Columns).keys(&[ENTER]);
        stage.typed("60");
        stage.on(Row::Bold).keys(&[ENTER]);
        assert_eq!(stage.on(Row::Export).keys(&[ENTER]), After::Stay);
        assert_eq!(stage.page.message, "exported pic-ascii.png");
        // the converter's own drawing for these settings
        let settings = stage.page.settings(0);
        let size = (settings.columns, settings.width, settings.height);
        assert_eq!(size, (Some(60), Some(960), Some(1080)));
        assert_eq!(
            (settings.crop, settings.look.bold),
            ([0.0, 0.0, 50.0, 100.0], true)
        );
        let drawn = convert::draw(&stage.page.picture, &settings, false).unwrap();
        assert!(drawn.dimensions() == (960, 1080) && stage.saved() == drawn);
        // the message shows under the menu
        assert_eq!(stage.column(30)[28], " exported pic-ascii.png");
    }

    #[test]
    fn an_export_that_fails_says_why() {
        // row R16a, change N12
        let mut stage = Stage::small();
        stage.page.custom = Some((160, 90));
        stage.page.out = stage.home.0.join("no such folder/pic-ascii.png");
        stage.keys(&[b"s"]);
        let message = stage.page.message.clone();
        assert!(
            message.starts_with("not exported: ") && message.contains("os error 2"),
            "{message}"
        );
        assert_eq!(
            stage.column(30)[28],
            cut(&format!(" {message}"), 45).trim_end()
        );
        // the page goes on
        stage.page.out = stage.home.0.join("pic-ascii.png");
        stage.keys(&[b"s"]);
        assert!(stage.page.message == "exported pic-ascii.png" && stage.page.out.exists());
    }

    #[test]
    fn a_render_that_fails_is_a_message_and_the_page_stays_usable() {
        // RenderPageKeysTest.test_a_failing_render_does_not_hang_export and test_s_after_a_failed_render_says_why_nothing_was_exported of tests/test_viewer.py:237, :256; row R17
        let mut stage = Stage::small();
        stage.page.custom = Some((320, 180));
        assert!(placed(&stage.draw(100, 30)).is_some());
        let theme = std::fs::read(stage.theme_file()).unwrap();
        std::fs::remove_file(stage.theme_file()).unwrap();
        // the same settings: nothing is drawn again, nothing fails
        stage.draw(100, 30);
        assert!(stage.page.message.is_empty() && stage.page.preview.is_some());
        // new settings: the reason, cut to the column; the screen cleared, no frame, no picture
        stage.keys(&[b"l"]);
        let out = stage.draw(100, 30);
        let reason = format!(
            "cannot read the kitty theme {}: ",
            stage.theme_file().display()
        );
        let message = stage.page.message.clone();
        assert!(
            message.starts_with(&format!("render failed: {reason}")),
            "{message}"
        );
        assert_eq!(
            column(&out, 30)[28],
            cut(&format!(" {message}"), 45).trim_end()
        );
        assert!(out.starts_with(CLEARED) && placed(&out).is_none() && !has(&out, "┌"));
        // not tried again for the same settings
        stage.page.message.clear();
        stage.draw(100, 30);
        assert!(stage.page.message.is_empty());
        // s says why nothing was exported, at once
        stage.keys(&[b"s"]);
        let message = stage.page.message.clone();
        assert!(
            message.starts_with(&format!("not exported: {reason}")),
            "{message}"
        );
        assert!(!stage.page.out.exists());
        // the next key clears it; the keys go on working
        assert_eq!(stage.keys(&[b"j", b"l"]), After::Stay);
        assert!(stage.page.message.is_empty() && stage.page.options.look.contrast == 5);
        // with the theme back the next settings are drawn
        std::fs::write(stage.theme_file(), theme).unwrap();
        stage.keys(&[b"l"]);
        let out = stage.draw(100, 30);
        assert!(stage.page.message.is_empty() && placed(&out).is_some());
        stage.keys(&[b"s"]);
        assert!(stage.page.message == "exported pic-ascii.png" && stage.page.out.exists());
    }

    #[test]
    fn a_crop_over_the_pixel_limit_is_refused() {
        // rows R13, R17: the crop editor's window has no upper bound; a picture may have 179 megapixels
        let mut stage = Stage::new();
        stage.on(Row::Crop).keys(&[ENTER]);
        stage.keys(&[&b"="[..]; 60]);
        stage.keys(&[ENTER]);
        // 2170 % of 1600 x 900 a side: 678 megapixels (the crop: RenderPage.crop_key of bin/ascii-viewer:812 at tag python-final, run by python3)
        assert_eq!(stage.crop(), [-1035.24, -1035.25, 2170.48, 2170.48]);
        let out = stage.draw(100, 30);
        assert_eq!(
            stage.page.message,
            "render failed: crop over 179 megapixels"
        );
        assert!(stage.page.preview.is_none() && placed(&out).is_none());
        stage.keys(&[b"s"]);
        assert_eq!(stage.page.message, "not exported: crop over 179 megapixels");
        assert!(!stage.page.out.exists());
        // the menu still shows it
        assert_eq!(stage.page.value(Row::CropW), "2170.48%");
    }

    #[test]
    fn a_drawing_taller_than_libchafa_takes_is_refused() {
        // row R17: libchafa 1.18.3 ends the process on a picture or a canvas over 65535 px a side (seen: 64 x 65536 px, and 8193 rows of 8 px at 8 columns; the second in the Python tools too)
        let mut stage = Stage::small();
        // a crop 45 times taller than wide: 20 rows of cells a column
        for (row, typed) in [
            (Row::CropW, "5"),
            (Row::CropH, "400"),
            (Row::Columns, "410"),
        ] {
            stage.on(row).keys(&[ENTER]);
            stage.typed(typed);
        }
        assert_eq!(
            (stage.page.target(), stage.page.columns()),
            ((42, 1920), 410)
        );
        let out = stage.draw(100, 30);
        let refusal = "410 x 8201 cells: libchafa takes 8191 a side at most";
        assert_eq!(stage.page.message, format!("render failed: {refusal}"));
        assert!(stage.page.preview.is_none() && placed(&out).is_none());
        stage.keys(&[b"s"]);
        assert_eq!(stage.page.message, format!("not exported: {refusal}"));
        assert!(!stage.page.out.exists());
        // the crop itself over 65535 px tall, 10 % of 320 px by 36500 % of 180: 32 x 65700 px, reduced to fit, at 8 columns
        stage.on(Row::Columns).keys(&[b"`"]);
        stage.page.options.crop = [0.0, 0.0, 10.0, 36500.0];
        assert!(
            placed(&stage.draw(100, 30)).is_some(),
            "{}",
            stage.page.message
        );
    }

    #[test]
    fn the_rows_are_in_menu_order_and_their_hints_state_the_limits() {
        assert!(
            ROWS.iter()
                .enumerate()
                .all(|(at, (row, ..))| *row as usize == at)
        );
        assert_eq!(
            (ROWS[SETTINGS - 1].0, ROWS[SETTINGS].0),
            (Row::Mono, Row::Export)
        );
        let hint = |row: Row| ROWS[row as usize].2.to_string();
        let span = |(low, high): (i32, i32)| format!("{low}..{high}");
        assert_eq!(
            hint(Row::Level),
            format!("{}, up darker", span(look::LEVEL))
        );
        assert_eq!(hint(Row::Contrast), span(look::CONTRAST));
        assert_eq!(
            hint(Row::Saturation),
            format!("percent, {}", span(look::SATURATION))
        );
        assert_eq!(
            hint(Row::Density),
            format!("px {}..{}", DENSITY.0, DENSITY.1)
        );
        assert_eq!(hint(Row::Columns), format!("{}..{}", COLUMNS.0, COLUMNS.1));
        assert_eq!(hint(Row::Size), format!("WxH, each {}..{}", PX.0, PX.1));
    }

    #[test]
    fn a_frame_s_title_is_centred_as_python_centres_it() {
        // (title, cells inside, rows) -> the frame: RenderPage.frame of bin/ascii-viewer:1208 at tag python-final, run by python3
        #[rustfmt::skip]
        let cases = [
            (" 1920x1080 · 160 columns ", 10, 4, "\x1b[1;47H\x1b[90m┌ 1920x1080┐\x1b[0m\x1b[2;47H\x1b[90m│\x1b[2;58H│\x1b[0m\x1b[3;47H\x1b[90m│\x1b[3;58H│\x1b[0m\x1b[4;47H\x1b[90m└──────────┘\x1b[0m"),
            (" ab ", 18, 3, "\x1b[1;47H\x1b[90m┌─────── ab ───────┐\x1b[0m\x1b[2;47H\x1b[90m│\x1b[2;66H│\x1b[0m\x1b[3;47H\x1b[90m└──────────────────┘\x1b[0m"),
            (" ab ", 19, 3, "\x1b[1;47H\x1b[90m┌──────── ab ───────┐\x1b[0m\x1b[2;47H\x1b[90m│\x1b[2;67H│\x1b[0m\x1b[3;47H\x1b[90m└───────────────────┘\x1b[0m"),
            (" abc ", 19, 3, "\x1b[1;47H\x1b[90m┌─────── abc ───────┐\x1b[0m\x1b[2;47H\x1b[90m│\x1b[2;67H│\x1b[0m\x1b[3;47H\x1b[90m└───────────────────┘\x1b[0m"),
            (" abc ", 18, 3, "\x1b[1;47H\x1b[90m┌────── abc ───────┐\x1b[0m\x1b[2;47H\x1b[90m│\x1b[2;66H│\x1b[0m\x1b[3;47H\x1b[90m└──────────────────┘\x1b[0m"),
        ];
        for (title, wide, rows, drawn) in cases {
            let frame = String::from_utf8(frame(title, wide, rows)).unwrap();
            assert_eq!(frame, drawn, "{title:?} in {wide}");
        }
    }

    #[test]
    fn two_decimals_as_python_rounds_them() {
        // round(x, 2) of python3, -0.0 as 0.0
        #[rustfmt::skip]
        let cases = [
            (0.125, 0.12), (0.375, 0.38), (2.675, 2.67), (1.005, 1.0), (0.285, 0.28), (47.365, 47.37), (33.335, 33.34), (-0.125, -0.12),
            (-0.001, 0.0), (0.005, 0.01), (100.0, 100.0), (-12.345, -12.35), (8.345, 8.35), (5.015, 5.01), (1.115, 1.11), (949.7249999, 949.72),
            (2.5, 2.5), (1e-09, 0.0), (136.0315, 136.03), (219.2515, 219.25), (256.6355, 256.64), (359.71549999999996, 359.72),
            (217.9295, 217.93), (345.6275, 345.63), (-279.69550000000004, -279.7), (25.9365, 25.94), (360.3505, 360.35), (154.2825, 154.28),
            (330.6305, 330.63), (-220.7555, -220.76),
        ];
        for (x, rounded) in cases {
            assert_eq!(round2(x), rounded, "{x:?}");
        }
        assert!(round2(-0.001).is_sign_positive());
    }

    #[test]
    fn no_run_of_keys_breaks_the_page() {
        // every key the page knows and some it does not, 2000 of them by a fixed draw of numbers, each drawn in a window of another size; no export: a file a key would be slow
        let mut stage = Stage::small();
        #[rustfmt::skip]
        let keys: [&[u8]; 40] = [
            b"j", b"j", b"k", b"h", b"l", b"l", UP, DOWN, LEFT, RIGHT, ENTER, ENTER, b"\n", ESC, b"q", b"`", b"/", b"\t", BACKSPACE, b"\x08",
            b"H", b"J", b"K", b"L", b"-", b"=", b"+", b"[", b"]", b"<", b">", b"{", b"}", b"c", b"0", b"1", b"6", b"x", b"t", b"\x1bOP",
        ];
        let mut seed = 7u32;
        let mut pick = |upto: u32| {
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            (seed >> 8) % upto
        };
        for round in 0..2000 {
            let key = keys[pick(40) as usize];
            let exports =
                matches!(stage.page.state, State::Menu) && stage.page.at == Row::Export as usize;
            if !(exports && matches!(key, b"\r" | b"\n")) {
                stage.page.key(key);
            }
            // a window with a frame now and then, while a drawing for it is quick
            let (framed, light) = (round % 100 == 0, stage.page.columns() <= 400);
            let cols = if framed && light {
                58 + pick(120)
            } else {
                pick(58)
            };
            let (rows, epoch) = (pick(60), pick(3) as u64);
            let out = stage
                .page
                .draw(window(cols, rows), epoch, &mut stage.transfer);
            assert_eq!(column(&out, rows).len(), rows as usize);
        }
        assert!(!stage.page.out.exists());
    }
}
