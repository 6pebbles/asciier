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
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant};

/// Seconds a toast shows.
const TOAST: Duration = Duration::from_millis(1200);
/// A frame arriving sooner after the last drawn one waits, the newest drawn once the gap is up: at most 30 a second (1/30 s, rounded up).
const FRAME_GAP: Duration = Duration::from_nanos(1_000_000_000u64.div_ceil(30));
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
            1.0, // the picture holds 8 px a cell side already
            self.look.glyphs,
            self.look.mono,
            &mut self.inks,
            ramp,
            0.2,
        )?;
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
    /// What the viewer asked for last, or mpv reported since.
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
    /// What each frame asked of mpv and not yet given is for, in the order asked.
    grabs: VecDeque<Grab>,
}

impl Playback {
    fn pause(&mut self, paused: bool) {
        self.mpv.pause(paused);
        self.paused = paused;
    }

    /// Leave the stopped state: `seek` (mpv's arguments), playing from there.
    fn play_from(&mut self, seek: &[&str]) {
        (self.stopped, self.playing, self.paused) = (false, true, false);
        self.mpv.seek_and_play(seek);
    }
}

#[derive(PartialEq)]
enum Grab {
    /// `s`, with the look of its press.
    Shot(Options),
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
    /// Keys typed while the frame for the render page is on its way: the page's once it opens.
    held: Vec<Key>,
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

/// libchafa's threads for a window: its default, one a core, spends more than it saves on a window's cells (6 s of a 1080p clip at 80 x 24: 2.2 CPU s with one thread, 3.0 with sixteen; at 400 x 100 one thread drops frames).
fn chafa_threads(geometry: Geometry) {
    let cores = std::thread::available_parallelism().map_or(1, |n| n.get()) as u32;
    cells::threads((geometry.cols * geometry.rows / 10_000 + 1).min(cores) as i32);
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
    chafa_threads(geometry);
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
            let mut playback = Playback {
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
                grabs: VecDeque::new(),
            };
            playback.pause(false);
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
        held: Vec::new(),
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
            let wait = poll_wait(self.timeout());
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
        chafa_threads(self.ui.geometry);
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
        let Some((dw, dh)) = p.shape else {
            return p.mpv.skip();
        };
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
        let Some(p) = self.playback() else { return };
        let events = p.mpv.events();
        self.mpv_batch(events);
    }

    /// One batch of mpv's events, as `Mpv::events` hands them.
    fn mpv_batch(&mut self, events: Vec<Event>) {
        let live = self.help.is_none() && self.page.is_none();
        let Some(p) = self.playback() else { return };
        let (mut frame, mut grabbed, mut ended) = (false, Vec::new(), false);
        for event in events {
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
                    p.shape = Some(if turn % 180 == 90 { (h, w) } else { (w, h) });
                    frame = true; // the frame mpv holds: the first one may have come before its shape
                }
                Event::Ended(true) if p.playing && !p.looping => ended = true,
                Event::Grabbed(picture, time) => grabbed.push((picture, time)),
                Event::Ended(_) | Event::Loaded { .. } | Event::Failed(_) => {}
            }
        }
        let p = self.playback().expect("media mode");
        if ended {
            // a picture never drawn shows before the quit: the only frame of a one-frame video, taken undrawn before its shape, was seen with the shape and the end in one batch; any other end quits on the last frame drawn
            if (frame || p.owed) && p.rows.is_empty() && live {
                self.frame();
                self.flush();
            }
            // an `s` whose frame came with the end is saved before the quit; the render page's frame goes unused
            for (picture, time) in grabbed {
                if let Mode::Video(p) = &mut self.mode
                    && p.grabs.front() == Some(&Grab::Page)
                {
                    p.grabs.pop_front();
                    continue;
                }
                self.grabbed(picture, time);
            }
            self.quit = true;
            return;
        }
        if frame {
            if !live || p.drawn.elapsed() < FRAME_GAP {
                p.mpv.skip();
                p.owed = live;
            } else {
                self.frame();
            }
        }
        for (picture, time) in grabbed {
            self.grabbed(picture, time);
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
        let opening = self.opening();
        let Some(mpris) = &mut self.mpris else { return };
        let requests = mpris.serve(&state);
        if self.page.is_some() || opening {
            return; // requests are dropped from `r` on: while the page's frame is on its way and while the page is open (row M5; the old page opened at once)
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
                        p.pause(true);
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
            return p.play_from(&["seek", "0", "absolute+exact"]);
        }
        let play = p.paused;
        p.playing = play;
        p.pause(!play);
    }

    /// Seek by `delta` seconds, exactly; from the stopped state playback starts there.
    fn seek(&mut self, delta: f64) {
        let Some(p) = self.playback() else { return };
        let delta = format!("{delta}");
        let seek = ["seek", &delta, "relative+exact"];
        if p.stopped {
            p.play_from(&seek);
        } else {
            p.mpv.seek(&seek);
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
            if self.opening() {
                self.held.push(key.to_vec());
                continue;
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
        (p.playing, p.paused) = (false, true); // a step ends paused, reported or not
        p.mpv.step(forward);
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
        // the frame drawn again first: a toast's time runs from `say`, and a redraw slow under load, or with the bold face's first load, outlasted it, the toast never drawn
        self.refresh();
        self.ui.say(toast);
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

    /// `s`: the picture on screen saved as the converter draws it with the look of the press, beside the file.
    fn screenshot(&mut self) {
        let options = self.options();
        match &mut self.mode {
            Mode::Audio(..) => {}
            Mode::Video(p) => {
                if p.shape.is_some() && p.mpv.grab() {
                    p.grabs.push_back(Grab::Shot(options));
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
                self.save(&picture, &out, false, &options);
            }
        }
    }

    fn save(&mut self, picture: &RgbImage, out: &Path, frame: bool, options: &Options) {
        let saved = convert::draw(picture, options, frame)
            .and_then(|drawn| picture::png(&drawn))
            .and_then(|png| Ok(std::fs::write(out, png)?));
        let name = out.file_name().unwrap_or_default().to_string_lossy();
        self.ui.say(if saved.is_ok() {
            format!("saved {name}")
        } else {
            "screenshot failed".to_string()
        });
    }

    /// The frame asked of mpv arrived, with its time: the screenshot's, or the render page's, which then takes the keys held for it.
    fn grabbed(&mut self, picture: Option<RgbImage>, time: Option<f64>) {
        let Mode::Video(p) = &mut self.mode else {
            return;
        };
        let time = time.unwrap_or(p.time);
        let (grab, time) = (p.grabs.pop_front(), (time * 10.0).round_ties_even() / 10.0);
        let out = convert::out_name(&self.file, Some(time), false);
        let page = grab == Some(Grab::Page);
        match (grab, picture) {
            (Some(Grab::Shot(options)), Some(picture)) => self.save(&picture, &out, true, &options),
            (Some(Grab::Shot(_)), None) => self.ui.say("screenshot failed"),
            (Some(Grab::Page), Some(picture)) => self.enter_page(picture, out),
            (Some(Grab::Page), None) => self.ui.say("no frame"),
            (None, _) => {}
        }
        if page {
            // typed for the page: with no page they go nowhere
            let held = std::mem::take(&mut self.held);
            if self.page.is_some() {
                self.keys(&held);
            }
        }
    }

    /// The frame for the render page is on its way: keys wait for the page.
    fn opening(&self) -> bool {
        matches!(&self.mode, Mode::Video(p) if p.grabs.contains(&Grab::Page))
    }

    /// `r`: the render page for the picture on screen; playback pauses meanwhile.
    fn open_page(&mut self) {
        match &mut self.mode {
            Mode::Audio(..) => {}
            Mode::Video(p) => {
                if p.shape.is_some() && p.mpv.grab() {
                    p.grabs.push_back(Grab::Page);
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
                // a layer open already keeps its resume
                self.resume |= self.pause_for_layer();
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
                p.pause(true);
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
                p.pause(false);
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
                    // the frame rendered last goes on screen: a step forward on the playing clip goes from it
                    p.mpv.shown();
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

/// A wait as poll takes it, whole milliseconds rounded up, -1 for none: rounded down, the loop spun on a wait of 0 through the last part of a millisecond before an owed frame or a toast's end.
fn poll_wait(wait: Option<Duration>) -> i32 {
    wait.map_or(-1, |d| {
        d.as_nanos().div_ceil(1_000_000).min(i32::MAX as u128) as i32
    })
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::picture::{Ground, Pic, Still};

    /// A `Ui` that draws without a terminal: `geometry`, the look at its defaults, the black-box tests' bl1nk theme, no kitty graphics.
    fn ui(geometry: Geometry) -> Ui {
        let theme = Theme::from_text(include_str!("../tests/bl1nk.conf")).unwrap();
        let look = Look::default();
        Ui {
            term: Term::detached(),
            geometry,
            look,
            inks: Inks::new(&theme, look.saturation, Skin::Grey),
            theme,
            graphics: false,
            transfer: kitty::Transfer::default(),
            jobs: channel().0,
            epoch: 0,
            toast: None,
            stale: 0,
        }
    }

    #[test]
    fn a_still_is_rendered_once_across_a_toast() {
        // row S16: a still's ASCII is rendered as a video frame is, by Ui::ascii from the picture at the grid's px, and kept until the picture, the view, the window or the look changes, so a status-only redraw renders nothing again, as at a toast's end (the cache of bin/ascii-viewer:2215 at tag python-final)
        let colours = RgbImage::from_fn(960, 480, |x, y| {
            image::Rgb([(x * 255 / 959) as u8, (y * 255 / 479) as u8, 96])
        });
        let pic = Pic::Rgb(colours);
        let still = Still {
            pic: pic.clone(),
            svg: None,
        };
        let mut gallery = Gallery::new(PathBuf::from("colours.png"), still, false);
        let mut ui = ui(Geometry {
            cols: 80,
            rows: 24,
            cell_w: 9.0,
            cell_h: 20.0,
        });
        let first = gallery.draw(&mut ui, false);
        // a frame of that shape: 80 x 18 cells, 640 x 144 px for libchafa
        let (cols, rows) = cells::geometry(960, 480, 80, Some(24), ui.ratio());
        let size = (cols * CELL_PX, rows * CELL_PX);
        let mut frame = picture::cut(
            &pic,
            [0.0, 0.0, 960.0, 480.0],
            size,
            false,
            Ground::Colour([128; 3]),
        );
        let frame = ui.ascii(&mut frame, size.0, size.1).unwrap();
        assert!(first.ends_with(&frame), "not drawn as a frame is");
        // a render from here on takes grey inks: a draw equal to the first rendered nothing
        ui.inks = Inks::new(&ui.theme, 0, Skin::Grey);
        ui.say("no kitty graphics");
        assert!(ui.toast_row().is_some());
        assert!(
            gallery.draw(&mut ui, false) == first,
            "rendered for the toast"
        );
        // the toast's end as Viewer::tick has it: gone, and a redraw
        ui.toast = None;
        ui.redraw();
        assert!(ui.toast_row().is_none());
        assert!(gallery.draw(&mut ui, false) == first, "rendered at its end");
        // the screen covered (help, the page, a resize): rendered again, in grey now, once
        ui.epoch += 1;
        let again = gallery.draw(&mut ui, false);
        assert!(again != first, "no render after a cover");
        assert!(gallery.draw(&mut ui, false) == again);
    }

    /// 80 x 24 cells of 9 x 20 px.
    const SMALL: Geometry = Geometry {
        cols: 80,
        rows: 24,
        cell_w: 9.0,
        cell_h: 20.0,
    };

    #[test]
    fn a_still_is_rendered_anew_once_after_each_change() {
        // row S16: a change of the look, the window or the view (zoom, pan) renders the still anew, and once: the draw after it is what a gallery that never rendered draws in that state, and the draw after that renders nothing; the rows alone and the pan keep the grid's px, so only the window and the view in the cache's key tell them
        let colours = RgbImage::from_fn(960, 480, |x, y| {
            image::Rgb([(x * 255 / 959) as u8, (y * 255 / 479) as u8, 96])
        });
        let still = || Still {
            pic: Pic::Rgb(colours.clone()),
            svg: None,
        };
        let path = PathBuf::from("colours.png");
        let mut gallery = Gallery::new(path.clone(), still(), false);
        let mut ui = ui(SMALL);
        let mut shown = gallery.draw(&mut ui, false);
        let mut keys: Vec<&[u8]> = Vec::new();
        for (n, what) in ["look", "window", "zoom", "pan"].into_iter().enumerate() {
            match what {
                "look" => ui.look.level = 5,
                "window" => ui.geometry.rows = 30,
                _ => {
                    let key: &[u8] = if what == "zoom" { b"+" } else { b"l" };
                    assert!(gallery.key(key, &mut ui), "{what}: no still key");
                    keys.push(key);
                }
            }
            let drawn = gallery.draw(&mut ui, false);
            assert!(drawn != shown, "{what}: drawn as before");
            let mut fresh = Gallery::new(path.clone(), still(), false);
            for key in &keys {
                fresh.key(key, &mut ui);
            }
            assert!(
                fresh.draw(&mut ui, false) == drawn,
                "{what}: not rendered anew"
            );
            // other inks from here on: a draw equal to the last one rendered nothing
            ui.inks = Inks::new(&ui.theme, if n % 2 == 0 { 0 } else { 100 }, Skin::Grey);
            assert!(
                gallery.draw(&mut ui, false) == drawn,
                "{what}: rendered again"
            );
            shown = drawn;
        }
    }

    #[test]
    fn a_toast_lasts_1_2_s_from_say() {
        // row V16: a toast's time is 1.2 s from the call that shows it (TOAST, bin/ascii-viewer:118 and :2129 at tag python-final); its row, reverse video cut to cols - 2, shows while that time runs and not after it
        let mut ui = ui(SMALL);
        let before = Instant::now();
        ui.say("level 5% darker");
        let after = Instant::now();
        let (text, until) = ui.toast.clone().expect("a toast");
        assert_eq!(text, "level 5% darker");
        let toast = Duration::from_millis(1200);
        assert!(
            before + toast <= until && until <= after + toast,
            "it ends {:?} after the call began",
            until - before
        );
        ui.toast = Some((text.clone(), Instant::now() + Duration::from_secs(60)));
        assert_eq!(
            ui.toast_row().as_deref(),
            Some(&b"\x1b[24;1H\x1b[7m level 5% darker \x1b[0m"[..])
        );
        ui.geometry.cols = 12;
        assert_eq!(
            ui.toast_row().as_deref(),
            Some(&b"\x1b[24;1H\x1b[7m level 5% d \x1b[0m"[..])
        );
        ui.toast = Some((text, Instant::now()));
        assert_eq!(ui.toast_row(), None);
    }

    #[test]
    fn a_shot_whose_frame_comes_with_the_end_is_saved_before_the_quit() {
        // rows P21, P14: `s` just before a clip's end: mpv's answer, the frame and its time, can come in the batch of events that holds the end, which quits; the shot is saved first, beside the clip and named by the frame's time, and the run then ends; seen before: the end taken first and the shot lost (the steps review, by code reading); the old viewer's converter, started for each press, died with the viewer (P21b), so a shot this late was lost there too
        let _env = crate::mpv::tests::env();
        let home = crate::mpv::tests::Scratch::new();
        for (file, text) in [
            (
                ".config/kitty/current-theme.conf",
                include_str!("../tests/bl1nk.conf"),
            ),
            (".config/mpv/mpv.conf", "ao=null\n"),
        ] {
            let path = home.0.join(file);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        }
        let _home = crate::mpv::tests::Var::set("HOME", &home.0);
        let file = home.0.join("clip.mp4");
        let options = Options {
            input: file.clone(),
            ..Options::default()
        };
        let playback = Playback {
            mpv: Mpv::new("asciier-test", || {}).unwrap(),
            audio: true,
            paused: false,
            time: 0.96,
            length: Some(1.0),
            speed: 100,
            volume: Some(100.0),
            muted: false,
            looping: false,
            stopped: false,
            playing: true,
            shape: Some((160, 90)),
            drawn: Instant::now(),
            owed: false,
            rows: b"\x1b[2;1H the last frame drawn".to_vec(),
            grabs: VecDeque::from([Grab::Shot(options)]),
        };
        let mut viewer = Viewer {
            ui: ui(SMALL),
            mode: Mode::Video(playback),
            page: None,
            held: Vec::new(),
            help: None,
            resume: false,
            mpris: None,
            file,
            results: channel().1,
            quit: false,
        };
        let frame = RgbImage::from_pixel(160, 90, image::Rgb([200, 200, 200]));
        viewer.mpv_batch(vec![
            Event::Grabbed(Some(frame), Some(0.96)),
            Event::Ended(true),
        ]);
        let shot = home.0.join("clip-ascii-1s.png");
        assert!(viewer.quit, "the end quits");
        let saved = image::open(&shot).map(|png| (png.width(), png.height()));
        assert!(matches!(saved, Ok((1920, 1080))), "{shot:?}: {saved:?}");
    }

    #[test]
    fn a_look_keys_toast_runs_from_the_frame_drawn_again() {
        // row V16: a toast shows 1.2 s; a look key draws the frame again, which under load, or with the bold face loaded at a run's first `b`, can outlast that: timed from before the redraw, the toast was never drawn (seen at load 116: `bold` never written after `b`, tests/playback.rs every_run_starts_at_the_defaults; the old viewer timed it at the key too, bin/ascii-viewer:2129 and :2154 at tag python-final); its time now starts once the frame is drawn again, so all of it is left for the draw that shows it
        let _env = crate::mpv::tests::env();
        let home = crate::mpv::tests::Scratch::new();
        std::fs::create_dir_all(home.0.join(".config/mpv")).unwrap();
        std::fs::write(home.0.join(".config/mpv/mpv.conf"), "ao=null\n").unwrap();
        let _home = crate::mpv::tests::Var::set("HOME", &home.0);
        let file = home.0.join("clip.mp4");
        let made = std::process::Command::new("ffmpeg")
            .args(["-v", "error", "-nostdin", "-y", "-f", "lavfi", "-i"])
            .args([
                "testsrc2=s=160x90:r=25:d=1",
                "-c:v",
                "libx264",
                "-pix_fmt",
                "yuv420p",
            ])
            .arg(&file)
            .status()
            .unwrap();
        assert!(made.success());
        let mut mpv = Mpv::new("asciier-test", || {}).unwrap();
        mpv.open(&file, 10.0).unwrap();
        let playback = Playback {
            mpv,
            audio: false,
            paused: true,
            time: 0.0,
            length: Some(1.0),
            speed: 100,
            volume: None,
            muted: false,
            looping: false,
            stopped: false,
            playing: false,
            shape: None,
            drawn: Instant::now() - FRAME_GAP,
            owed: false,
            rows: Vec::new(),
            grabs: VecDeque::new(),
        };
        let mut viewer = Viewer {
            ui: ui(SMALL),
            mode: Mode::Video(playback),
            page: None,
            held: Vec::new(),
            help: None,
            resume: false,
            mpris: None,
            file,
            results: channel().1,
            quit: false,
        };
        // the first frame drawn, as the loop draws it
        let end = Instant::now() + Duration::from_secs(10);
        let drawn = |viewer: &Viewer| matches!(&viewer.mode, Mode::Video(p) if !p.rows.is_empty());
        while !drawn(&viewer) && Instant::now() < end {
            viewer.mpv_events();
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(drawn(&viewer), "no frame drawn");
        let mut wrong = Vec::new();
        for key in [b"b", b"3"] {
            let before = Instant::now();
            viewer.look_key(key);
            let after = Instant::now();
            let (text, until) = viewer.ui.toast.clone().expect("a toast");
            // all of the toast's time left once the key is done
            if until + Duration::from_millis(1) < after + TOAST {
                let left = until.saturating_duration_since(after);
                wrong.push(format!(
                    "{text}: {left:?} left of {TOAST:?}, the key took {:?}",
                    after - before
                ));
            }
        }
        assert!(wrong.is_empty(), "{wrong:#?}");
    }

    #[test]
    fn the_loop_sleeps_to_the_end_of_its_wait() {
        // no inventory row: an owed frame is due 1/30 s after the last drawn, and a 30 fps source's frames come at or just under that gap, so many waits end within a millisecond; each was cut to a wait of 0, and the loop spun on poll until the frame was due
        assert_eq!(poll_wait(None), -1);
        assert_eq!(poll_wait(Some(Duration::ZERO)), 0);
        assert_eq!(poll_wait(Some(Duration::from_nanos(1))), 1);
        assert_eq!(poll_wait(Some(Duration::from_micros(1000))), 1);
        assert_eq!(poll_wait(Some(Duration::from_micros(1001))), 2);
        assert_eq!(poll_wait(Some(FRAME_GAP)), 34);
        assert_eq!(poll_wait(Some(Duration::MAX)), i32::MAX);
    }
}
