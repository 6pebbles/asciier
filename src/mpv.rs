//! libmpv in-process: it decodes, keeps time, seeks, steps and loops; the viewer draws the frames it renders. Driven from one thread with the calls client.h marks safe beside rendering.

use crate::{Result, err};
use image::RgbImage;
use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::time::{Duration, Instant};

/// What mpv reported since the last look.
#[derive(Clone, PartialEq, Debug)]
pub enum Event {
    /// The file is open: which tracks play.
    Loaded {
        video: bool,
        audio: bool,
    },
    /// The file could not be opened or played: mpv's reason.
    Failed(String),
    /// A new frame is due (`Mpv::render`, or `Mpv::skip`); the first one can come before the first `Shape` or after it. Also reported when `events` comes later than mpv waits for a frame to be taken: a render then shows the frame mpv holds.
    Frame,
    Pause(bool),
    /// Position in seconds.
    Time(f64),
    /// Length in seconds; `None` when unknown (or 0).
    Length(Option<f64>),
    /// Speed as a factor (1.0 = 100 %).
    Speed(f64),
    Volume(f64),
    Mute(bool),
    Loop(bool),
    /// The end of the file was reached and mpv has paused there (`keep-open`): the sound has played out. A step or a seek onto the end reports it too; `false`: a seek left the end.
    Ended(bool),
    /// The picture's display size as stored and the clockwise turn (0, 90, 180, 270) the stream asks for: upright it is `h` x `w` for a turn of 90 or 270.
    Shape {
        w: u32,
        h: u32,
        turn: u32,
    },
    /// The frame asked for with `grab`, upright, at source size (`None` when mpv had none), and that frame's time: the position mpv gave right after it took the frame; past the picture's end, where the position is the sound's, the position given with the last frame mpv handed over, later than that frame's by the wait for the next look, and an earlier frame's when mpv dropped the last frames untaken.
    Grabbed(Option<RgbImage>, Option<f64>),
}

pub struct Mpv {
    handle: *mut c_void,
    render: *mut c_void,
    /// What `open` heard while it waited: the next `events` hands it on.
    reported: Vec<Event>,
    /// `dwidth`, `dheight` and `video-dec-params/rotate` as last observed (0: unknown); `reshaped`: one changed since the last `Shape`.
    shape: [i64; 3],
    reshaped: bool,
    /// `pause`, `eof-reached` and `time-pos` as last observed; `ended`: the last `Ended` reported.
    paused: bool,
    at_end: bool,
    time: f64,
    ended: bool,
    /// Steps since the position last moved by other means: `stepped`, the last went forward; `back`, where the last step forward started, if paused, the target of a step back; `before`, `back` before that step, again `back` when the step did not move (on the last frame). `landing`: where a step forward started, until mpv reports the mark of the last one (`steps` sent): by then one that moves has reported a play or a position 1 ms or more away (`moved`).
    stepped: bool,
    back: Option<f64>,
    before: Option<f64>,
    landing: Option<f64>,
    moved: bool,
    steps: u64,
    /// Requests to pause or play that mpv has not confirmed yet: its reports of `pause` until then can be older than them.
    pausing: u32,
    /// Seeks of `seek_and_play` mpv has not answered; `seek_due`: all answered, the last not run yet; `seek_plays`: the play still wanted, which a pause or a step cancels.
    play_seeks: u32,
    seek_due: bool,
    seek_plays: bool,
    /// The frame of a grab whose time mpv has not given yet.
    taken: Option<Option<RgbImage>>,
    /// A new position comes with a new frame, except past the picture's end, where the sound plays on: `frameless` counts the looks (`events` calls, late ones aside: frames may have gone unseen before them) that brought a position and no new frame; `frame_time`, the position given with the last new frame (`None` while it is to come); `first`, the first position of the look under way.
    frameless: u32,
    frame_time: Option<f64>,
    first: Option<f64>,
    /// When `events` last returned.
    looked: Instant,
    /// What mpv renders into, 63 bytes longer than the picture: it starts at a multiple of 64.
    surface: Vec<u8>,
    /// mpv's `volume-max`, read when a file opened.
    volume_max: f64,
}

// client.h and render.h, as far as used.

#[repr(C)]
struct RawEvent {
    id: c_int,
    error: c_int,
    userdata: u64,
    data: *const c_void,
}

#[repr(C)]
struct Property {
    name: *const c_char,
    format: c_int,
    data: *const c_void,
}

/// The first fields of `mpv_event_end_file`.
#[repr(C)]
struct EndFile {
    reason: c_int,
    error: c_int,
}

#[repr(C)]
union Value {
    text: *const c_char,
    whole: i64,
    list: *const List,
    bytes: *const Bytes,
}

#[repr(C)]
struct Node {
    value: Value,
    format: c_int,
}

#[repr(C)]
struct List {
    len: c_int,
    values: *const Node,
    keys: *const *const c_char,
}

#[repr(C)]
struct Bytes {
    data: *const u8,
    len: usize,
}

#[repr(C)]
struct Param {
    kind: c_int,
    data: *mut c_void,
}

#[repr(C)]
struct FrameInfo {
    flags: u64,
    target_time: i64,
}

const EVENT_NONE: c_int = 0;
const GET_PROPERTY_REPLY: c_int = 3;
const SET_PROPERTY_REPLY: c_int = 4;
const COMMAND_REPLY: c_int = 5;
const END_FILE: c_int = 7;
const FILE_LOADED: c_int = 8;
const SEEK: c_int = 20;
const PROPERTY_CHANGE: c_int = 22;
const END_ERROR: c_int = 4;
const END_REDIRECT: c_int = 5;
const STRING: c_int = 1;
const FLAG: c_int = 3;
const INT64: c_int = 4;
const DOUBLE: c_int = 5;
const NODE_MAP: c_int = 8;
const BYTE_ARRAY: c_int = 9;
const OPTION_NOT_FOUND: c_int = -5;
const NOTHING_TO_PLAY: c_int = -16;
const API_TYPE: c_int = 1;
const BLOCK_FOR_TARGET_TIME: c_int = 12;
const SKIP_RENDERING: c_int = 13;
const SW_SIZE: c_int = 17;
const SW_FORMAT: c_int = 18;
const SW_STRIDE: c_int = 19;
const SW_POINTER: c_int = 20;
const NEXT_FRAME_INFO: c_int = 11;
const UPDATE_FRAME: u64 = 1;
const FRAME_PRESENT: u64 = 1;
/// `reply_userdata` of the `screenshot-raw` and the `time-pos` read behind `grab`.
const GRAB: u64 = 1;
/// `reply_userdata` of the `pause` set behind `pause`.
const PAUSE: u64 = 2;
/// `reply_userdata` of the seek behind `seek_and_play`.
const PLAY: u64 = 3;

type Callback = extern "C" fn(*mut c_void);

#[link(name = "mpv")]
unsafe extern "C" {
    fn mpv_create() -> *mut c_void;
    fn mpv_initialize(mpv: *mut c_void) -> c_int;
    fn mpv_terminate_destroy(mpv: *mut c_void);
    fn mpv_error_string(error: c_int) -> *const c_char;
    fn mpv_free(data: *mut c_void);
    fn mpv_set_option_string(mpv: *mut c_void, name: *const c_char, value: *const c_char) -> c_int;
    fn mpv_command_async(mpv: *mut c_void, userdata: u64, args: *const *const c_char) -> c_int;
    fn mpv_set_property_async(
        mpv: *mut c_void,
        userdata: u64,
        name: *const c_char,
        format: c_int,
        value: *mut c_void,
    ) -> c_int;
    fn mpv_get_property_async(
        mpv: *mut c_void,
        userdata: u64,
        name: *const c_char,
        format: c_int,
    ) -> c_int;
    fn mpv_get_property_string(mpv: *mut c_void, name: *const c_char) -> *mut c_char;
    fn mpv_observe_property(
        mpv: *mut c_void,
        userdata: u64,
        name: *const c_char,
        format: c_int,
    ) -> c_int;
    fn mpv_wait_event(mpv: *mut c_void, timeout: f64) -> *const RawEvent;
    fn mpv_set_wakeup_callback(mpv: *mut c_void, callback: Callback, data: *mut c_void);
    fn mpv_render_context_create(
        render: *mut *mut c_void,
        mpv: *mut c_void,
        params: *mut Param,
    ) -> c_int;
    fn mpv_render_context_set_update_callback(
        render: *mut c_void,
        callback: Callback,
        data: *mut c_void,
    );
    fn mpv_render_context_update(render: *mut c_void) -> u64;
    fn mpv_render_context_get_info(render: *mut c_void, param: Param) -> c_int;
    fn mpv_render_context_render(render: *mut c_void, params: *mut Param) -> c_int;
    fn mpv_render_context_free(render: *mut c_void);
}

/// Set before `mpv_initialize`, after mpv.conf's audio options: these win.
const OPTIONS: [(&str, &str); 29] = [
    ("config", "no"),
    ("vo", "libmpv"),
    ("terminal", "no"),
    ("idle", "yes"),
    ("keep-open", "yes"),
    ("hwdec", "no"),
    ("video-rotate", "no"),
    ("keepaspect", "no"),
    ("video-timing-offset", "0"),
    ("audio-display", "no"),
    ("audio-pitch-correction", "no"),
    ("image-display-duration", "inf"),
    ("sid", "no"),
    ("sub-auto", "no"),
    ("osd-level", "0"),
    ("resume-playback", "no"),
    ("save-position-on-quit", "no"),
    ("load-scripts", "no"),
    ("osc", "no"),
    ("ytdl", "no"),
    ("load-stats-overlay", "no"),
    ("load-console", "no"),
    ("load-auto-profiles", "no"),
    ("load-select", "no"),
    ("load-positioning", "no"),
    ("load-commands", "no"),
    ("load-context-menu", "no"),
    ("clipboard-backends", ""),
    ("input-default-bindings", "no"),
];

/// Set behind each step forward: mpv reports properties at the start of each pass, so the report of this value comes once mpv has run a pass past the step, after the others of that pass (observed last).
const STEP_MARK: &CStr = c"user-data/asciier/step";

const OBSERVED: [(&CStr, c_int); 12] = [
    (c"pause", FLAG),
    (c"time-pos", DOUBLE),
    (c"duration", DOUBLE),
    (c"speed", DOUBLE),
    (c"volume", DOUBLE),
    (c"mute", FLAG),
    (c"loop-file", STRING),
    (c"eof-reached", FLAG),
    (c"dwidth", INT64),
    (c"dheight", INT64),
    (c"video-dec-params/rotate", INT64),
    (STEP_MARK, INT64),
];

/// mpv's words for one of its error codes.
fn explain(error: c_int) -> String {
    // SAFETY: mpv returns a static string for every code.
    let words = unsafe { CStr::from_ptr(mpv_error_string(error)) };
    words.to_string_lossy().into_owned()
}

/// Both of mpv's callbacks: `wake` is the function given to `Mpv::new`.
extern "C" fn woken(wake: *mut c_void) {
    // SAFETY: `new` hands the callbacks nothing but that `fn()`.
    let wake = unsafe { std::mem::transmute::<*mut c_void, fn()>(wake) };
    wake();
}

/// The audio options among mpv.conf's top-level lines (those before any `[profile]`), read as mpv reads them: `name=value` or `name` (`yes`), `#` comments, values in quotes or as `%length%text`.
fn audio_options(conf: &str) -> Vec<(String, String)> {
    let mut options = Vec::new();
    for line in conf.trim_start_matches('\u{feff}').lines() {
        let line = line.trim_start();
        if line.starts_with('[') {
            break;
        }
        let line = line.strip_prefix("--").unwrap_or(line);
        let named = |c: char| c.is_ascii_alphanumeric() || c == '_' || c == '-';
        let (name, rest) = line.split_at(line.find(|c| !named(c)).unwrap_or(line.len()));
        let starts = |start: &&str| name.starts_with(*start);
        let audio = matches!(name, "ao" | "af" | "mute")
            || ["volume", "audio-", "replaygain"].iter().any(starts);
        let rest = rest.trim_start();
        let value = match rest.strip_prefix('=').map(str::trim_start) {
            None => (rest.is_empty() || rest.starts_with('#')).then_some("yes"),
            Some(value) => match value.chars().next() {
                Some(quote @ ('"' | '\'')) => value[1..].split_once(quote).map(|(text, _)| text),
                Some('%') => {
                    let counted = value[1..].split_once('%');
                    counted.and_then(|(length, text)| text.get(..length.parse().ok()?))
                }
                _ => Some(value.split('#').next().unwrap_or(value).trim_end()),
            },
        };
        if let (true, Some(value)) = (audio, value) {
            options.push((name.to_string(), value.to_string()));
        }
    }
    options
}

/// Packed RGB from `w` x `h` px of 4 bytes in rows of `stride` bytes (`red`: where red sits, 0 in rgb0, 2 in bgr0), turned clockwise by `turn` degrees.
fn upright(pixels: &[u8], stride: usize, w: usize, h: usize, red: usize, turn: u32) -> Vec<u8> {
    let (wide, high) = if turn % 180 == 90 { (h, w) } else { (w, h) };
    let mut rgb = Vec::with_capacity(wide * high * 3);
    for y in 0..high {
        // a row of the upright picture is a walk through the stored one: where it starts, and the bytes from px to px
        let (start, step) = match turn {
            90 => ((h - 1) * stride + y * 4, -(stride as isize)),
            180 => ((h - 1 - y) * stride + (w - 1) * 4, -4),
            270 => ((w - 1 - y) * 4, stride as isize),
            _ => (y * stride, 4),
        };
        for x in 0..wide {
            let at = (start as isize + x as isize * step) as usize;
            rgb.extend([pixels[at + red], pixels[at + 1], pixels[at + 2 - red]]);
        }
    }
    rgb
}

impl Mpv {
    /// libmpv with the viewer's options, the audio options of `~/.config/mpv/mpv.conf`, a software render context; `wake` is called (from mpv's threads) whenever `events` has something new; `node`: the PipeWire node name (`PIPEWIRE_PROPS` in the own environment).
    pub fn new(node: &str, wake: fn()) -> Result<Mpv> {
        // SAFETY: the caller is the thread that owns the program's state, and libmpv's threads do not run yet.
        unsafe { std::env::set_var("PIPEWIRE_PROPS", format!("{{ node.name = {node} }}")) };
        // SAFETY: no arguments; null is checked.
        let handle = unsafe { mpv_create() };
        if handle.is_null() {
            err!("mpv: no player");
        }
        let mut mpv = Mpv {
            handle,
            render: std::ptr::null_mut(),
            reported: Vec::new(),
            shape: [0; 3],
            reshaped: false,
            paused: false,
            at_end: false,
            ended: false,
            stepped: false,
            time: 0.0,
            back: None,
            before: None,
            landing: None,
            moved: false,
            steps: 0,
            pausing: 0,
            play_seeks: 0,
            seek_due: false,
            seek_plays: false,
            taken: None,
            frameless: 0,
            frame_time: None,
            first: None,
            looked: Instant::now(),
            surface: Vec::new(),
            volume_max: 130.0,
        };
        let conf =
            std::env::var_os("HOME").map(|home| Path::new(&home).join(".config/mpv/mpv.conf"));
        let conf = conf
            .and_then(|file| std::fs::read(file).ok())
            .unwrap_or_default();
        for (name, value) in audio_options(&String::from_utf8_lossy(&conf)) {
            mpv.option(&name, &value); // what mpv cannot use is passed over, as mpv does with its own file
        }
        for (name, value) in OPTIONS {
            let set = mpv.option(name, value);
            if set < 0 && set != OPTION_NOT_FOUND {
                err!("mpv: option {name}={value}: {}", explain(set));
            }
        }
        let mut software = [
            (API_TYPE, c"sw".as_ptr() as *mut c_void),
            (0, std::ptr::null_mut()),
        ]
        .map(|(kind, data)| Param { kind, data });
        // SAFETY: `handle` is valid; the parameters live through the call; the callbacks get the `fn()` they expect.
        unsafe {
            let started = mpv_initialize(handle);
            if started < 0 {
                err!("mpv: {}", explain(started));
            }
            // before any file: without it a video would open as sound alone
            let made = mpv_render_context_create(&mut mpv.render, handle, software.as_mut_ptr());
            if made < 0 {
                err!("mpv: no software renderer: {}", explain(made));
            }
            mpv_set_wakeup_callback(handle, woken, wake as *mut c_void);
            mpv_render_context_set_update_callback(mpv.render, woken, wake as *mut c_void);
            for (name, format) in OBSERVED {
                mpv_observe_property(handle, 0, name.as_ptr(), format);
            }
        }
        Ok(mpv)
    }

    fn option(&self, name: &str, value: &str) -> c_int {
        let (Ok(name), Ok(value)) = (CString::new(name), CString::new(value)) else {
            return OPTION_NOT_FOUND;
        };
        // SAFETY: both strings live through the call.
        unsafe { mpv_set_option_string(self.handle, name.as_ptr(), value.as_ptr()) }
    }

    /// A property read at once: it waits for mpv, which only `open` may.
    fn get(&self, name: &CStr) -> Option<String> {
        // SAFETY: mpv returns null or a string that is freed here.
        unsafe {
            let text = mpv_get_property_string(self.handle, name.as_ptr());
            if text.is_null() {
                return None;
            }
            let value = CStr::from_ptr(text).to_string_lossy().into_owned();
            mpv_free(text as *mut c_void);
            Some(value)
        }
    }

    /// Open `file` (an absolute path) paused and wait for `Loaded` or `Failed`, at most `seconds`.
    pub fn open(&mut self, file: &Path, seconds: f64) -> Result<(bool, bool)> {
        // mpv reads a relative name as it likes: `-` is standard input
        let file = std::path::absolute(file)?;
        self.pause(true);
        self.send(
            0,
            &[
                c"loadfile".into(),
                CString::new(file.as_os_str().as_bytes())?,
            ],
        );
        let start = std::time::Instant::now();
        loop {
            let left = seconds - start.elapsed().as_secs_f64();
            if left <= 0.0 {
                err!("mpv did not open {} within {seconds} s", file.display());
            }
            // SAFETY: mpv keeps the event until the next wait.
            match self.report(unsafe { &*mpv_wait_event(self.handle, left) }) {
                Some(Event::Loaded { video, audio }) => {
                    self.volume_max = self
                        .get(c"volume-max")
                        .and_then(|max| max.parse().ok())
                        .unwrap_or(130.0);
                    return Ok((video, audio));
                }
                Some(Event::Failed(why)) if why == explain(NOTHING_TO_PLAY) => {
                    err!("no video or audio stream in {}", file.display())
                }
                Some(Event::Failed(why)) => return Err(crate::Error(why)),
                event => self.reported.extend(event),
            }
        }
    }

    /// The most the volume goes to (mpv's `volume-max`).
    pub fn volume_max(&self) -> f64 {
        self.volume_max
    }

    /// Audio alone: give up the render context.
    pub fn no_video(&mut self) {
        // SAFETY: the context is freed once; null is allowed.
        unsafe { mpv_render_context_free(self.render) };
        self.render = std::ptr::null_mut();
    }

    /// An mpv command, not waited for (`["seek", "5", "relative+exact"]`).
    pub fn command(&self, args: &[&str]) {
        self.order(0, args);
    }

    /// `command` with mpv's `reply_userdata`; whether mpv took the request.
    fn order(&self, userdata: u64, args: &[&str]) -> bool {
        let args: std::result::Result<Vec<CString>, _> =
            args.iter().map(|arg| CString::new(*arg)).collect();
        args.is_ok_and(|args| self.send(userdata, &args))
    }

    fn send(&self, userdata: u64, args: &[CString]) -> bool {
        let mut pointers: Vec<*const c_char> = args.iter().map(|arg| arg.as_ptr()).collect();
        pointers.push(std::ptr::null());
        // SAFETY: a null-terminated list of strings that live through the call.
        unsafe { mpv_command_async(self.handle, userdata, pointers.as_ptr()) >= 0 }
    }

    /// A seek, `command`'s arguments (`["seek", "5", "relative+exact"]`).
    pub fn seek(&mut self, args: &[&str]) {
        self.forget_steps();
        self.command(args);
    }

    /// The position moves by other means than steps: no step back seeks to where a step started, and no end is a step's.
    fn forget_steps(&mut self) {
        (self.stepped, self.back, self.before, self.landing) = (false, None, None, None);
    }

    /// A seek (`command`'s arguments), then play once mpv has run it (`MPV_EVENT_SEEK`). Played sooner, at once or on mpv's answer (given when it queues the seek, before the requests that reach it meanwhile, a play among them), mpv shows the frame due since it paused and counts a relative seek from that frame.
    pub fn seek_and_play(&mut self, args: &[&str]) {
        self.forget_steps();
        self.reported
            .retain(|event| !matches!(event, Event::Pause(_)));
        if !self.order(PLAY, args) {
            return self.pause(false);
        }
        if self.play_seeks == 0 && !self.seek_due {
            self.pausing += 1;
        }
        (self.play_seeks, self.seek_due, self.seek_plays) = (self.play_seeks + 1, false, true);
    }

    /// The seek of `seek_and_play` ran, or mpv refused it: play, unless a pause or a step came since.
    fn seek_ran(&mut self) {
        self.seek_due = false;
        self.pausing = self.pausing.saturating_sub(1);
        if std::mem::take(&mut self.seek_plays) {
            self.pause(false);
        }
    }

    /// One frame forward or back, then paused; a play that waits for a seek is dropped. A step forward onto the last frame leaves the position at the sound's, past that frame (at the sound's end when the step played it out, as it does when little is left), and mpv's step back counts from there: it finds the last frame again. So a step back right after steps forward that started paused seeks to where the last of them that moved started, and a step forward that ends the file is followed by a seek past the end (in `events`), which holds the last frame at its own time.
    pub fn step(&mut self, forward: bool) {
        self.seek_plays = false;
        if forward {
            let settled = self.paused && self.pausing == 0 && self.landing.is_none();
            self.before = self.back.take();
            (self.back, self.landing) = (settled.then_some(self.time), Some(self.time));
            (self.stepped, self.moved, self.steps) = (true, false, self.steps + 1);
            self.command(&["frame-step"]);
            let mut mark = self.steps as i64;
            // SAFETY: mpv copies the value before the call returns.
            unsafe {
                let mark = &mut mark as *mut i64 as *mut c_void;
                mpv_set_property_async(self.handle, 0, STEP_MARK.as_ptr(), INT64, mark);
            }
            return;
        }
        let back = self.back.take();
        self.forget_steps();
        match back {
            // an exact seek shows the first frame at most 5 ms before its target
            Some(from) => {
                self.command(&["seek", &format!("{}", from + 0.004), "absolute+exact"]);
                self.pause(true);
            }
            None => self.command(&["frame-back-step"]),
        }
    }

    /// Set a property, not waited for.
    pub fn set(&self, name: &str, value: &str) {
        self.put(0, name, value);
    }

    /// Pause or play, not waited for. Reports of `pause` that `open` heard, and those mpv makes before it confirms this request, are not handed on: they can be older than it, and mpv reports only a value that differs from the last it reported.
    pub fn pause(&mut self, paused: bool) {
        self.seek_plays = false;
        if !paused {
            self.forget_steps();
        }
        self.reported
            .retain(|event| !matches!(event, Event::Pause(_)));
        if self.put(PAUSE, "pause", if paused { "yes" } else { "no" }) {
            self.pausing += 1;
        }
    }

    /// `set` with mpv's `reply_userdata`; whether mpv took the request.
    fn put(&self, userdata: u64, name: &str, value: &str) -> bool {
        let (Ok(name), Ok(value)) = (CString::new(name), CString::new(value)) else {
            return false;
        };
        let mut text = value.as_ptr();
        let text = &mut text as *mut *const c_char as *mut c_void;
        // SAFETY: mpv copies the value before the call returns.
        unsafe { mpv_set_property_async(self.handle, userdata, name.as_ptr(), STRING, text) >= 0 }
    }

    /// Everything mpv reported since the last call; never blocks.
    pub fn events(&mut self) -> Vec<Event> {
        // mpv waits 200 ms at most for a frame it hands over to be taken, then holds it as shown and says no more of it
        let late = !self.render.is_null() && self.looked.elapsed() > Duration::from_millis(200);
        let mut events = std::mem::take(&mut self.reported);
        loop {
            // SAFETY: mpv keeps the event until the next wait.
            let event = unsafe { &*mpv_wait_event(self.handle, 0.0) };
            if event.id == EVENT_NONE {
                break;
            }
            events.extend(self.report(event));
        }
        if std::mem::take(&mut self.reshaped)
            && let Some((w, h, turn)) = self.shape()
        {
            events.push(Event::Shape { w, h, turn });
        }
        // mpv flags the end when the last sound is handed to the device, and pauses when it has played
        let ended = self.at_end && self.paused;
        if ended != std::mem::replace(&mut self.ended, ended) {
            if ended && std::mem::take(&mut self.stepped) {
                self.command(&["seek", "1", "relative+exact"]);
            }
            events.push(Event::Ended(ended));
        }
        // SAFETY: the context is valid while not null.
        let due = !self.render.is_null()
            && unsafe { mpv_render_context_update(self.render) } & UPDATE_FRAME != 0;
        events.extend((due || late).then_some(Event::Frame));
        let mut info = FrameInfo {
            flags: 0,
            target_time: 0,
        };
        let info_param = Param {
            kind: NEXT_FRAME_INFO,
            data: &mut info as *mut FrameInfo as *mut c_void,
        };
        // SAFETY: the context is valid while `due`; `info` lives through the call.
        let new = due && unsafe { mpv_render_context_get_info(self.render, info_param) } >= 0;
        // a redraw shows no new frame and has no time to be shown at
        let new = new && info.flags & FRAME_PRESENT != 0 && info.target_time != 0;
        // the position of a frame comes with it or in the next look
        match (new, self.first.take()) {
            (true, first) => (self.frameless, self.frame_time) = (0, first.map(|_| self.time)),
            (false, Some(first)) if !late => match self.frame_time {
                None => self.frame_time = Some(first),
                Some(_) => self.frameless += 1,
            },
            _ => {}
        }
        self.looked = Instant::now();
        events
    }

    /// One of mpv's events as the viewer's, if it is one.
    fn report(&mut self, event: &RawEvent) -> Option<Event> {
        match event.id {
            // reached from `open` alone, which may wait for mpv: nothing else loads a file
            FILE_LOADED => {
                // an exact seek in MPEG-TS and MPEG-PS lands past its target unless the demuxer starts earlier
                if matches!(self.get(c"file-format").as_deref(), Some("mpegts" | "mpeg")) {
                    self.set("hr-seek-demuxer-offset", "1.5");
                }
                let plays = |track| self.get(track).is_some_and(|id| id != "no");
                Some(Event::Loaded {
                    video: plays(c"vid"),
                    audio: plays(c"aid"),
                })
            }
            END_FILE => {
                // SAFETY: the data of this event is an `mpv_event_end_file`.
                let end = unsafe { &*(event.data as *const EndFile) };
                Some(match end.reason {
                    END_ERROR => Event::Failed(explain(end.error)),
                    END_REDIRECT => {
                        self.command(&["stop"]); // before mpv opens what the list names
                        Event::Failed("playlists are not played".into())
                    }
                    _ => Event::Ended(true),
                })
            }
            COMMAND_REPLY if event.userdata == GRAB => {
                self.taken = Some(self.grabbed(event));
                None
            }
            GET_PROPERTY_REPLY if event.userdata == GRAB => {
                // SAFETY: the data of this event is an `mpv_event_property` whose value, without an error, is the double asked for.
                let time = (event.error >= 0)
                    .then(|| unsafe { *((*(event.data as *const Property)).data as *const f64) });
                let time = if self.frameless >= 2 {
                    self.frame_time.or(time)
                } else {
                    time
                };
                Some(Event::Grabbed(self.taken.take().flatten(), time))
            }
            SET_PROPERTY_REPLY if event.userdata == PAUSE => {
                self.pausing = self.pausing.saturating_sub(1);
                None
            }
            COMMAND_REPLY if event.userdata == PLAY => {
                self.play_seeks = self.play_seeks.saturating_sub(1);
                match (self.play_seeks, event.error < 0) {
                    (0, true) => self.seek_ran(),
                    (0, false) => self.seek_due = true,
                    _ => {}
                }
                None
            }
            SEEK if self.seek_due => {
                self.seek_ran();
                None
            }
            // SAFETY: the data of this event is an `mpv_event_property`.
            PROPERTY_CHANGE => self.changed(unsafe { &*(event.data as *const Property) }),
            _ => None,
        }
    }

    fn changed(&mut self, property: &Property) -> Option<Event> {
        let is = |format| property.format == format;
        // SAFETY: mpv hands the value in the format the property was observed in (each is read by that format alone), or format 0 without a value.
        let (name, flag, number, whole, text) = unsafe {
            (
                CStr::from_ptr(property.name).to_bytes(),
                is(FLAG).then(|| *(property.data as *const c_int) != 0),
                is(DOUBLE).then(|| *(property.data as *const f64)),
                is(INT64).then(|| *(property.data as *const i64)),
                is(STRING)
                    .then(|| CStr::from_ptr(*(property.data as *const *const c_char)).to_bytes()),
            )
        };
        match name {
            b"pause" => {
                self.paused = flag.unwrap_or(self.paused);
                self.moved |= flag == Some(false);
                flag.filter(|_| self.pausing == 0).map(Event::Pause)
            }
            b"eof-reached" => {
                self.at_end = flag.unwrap_or(false);
                None
            }
            b"mute" => flag.map(Event::Mute),
            b"time-pos" => {
                let time = number?;
                self.moved |= self
                    .landing
                    .is_some_and(|from| (time - from).abs() >= 0.001);
                (self.time, self.first) = (time, self.first.or(Some(time)));
                Some(Event::Time(time))
            }
            _ if name == STEP_MARK.to_bytes() => {
                let marked = whole == Some(self.steps as i64);
                if marked && self.landing.take().is_some() && !self.moved {
                    self.back = self.before.take();
                }
                None
            }
            b"duration" => Some(Event::Length(number.filter(|length| *length > 0.0))),
            b"speed" => number.map(Event::Speed),
            b"volume" => number.map(Event::Volume),
            b"loop-file" => text.map(|loops| Event::Loop(loops == b"inf")),
            _ => {
                let shape: [&[u8]; 3] = [b"dwidth", b"dheight", b"video-dec-params/rotate"];
                self.shape[shape.iter().position(|part| *part == name)?] = whole.unwrap_or(0);
                self.reshaped = true;
                None
            }
        }
    }

    /// The picture's size as stored and the turn its stream asks for, once mpv has told the size.
    fn shape(&self) -> Option<(u32, u32, u32)> {
        let [w, h, turn] = self.shape;
        let turn = [90u32, 180, 270]
            .into_iter()
            .find(|quarter| *quarter as i64 == turn);
        (w > 0 && h > 0).then_some((
            u32::try_from(w).ok()?,
            u32::try_from(h).ok()?,
            turn.unwrap_or(0),
        ))
    }

    /// The current frame as packed RGB, upright, scaled to exactly `w` x `h` (aspect not kept: the caller chose the size); `None` while mpv has no frame or has not told its shape (a due frame is then taken undrawn).
    pub fn render(&mut self, w: u32, h: u32) -> Option<Vec<u8>> {
        if self.render.is_null() {
            return None;
        }
        // mpv tells size and turn only once its first frame is taken
        let Some((.., turn)) = self.shape().filter(|_| w > 0 && h > 0) else {
            self.skip();
            return None;
        };
        let (w, h) = if turn % 180 == 90 { (h, w) } else { (w, h) };
        let (w, h) = (w as usize, h as usize);
        let mut stride = (w * 4 + 63) & !63;
        self.surface.resize(stride * h + 63, 0);
        let start = self.surface.as_ptr().align_offset(64);
        let (mut size, mut block) = ([w as c_int, h as c_int], 0 as c_int);
        let mut params = [
            (
                BLOCK_FOR_TARGET_TIME,
                &mut block as *mut c_int as *mut c_void,
            ),
            (SW_SIZE, size.as_mut_ptr() as *mut c_void),
            (SW_FORMAT, c"rgb0".as_ptr() as *mut c_void),
            (SW_STRIDE, &mut stride as *mut usize as *mut c_void),
            (
                SW_POINTER,
                self.surface[start..].as_mut_ptr() as *mut c_void,
            ),
            (0, std::ptr::null_mut()),
        ]
        .map(|(kind, data)| Param { kind, data });
        // SAFETY: the surface holds `h` rows of `stride` bytes from `start` on; the parameters live through the call.
        let drawn = unsafe { mpv_render_context_render(self.render, params.as_mut_ptr()) };
        (drawn >= 0).then(|| upright(&self.surface[start..], stride, w, h, 0, turn))
    }

    /// Tell mpv the due frame was not drawn.
    pub fn skip(&mut self) {
        if self.render.is_null() {
            return;
        }
        let (mut skip, mut block): (c_int, c_int) = (1, 0);
        let mut params = [
            (SKIP_RENDERING, &mut skip as *mut c_int as *mut c_void),
            (
                BLOCK_FOR_TARGET_TIME,
                &mut block as *mut c_int as *mut c_void,
            ),
            (0, std::ptr::null_mut()),
        ]
        .map(|(kind, data)| Param { kind, data });
        // SAFETY: the context is valid while not null; the parameters live through the call.
        unsafe { mpv_render_context_render(self.render, params.as_mut_ptr()) };
    }

    /// Ask for the frame mpv holds, at source size, and its time; they arrive as `Event::Grabbed`. mpv takes both requests in order on its own thread, the time right after the frame, before it plays on; a `time-pos` report read later is a later frame's.
    pub fn grab(&self) {
        self.send(GRAB, &[c"screenshot-raw".into(), c"video".into()]);
        // SAFETY: the name lives through the call.
        unsafe { mpv_get_property_async(self.handle, GRAB, c"time-pos".as_ptr(), DOUBLE) };
    }

    /// The picture in the reply to `screenshot-raw`: a map of `w`, `h`, `stride`, `format` (bgr0) and `data`.
    fn grabbed(&self, reply: &RawEvent) -> Option<RgbImage> {
        if reply.error < 0 {
            return None;
        }
        let (mut w, mut h, mut stride, mut bgr0, mut pixels) = (0, 0, 0, false, &[][..]);
        // SAFETY: the data of a command reply is an `mpv_event_command`, that is its result node; mpv keeps it until the next wait; each value is read by its own format.
        unsafe {
            let result = &*(reply.data as *const Node);
            if result.format != NODE_MAP {
                return None;
            }
            let map = &*result.value.list;
            for entry in 0..map.len as usize {
                let value = &*map.values.add(entry);
                match (
                    CStr::from_ptr(*map.keys.add(entry)).to_bytes(),
                    value.format,
                ) {
                    (b"w", INT64) => w = value.value.whole as usize,
                    (b"h", INT64) => h = value.value.whole as usize,
                    (b"stride", INT64) => stride = value.value.whole as usize,
                    (b"format", STRING) => {
                        bgr0 = CStr::from_ptr(value.value.text).to_bytes() == b"bgr0"
                    }
                    (b"data", BYTE_ARRAY) if !(*value.value.bytes).data.is_null() => {
                        pixels = std::slice::from_raw_parts(
                            (*value.value.bytes).data,
                            (*value.value.bytes).len,
                        )
                    }
                    _ => {}
                }
            }
        }
        let whole = w > 0
            && h > 0
            && stride >= w.checked_mul(4)?
            && pixels.len() >= stride.checked_mul(h)?;
        if !bgr0 || !whole {
            return None;
        }
        let turn = self.shape().map_or(0, |(.., turn)| turn);
        let (wide, high) = if turn % 180 == 90 { (h, w) } else { (w, h) };
        RgbImage::from_raw(
            wide as u32,
            high as u32,
            upright(pixels, stride, w, h, 2, turn),
        )
    }
}

impl Drop for Mpv {
    fn drop(&mut self) {
        // render context first, then the core: the other order aborts
        self.no_video();
        // SAFETY: the handle is valid until here and not used after.
        unsafe { mpv_terminate_destroy(self.handle) };
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::ffi::OsString;
    use std::path::PathBuf;
    use std::process::Command;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Mutex, MutexGuard};
    use std::time::{Duration, Instant};

    /// Held by every test that changes the process environment.
    static ENV: Mutex<()> = Mutex::new(());

    pub(crate) fn env() -> MutexGuard<'static, ()> {
        ENV.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// A directory of its own under the temp dir, removed on drop.
    pub(crate) struct Scratch(pub(crate) PathBuf);

    impl Scratch {
        pub(crate) fn new() -> Scratch {
            static COUNT: AtomicUsize = AtomicUsize::new(0);
            let count = COUNT.fetch_add(1, Ordering::Relaxed);
            let dir =
                std::env::temp_dir().join(format!("asciier-test-{}-{count}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            Scratch(dir)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// A variable of the process environment set for a test (under `env()`), put back on drop.
    pub(crate) struct Var(&'static str, Option<OsString>);

    impl Var {
        pub(crate) fn set(name: &'static str, value: impl AsRef<std::ffi::OsStr>) -> Var {
            let old = std::env::var_os(name);
            // SAFETY: the tests that read or write the environment hold `env()`.
            unsafe { std::env::set_var(name, value) };
            Var(name, old)
        }
    }

    impl Drop for Var {
        fn drop(&mut self) {
            // SAFETY: as in `set`.
            match &self.1 {
                Some(old) => unsafe { std::env::set_var(self.0, old) },
                None => unsafe { std::env::remove_var(self.0) },
            }
        }
    }

    static WOKEN: AtomicUsize = AtomicUsize::new(0);

    fn wake() {
        WOKEN.fetch_add(1, Ordering::Relaxed);
    }

    fn ffmpeg(args: &[&str]) -> Vec<u8> {
        let run = Command::new("ffmpeg")
            .args(["-v", "error", "-nostdin", "-y"])
            .args(args)
            .output()
            .unwrap();
        let said = String::from_utf8_lossy(&run.stderr);
        assert!(run.status.success(), "ffmpeg {args:?}: {said}");
        run.stdout
    }

    /// 160 x 120 px; frame N shows N in binary as 8 bars, bit 0 on the left, a set bit white.
    const COUNTER: &str = "format=gray,geq=lum='if(bitand(N,pow(2,floor(X*8/W))),235,16)'";
    /// Four quadrants of distinct colours.
    const QUADRANTS: &str = "nullsrc=s=160x120:r=25,format=gbrp,geq=r='if(lt(X,W/2),255,0)':g='if(lt(Y,H/2),255,0)':b=128";
    const H264: [&str; 6] = ["-c:v", "libx264", "-g", "5", "-pix_fmt", "yuv420p"];

    /// The number a counter frame shows.
    fn number(rgb: &[u8], w: usize, h: usize) -> usize {
        let lit = |bar: &usize| rgb[(h / 2 * w + bar * w / 8 + w / 16) * 3 + 1] > 128;
        (0..8).filter(lit).map(|bar| 1 << bar).sum()
    }

    /// Mean colour of the middle of each quadrant: top left, top right, bottom left, bottom right.
    fn quadrants(rgb: &[u8], w: usize, h: usize) -> [[i32; 3]; 4] {
        std::array::from_fn(|q| {
            let x0 = if q % 2 == 0 { w / 8 } else { w * 5 / 8 };
            let y0 = if q < 2 { h / 8 } else { h * 5 / 8 };
            let mut sum = [0usize; 3];
            for y in y0..y0 + h / 4 {
                for x in x0..x0 + w / 4 {
                    for (c, sum) in sum.iter_mut().enumerate() {
                        *sum += rgb[(y * w + x) * 3 + c] as usize;
                    }
                }
            }
            sum.map(|s| (s / (w / 4 * (h / 4))) as i32)
        })
    }

    fn near(a: f64, b: f64, by: f64) -> bool {
        (a - b).abs() <= by
    }

    fn times(events: &[Event]) -> Vec<f64> {
        let time = |e: &Event| {
            if let Event::Time(t) = e {
                Some(*t)
            } else {
                None
            }
        };
        events.iter().filter_map(time).collect()
    }

    /// libmpv on a home of its own (mpv.conf: `ao=null`), and what a viewer does with its events: every due frame drawn at the picture's size.
    struct Stage {
        mpv: Mpv,
        /// Upright size of the picture, from the last `Shape`.
        size: (u32, u32),
        /// The last frame drawn, the number each drawn frame showed, the last `Time` and `Length`.
        picture: Vec<u8>,
        shown: Vec<usize>,
        time: f64,
        length: Option<f64>,
        waiting: VecDeque<Event>,
        dir: Scratch,
        _home: Var,
        _env: MutexGuard<'static, ()>,
    }

    impl Stage {
        fn new() -> Stage {
            Stage::with_conf("ao=null\n")
        }

        fn with_conf(conf: &str) -> Stage {
            let _env = env();
            let dir = Scratch::new();
            std::fs::create_dir_all(dir.0.join(".config/mpv")).unwrap();
            std::fs::write(dir.0.join(".config/mpv/mpv.conf"), conf).unwrap();
            let _home = Var::set("HOME", &dir.0);
            let mpv = Mpv::new("asciier-test", wake).unwrap();
            assert_eq!(
                mpv.get(c"ao").as_deref(),
                Some("null"),
                "no test plays sound"
            );
            Stage {
                mpv,
                size: (0, 0),
                picture: Vec::new(),
                shown: Vec::new(),
                time: f64::NAN,
                length: None,
                waiting: VecDeque::new(),
                dir,
                _home,
                _env,
            }
        }

        fn file(&self, name: &str) -> PathBuf {
            self.dir.0.join(name)
        }

        /// A counter clip at `rate` frames a second: `video` seconds, with a tone of `audio` seconds (0: no audio track).
        fn counter(
            &self,
            name: &str,
            rate: u32,
            video: f64,
            audio: f64,
            extra: &[&str],
        ) -> PathBuf {
            let file = self.file(name);
            let picture = format!("nullsrc=s=160x120:r={rate}:d={video},{COUNTER}");
            let tone = format!("sine=d={audio}");
            let mut args = vec!["-f", "lavfi", "-i", &picture];
            if audio > 0.0 {
                args.extend(["-f", "lavfi", "-i", &tone, "-c:a", "aac"]);
            }
            args.extend(H264);
            args.extend(extra);
            args.push(file.to_str().unwrap());
            ffmpeg(&args);
            file
        }

        /// A counter clip at 25 frames a second, opened.
        fn clip(&mut self, video: f64, audio: f64) -> (bool, bool) {
            let file = self.counter("clip.mp4", 25, video, audio, &[]);
            self.open(&file)
        }

        fn open(&mut self, file: &Path) -> (bool, bool) {
            let opened = self.mpv.open(file, 10.0);
            opened.unwrap_or_else(|e| panic!("{}: {}", file.display(), e.0))
        }

        fn draw(&mut self) {
            let (w, h) = self.size;
            if let Some(rgb) = self.mpv.render(w, h) {
                assert_eq!(rgb.len(), (w * h * 3) as usize);
                self.shown.push(number(&rgb, w as usize, h as usize));
                self.picture = rgb;
            }
        }

        /// The events of at most `seconds`, up to the first that `until` accepts.
        fn pump(&mut self, seconds: f64, mut until: impl FnMut(&Event) -> bool) -> Vec<Event> {
            let (start, mut seen) = (Instant::now(), Vec::new());
            loop {
                if self.waiting.is_empty() {
                    self.waiting.extend(self.mpv.events());
                }
                while let Some(event) = self.waiting.pop_front() {
                    match event {
                        Event::Shape { w, h, turn } => {
                            self.size = if turn % 180 == 90 { (h, w) } else { (w, h) };
                            self.draw();
                        }
                        Event::Frame => self.draw(),
                        Event::Time(time) => self.time = time,
                        Event::Length(length) => self.length = length,
                        _ => {}
                    }
                    seen.push(event);
                    if until(seen.last().unwrap()) {
                        return seen;
                    }
                }
                if start.elapsed().as_secs_f64() >= seconds {
                    return seen;
                }
                std::thread::sleep(Duration::from_millis(1));
            }
        }

        /// The events until mpv has been quiet for 0.2 s.
        fn settle(&mut self) -> Vec<Event> {
            let mut seen = Vec::new();
            loop {
                let more = self.pump(0.2, |_| false);
                if more.is_empty() {
                    return seen;
                }
                seen.extend(more);
            }
        }

        /// A command, then the events until mpv is quiet again.
        fn after(&mut self, command: &[&str]) -> Vec<Event> {
            self.mpv.command(command);
            self.settle()
        }

        /// A command, then the events until frame `number` shows at `time` (or 3 s have passed) and what follows at once.
        fn until_at(&mut self, command: &[&str], number: usize, time: f64) -> Vec<Event> {
            self.mpv.command(command);
            let (start, mut seen) = (Instant::now(), Vec::new());
            let there = |stage: &Stage| {
                stage.shown.last() == Some(&number) && near(stage.time, time, 0.0005)
            };
            while !there(self) && start.elapsed().as_secs_f64() < 3.0 {
                seen.extend(self.pump(0.0, |_| false));
            }
            seen.extend(self.pump(0.05, |_| false));
            seen
        }

        /// Play until the clip is at `time` or past it.
        fn play_to(&mut self, time: f64) {
            self.mpv.set("pause", "no");
            self.pump(5.0, |e| matches!(e, Event::Time(t) if *t >= time));
        }
    }

    #[test]
    fn conf_gives_its_audio_options_only() {
        let conf = "\u{feff}# audio\nao=null\nvolume-max=200\n  volume = 70 # loud\nvo=gpu\npause\nmute\n\
                    audio-device='alsa/default' # quoted\naf=\"lavfi=[loudnorm]\"\naudio-client-name=%5%a b#c\n\
                    replaygain=track\ninput-ipc-server=/tmp/sock\n--audio-channels=stereo\nhwdec=auto\nmute now\n\
                    volume-gain\n[quiet]\nvolume=5\nao=alsa\n";
        let expected = [
            ("ao", "null"),
            ("volume-max", "200"),
            ("volume", "70"),
            ("mute", "yes"),
            ("audio-device", "alsa/default"),
            ("af", "lavfi=[loudnorm]"),
            ("audio-client-name", "a b#c"),
            ("replaygain", "track"),
            ("audio-channels", "stereo"),
            ("volume-gain", "yes"),
        ];
        let options = audio_options(conf);
        let options: Vec<(&str, &str)> = options
            .iter()
            .map(|(n, v)| (n.as_str(), v.as_str()))
            .collect();
        assert_eq!(options, expected);
        assert!(audio_options("").is_empty());
    }

    #[test]
    fn pixels_turn_clockwise_and_pack() {
        // 2 x 3 px, each px (10 x + y, 1, 2, 0), rows of 12 bytes
        let px = |x: u8, y: u8| [10 * x + y, 1, 2, 0];
        let surface: Vec<u8> = (0..3)
            .flat_map(|y| [px(0, y), px(1, y), [9; 4]].concat())
            .collect();
        let reds = |turn| -> Vec<u8> {
            upright(&surface, 12, 2, 3, 0, turn)
                .chunks(3)
                .map(|p| p[0])
                .collect()
        };
        assert_eq!(reds(0), [0, 10, 1, 11, 2, 12]);
        assert_eq!(reds(90), [2, 1, 0, 12, 11, 10]);
        assert_eq!(reds(180), [12, 2, 11, 1, 10, 0]);
        assert_eq!(reds(270), [10, 11, 12, 0, 1, 2]);
        assert_eq!(upright(&surface, 12, 2, 3, 0, 0)[..3], [0, 1, 2]);
        assert_eq!(upright(&surface, 12, 2, 3, 2, 0)[..3], [2, 1, 0], "bgr0");
    }

    #[test]
    fn new_names_the_stream_and_keeps_the_viewers_options() {
        let conf = "ao=null\nvolume-max=200\nvo=null\npause=yes\naudio-pitch-correction=yes\n[x]\nvolume=5\n";
        let stage = Stage::with_conf(conf);
        // tests/test_viewer.py:647: PIPEWIRE_PROPS == "{ node.name = <stream> }"
        assert_eq!(
            std::env::var("PIPEWIRE_PROPS").unwrap(),
            "{ node.name = asciier-test }"
        );
        let get = |name| stage.mpv.get(name).unwrap();
        assert_eq!(
            get(c"volume-max"),
            "200.000000",
            "mpv.conf's audio options apply"
        );
        assert_eq!(
            [get(c"vo"), get(c"pause"), get(c"volume")],
            ["libmpv", "no", "100.000000"]
        );
        assert_eq!(
            get(c"audio-pitch-correction"),
            "no",
            "the viewer's options win"
        );
        assert_eq!(
            [
                get(c"config"),
                get(c"keep-open"),
                get(c"sid"),
                get(c"hwdec")
            ],
            ["no", "yes", "no", "no"]
        );
        let threads: Vec<String> = std::fs::read_dir("/proc/self/task")
            .unwrap()
            .filter_map(|task| std::fs::read_to_string(task.ok()?.path().join("comm")).ok())
            .collect();
        // mpv's own thread shows; its scripts' threads (lua/ytdl_hook, lua/stats, ...) do not exist
        assert!(threads.iter().any(|name| name == "core\n"), "{threads:?}");
        assert!(
            !threads.iter().any(|name| name.starts_with("lua")),
            "{threads:?}"
        );
    }

    #[test]
    fn a_home_without_mpv_conf_is_fine() {
        let _env = env();
        let dir = Scratch::new();
        let _home = Var::set("HOME", &dir.0);
        assert!(Mpv::new("asciier-test", wake).is_ok());
    }

    #[test]
    fn open_reports_the_tracks() {
        let tracks = |make: &dyn Fn(&Stage) -> PathBuf| {
            let mut stage = Stage::new();
            let file = make(&stage);
            stage.open(&file)
        };
        assert_eq!(
            tracks(&|s| s.counter("av.mp4", 25, 0.4, 0.4, &[])),
            (true, true)
        );
        assert_eq!(
            tracks(&|s| s.counter("v.mp4", 25, 0.4, 0.0, &[])),
            (true, false)
        );
        let tone = |s: &Stage| {
            let file = s.file("a.mp3");
            ffmpeg(&[
                "-f",
                "lavfi",
                "-i",
                "sine=d=1",
                "-c:a",
                "libmp3lame",
                file.to_str().unwrap(),
            ]);
            file
        };
        assert_eq!(tracks(&tone), (false, true));
        // tests/test_viewer.py:82-87: the cover is no video to play
        let cover = |s: &Stage, name: &str, codec: &[&str]| {
            let file = s.file(name);
            let mut args = vec![
                "-f",
                "lavfi",
                "-i",
                "sine=d=1",
                "-f",
                "lavfi",
                "-i",
                "color=s=32x32:d=1",
            ];
            args.extend([
                "-map",
                "0",
                "-map",
                "1",
                "-c:v",
                "png",
                "-disposition:v",
                "attached_pic",
            ]);
            args.extend(codec);
            args.push(file.to_str().unwrap());
            ffmpeg(&args);
            file
        };
        assert_eq!(
            tracks(&|s| cover(s, "cover.mp3", &["-c:a", "libmp3lame"])),
            (false, true)
        );
        assert_eq!(
            tracks(&|s| cover(s, "cover.m4a", &["-c:a", "aac", "-frames:v", "1"])),
            (false, true)
        );
    }

    #[test]
    fn what_is_no_media_file_fails_to_open() {
        let failure = |make: &dyn Fn(&Stage) -> PathBuf| {
            let mut stage = Stage::new();
            let file = make(&stage);
            stage.mpv.open(&file, 10.0).unwrap_err().0
        };
        // tests/test_viewer.py:89-90: a missing file ends the program
        assert_eq!(failure(&|s| s.file("missing.mp4")), "loading failed");
        let text = |s: &Stage, name: &str, text: &str| {
            std::fs::write(s.file(name), text).unwrap();
            s.file(name)
        };
        assert_eq!(
            failure(&|s| text(s, "notes.txt", "no media\n")),
            "unrecognized file format"
        );
        assert_eq!(
            failure(&|s| s.dir.0.clone()),
            "unrecognized file format",
            "a directory"
        );
        let list = |s: &Stage| {
            let clip = s.counter("in.mp4", 25, 0.2, 0.0, &[]);
            text(s, "list.m3u", &format!("{}\n", clip.display()))
        };
        assert_eq!(failure(&list), "playlists are not played");
        // row V19: `no video or audio stream in <path>`
        let words = |s: &Stage| {
            let words = text(s, "words.srt", "1\n00:00:00,000 --> 00:00:01,000\nwords\n");
            ffmpeg(&[
                "-i",
                words.to_str().unwrap(),
                "-c:s",
                "srt",
                s.file("words.mkv").to_str().unwrap(),
            ]);
            s.file("words.mkv")
        };
        let failure = failure(&words);
        let (said, file) = failure.split_once(" in ").unwrap();
        assert_eq!(
            (said, file.ends_with("/words.mkv")),
            ("no video or audio stream", true),
            "{failure}"
        );
    }

    #[test]
    fn plays_at_the_clips_rate() {
        let mut stage = Stage::new();
        let woken = WOKEN.load(Ordering::Relaxed);
        assert_eq!(stage.clip(2.0, 2.0), (true, true));
        let events = stage.settle();
        assert!(
            WOKEN.load(Ordering::Relaxed) > woken,
            "the wake function is called"
        );
        // tests/test_viewer.py:80: probe gives (25, True, round(length) == 2)
        assert_eq!(stage.length.map(f64::round), Some(2.0), "{events:?}");
        let state = [
            Event::Pause(true),
            Event::Speed(1.0),
            Event::Volume(100.0),
            Event::Mute(false),
            Event::Loop(false),
        ];
        for expected in state {
            assert!(events.contains(&expected), "{expected:?} in {events:?}");
        }
        // the first frame and the shape come in either order: seen in this test under a loaded machine; the stage draws on both
        let shape = Event::Shape {
            w: 160,
            h: 120,
            turn: 0,
        };
        for expected in [Event::Frame, shape] {
            assert!(events.contains(&expected), "{expected:?} in {events:?}");
        }
        assert_eq!(
            (stage.time, stage.shown.first()),
            (0.0, Some(&0)),
            "paused on the first frame: {events:?}"
        );
        for (w, h) in [(64, 48), (33, 17), (1, 1), (801, 3)] {
            assert_eq!(
                stage.mpv.render(w, h).map(|rgb| rgb.len()),
                Some((w * h * 3) as usize)
            );
        }
        assert_eq!(stage.mpv.render(0, 5), None);

        stage.shown.clear();
        stage.mpv.set("pause", "no");
        let start = Instant::now();
        let events = stage.pump(1.0, |_| false);
        let (wall, shown, times) = (
            start.elapsed().as_secs_f64(),
            stage.shown.clone(),
            times(&events),
        );
        assert!(events.contains(&Event::Pause(false)));
        assert!(
            times.windows(2).all(|t| t[0] < t[1]),
            "time advances: {times:?}"
        );
        assert!(
            near(stage.time, wall, 0.15),
            "at the clock's pace: {times:?} in {wall} s"
        );
        assert!(
            shown.windows(2).all(|n| n[0] < n[1]),
            "frames in order: {shown:?}"
        );
        let rate = *shown.last().unwrap() as f64 / wall;
        assert!(near(rate, 25.0, 4.0), "{rate} frames a second: {shown:?}");
        assert!(shown.len() >= 18, "few frames lost: {shown:?}");
    }

    #[test]
    fn no_older_report_undoes_a_pause_asked_for() {
        // rows P8, H3: the open's `pause=yes`, heard while it waited, reached the viewer after its own `pause=no`: F1 and space did nothing for the first 0.1 s
        let mut stage = Stage::new();
        stage.clip(2.0, 2.0);
        stage.mpv.pause(false);
        let events = stage.pump(3.0, |e| matches!(e, Event::Time(t) if *t > 0.3));
        assert!(stage.time > 0.3, "it plays: {events:?}");
        assert!(!events.contains(&Event::Pause(true)), "{events:?}");
        // asked for and taken back at once: mpv may report the first, never after the second
        stage.mpv.pause(true);
        stage.mpv.pause(false);
        let events = stage.pump(0.5, |_| false);
        assert!(!events.contains(&Event::Pause(true)), "{events:?}");
    }

    /// Requests mpv's core answers one by one before it plays on, as a loaded machine delays it.
    fn keep_busy(mpv: &Mpv, requests: usize) {
        for _ in 0..requests {
            // SAFETY: the name lives through the call.
            unsafe { mpv_get_property_async(mpv.handle, 0, c"time-pos".as_ptr(), DOUBLE) };
        }
    }

    #[test]
    fn a_seek_that_plays_counts_from_the_frame_on_screen() {
        // rows P8a, P9: a client that draws slower than the clip (70 ms a frame against 40) leaves mpv a frame due when it pauses; play asked with the seek showed that frame first and counted the seek from it (seen in this test before `seek_and_play`: from frame 35, frame 36 first, then 61); played on mpv's answer to the seek, given before mpv runs it, a core still busy with requests took the play first (seen with `keep_busy` before the play waited for the seek to run: the same, 3 of 3)
        for (offset, busy) in [(0, 0), (10, 300), (20, 0), (30, 300)] {
            let mut stage = Stage::new();
            stage.clip(4.0, 4.0);
            stage.settle();
            stage.mpv.pause(false);
            stage.pump(5.0, |e| matches!(e, Event::Time(t) if *t >= 1.0));
            let start = Instant::now();
            while start.elapsed() < Duration::from_millis(400 + offset) {
                if stage.mpv.events().contains(&Event::Frame) {
                    stage.draw();
                    std::thread::sleep(Duration::from_millis(70));
                }
                std::thread::sleep(Duration::from_millis(1));
            }
            stage.mpv.pause(true);
            stage.settle();
            let (held, drawn) = (*stage.shown.last().unwrap(), stage.shown.len());
            stage.mpv.seek_and_play(&["seek", "1", "relative+exact"]);
            keep_busy(&stage.mpv, busy);
            stage.pump(0.3, |_| false);
            let after = &stage.shown[drawn..];
            assert_eq!(
                after.first(),
                Some(&(held + 25)),
                "{offset} ms on: from frame {held}: {after:?}"
            );
        }
    }

    #[test]
    fn a_step_back_after_a_seek_that_plays_ends_paused() {
        // rows P8a, P15: `,` right after a seek that plays (`l,` after MPRIS Stop) ends paused, the step taking the place of the seek mpv had not run; the play that waited for the seek came after it (seen in this test before a step dropped the play: at 1.92 s on frame 48 a second later, 3 of 3)
        let mut stage = Stage::new();
        stage.clip(4.0, 4.0);
        stage.until_at(&["seek", "1", "absolute+exact"], 25, 1.0);
        stage.mpv.seek_and_play(&["seek", "5", "relative+exact"]);
        stage.mpv.step(false);
        let events = stage.pump(1.0, |_| false);
        assert!(
            stage.mpv.paused && stage.time < 1.0,
            "at {} on frame {:?}: {events:?}",
            stage.time,
            stage.shown.last()
        );
    }

    #[test]
    fn a_caller_slower_than_mpv_waits_draws_the_frame_mpv_holds() {
        // rows N5, P9: mpv waits 200 ms at most for a frame it hands over to be taken; a caller that draws a frame in 450 ms, busy as well while its pause goes out (as the viewer writing its screen under load), missed what mpv then held as shown and stayed paused on frame 36 while mpv held 47, the frame it grabs and counts seeks from (seen in this test without the late `Frame`; a grab hands the held frame over again, so the drawn one is read before it)
        let slow = |stage: &mut Stage, run: Duration| {
            let start = Instant::now();
            while start.elapsed() < run {
                if stage.mpv.events().contains(&Event::Frame) {
                    stage.draw();
                    std::thread::sleep(Duration::from_millis(450));
                }
                std::thread::sleep(Duration::from_millis(1));
            }
        };
        for offset in [0, 100] {
            let mut stage = Stage::new();
            stage.clip(4.0, 4.0);
            stage.settle();
            stage.mpv.pause(false);
            stage.pump(5.0, |e| matches!(e, Event::Time(t) if *t >= 1.0));
            slow(&mut stage, Duration::from_millis(600 + offset));
            stage.mpv.pause(true);
            std::thread::sleep(Duration::from_millis(450));
            slow(&mut stage, Duration::from_millis(1500));
            let drawn = stage.shown.last().copied();
            stage.mpv.grab();
            let events = stage.pump(2.0, |e| matches!(e, Event::Grabbed(..)));
            let Some(Event::Grabbed(Some(held), _)) = events.last() else {
                panic!("{events:?}")
            };
            let held = number(held.as_raw(), 160, 120);
            assert_eq!(drawn, Some(held), "{offset} ms on");
        }
    }

    #[test]
    fn seeks_and_steps_land_on_the_frame() {
        // tests/test_viewer.py:47-58: times count from the container's start (the first frame under 0.25 s) and a seek lands on time: there within 0.021 s, on the frame at or before the target; mpv shows the first frame at or after it
        // rows N2, N3: a step is one source frame, also above 30 frames a second
        let late: &[&str] = &["-output_ts_offset", "3"];
        for (name, rate, extra) in [
            ("plain.mp4", 25, &[][..]),
            ("late.ts", 25, late),
            ("fast.mp4", 60, &[][..]),
        ] {
            let mut stage = Stage::new();
            let file = stage.counter(name, rate, 2.0, 2.0, extra);
            stage.open(&file);
            stage.settle();
            let (first, frame, last) = (stage.time, 1.0 / rate as f64, 2 * rate as usize - 1);
            assert!(first < 0.25, "{name}: starts at {first}");
            let at = |number: usize| first + number as f64 * frame;

            stage.play_to(0.2);
            stage.mpv.set("pause", "yes");
            let events = stage.settle();
            assert!(events.contains(&Event::Pause(true)), "{name}: {events:?}");
            let (paused, number) = (stage.time, *stage.shown.last().unwrap());
            assert!(
                near(paused, at(number), 0.001),
                "{name}: paused at {paused} on frame {number}"
            );
            assert_eq!(
                stage.pump(0.3, |_| false),
                [],
                "{name}: nothing moves while paused"
            );

            let to = rate as usize;
            stage.until_at(&["seek", "1.0", "absolute+exact"], to, at(to));
            assert_eq!(
                stage.shown.last(),
                Some(&to),
                "{name}: a seek to 1.0 shows frame {to}"
            );
            assert!(
                near(stage.time, 1.0, 0.0233),
                "{name}: a seek to 1.0 lands at {}",
                stage.time
            );
            stage.until_at(&["frame-step"], to + 1, at(to + 1));
            assert_eq!(
                stage.shown.last(),
                Some(&(to + 1)),
                "{name}: a step forward"
            );
            assert!(
                near(stage.time, at(to + 1), 0.001),
                "{name}: a step forward to {}",
                stage.time
            );
            for back in [to, to - 1] {
                stage.until_at(&["frame-back-step"], back, at(back));
                assert_eq!(stage.shown.last(), Some(&back), "{name}: a step back");
                assert!(
                    near(stage.time, at(back), 0.001),
                    "{name}: a step back to {}",
                    stage.time
                );
            }
            stage.after(&["seek", "-0.5", "relative+exact"]);
            assert!(
                near(stage.time, at(to - 1) - 0.5, frame),
                "{name}: a relative seek lands at {}",
                stage.time
            );
            assert!(
                near(stage.time, at(*stage.shown.last().unwrap()), 0.001),
                "{name}"
            );
            // row P9: a seek to the length; paused: the last frame shows, still paused
            let length = stage.length.unwrap();
            let events = stage.after(&["seek", &length.to_string(), "absolute+exact"]);
            assert_eq!(
                stage.shown.last(),
                Some(&last),
                "{name}: a seek to the length"
            );
            assert!(
                stage.time >= at(last) - 0.001 && stage.time <= length,
                "{name}: the end is at {}",
                stage.time
            );
            assert!(
                !events
                    .iter()
                    .any(|e| matches!(e, Event::Ended(true) | Event::Pause(_))),
                "{name}: {events:?}"
            );
            stage.until_at(&["seek", "-9", "relative+exact"], 0, first);
            assert_eq!(
                (stage.shown.last(), stage.time),
                (Some(&0), first),
                "{name}: a seek before the start"
            );
        }
    }

    #[test]
    fn speed_volume_and_mute_are_reported() {
        let mut stage = Stage::new();
        stage.clip(2.0, 2.0);
        stage.settle();
        stage.mpv.set("volume", "40");
        stage.mpv.set("mute", "yes");
        stage.mpv.set("speed", "2");
        let events = stage.settle();
        for expected in [Event::Volume(40.0), Event::Mute(true), Event::Speed(2.0)] {
            assert!(events.contains(&expected), "{expected:?} in {events:?}");
        }
        assert_eq!(
            stage.after(&["add", "volume", "-2"]).last(),
            Some(&Event::Volume(38.0))
        );
        stage.play_to(0.2);
        let (start, from) = (Instant::now(), stage.time);
        let events = stage.pump(0.5, |_| false);
        let pace = (stage.time - from) / start.elapsed().as_secs_f64();
        assert!(near(pace, 2.0, 0.4), "{pace} s of the clip a second");
        // row P7: volume and mute stay through a seek
        let events = [events, stage.after(&["seek", "0.1", "absolute+exact"])].concat();
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, Event::Volume(_) | Event::Mute(_) | Event::Speed(_))),
            "{events:?}"
        );
    }

    #[test]
    fn a_loop_wraps_without_an_end() {
        let mut stage = Stage::new();
        stage.clip(1.0, 1.0);
        stage.mpv.set("loop-file", "inf");
        assert!(stage.settle().contains(&Event::Loop(true)));
        stage.shown.clear();
        stage.mpv.set("pause", "no");
        let mut events = stage.pump(1.6, |_| false);
        // once round: up to the last frames, then from the first again (a busy machine may lose a frame)
        let round = |shown: &[usize]| {
            let wraps = shown.windows(2).filter(|n| n[1] < n[0]).count();
            wraps == 1 && shown.iter().max() >= Some(&23) && shown.last() < Some(&20)
        };
        assert!(round(&stage.shown), "{:?}", stage.shown);
        // row N4: no gap at the wrap: 0.6 s after it the clip is 0.6 s in
        assert!(
            near(stage.time, 0.6, 0.15),
            "{} s in, {:?}",
            stage.time,
            stage.shown
        );
        // row N23: a seek past the end of a looping clip loops
        stage.shown.clear();
        stage.mpv.command(&["seek", "5", "relative+exact"]);
        events.extend(stage.pump(0.4, |_| false));
        assert!(round(&stage.shown), "{:?}", stage.shown);
        assert!(
            near(stage.time, 0.35, 0.15),
            "{} s in, {:?}",
            stage.time,
            stage.shown
        );
        let stop = |e: &Event| matches!(e, Event::Ended(_) | Event::Pause(true) | Event::Failed(_));
        assert!(!events.iter().any(stop), "{events:?}");
        stage.mpv.set("loop-file", "no");
        let events = stage.pump(2.0, |e| *e == Event::Ended(true));
        assert!(
            events.contains(&Event::Loop(false)) && events.contains(&Event::Ended(true)),
            "{events:?}"
        );
    }

    #[test]
    fn the_end_pauses_on_the_last_frame() {
        let mut stage = Stage::new();
        stage.clip(2.0, 2.0);
        stage.after(&["seek", "1.5", "absolute+exact"]);
        stage.mpv.set("pause", "no");
        let start = Instant::now();
        let events = stage.pump(3.0, |e| *e == Event::Ended(true));
        let wall = start.elapsed().as_secs_f64();
        assert_eq!(
            events[events.len() - 2..],
            [Event::Pause(true), Event::Ended(true)],
            "{events:?}"
        );
        assert!(
            near(wall, 0.5, 0.15),
            "the sound plays out: ended after {wall} s"
        );
        assert!(
            near(stage.time, stage.length.unwrap(), 1e-9),
            "at {} of {:?}",
            stage.time,
            stage.length
        );
        assert_eq!(
            stage.settle().iter().find(|e| **e != Event::Frame),
            None,
            "nothing follows the end"
        );
        assert_eq!(stage.shown.last(), Some(&49));
        let held = stage.mpv.render(160, 120).unwrap();
        assert_eq!(
            number(&held, 160, 120),
            49,
            "the file stays open on its last frame"
        );
        // a seek leaves the end
        let events = stage.until_at(&["seek", "1.88", "absolute+exact"], 47, 1.88);
        assert!(events.contains(&Event::Ended(false)), "{events:?}");
        // row P15: a step onto the last frame, and one on it, only pause
        let events = [
            stage.after(&["frame-step"]),
            stage.after(&["frame-step"]),
            stage.after(&["frame-step"]),
        ]
        .concat();
        assert_eq!(stage.shown.last(), Some(&49));
        assert!(
            !events.iter().any(|e| matches!(e, Event::Failed(_))),
            "{events:?}"
        );
        assert_eq!(
            events.iter().rfind(|e| matches!(e, Event::Pause(_))),
            Some(&Event::Pause(true))
        );
    }

    #[test]
    fn a_step_onto_the_last_frame_holds_its_time() {
        // rows P15, N2, N21: mpv's step onto the last frame, or on it, plays the rest of the sound and leaves the position at the sound's end (2.0 and 2.3 here, seen in this test without the seek past the end), where its step back finds the last frame again; with sound seconds past the picture (6.0) the step pauses at once at the sound's position, just past the frame, with the same effect, also for a step back sent while mpv plays the sound out; one sent with the step forward came first and counted from the frame before (seen before a step back went to where the step forward started: 49 at 1.77 s for 6.0; 47 at 1.88 sent at once; 49 at 2.30 sent 50 or 150 ms on for 2.3; 49 at 1.80 sent 50 ms on for 6.0)
        let soon = [
            (2.0, 0.0),
            (2.3, 0.0),
            (2.3, 0.05),
            (2.3, 0.15),
            (6.0, 0.05),
        ];
        let cases = [(2.0, None), (2.3, None), (6.0, None)]
            .into_iter()
            .chain(soon.map(|(audio, wait)| (audio, Some(wait))));
        let mut wrong = Vec::new();
        for (audio, back_after) in cases {
            let mut stage = Stage::new();
            stage.clip(2.0, audio);
            stage.after(&["seek", "1.92", "absolute+exact"]);
            match back_after {
                None => {
                    for _ in 0..2 {
                        stage.mpv.step(true);
                        let events = stage.settle();
                        assert_eq!(stage.shown.last(), Some(&49), "{audio}: {events:?}");
                        assert!(
                            near(stage.time, 1.96, 0.001),
                            "{audio}: at {} on the last frame: {events:?}",
                            stage.time
                        );
                    }
                }
                Some(wait) => {
                    stage.mpv.step(true);
                    stage.pump(wait, |_| false);
                }
            }
            stage.mpv.step(false);
            // mpv playing the sound out is silent for its rest
            let (start, there) = (Instant::now(), |stage: &Stage| {
                stage.shown.last() == Some(&48) && near(stage.time, 1.92, 0.001)
            });
            while !there(&stage) && start.elapsed() < Duration::from_secs(3) {
                stage.pump(0.05, |_| false);
            }
            stage.settle();
            if !there(&stage) {
                let on = (stage.shown.last(), stage.time);
                wrong.push(format!(
                    "{audio} s of sound, back after {back_after:?}: {on:?}"
                ));
            }
        }
        assert!(wrong.is_empty(), "a step back: {wrong:#?}");
    }

    #[test]
    fn audio_that_outlasts_the_video_plays_on() {
        // row N21
        let mut stage = Stage::new();
        stage.clip(2.0, 4.0);
        stage.after(&["seek", "1.6", "absolute+exact"]);
        stage.mpv.set("pause", "no");
        let start = Instant::now();
        let events = stage.pump(5.0, |e| *e == Event::Ended(true));
        let wall = start.elapsed().as_secs_f64();
        assert_eq!(events.last(), Some(&Event::Ended(true)), "{events:?}");
        assert!(near(wall, 2.4, 0.3), "ended after {wall} s");
        assert!(near(stage.time, 4.0, 0.05), "ended at {}", stage.time);
        stage.settle();
        assert_eq!(stage.shown.last(), Some(&49), "on the video's last frame");
    }

    #[test]
    fn a_turned_clip_comes_upright() {
        for (asked, turn) in [("90", 270), ("270", 90), ("180", 180)] {
            let mut stage = Stage::new();
            let (flat, file) = (stage.file("flat.mp4"), stage.file("turned.mp4"));
            let mut args = vec!["-f", "lavfi", "-i", QUADRANTS, "-t", "0.4"];
            args.extend(H264);
            args.push(flat.to_str().unwrap());
            ffmpeg(&args);
            ffmpeg(&[
                "-display_rotation",
                asked,
                "-i",
                flat.to_str().unwrap(),
                "-c",
                "copy",
                file.to_str().unwrap(),
            ]);
            // the oracle: ffmpeg's own upright frame
            let frame = ["-frames:v", "1", "-f", "rawvideo", "-pix_fmt", "rgb24", "-"];
            let reference = ffmpeg(&[&["-i", file.to_str().unwrap()][..], &frame[..]].concat());
            let (w, h) = if turn == 180 { (160, 120) } else { (120, 160) };
            assert_eq!(reference.len(), w * h * 3);

            assert_eq!(stage.open(&file), (true, false));
            let events = stage.settle();
            assert!(
                events.contains(&Event::Shape {
                    w: 160,
                    h: 120,
                    turn
                }),
                "{asked}: {events:?}"
            );
            // tests/test_viewer.py:73: 320 x 240 stored, 240 x 320 shown
            assert_eq!(stage.size, (w as u32, h as u32), "{asked}");
            let expected = quadrants(&reference, w, h);
            let close = |picture: &[u8]| {
                let mine = quadrants(picture, w, h);
                (0..4).all(|q| (0..3).all(|c| (mine[q][c] - expected[q][c]).abs() < 8))
            };
            assert!(
                close(&stage.picture),
                "{asked}: {:?} for {expected:?}",
                quadrants(&stage.picture, w, h)
            );
            // tests/test_viewer.py:75-76: mean difference under 4
            let gap = |picture: &[u8]| {
                let sum: usize = picture
                    .iter()
                    .zip(&reference)
                    .map(|(a, b)| a.abs_diff(*b) as usize)
                    .sum();
                sum as f64 / reference.len() as f64
            };
            assert!(
                gap(&stage.picture) < 4.0,
                "{asked}: mean gap {}",
                gap(&stage.picture)
            );

            stage.mpv.grab();
            let events = stage.pump(2.0, |e| matches!(e, Event::Grabbed(..)));
            let Some(Event::Grabbed(Some(grabbed), _)) = events.last() else {
                panic!("{asked}: {events:?}")
            };
            assert_eq!(grabbed.dimensions(), (w as u32, h as u32), "{asked}");
            let grabbed = grabbed.as_raw();
            assert!(
                close(grabbed) && gap(grabbed) < 4.0,
                "{asked}: {:?}",
                quadrants(grabbed, w, h)
            );
        }
    }

    #[test]
    fn a_grab_gives_the_frame_mpv_holds() {
        let mut stage = Stage::new();
        assert_eq!(stage.clip(2.0, 0.0), (true, false));
        stage.settle();
        // the frame, read `busy` after asking, and its time: row N5, that frame's
        let grab = |stage: &mut Stage, busy: Duration| {
            stage.mpv.grab();
            std::thread::sleep(busy);
            let events = stage.pump(2.0, |e| matches!(e, Event::Grabbed(..)));
            let Some(Event::Grabbed(Some(grabbed), Some(time))) = events.last() else {
                panic!("{events:?}")
            };
            // at source size, whatever size is drawn
            assert_eq!(grabbed.dimensions(), (160, 120));
            let number = number(grabbed.as_raw(), 160, 120);
            assert!(
                near(*time, number as f64 / 25.0, 0.001),
                "frame {number} at {time}"
            );
            number
        };
        stage.size = (16, 12);
        // row N5: the frame on screen when paused, also after a step back
        stage.until_at(&["seek", "1.2", "absolute+exact"], 30, 1.2);
        assert_eq!(grab(&mut stage, Duration::ZERO), 30);
        stage.until_at(&["frame-back-step"], 29, 1.16);
        assert_eq!(grab(&mut stage, Duration::ZERO), 29);
        // row P6: without sound the picture plays on at its pace
        stage.play_to(1.2);
        let (start, from) = (Instant::now(), stage.time);
        stage.pump(0.4, |_| false);
        let pace = (stage.time - from) / start.elapsed().as_secs_f64();
        assert!(near(pace, 1.0, 0.2), "{pace} s of the clip a second");
        // row N5: playing, within 3 frames of the frame drawn last
        let grabbed = grab(&mut stage, Duration::ZERO);
        let drawn = *stage.shown.last().unwrap();
        assert!(
            grabbed + 3 >= drawn && grabbed <= drawn + 3,
            "frame {grabbed} grabbed, {drawn} drawn"
        );
        // a client busy while mpv plays on: its later reports of `time-pos` are later frames'
        grab(&mut stage, Duration::from_millis(300));
    }

    #[test]
    fn a_grab_while_the_sound_plays_on_names_the_last_frame() {
        // rows N5, N21: past the picture's end, the sound playing on, mpv holds the last frame and the position is the sound's: the frame is named by the position given with it, paused there too (seen before: frame 49 named 3.06 s playing, 3.07 s paused; with a redraw on the pause taken for a new frame, 3.07 s paused)
        let mut stage = Stage::new();
        stage.clip(2.0, 4.0);
        stage.after(&["seek", "1.7", "absolute+exact"]);
        let grab = |stage: &mut Stage| {
            stage.mpv.grab();
            let events = stage.pump(2.0, |e| matches!(e, Event::Grabbed(..)));
            let Some(Event::Grabbed(Some(grabbed), Some(time))) = events.last() else {
                panic!("{events:?}")
            };
            (number(grabbed.as_raw(), 160, 120), *time)
        };
        stage.mpv.pause(false);
        stage.pump(3.0, |e| matches!(e, Event::Time(t) if *t > 3.0));
        let playing = grab(&mut stage);
        stage.mpv.pause(true);
        stage.settle();
        let paused = grab(&mut stage);
        for (number, time) in [playing, paused] {
            assert!(
                number == 49 && near(time, 1.96, 0.01),
                "{playing:?} {paused:?}"
            );
        }
    }

    #[test]
    fn subtitles_stay_out_of_the_picture() {
        let mut stage = Stage::new();
        // a track inside the file, flagged default, and a file of the same name beside it
        let (words, file) = (stage.file("clip.srt"), stage.file("clip.mkv"));
        std::fs::write(
            &words,
            "1\n00:00:00,000 --> 00:00:02,000\nWORDS ON THE PICTURE\n",
        )
        .unwrap();
        let mut args = vec![
            "-f",
            "lavfi",
            "-i",
            "color=c=gray:s=320x240:r=25:d=1",
            "-i",
            words.to_str().unwrap(),
        ];
        args.extend(H264);
        args.extend([
            "-c:s",
            "srt",
            "-disposition:s:0",
            "default",
            file.to_str().unwrap(),
        ]);
        ffmpeg(&args);
        assert_eq!(stage.open(&file), (true, false));
        stage.after(&["seek", "0.5", "absolute+exact"]);
        assert_eq!(stage.size, (320, 240));
        let grey = stage.picture[..3].to_vec();
        let off = |px: &&[u8]| (0..3).any(|c| px[c].abs_diff(grey[c]) > 8);
        assert_eq!(
            stage.picture.chunks(3).filter(off).count(),
            0,
            "px off the background {grey:?}"
        );
    }

    #[test]
    fn audio_alone_plays_without_a_picture() {
        let mut stage = Stage::new();
        let file = stage.file("tone.mp3");
        ffmpeg(&[
            "-f",
            "lavfi",
            "-i",
            "sine=d=1",
            "-c:a",
            "libmp3lame",
            file.to_str().unwrap(),
        ]);
        assert_eq!(stage.open(&file), (false, true));
        assert_eq!(stage.mpv.render(160, 120), None);
        stage.mpv.no_video();
        assert_eq!(stage.mpv.render(160, 120), None);
        stage.mpv.skip();
        stage.mpv.grab();
        stage.mpv.set("pause", "no");
        let start = Instant::now();
        let events = stage.pump(3.0, |e| *e == Event::Ended(true));
        assert_eq!(events.last(), Some(&Event::Ended(true)), "{events:?}");
        assert!(near(start.elapsed().as_secs_f64(), 1.0, 0.3));
        assert!(
            events.iter().any(|e| matches!(e, Event::Grabbed(None, _))),
            "{events:?}"
        );
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, Event::Frame | Event::Shape { .. })),
            "{events:?}"
        );
        let times = times(&events);
        assert!(
            times.len() > 5 && times.windows(2).all(|t| t[0] < t[1]),
            "{times:?}"
        );
        assert!(
            near(stage.time, stage.length.unwrap(), 0.01),
            "at {} of {:?}",
            stage.time,
            stage.length
        );
    }
    #[test]
    fn a_single_picture_holds_without_an_end() {
        let mut stage = Stage::new();
        let file = stage.file("still.png");
        ffmpeg(&[
            "-f",
            "lavfi",
            "-i",
            QUADRANTS,
            "-frames:v",
            "1",
            file.to_str().unwrap(),
        ]);
        assert_eq!(stage.open(&file), (true, false));
        stage.settle();
        assert_eq!(
            (stage.size, stage.length, stage.picture.len()),
            ((160, 120), None, 160 * 120 * 3)
        );
        stage.mpv.set("pause", "no");
        assert_eq!(stage.pump(0.5, |_| false), [Event::Pause(false)]);
    }

    #[test]
    fn a_gap_between_frames_holds_the_last_one() {
        // row P24: frames 10 to 34 of 50 are missing; the sound runs through
        let mut stage = Stage::new();
        let file = stage.file("gap.mkv");
        let picture = format!("nullsrc=s=160x120:r=25:d=2,{COUNTER},select='lt(n,10)+gte(n,35)'");
        let mut args = vec![
            "-f", "lavfi", "-i", &picture, "-f", "lavfi", "-i", "sine=d=2",
        ];
        args.extend(["-c:a", "aac", "-fps_mode", "vfr"]);
        args.extend(H264);
        args.push(file.to_str().unwrap());
        ffmpeg(&args);
        stage.open(&file);
        stage.settle();
        stage.mpv.set("pause", "no");
        let start = Instant::now();
        stage.pump(1.0, |_| false);
        assert_eq!(
            stage.shown.last(),
            Some(&9),
            "held through the gap: {:?}",
            stage.shown
        );
        stage.pump(2.0, |e| matches!(e, Event::Time(time) if *time >= 1.4));
        let wall = start.elapsed().as_secs_f64();
        assert!(near(wall, 1.4, 0.15), "frame 35 is due 1.4 s in: {wall}");
        stage.pump(0.2, |_| false);
        let shown = &stage.shown;
        assert!(
            shown.last() >= Some(&36) && !shown.iter().any(|n| (10..35).contains(n)),
            "{shown:?}"
        );
    }

    #[test]
    fn damage_does_not_stop_the_clip() {
        // row P25
        let mut stage = Stage::new();
        let file = stage.counter("hurt.mp4", 25, 2.0, 0.0, &[]);
        let mut bytes = std::fs::read(&file).unwrap();
        let middle = bytes.len() / 2;
        bytes[middle..middle + 300].fill(0);
        std::fs::write(&file, bytes).unwrap();
        assert_eq!(stage.open(&file), (true, false));
        stage.settle();
        stage.shown.clear();
        stage.mpv.set("pause", "no");
        let events = stage.pump(4.0, |e| matches!(e, Event::Ended(true) | Event::Failed(_)));
        assert_eq!(events.last(), Some(&Event::Ended(true)), "{events:?}");
        let (shown, whole) = (&stage.shown, (1..=49).collect::<Vec<usize>>());
        assert!(
            *shown != whole && shown.starts_with(&whole[..20]) && shown.ends_with(&whole[45..]),
            "{shown:?}"
        );
    }
}
