//! The still view: zoom, pan, turn, the folder's other images, the image itself or its ASCII, copy, trash.

use crate::picture::{self, Ground, NoCopy, Opened, Pic, Still, Svg};
use crate::term::{self, Wake, kitty};
use crate::theme::Rgb;
use crate::trash::{self, Refused, Trashed};
use crate::view::{Job, Ui};
use crate::{cells, desktop};
use image::RgbImage;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// A still zooms in until one of its pixels covers this many screen px.
const ZOOM_MAX_PX: f64 = 8.0;
/// What transparency shows over in ASCII, where a checkerboard would be noise.
const GREY: Rgb = [128, 128, 128];

/// An SVG part rendered in the background: the view it was asked for and its pixels (`None`: resvg failed).
pub struct Part {
    pub key: String,
    pub image: Option<image::RgbaImage>,
}

/// The still on screen, its view, its folder, and what was trashed in this run.
pub struct Gallery {
    path: PathBuf,
    /// `None` once the folder's last image is gone.
    pic: Option<Pic>,
    svg: Option<Arc<Svg>>,
    /// The picture's size at its first raster: zoom is measured in this.
    logical: (u32, u32),
    /// Quarter turns clockwise.
    rotation: u8,
    turned: Option<(u8, Pic)>,
    /// Zoom steps in, each a factor of 2^0.5; 0 is the fit.
    zoom: i64,
    /// The view's centre, as fractions of the turned image.
    pan: (f64, f64),
    /// The image itself over the kitty protocol, else ASCII.
    image: bool,
    confirming: bool,
    /// `h` `l` not acted on yet: the listing, the index shown, the steps.
    pending: Option<(Vec<PathBuf>, usize, i64)>,
    trashed: Vec<Trashed>,
    oversize: Vec<String>,
    /// What the kitty image on screen shows.
    placed: Option<String>,
    /// The ASCII rows of the last render and what they show.
    ascii: Option<(String, Vec<u8>)>,
    /// The last SVG part rendered, and the one asked for.
    detail: Option<Part>,
    asked: Option<String>,
}

fn name(path: &Path) -> String {
    path.file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned()
}

/// The folder's stills by extension, sorted by lower-cased name.
fn listing(path: &Path) -> Vec<PathBuf> {
    let folder = entry(path)
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_default();
    let mut files: Vec<PathBuf> = std::fs::read_dir(folder)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| picture::listed(p) && p.is_file())
        .collect();
    files.sort_by_key(|p| name(p).to_lowercase());
    files
}

/// The file as its folder lists it: the folder resolved, its own name (a symlink stays itself).
fn entry(path: &Path) -> PathBuf {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    parent
        .canonicalize()
        .unwrap_or_else(|_| parent.to_path_buf())
        .join(path.file_name().unwrap_or_default())
}

impl Gallery {
    /// The still at `path`, unturned, at the fit; `image`: shown as itself over the kitty protocol, else as ASCII.
    pub fn new(path: PathBuf, still: Still, image: bool) -> Gallery {
        let mut gallery = Gallery {
            path: PathBuf::new(),
            pic: None,
            svg: None,
            logical: (1, 1),
            rotation: 0,
            turned: None,
            zoom: 0,
            pan: (0.5, 0.5),
            image,
            confirming: false,
            pending: None,
            trashed: Vec::new(),
            oversize: Vec::new(),
            placed: None,
            ascii: None,
            detail: None,
            asked: None,
        };
        gallery.show(path, still);
        gallery
    }

    /// Make `still` the picture on screen: unturned, at the fit.
    fn show(&mut self, path: PathBuf, still: Still) {
        self.logical = still.pic.size();
        (self.path, self.pic, self.svg) = (path, Some(still.pic), still.svg);
        (self.rotation, self.turned, self.confirming, self.pending) = (0, None, false, None);
        (self.detail, self.asked) = (None, None);
        self.reset_view();
    }

    fn reset_view(&mut self) {
        (self.zoom, self.pan) = (0, (0.5, 0.5));
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn empty(&self) -> bool {
        self.pic.is_none()
    }

    /// The picture turned as on screen, kept while neither changes.
    fn turned(&mut self) -> &Pic {
        if self.rotation == 0 {
            return self.pic.as_ref().expect("a picture");
        }
        if self
            .turned
            .as_ref()
            .is_none_or(|(turns, _)| *turns != self.rotation)
        {
            self.turned = Some((
                self.rotation,
                picture::turned(self.pic.as_ref().expect("a picture"), self.rotation),
            ));
        }
        &self.turned.as_ref().expect("just made").1
    }

    /// The turned image's size at the logical size.
    fn extent(&self) -> (f64, f64) {
        let (w, h) = (self.logical.0 as f64, self.logical.1 as f64);
        if self.rotation % 2 == 1 {
            (h, w)
        } else {
            (w, h)
        }
    }

    /// Screen px per image px at the fit: the picture fills the window, never enlarged.
    fn fit(&self, ui: &Ui) -> f64 {
        let ((w, h), (room_w, room_h)) = (self.extent(), ui.room());
        (room_w / w).min(room_h / h).min(1.0)
    }

    /// The most zoom steps: the last step at or under `ZOOM_MAX_PX` screen px an image px.
    fn limit(&self, ui: &Ui) -> i64 {
        (2.0 * (ZOOM_MAX_PX / self.fit(ui)).log2() + 1e-9) as i64
    }

    /// The zoom steps in force: a count kept from a smaller window can be over the limit.
    fn steps(&self, ui: &Ui) -> i64 {
        self.zoom.min(self.limit(ui))
    }

    fn scale(&self, ui: &Ui, steps: i64) -> f64 {
        self.fit(ui) * 2f64.powf(steps as f64 / 2.0)
    }

    /// The visible part of the turned image (left, top, right, bottom in logical px) at `steps`: the window's room at that scale around the pan, kept inside the image.
    fn view(&self, ui: &Ui, steps: i64) -> [f64; 4] {
        let (w, h) = self.extent();
        if steps == 0 {
            return [0.0, 0.0, w, h];
        }
        let ((room_w, room_h), s) = (ui.room(), self.scale(ui, steps));
        let (bw, bh) = (w.min(room_w / s), h.min(room_h / s));
        let cx = (self.pan.0 * w).max(bw / 2.0).min(w - bw / 2.0);
        let cy = (self.pan.1 * h).max(bh / 2.0).min(h - bh / 2.0);
        [cx - bw / 2.0, cy - bh / 2.0, cx + bw / 2.0, cy + bh / 2.0]
    }

    /// Keep the view's centre as it is shown, inside the image.
    fn settle_view(&mut self, ui: &Ui) {
        let ([left, top, right, bottom], (w, h)) = (self.view(ui, self.steps(ui)), self.extent());
        self.pan = ((left + right) / 2.0 / w, (top + bottom) / 2.0 / h);
    }

    /// `+` and `-`: zoom about the view's centre, between the fit and the limit; in ASCII, steps whose view is still the whole image are passed over.
    fn zoom_by(&mut self, by: i64, ui: &mut Ui) {
        let (limit, (w, h)) = (self.limit(ui), self.extent());
        let mut n = self.steps(ui) + by;
        while !self.image && by != 0 && n > 0 && n <= limit && self.view(ui, n) == [0.0, 0.0, w, h]
        {
            n += by;
        }
        self.zoom = if n > limit { self.steps(ui) } else { n.max(0) };
        self.settle_view(ui);
        let toast = if self.zoom == 0 {
            "fit".to_string()
        } else {
            format!(
                "zoom {}%",
                (self.scale(ui, self.steps(ui)) * 100.0).round_ties_even()
            )
        };
        ui.say(toast);
        ui.clear();
    }

    /// Zoomed in: move the view `dx` of its width and `dy` of its height.
    fn pan_by(&mut self, dx: f64, dy: f64, ui: &mut Ui) {
        let ([left, top, right, bottom], (w, h)) = (self.view(ui, self.steps(ui)), self.extent());
        self.pan = (
            ((left + right) / 2.0 + dx * (right - left)) / w,
            ((top + bottom) / 2.0 + dy * (bottom - top)) / h,
        );
        self.settle_view(ui);
        ui.clear();
    }

    /// An SVG drawn larger than its first raster: the box rasterized alone at `size`, once the background render has it; till then `None`, and the raster shows enlarged.
    fn part(&mut self, bbox: [f64; 4], size: (u32, u32), ui: &Ui) -> Option<&image::RgbaImage> {
        let svg = self.svg.clone()?;
        let key = format!(
            "{}|{}|{:.2?}|{size:?}",
            self.path.display(),
            self.rotation,
            bbox
        );
        if self.detail.as_ref().is_some_and(|part| part.key == key) {
            return self.detail.as_ref().and_then(|part| part.image.as_ref());
        }
        if self.asked.as_ref() != Some(&key) {
            self.asked = Some(key.clone());
            let (logical, rotation, jobs) = (self.logical, self.rotation, ui.jobs.clone());
            // named: tests/stills.rs waits for the parts in flight to end by this name
            std::thread::Builder::new()
                .name("svg part".into())
                .spawn(move || {
                    let image = svg.part(logical, rotation, bbox, size).ok();
                    if jobs.send(Job::Part(Part { key, image })).is_ok() {
                        term::wake(Wake::Job);
                    }
                })
                .expect("failed to spawn thread");
        }
        None
    }

    /// A background SVG part arrived: the newest request's is kept and drawn.
    pub fn part_done(&mut self, part: Part, ui: &mut Ui) {
        if self.asked.as_ref() == Some(&part.key) {
            self.detail = Some(part);
            ui.redraw();
        }
    }

    /// The visible box cut to `size`: from the SVG part when one is there, else from the picture.
    fn cut(
        &mut self,
        bbox: [f64; 4],
        size: (u32, u32),
        scale: f64,
        ground: Ground,
        ui: &Ui,
    ) -> RgbImage {
        if scale > 1.0
            && let Some(part) = self.part(bbox, size, ui)
        {
            let whole = [0.0, 0.0, part.width() as f64, part.height() as f64];
            return picture::cut(&Pic::Rgba(part.clone()), whole, size, false, ground);
        }
        // the picture may be a first raster of another size than the logical one only by rounding: boxes are in its px
        let picture = self.turned().clone();
        let (w, h) = picture.size();
        let clipped = [
            bbox[0].max(0.0),
            bbox[1].max(0.0),
            bbox[2].min(w as f64),
            bbox[3].min(h as f64),
        ];
        picture::cut(&picture, clipped, size, scale > 1.0, ground)
    }

    /// The screen without its status row.
    pub fn draw(&mut self, ui: &mut Ui, clear: bool) -> Vec<u8> {
        let (geometry, rows) = (ui.geometry, ui.geometry.rows);
        let erase_row = format!("\x1b[{rows};1H\x1b[2K").into_bytes();
        if self.empty() {
            self.placed = None;
            return [kitty::CLEAR, b"\x1b[2J", &self.notice(ui)].concat();
        }
        let bbox = self.view(ui, self.steps(ui));
        let (bw, bh) = (bbox[2] - bbox[0], bbox[3] - bbox[1]);
        let what = |size: (u32, u32), this: &Gallery| {
            format!(
                "{}|{}|{:.2?}|{size:?}|{}|{}",
                this.path.display(),
                this.rotation,
                bbox,
                ui.epoch,
                this.detail.as_ref().map_or("", |p| &p.key)
            )
        };
        if self.image {
            let (room_w, room_h) = ui.room();
            let scale = self
                .scale(ui, self.steps(ui))
                .min(room_w / bw)
                .min(room_h / bh);
            let size = (
                ((bw * scale).round_ties_even() as u32).max(1),
                ((bh * scale).round_ties_even() as u32).max(1),
            );
            let key = what(size, self);
            if !clear && self.placed.as_ref() == Some(&key) {
                return erase_row; // the image is there already: the status row alone
            }
            let shown = self.cut(bbox, size, scale, Ground::Checker, ui);
            self.placed = Some(what(size, self));
            let place = ui.transfer.place(
                size.0,
                size.1,
                shown.as_raw(),
                24,
                1,
                1,
                geometry.cols,
                geometry.rows,
                geometry,
            );
            return [kitty::CLEAR, b"\x1b[2J", &place].concat();
        }
        self.placed = None;
        let (cw, ch) = cells::geometry(
            bw.round().max(1.0) as u32,
            bh.round().max(1.0) as u32,
            geometry.cols,
            Some(geometry.rows),
            ui.ratio(),
        );
        let size = (cw * cells::CELL_PX, ch * cells::CELL_PX);
        let scale = (size.0 as f64 / bw).min(size.1 as f64 / bh);
        let key = format!("{}|{:?}|{:?}", what(size, self), ui.look, geometry);
        if self.ascii.as_ref().is_none_or(|(shown, _)| *shown != key) {
            let mut shown = self.cut(bbox, size, scale, Ground::Colour(GREY), ui);
            let key = format!("{}|{:?}|{:?}", what(size, self), ui.look, geometry);
            let rows = match ui.ascii(&mut shown, size.0, size.1) {
                Ok(rows) => rows,
                Err(e) => {
                    ui.say(e.0);
                    Vec::new()
                }
            };
            self.ascii = Some((key, rows));
        }
        let cleared: &[u8] = if clear { b"\x1b[2J" } else { b"" };
        [
            kitty::CLEAR,
            cleared,
            &erase_row,
            &self.ascii.as_ref().expect("just rendered").1,
        ]
        .concat()
    }

    /// The empty folder's two lines, centred: what is missing, and the keys that answer.
    fn notice(&self, ui: &Ui) -> Vec<u8> {
        let (cols, rows) = (ui.geometry.cols as usize, ui.geometry.rows as usize);
        let folder = entry(&self.path)
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_default();
        let shown = if folder.file_name().is_some() {
            name(&folder)
        } else {
            folder.display().to_string()
        };
        let hint = format!(
            "{}q quit",
            if self.trashed.is_empty() {
                ""
            } else {
                "u undo  "
            }
        );
        let mut out = Vec::new();
        for (i, (text, style)) in [(format!("no images in {shown}"), ""), (hint, "\x1b[2m")]
            .into_iter()
            .enumerate()
        {
            let text: String = text.chars().take(cols).collect();
            out.extend(
                format!(
                    "\x1b[{};{}H{style}{text}\x1b[0m",
                    (rows / 2).max(1) + i,
                    (cols - text.chars().count()) / 2 + 1
                )
                .bytes(),
            );
        }
        out
    }

    /// Show the first image that opens, going `way` places at a time from index `at` of `images`, wrapping round; those passed over as too large are kept for the toast.
    fn show_next(&mut self, images: &[PathBuf], at: i64, way: i64, ui: &mut Ui) -> Option<PathBuf> {
        self.oversize.clear();
        let count = images.len() as i64;
        for n in 1..=count {
            let candidate = &images[(at + way * n).rem_euclid(count) as usize];
            match picture::open(candidate) {
                Opened::TooLarge(_) => self.oversize.push(name(candidate)),
                Opened::Still(still) => {
                    self.show(candidate.clone(), still);
                    ui.clear();
                    return Some(candidate.clone());
                }
                Opened::Animation | Opened::NotImage => {}
            }
        }
        None
    }

    fn passed(&self) -> String {
        self.oversize
            .iter()
            .map(|name| format!("; {name} too large"))
            .collect()
    }

    /// Move `by` images along the folder; only counted: a batch of held keys opens one image, in `settle`.
    fn step(&mut self, by: i64, ui: &mut Ui) {
        let (images, at, steps) = self.pending.take().unwrap_or_else(|| {
            let (images, here) = (listing(&self.path), entry(&self.path));
            let at = images.iter().position(|f| *f == here).unwrap_or(0);
            (images, at, 0)
        });
        if !images.is_empty() {
            self.pending = Some((images, at, steps + by));
            ui.clear();
        }
    }

    /// The end of a key batch: show the image the steps reached, as one press at a time would; the files passed are told by their header, so only the last is decoded.
    pub fn settle(&mut self, ui: &mut Ui) {
        let Some((images, mut at, steps)) = self.pending.take() else {
            return;
        };
        let way = steps.signum();
        for _ in 0..steps.abs() {
            for _ in 0..images.len() {
                at = (at as i64 + way).rem_euclid(images.len() as i64) as usize;
                if picture::identifies(&images[at]) {
                    break;
                }
            }
        }
        let way = if way == 0 { 1 } else { way };
        if let Some(shown) = self.show_next(&images, at as i64 - way, way, ui) {
            let place = images.iter().position(|f| *f == shown).map_or(0, |i| i + 1);
            let toast = format!(
                "{}  {place}/{}{}",
                name(&shown),
                images.len(),
                self.passed()
            );
            ui.say(toast);
        }
    }

    /// `here`, a place in `images`, is gone: show the image after it by name, or the empty folder's screen when none left opens.
    fn next_after(&mut self, images: &[PathBuf], here: &Path, ui: &mut Ui) {
        let at = images.iter().position(|f| f == here);
        let rest: Vec<PathBuf> = match at {
            Some(at) => images[at + 1..]
                .iter()
                .chain(&images[..at])
                .cloned()
                .collect(),
            None => images.to_vec(),
        };
        let rest: Vec<PathBuf> = rest
            .into_iter()
            .filter(|f| f != here && f.exists())
            .collect();
        if rest.is_empty() || self.show_next(&rest, -1, 1, ui).is_none() {
            (self.pic, self.svg, self.turned, self.confirming) = (None, None, None, false);
            ui.transfer.clear();
            ui.clear();
        }
    }

    /// `action` on the image on screen, then the next one of the folder and the toast `<done> <name>`; on failure `<verb> failed`, nothing else.
    fn remove(
        &mut self,
        action: impl FnOnce(&Path) -> std::io::Result<Option<Trashed>>,
        verb: &str,
        done: &str,
        ui: &mut Ui,
    ) {
        let (images, here) = (listing(&self.path), entry(&self.path));
        match action(&self.path) {
            Err(_) => ui.say(format!("{verb} failed")),
            Ok(trashed) => {
                self.trashed.extend(trashed);
                let gone = name(&self.path);
                self.next_after(&images, &here, ui);
                let toast = format!("{done} {gone}{}", self.passed());
                ui.say(toast);
            }
        }
    }

    /// `u`: the last image trashed back where it was, and on screen (from the empty folder too); one that cannot go back stays on the stack.
    fn undo(&mut self, ui: &mut Ui) {
        let Some(last) = self.trashed.last().cloned() else {
            return ui.say("nothing to undo");
        };
        let file = name(&last.path);
        match trash::untrash(&last) {
            Err(Refused::Exists) => ui.say(format!("cannot restore: {file} exists")),
            Err(Refused::Gone) => {
                self.trashed.pop(); // nothing to put back, and it would block the earlier entries for good
                ui.say(format!("{file} is no longer in the trash"));
            }
            Err(Refused::Io) => ui.say("restore failed"),
            Ok(()) => {
                self.trashed.pop();
                if let Opened::Still(still) = picture::open(&last.path) {
                    self.show(last.path.clone(), still);
                }
                ui.say(format!("restored {file}"));
                ui.clear();
            }
        }
    }

    /// `y`: the image itself to the clipboard, in a background thread.
    fn copy(&mut self, ui: &mut Ui) {
        let (path, rotation, jobs) = (self.path.clone(), self.rotation, ui.jobs.clone());
        ui.say("copying ...");
        std::thread::spawn(move || {
            let toast = match picture::original_png(&path, rotation) {
                Ok((png, (w, h))) if desktop::copy_png(&png) => format!("copied {w}x{h}"),
                Err(NoCopy::TooLarge) => "too large to copy".to_string(),
                Ok(_) | Err(NoCopy::Failed) => "copy failed".to_string(),
            };
            if jobs.send(Job::Copied(toast)).is_ok() {
                term::wake(Wake::Job);
            }
        });
    }

    /// A key of the still view; false when the key is not the still's: the viewer then tries the keys shared with video. On the empty folder everything but `u`, `q`, `Q`, `f` counts as taken.
    pub fn key(&mut self, key: &[u8], ui: &mut Ui) -> bool {
        let step = match key {
            b"h" | b"\x1b[D" => Some(-1),
            b"l" | b"\x1b[C" => Some(1),
            _ => None,
        };
        if self.pending.is_some() && step.is_none() {
            self.settle(ui); // the next key acts on the image the steps reached
        }
        if self.confirming {
            self.confirming = false;
            if key == b"y" {
                self.remove(
                    |path| std::fs::remove_file(path).map(|()| None),
                    "delete",
                    "deleted",
                    ui,
                );
            } else {
                ui.say(format!("kept {}", name(&self.path)));
            }
            return true;
        }
        if key == b"u" {
            self.undo(ui);
            return true;
        }
        if self.empty() {
            return !matches!(key, b"q" | b"Q" | b"f");
        }
        let zoomed = self.steps(ui) > 0;
        let pan = match key {
            b"h" | b"\x1b[D" => Some((-0.25, 0.0)),
            b"l" | b"\x1b[C" => Some((0.25, 0.0)),
            b"k" | b"\x1b[A" => Some((0.0, -0.25)),
            b"j" | b"\x1b[B" => Some((0.0, 0.25)),
            b"H" | b"\x1b[1;2D" => Some((-0.0625, 0.0)),
            b"L" | b"\x1b[1;2C" => Some((0.0625, 0.0)),
            b"K" | b"\x1b[1;2A" => Some((0.0, -0.0625)),
            b"J" | b"\x1b[1;2B" => Some((0.0, 0.0625)),
            _ => None,
        };
        match key {
            b"d" => self.remove(|path| trash::trash(path).map(Some), "trash", "trashed", ui),
            b"D" => {
                self.confirming = true;
                ui.redraw();
            }
            b"+" | b"=" => self.zoom_by(1, ui),
            b"-" => self.zoom_by(-1, ui),
            b"\x7f" | b"\x08" => self.zoom_by(-self.steps(ui), ui),
            _ if zoomed && pan.is_some() => {
                let (dx, dy) = pan.expect("checked");
                self.pan_by(dx, dy, ui);
            }
            _ if step.is_some() => self.step(step.expect("checked"), ui),
            b"k" | b"\x1b[A" | b"j" | b"\x1b[B" => {
                self.rotation = (self.rotation
                    + if matches!(key, b"k" | b"\x1b[A") {
                        1
                    } else {
                        3
                    })
                    % 4;
                self.reset_view();
                ui.clear();
            }
            b"v" => {
                if !self.image && !ui.graphics {
                    ui.say("no kitty graphics");
                } else {
                    self.image = !self.image;
                    ui.say(if self.image { "image" } else { "ascii" });
                    ui.clear();
                }
            }
            b"y" => self.copy(ui),
            _ => return false,
        }
        true
    }

    /// The whole picture as turned, transparency over #808080: what `s` and `r` take. `None` on the empty folder.
    pub fn source(&mut self) -> Option<RgbImage> {
        if self.empty() {
            return None;
        }
        Some(picture::flatten(self.turned(), GREY))
    }

    /// `D` asked: the status row waits for `y`.
    pub fn confirming(&self) -> bool {
        self.confirming
    }

    /// The `D` question for a status row of `cols` cells, while it waits.
    pub fn question(&self, cols: u32) -> Option<String> {
        if !self.confirming {
            return None;
        }
        let (file, tail) = (name(&self.path), " permanently? y/n");
        let room = (cols as usize)
            .saturating_sub(2 + "delete ".len() + tail.len())
            .max(1);
        let shown = if file.chars().count() <= room {
            file
        } else {
            file.chars().take(room - 1).chain(['…']).collect()
        };
        Some(format!("delete {shown}{tail}"))
    }
}
