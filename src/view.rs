//! The viewer: the event loop, the modes (video, audio, still), the layers over them (render page, help), the status row and toasts.

use crate::cells::{self, CELL_PX, Inks};
use crate::convert::{self, Options};
use crate::desktop::{self, Scope};
use crate::look::{self, Look, Skin};
use crate::mpris::{Mpris, Request, State};
use crate::mpv::{Event, Mpv};
use crate::page::{After, Page};
use crate::picture::{self, Opened};
use crate::still::Gallery;
use crate::term::{self, Geometry, Key, Term, Wake, kitty};
use crate::theme::Theme;
use crate::{NAME, Result, err, font};
use image::RgbImage;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant};

/// Seconds a toast shows.
const TOAST: Duration = Duration::from_millis(1200);
/// A frame arriving sooner after the last drawn one is skipped: at most about 30 a second.
const FRAME_GAP: Duration = Duration::from_millis(30);
const HELP: &str = include_str!("keys.txt");
const F1: [&[u8]; 3] = [b"\x1bOP", b"\x1b[11~", b"\x1b[P"];

/// What a background thread hands back to the loop (through the channel, then `term::wake(Wake::Job)`).
pub enum Job {
    /// An SVG part for the still view: its key and the pixels (`None`: it failed).
    Part(crate::still::Part),
    /// A clipboard copy ended: the toast to show.
    Copied(String),
}

/// What every mode and layer draws with: the terminal, its geometry, the look, the theme, the toast.
pub struct Ui {
    pub term: Term,
    pub geometry: Geometry,
    pub look: Look,
    pub theme: Theme,
    pub inks: Inks,
    /// The terminal shows kitty images (asked once at start).
    pub graphics: bool,
    pub transfer: kitty::Transfer,
    /// Where background threads send their results.
    pub jobs: Sender<Job>,
    /// Bumped whenever what is on screen was covered or the window changed: holders of "already sent" records compare it.
    pub epoch: u64,
    toast: Option<(String, Instant)>,
    /// What the next flush draws: 0 nothing, 1 the screen, 2 from a cleared screen.
    stale: u8,
}

impl Ui {
    pub fn say(&mut self, text: impl Into<String>) {
        self.toast = Some((text.into(), Instant::now() + TOAST));
        self.redraw();
    }

    /// Mark the screen for the next flush: a batch of keys draws once.
    pub fn redraw(&mut self) {
        self.stale = self.stale.max(1);
    }

    /// As `redraw`, from a cleared screen.
    pub fn clear(&mut self) {
        self.stale = 2;
    }

    /// The window's room for a picture in px.
    pub fn room(&self) -> (f64, f64) {
        (
            self.geometry.cols as f64 * self.geometry.cell_w,
            self.geometry.rows as f64 * self.geometry.cell_h,
        )
    }

    /// The font's cell ratio on screen, for libchafa's geometry.
    pub fn ratio(&self) -> f32 {
        (self.geometry.cell_w / self.geometry.cell_h) as f32
    }

    /// A picture (packed RGB) as ASCII, centred in the window: positioned rows in the look's glyphs and colours (row P20).
    pub fn ascii(&mut self, rgb: &mut [u8], w: u32, h: u32) -> Result<Vec<u8>> {
        if let Some(table) = self.look.tone() {
            look::apply(&table, rgb);
        }
        let ramp = font::ramp(self.look.bold)?;
        let (cols, rows) = (self.geometry.cols, self.geometry.rows);
        let grid = cells::grid(
            rgb,
            w,
            h,
            cols,
            Some(rows),
            self.ratio(),
            self.look.glyphs,
            self.look.mono,
            &mut self.inks,
            ramp,
            0.2,
        );
        let (top, left) = (
            (rows as usize - grid.rows.min(rows as usize)) / 2,
            (cols as usize - grid.cols.min(cols as usize)) / 2,
        );
        let mut out = Vec::new();
        for (i, row) in cells::text(&grid, self.look.mono.then_some(self.inks.mono))
            .iter()
            .enumerate()
        {
            out.extend(
                format!(
                    "\x1b[{};{}H{}",
                    top + i + 1,
                    left + 1,
                    if self.look.bold { "\x1b[1m" } else { "" }
                )
                .bytes(),
            );
            out.extend(row);
            out.extend(b"\x1b[0m");
        }
        Ok(out)
    }

    /// The toast row, or nothing when no toast shows.
    fn toast_row(&self) -> Option<Vec<u8>> {
        let (text, until) = self.toast.as_ref()?;
        (Instant::now() < *until).then(|| reverse_row(self.geometry, text))
    }
}

/// ` text ` in reverse video on the bottom row, cut to the width.
pub fn reverse_row(geometry: Geometry, text: &str) -> Vec<u8> {
    let cut: String = text
        .chars()
        .take(geometry.cols.saturating_sub(2) as usize)
        .collect();
    format!("\x1b[{};1H\x1b[7m {cut} \x1b[0m", geometry.rows).into_bytes()
}

/// What mpv plays and what the viewer last heard of it.
struct Playback {
    mpv: Mpv,
    audio: bool,
    paused: bool,
    time: f64,
    length: Option<f64>,
    /// Percent.
    speed: i32,
    volume: Option<f64>,
    muted: bool,
    looping: bool,
    /// After MPRIS Stop: paused with the mark `.`, until a seek or `space`.
    stopped: bool,
    /// What the user last asked for: tells the natural end from a step onto the last frame.
    playing: bool,
    /// The picture's display size, once known.
    shape: Option<(u32, u32)>,
    drawn: Instant,
    /// A frame was skipped for the cap: draw it when no newer one comes.
    owed: bool,
    /// The last frame as positioned ASCII rows.
    rows: Vec<u8>,
    /// What a frame grab is for, while one is under way.
    grab: Option<Grab>,
}

enum Grab {
    Shot,
    Page,
}

enum Mode {
    Video(Playback),
    Audio(Playback, Scope),
    Still(Box<Gallery>),
}

struct Viewer {
    ui: Ui,
    mode: Mode,
    page: Option<Page>,
    /// The first line shown of the keys, while they show.
    help: Option<usize>,
    /// Playback to resume when the help or the page closes.
    resume: bool,
    mpris: Option<Mpris>,
    file: PathBuf,
    results: Receiver<Job>,
    quit: bool,
}

/// Run `asciier FILE`; the exit code.
pub fn main(file: &Path) -> u8 {
    if !term::is_tty() {
        eprintln!("usage: asciier FILE (needs a terminal)");
        return 1;
    }
    match run(file) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("{NAME}: {}", e.0);
            1
        }
    }
}

fn wake_mpv() {
    term::wake(Wake::Mpv);
}

fn run(file: &Path) -> Result<()> {
    // everything that can fail with a message happens before the alternate screen
    let theme = Theme::current()?;
    let still = match picture::open(file) {
        Opened::Still(still) => Some(still),
        Opened::TooLarge(message) => err!("{message}"),
        Opened::Animation | Opened::NotImage => None,
    };
    let media = match still {
        Some(_) => None,
        None => {
            if !file.is_file() {
                err!("{}: not a file", file.display());
            }
            font::ramp(false)?;
            let mut mpv = Mpv::new(&format!("{NAME}-{}", std::process::id()), wake_mpv)?;
            let (video, audio) = mpv.open(&std::fs::canonicalize(file)?, 10.0)?;
            if !video && !audio {
                err!("no video or audio stream in {}", file.display());
            }
            Some((mpv, video, audio))
        }
    };
    let look = Look::default();
    let inks = Inks::new(&theme, look.saturation, Skin::Grey);
    let mut term = Term::enter(NAME)?;
    desktop::ligatures(true);
    let graphics = still.is_some() && kitty::named() && kitty::probe();
    let geometry = term.geometry();
    let (jobs, results) = channel();
    let ui = Ui {
        term,
        geometry,
        look,
        theme,
        inks,
        graphics,
        transfer: kitty::Transfer::default(),
        jobs,
        epoch: 0,
        toast: None,
        stale: 1,
    };
    let mut mpris = None;
    let mode = match (still, media) {
        (Some(still), _) => {
            Mode::Still(Box::new(Gallery::new(file.to_path_buf(), still, graphics)))
        }
        (None, Some((mut mpv, video, audio))) => {
            mpris = Mpris::start();
            if !video {
                mpv.no_video();
            }
            let playback = Playback {
                mpv,
                audio,
                paused: false,
                time: 0.0,
                length: None,
                speed: 100,
                volume: None,
                muted: false,
                looping: false,
                stopped: false,
                playing: true,
                shape: None,
                drawn: Instant::now() - FRAME_GAP,
                owed: false,
                rows: Vec::new(),
                grab: None,
            };
            playback.mpv.set("pause", "no");
            if video {
                Mode::Video(playback)
            } else {
                Mode::Audio(
                    playback,
                    Scope::start(&format!("{NAME}-{}", std::process::id())),
                )
            }
        }
        (None, None) => unreachable!("a file is a still or media"),
    };
    let mut viewer = Viewer {
        ui,
        mode,
        page: None,
        help: None,
        resume: false,
        mpris,
        file: file.to_path_buf(),
        results,
        quit: false,
    };
    viewer.run();
    viewer.leave();
    Ok(())
}

impl Viewer {
    fn playback(&mut self) -> Option<&mut Playback> {
        match &mut self.mode {
            Mode::Video(p) | Mode::Audio(p, _) => Some(p),
            Mode::Still(_) => None,
        }
    }

    fn run(&mut self) {
        self.flush();
        while !self.quit {
            let mut fds = [
                libc::pollfd {
                    fd: 0,
                    events: libc::POLLIN,
                    revents: 0,
                },
                libc::pollfd {
                    fd: self.ui.term.wake_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                },
                libc::pollfd {
                    fd: self.mpris.as_ref().map_or(-1, Mpris::fd),
                    events: libc::POLLIN,
                    revents: 0,
                },
            ];
            let wait = self
                .timeout()
                .map_or(-1, |d| d.as_millis().min(i32::MAX as u128) as i32);
            unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, wait) };
            if fds[1].revents != 0 {
                for wake in self.ui.term.wakes() {
                    match wake {
                        Wake::Quit => self.quit = true,
                        Wake::Resize => self.resized(),
                        Wake::Mpv => self.mpv_events(),
                        Wake::Job => self.jobs(),
                    }
                }
            }
            self.serve_mpris();
            if fds[0].revents != 0 {
                match self.ui.term.keys() {
                    Some(keys) => self.keys(&keys),
                    None => self.quit = true, // the terminal is gone
                }
            }
            self.tick();
            if !self.quit {
                self.flush();
            }
        }
    }

    /// How long the loop may sleep: until the toast ends, or the owed frame is due.
    fn timeout(&self) -> Option<Duration> {
        let now = Instant::now();
        let toast = self
            .ui
            .toast
            .as_ref()
            .map(|(_, until)| until.saturating_duration_since(now));
        let owed = match &self.mode {
            Mode::Video(p) if p.owed => Some((p.drawn + FRAME_GAP).saturating_duration_since(now)),
            _ => None,
        };
        [toast, owed].into_iter().flatten().min()
    }

    /// Things that happen by time: the toast's end, the frame the cap owes.
    fn tick(&mut self) {
        if self
            .ui
            .toast
            .as_ref()
            .is_some_and(|(_, until)| Instant::now() >= *until)
        {
            self.ui.toast = None;
            self.ui.redraw();
        }
        let live = self.help.is_none() && self.page.is_none();
        if let Mode::Video(p) = &mut self.mode
            && p.owed
            && p.drawn.elapsed() >= FRAME_GAP
            && live
        {
            self.frame();
        }
    }

    fn resized(&mut self) {
        self.ui.term.resized();
        self.ui.geometry = self.ui.term.geometry();
        self.ui.epoch += 1;
        self.ui.clear();
        self.refresh();
        self.flush();
    }

    /// Draw the current video frame again (new look, new size, a layer closed).
    fn refresh(&mut self) {
        if matches!(self.mode, Mode::Video(_)) && self.help.is_none() && self.page.is_none() {
            self.frame();
        }
    }

    /// Render mpv's current frame as ASCII rows and mark the screen.
    fn frame(&mut self) {
        let Mode::Video(p) = &mut self.mode else {
            return;
        };
        let Some((dw, dh)) = p.shape else { return };
        let ui = &mut self.ui;
        let (cw, ch) =
            cells::geometry(dw, dh, ui.geometry.cols, Some(ui.geometry.rows), ui.ratio());
        let (w, h) = (cw * CELL_PX, ch * CELL_PX);
        (p.drawn, p.owed) = (Instant::now(), false);
        if let Some(mut rgb) = p.mpv.render(w, h)
            && let Ok(rows) = ui.ascii(&mut rgb, w, h)
        {
            p.rows = rows;
            ui.redraw();
        }
    }

    fn mpv_events(&mut self) {
        let live = self.help.is_none() && self.page.is_none();
        let Some(p) = self.playback() else { return };
        let (mut frame, mut grabbed, mut ended) = (false, None, false);
        for event in p.mpv.events() {
            match event {
                Event::Frame => frame = true,
                Event::Pause(paused) => p.paused = paused,
                Event::Time(time) => p.time = time,
                Event::Length(length) => p.length = length,
                Event::Speed(factor) => p.speed = (factor * 100.0).round() as i32,
                Event::Volume(volume) => p.volume = Some(volume),
                Event::Mute(muted) => p.muted = muted,
                Event::Loop(looping) => p.looping = looping,
                Event::Shape { w, h, turn } => {
                    p.shape = Some(if turn % 180 == 90 { (h, w) } else { (w, h) })
                }
                Event::Ended(true) if p.playing && !p.looping => ended = true,
                Event::Grabbed(picture) => grabbed = Some(picture),
                Event::Ended(_) | Event::Loaded { .. } | Event::Failed(_) => {}
            }
        }
        if ended {
            self.quit = true;
            return;
        }
        let p = self.playback().expect("media mode");
        if frame {
            if !live || p.drawn.elapsed() < FRAME_GAP {
                p.mpv.skip();
                p.owed = live;
            } else {
                self.frame();
            }
        }
        if let Some(picture) = grabbed {
            self.grabbed(picture);
        }
        self.ui.redraw(); // the status row follows the clock
    }

    fn jobs(&mut self) {
        while let Ok(job) = self.results.try_recv() {
            match job {
                Job::Copied(toast) => self.ui.say(toast),
                Job::Part(part) => {
                    if let Mode::Still(gallery) = &mut self.mode {
                        gallery.part_done(part, &mut self.ui);
                    }
                }
            }
        }
    }

    fn state(&self) -> Option<State> {
        let (Mode::Video(p) | Mode::Audio(p, _)) = &self.mode else {
            return None;
        };
        let status = if p.stopped {
            "Stopped"
        } else if p.paused {
            "Paused"
        } else {
            "Playing"
        };
        Some(State {
            status,
            position: p.time,
            length: p.length,
            rate: p.speed as f64 / 100.0,
            volume: p.audio.then_some(p.volume).flatten(),
            looping: p.looping,
            file: self.file.clone(),
        })
    }

    fn serve_mpris(&mut self) {
        let Some(state) = self.state() else { return };
        let Some(mpris) = &mut self.mpris else { return };
        let requests = mpris.serve(&state);
        if self.page.is_some() {
            return; // the page holds playback paused: requests are dropped there
        }
        for request in requests {
            let playing = state.status == "Playing";
            match request {
                Request::PlayPause => self.toggle(),
                Request::Play if !playing => self.toggle(),
                Request::Pause if playing => self.toggle(),
                Request::Play | Request::Pause => {}
                Request::Seek(us) => self.seek(us as f64 / 1e6),
                Request::SetPosition(us) => self.seek(us as f64 / 1e6 - state.position),
                Request::Previous => self.seek(-state.position),
                Request::Stop => {
                    if let Some(p) = self.playback() {
                        (p.stopped, p.playing) = (true, false);
                        p.mpv.set("pause", "yes");
                    }
                }
                Request::Quit => self.quit = true,
            }
            self.ui.redraw();
        }
    }

    /// `space`: pause, play; stopped: from 0 s.
    fn toggle(&mut self) {
        let Some(p) = self.playback() else { return };
        if p.stopped {
            p.mpv.command(&["seek", "0", "absolute+exact"]);
        }
        let play = p.stopped || p.paused;
        (p.stopped, p.playing) = (false, play);
        p.mpv.set("pause", if play { "no" } else { "yes" });
    }

    /// Seek by `delta` seconds, exactly; from the stopped state playback starts there.
    fn seek(&mut self, delta: f64) {
        let Some(p) = self.playback() else { return };
        p.mpv
            .command(&["seek", &format!("{delta}"), "relative+exact"]);
        if p.stopped {
            (p.stopped, p.playing) = (false, true);
            p.mpv.set("pause", "no");
        }
    }

    fn keys(&mut self, keys: &[Key]) {
        let mut delta = 0.0; // seek keys that piled up add up to one seek
        for key in keys {
            let key = key.as_slice();
            if key == b"\x03" {
                self.quit = true;
                return;
            }
            let seeks =
                !matches!(self.mode, Mode::Still(_)) && self.help.is_none() && self.page.is_none();
            if let Some(step) = seek_step(key).filter(|_| seeks) {
                delta += step;
                continue;
            }
            if delta != 0.0 {
                self.seek(delta);
                delta = 0.0;
            }
            self.key(key);
            if self.quit {
                return;
            }
        }
        if delta != 0.0 {
            self.seek(delta);
            self.ui.redraw();
        }
        if self.page.is_some() {
            self.ui.redraw();
        }
    }

    fn key(&mut self, key: &[u8]) {
        if self.help.is_some() {
            return self.help_key(key);
        }
        let confirming = matches!(&self.mode, Mode::Still(g) if g.confirming());
        if F1.contains(&key) && !confirming {
            return self.open_help();
        }
        if let Some(page) = &mut self.page {
            if page.key(key) == After::Close {
                self.close_page();
            }
            return;
        }
        match &mut self.mode {
            Mode::Still(gallery) => {
                // the still's own keys first; the look, s, r, f and q are shared with video
                if gallery.key(key, &mut self.ui)
                    || !matches!(
                        key,
                        b"q" | b"Q"
                            | b"`"
                            | b"f"
                            | b"s"
                            | b"b"
                            | b"c"
                            | b"g"
                            | b"r"
                            | b"1"
                            | b"2"
                            | b"3"
                            | b"4"
                            | b"7"
                            | b"8"
                    )
                {
                    return;
                }
            }
            Mode::Audio(..) => {
                // the oscilloscope: nothing of the picture to adjust
                if !matches!(
                    key,
                    b"q" | b"Q"
                        | b" "
                        | b"p"
                        | b"m"
                        | b"L"
                        | b"f"
                        | b"["
                        | b"]"
                        | b"{"
                        | b"}"
                        | b"\x7f"
                        | b"\x08"
                        | b"\\"
                        | b"0"
                        | b"9"
                        | b"*"
                        | b"/"
                ) {
                    return;
                }
            }
            Mode::Video(_) => {}
        }
        match key {
            b"q" | b"Q" => self.quit = true,
            b" " | b"p" => self.toggle(),
            b"]" | b"[" | b"}" | b"{" | b"\x7f" | b"\x08" | b"\\" => self.speed(key),
            b"3" | b"4" | b"1" | b"2" | b"`" | b"7" | b"8" | b"c" | b"b" | b"g" => {
                self.look_key(key)
            }
            b"r" => self.open_page(),
            b"." | b"," => self.step(key == b"."),
            b"s" => self.screenshot(),
            b"f" => desktop::fullscreen(),
            b"0" | b"*" | b"9" | b"/" => self.volume(if matches!(key, b"0" | b"*") {
                2.0
            } else {
                -2.0
            }),
            b"m" => {
                if let Some(p) = self.playback() {
                    p.muted = !p.muted;
                    p.mpv.set("mute", if p.muted { "yes" } else { "no" });
                    let toast = if p.muted { "muted" } else { "unmuted" };
                    self.ui.say(toast);
                }
            }
            b"L" => {
                if let Some(p) = self.playback() {
                    p.looping = !p.looping;
                    p.mpv.set("loop-file", if p.looping { "inf" } else { "no" });
                    let toast = format!("loop {}", if p.looping { "on" } else { "off" });
                    self.ui.say(toast);
                }
            }
            _ => {}
        }
        self.ui.redraw();
    }

    fn speed(&mut self, key: &[u8]) {
        let Some(p) = self.playback() else { return };
        let s = p.speed as f64;
        let percent = match key {
            b"]" => s * 1.1,
            b"[" => s / 1.1,
            b"}" => s * 2.0,
            b"{" => s / 2.0,
            _ => 100.0,
        };
        p.speed = (percent.round_ties_even() as i32).clamp(25, 400);
        p.mpv.set("speed", &format!("{}", p.speed as f64 / 100.0));
        let toast = format!("speed {}%", p.speed);
        self.ui.say(toast);
    }

    fn volume(&mut self, step: f64) {
        let Some(p) = self.playback() else { return };
        let toast = match p.volume.filter(|_| p.audio) {
            None => "silent".to_string(),
            Some(volume) => {
                let volume = (volume + step).clamp(0.0, p.mpv.volume_max());
                p.volume = Some(volume);
                p.mpv.set("volume", &format!("{volume}"));
                format!("volume {}", volume.round_ties_even())
            }
        };
        self.ui.say(toast);
    }

    /// `.` `,`: one frame forward or back, then paused; nothing while stopped.
    fn step(&mut self, forward: bool) {
        let Mode::Video(p) = &mut self.mode else {
            return;
        };
        if p.stopped {
            return;
        }
        p.playing = false;
        p.mpv.command(&[if forward {
            "frame-step"
        } else {
            "frame-back-step"
        }]);
    }

    fn look_key(&mut self, key: &[u8]) {
        let look = &mut self.ui.look;
        let toast = match key {
            b"`" => {
                (look.level, look.contrast) = (0, 0);
                "level 0, contrast 0".to_string()
            }
            b"3" | b"4" => {
                look.level =
                    look::clamp(look.level + if key == b"3" { 5 } else { -5 }, look::LEVEL);
                format!("level {}", look::level_label(look.level))
            }
            b"1" | b"2" => {
                look.contrast = look::clamp(
                    look.contrast + if key == b"2" { 5 } else { -5 },
                    look::CONTRAST,
                );
                format!("contrast {}", look::contrast_label(look.contrast))
            }
            b"7" | b"8" => {
                look.saturation = look::clamp(
                    look.saturation + if key == b"8" { 10 } else { -10 },
                    look::SATURATION,
                );
                self.ui.inks = Inks::new(&self.ui.theme, look.saturation, Skin::Grey);
                format!("saturation {}%", look.saturation)
            }
            b"g" => {
                look.glyphs = look.glyphs.step(1);
                format!("glyphs: {}", look.glyphs.name())
            }
            b"b" => {
                look.bold = !look.bold;
                if look.bold { "bold" } else { "regular" }.to_string()
            }
            _ => {
                look.mono = !look.mono;
                if look.mono { "mono" } else { "colour" }.to_string()
            }
        };
        self.ui.say(toast);
        self.ui.epoch += 1;
        self.refresh();
    }

    /// The look as converter options for a picture of this file.
    fn options(&self) -> Options {
        Options {
            look: Look {
                skin: Skin::Grey,
                ..self.ui.look
            },
            input: self.file.clone(),
            ..Options::default()
        }
    }

    /// `s`: the picture on screen saved as the converter draws it, beside the file.
    fn screenshot(&mut self) {
        match &mut self.mode {
            Mode::Audio(..) => {}
            Mode::Video(p) => {
                if p.shape.is_some() && p.grab.is_none() {
                    p.grab = Some(Grab::Shot);
                    p.mpv.grab();
                    self.ui.say("screenshot ...");
                }
            }
            Mode::Still(gallery) => {
                let Some(picture) = gallery.source() else {
                    return;
                };
                let out = convert::out_name(gallery.path(), None, false);
                self.ui.say("screenshot ...");
                self.flush();
                self.save(&picture, &out, false);
            }
        }
    }

    fn save(&mut self, picture: &RgbImage, out: &Path, frame: bool) {
        let saved = convert::draw(picture, &self.options(), frame)
            .and_then(|drawn| picture::png(&drawn))
            .and_then(|png| Ok(std::fs::write(out, png)?));
        let name = out.file_name().unwrap_or_default().to_string_lossy();
        self.ui.say(if saved.is_ok() {
            format!("saved {name}")
        } else {
            "screenshot failed".to_string()
        });
    }

    /// The frame asked of mpv arrived: the screenshot's, or the render page's.
    fn grabbed(&mut self, picture: Option<RgbImage>) {
        let Mode::Video(p) = &mut self.mode else {
            return;
        };
        let (grab, time) = (p.grab.take(), (p.time * 10.0).round_ties_even() / 10.0);
        let out = convert::out_name(&self.file, Some(time), false);
        match (grab, picture) {
            (Some(Grab::Shot), Some(picture)) => self.save(&picture, &out, true),
            (Some(Grab::Shot), None) => self.ui.say("screenshot failed"),
            (Some(Grab::Page), Some(picture)) => self.enter_page(picture, out),
            (Some(Grab::Page), None) => self.ui.say("no frame"),
            (None, _) => {}
        }
    }

    /// `r`: the render page for the picture on screen; playback pauses meanwhile.
    fn open_page(&mut self) {
        match &mut self.mode {
            Mode::Audio(..) => {}
            Mode::Video(p) => {
                if p.shape.is_some() && p.grab.is_none() {
                    p.grab = Some(Grab::Page);
                    p.mpv.grab();
                }
            }
            Mode::Still(gallery) => {
                if let Some(picture) = gallery.source() {
                    let out = convert::out_name(gallery.path(), None, false);
                    self.enter_page(picture, out);
                }
            }
        }
    }

    fn enter_page(&mut self, picture: RgbImage, out: PathBuf) {
        match Page::new(picture, out, self.options()) {
            Ok(page) => {
                self.resume = self.pause_for_layer();
                self.page = Some(page);
                self.ui.clear();
            }
            Err(e) => self.ui.say(e.0),
        }
    }

    fn close_page(&mut self) {
        self.page = None;
        self.unlayer();
    }

    /// Pause playing media for a layer; whether it was playing.
    fn pause_for_layer(&mut self) -> bool {
        match self.playback() {
            Some(p) if !p.paused => {
                p.mpv.set("pause", "yes");
                true
            }
            _ => false,
        }
    }

    /// A layer closed: what it covered is drawn again, playback goes on.
    fn unlayer(&mut self) {
        self.ui.term.write(kitty::CLEAR);
        self.ui.epoch += 1;
        if self.resume && self.help.is_none() && self.page.is_none() {
            self.resume = false;
            if let Some(p) = self.playback() {
                p.mpv.set("pause", "no");
            }
        }
        self.ui.clear();
        self.refresh();
    }

    /// F1: the keys over the screen; playing media pauses, the scope is stopped with its cursor and colour saved.
    fn open_help(&mut self) {
        self.help = Some(0);
        if self.page.is_none() {
            self.resume = self.pause_for_layer();
        }
        if let Mode::Audio(_, scope) = &mut self.mode
            && scope.stop()
        {
            self.ui.term.write(b"\x1b7");
        }
        self.ui.clear();
    }

    /// The help's lines for the player or the page, prose wrapped to the window.
    fn help_lines(&self) -> Vec<String> {
        let (player, page) = HELP.split_at(
            HELP.find("\nRender page (r):")
                .map_or(HELP.len(), |at| at + 1),
        );
        let text = if self.page.is_some() { page } else { player };
        let width = self.ui.geometry.cols.saturating_sub(2).max(20) as usize;
        let mut lines = Vec::new();
        for line in text.trim_end().lines() {
            if line.contains("  ") || line.chars().count() <= width {
                lines.push(line.to_string()); // a table row: cut at draw time
                continue;
            }
            let mut row = String::new();
            for word in line.split(' ') {
                if !row.is_empty() && row.chars().count() + 1 + word.chars().count() > width {
                    lines.push(std::mem::take(&mut row));
                }
                if !row.is_empty() {
                    row.push(' ');
                }
                row.push_str(word);
            }
            lines.push(row);
        }
        lines
    }

    /// A key while the keys show: `j k` Down Up scroll them when they do not fit, any other closes them.
    fn help_key(&mut self, key: &[u8]) {
        let (lines, room) = (
            self.help_lines().len(),
            self.ui.geometry.rows.saturating_sub(1).max(1) as usize,
        );
        let down = matches!(key, b"j" | b"\x1b[B");
        if (down || matches!(key, b"k" | b"\x1b[A")) && lines > room {
            let top = self.help.unwrap_or(0);
            self.help = Some(if down {
                (top + 1).min(lines - room)
            } else {
                top.saturating_sub(1)
            });
            return self.ui.redraw();
        }
        self.help = None;
        if let Mode::Audio(_, scope) = &mut self.mode {
            self.ui.term.write(b"\x1b[2J\x1b8");
            scope.resume();
        }
        self.unlayer();
    }

    fn help_screen(&mut self) -> Vec<u8> {
        let (lines, geometry) = (self.help_lines(), self.ui.geometry);
        let room = geometry.rows.saturating_sub(1).max(1) as usize;
        let top = self.help.unwrap_or(0).min(lines.len().saturating_sub(room));
        self.help = Some(top);
        let mut out = [b"\x1b[?2026h\x1b[0m", kitty::CLEAR, b"\x1b[2J"].concat();
        let cut = |text: &str| {
            text.chars()
                .take(geometry.cols.saturating_sub(1) as usize)
                .collect::<String>()
        };
        for (i, line) in lines.iter().skip(top).take(room).enumerate() {
            out.extend(format!("\x1b[{};1H {}", i + 1, cut(line)).bytes());
        }
        let hint = format!(
            "{}any other key closes",
            if lines.len() > room {
                "j k scroll   "
            } else {
                ""
            }
        );
        out.extend(
            format!(
                "\x1b[{};1H\x1b[2m {}\x1b[0m\x1b[?2026l",
                geometry.rows,
                cut(&hint)
            )
            .bytes(),
        );
        out
    }

    /// The status row as positioned writes (row P16): a toast while one shows, else state, time and progress.
    fn status(&self) -> Vec<u8> {
        let geometry = self.ui.geometry;
        if let Mode::Still(gallery) = &self.mode
            && let Some(question) = gallery.question(geometry.cols)
        {
            return reverse_row(geometry, &question);
        }
        if let Some(toast) = self.ui.toast_row() {
            return toast;
        }
        let (Mode::Video(p) | Mode::Audio(p, _)) = &self.mode else {
            return Vec::new();
        };
        let cols = geometry.cols as usize;
        let mark = if p.stopped {
            '.'
        } else if p.paused {
            '|'
        } else {
            '>'
        };
        let clock = |t: f64| format!("{:02}:{:02}", t as u64 / 60, t as u64 % 60);
        let now = p
            .length
            .map_or(p.time, |length| p.time.min(length))
            .max(0.0);
        let mut text = format!("{mark} {}", clock(now));
        if let Some(length) = p.length {
            text += &format!(" / {}", clock(length));
        }
        let room = cols as i64 - text.chars().count() as i64 - if p.looping { 4 } else { 1 };
        let bar = match p.length {
            Some(length) if length > 0.0 && room > 0 => {
                "━".repeat((room as f64 * now / length).round_ties_even() as usize)
            }
            _ => String::new(),
        };
        let line: String = format!("{text} {bar}")
            .trim_end()
            .chars()
            .take(cols)
            .collect();
        let mut out = format!("\x1b[{};1H\x1b[0m{line}", geometry.rows).into_bytes();
        if p.looping {
            out.extend(format!("\x1b[{};{}HL", geometry.rows, cols).bytes());
        }
        out.extend(b"\x1b[0m");
        out
    }

    fn flush(&mut self) {
        if let Mode::Still(gallery) = &mut self.mode {
            gallery.settle(&mut self.ui);
        }
        let stale = std::mem::take(&mut self.ui.stale);
        if stale == 0 {
            return;
        }
        let rows = self.ui.geometry.rows;
        let out = if self.help.is_some() {
            self.help_screen()
        } else if let Some(page) = &mut self.page {
            page.draw(self.ui.geometry, self.ui.epoch, &mut self.ui.transfer)
        } else {
            let status = self.status();
            match &mut self.mode {
                // the scope owns the screen but its spare bottom row; its cursor and colour are kept
                Mode::Audio(..) => [
                    format!("\x1b7\x1b[{rows};1H\x1b[0m\x1b[2K").as_bytes(),
                    &status,
                    b"\x1b8",
                ]
                .concat(),
                Mode::Video(p) => {
                    let clear: &[u8] = if stale == 2 { b"\x1b[2J" } else { b"" };
                    [
                        b"\x1b[?2026h\x1b[0m",
                        clear,
                        format!("\x1b[{rows};1H\x1b[2K").as_bytes(),
                        &p.rows,
                        &status,
                        b"\x1b[?2026l",
                    ]
                    .concat()
                }
                Mode::Still(gallery) => [
                    b"\x1b[?2026h\x1b[0m".as_slice(),
                    &gallery.draw(&mut self.ui, stale == 2),
                    &status,
                    b"\x1b[?2026l",
                ]
                .concat(),
            }
        };
        self.ui.term.write(&out);
    }

    fn leave(&mut self) {
        if let Mode::Audio(_, scope) = &mut self.mode {
            scope.end();
        }
        self.ui.transfer.clear();
        desktop::ligatures(false);
        self.ui.term.leave();
    }
}

/// Seconds a seek key moves: mpv's arrows, plus `h l` and `j k`.
fn seek_step(key: &[u8]) -> Option<f64> {
    Some(match key {
        b"\x1b[C" | b"l" => 5.0,
        b"\x1b[D" | b"h" => -5.0,
        b"\x1b[A" | b"k" => 60.0,
        b"\x1b[B" | b"j" => -60.0,
        b"\x1b[1;2C" => 1.0,
        b"\x1b[1;2D" => -1.0,
        b"\x1b[1;2A" => 5.0,
        b"\x1b[1;2B" => -5.0,
        _ => return None,
    })
}
