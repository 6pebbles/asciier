//! `asciier convert`: an image or a video drawn as the viewer draws it, written as PNG or MP4.

use crate::cells::{CELL_PX, Grid};
use crate::look::{self, Glyphs, Look, Skin};
use crate::paint::Style;
use crate::picture::{self, Opened, Still};
use crate::theme::{self, Theme};
use crate::{Error, Result, desktop, err, term};
use image::RgbImage;
use std::borrow::Cow;
use std::ffi::OsString;
use std::io::{BufRead, BufReader, IsTerminal, Read, Write};
use std::os::fd::FromRawFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Fit {
    Contain,
    Cover,
}

/// The converter's settings: what its options say, what the render page edits, what a screenshot uses.
#[derive(Clone, PartialEq, Debug)]
pub struct Options {
    pub look: Look,
    /// `--theme`: a kitty theme by name, or a file.
    pub theme: Option<OsString>,
    /// `--crop`: X, Y, W, H in percent of the picture.
    pub crop: [f64; 4],
    pub output: Option<PathBuf>,
    pub columns: Option<u32>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub fit: Fit,
    /// `--at`: a video's frame at that time, as a PNG.
    pub at: Option<f64>,
    pub input: PathBuf,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            look: Look::default(),
            theme: None,
            crop: FULL,
            output: None,
            columns: None,
            width: None,
            height: None,
            fit: Fit::Contain,
            at: None,
            input: PathBuf::new(),
        }
    }
}

impl Options {
    fn set_frame_size(&mut self, size: (u32, u32)) {
        if self.width.is_none() && self.height.is_none() {
            let (w, h) = web_size(size);
            (self.width, self.height) = (Some(w), Some(h));
        }
    }
}

/// `--crop`'s default: the whole picture.
pub const FULL: [f64; 4] = [0.0, 0.0, 100.0, 100.0];
/// Limits of `--columns`, and of `--width` and `--height`.
pub const COLUMNS: (u32, u32) = (8, 2000);
pub const PX: (u32, u32) = (16, 16384);
/// The font size in px a column is one cell of.
pub const SIZE: f64 = 20.0;
/// The render node of the GPU that encodes H.264 (VAAPI).
const VAAPI: &str = "/dev/dri/renderD128";
/// Light denoise of video frames (hqdn3d: spatial luma, chroma, temporal luma, chroma): grain otherwise flips glyphs from frame to frame.
const DENOISE: (f64, f64, &str, &str) = (2.0, 1.5, "3", "2.25");

const USAGE: &str = "usage: asciier convert [options] INPUT";
const HELP: &str = "\
usage: asciier convert [options] INPUT

Render an image or a video as asciier draws it.

look:
  -l, --level N                     -95..95: up raises the black point (dark areas go blank), down lowers the white point
  --contrast N                      -95..95, as mpv's contrast
  -s, --saturation PERCENT          0..300: how much colour a cell needs to take a hue: lower, more grey (default 100)
  --skin grey|theme                 skin tones grey (default) or in the theme's warm colours
  -g, --glyphs mixed|density|shape  mixed (shapes on edges, dithered density on flat areas; default), density, shape
  -m, --mono                        the theme's brightest colour on its darkest
  -b, --bold                        the font's heaviest weight
  -t, --theme NAME|FILE             a kitty theme by name or .conf file; colours it leaves out come from the current
  --crop X,Y,W,H                    corner and size in percent of the picture; past its borders is background (a negative X: --crop=-10,0,120,100)

output:
  -o, --output PATH                 the file to write (default: beside INPUT)
  -c, --columns N                   text width in columns
  -W, --width PX                    output width
  -H, --height PX                   output height
  --fit contain|cover               with --columns and a size: the whole grid inside (default) or the canvas filled
  -a, --at SECONDS                  a video's frame at that time, as a PNG
  -h, --help                        this text";

/// Ctrl-C, noted for the whole run: before a video's encoding what runs is killed and nothing made counts; while its frames are read the reading stops and the file is finished (row X9), unless no frame was written: then the run ends as before the encoding (N25); after the reading the file is finished and nothing said, however the encoder ends (row C10).
static INTERRUPTED: AtomicBool = AtomicBool::new(false);
/// Set once a video's decoder and encoder are to start: from then on Ctrl-C kills the decoder alone, while its frames are read.
static ENCODING: AtomicBool = AtomicBool::new(false);
/// The decoder's process group while its frames are read (0: none): Ctrl-C kills it then, since a decoder that writes nothing holds the reading up; locked under the kill, so that it is not reaped meanwhile.
static DECODER: Mutex<libc::pid_t> = Mutex::new(0);
/// The pipe's end through which SIGINT's handler wakes the Ctrl-C thread.
static WAKE: AtomicI32 = AtomicI32::new(-1);

/// How a run ended without its output path.
enum Stop {
    /// A usage error: exit 2.
    Usage(String),
    /// A failure: exit 1.
    Failed(String),
    /// Ctrl-C: death by SIGINT; the path of a file that holds the part converted (row X9), else nothing printed (row C10).
    Interrupted(Option<PathBuf>),
}

impl From<Error> for Stop {
    fn from(e: Error) -> Self {
        Stop::Failed(e.0)
    }
}

/// Run `asciier convert` with its arguments; the exit code (0, 1, 2), or none after Ctrl-C: the process ends by SIGINT.
pub fn main(args: impl Iterator<Item = OsString>) -> u8 {
    let options = match parse(args) {
        Ok(options) => options,
        Err((0, text)) => {
            println!("{text}");
            return 0;
        }
        Err((code, text)) => {
            say(format!("{USAGE}\nasciier convert: error: {text}\n"));
            return code;
        }
    };
    catch_ctrl_c();
    let ended = run(&options).and_then(|out| {
        // the path goes with a done run (row C4); a Ctrl-C while the file was written leaves it as it got (row C10)
        if INTERRUPTED.load(Ordering::SeqCst) {
            return Err(Stop::Interrupted(None));
        }
        print_path(&out).map_err(Error::from)?;
        if options.at.is_some() && std::io::stdout().is_terminal() && term::kitty::named() {
            desktop::icat(&out);
        }
        Ok(())
    });
    let ended = match ended {
        // what Ctrl-C killed leaves nothing to report
        _ if INTERRUPTED.load(Ordering::SeqCst) && !ENCODING.load(Ordering::SeqCst) => {
            Err(Stop::Interrupted(None))
        }
        ended => ended,
    };
    match ended {
        Ok(()) => 0,
        Err(Stop::Usage(text)) => {
            say(format!("{USAGE}\nasciier convert: error: {text}\n"));
            2
        }
        Err(Stop::Failed(text)) => {
            say(format!("asciier: {text}\n"));
            1
        }
        Err(Stop::Interrupted(out)) => {
            if let Some(out) = out {
                let _ = print_path(&out);
            }
            die_by_sigint()
        }
    }
}

fn print_path(out: &Path) -> std::io::Result<()> {
    let mut stdout = std::io::stdout().lock();
    stdout.write_all(out.as_os_str().as_bytes())?;
    stdout.write_all(b"\n")
}

/// Text to stderr in one write, so that a line a child writes there meanwhile lands between two of the converter's, never inside one, as it can between the pieces one `eprint!` writes apart (stderr is unbuffered).
fn say(text: String) {
    let _ = std::io::stderr().write_all(text.as_bytes());
}

/// Ctrl-C's end once its cleanup and messages are done: death by SIGINT, which a shell shows as 130 and which stops a script that ran the converter, where an exit 130 lets the script go on (N25); 130 only should the signal return.
fn die_by_sigint() -> u8 {
    let _ = std::io::stdout().flush();
    raise_sigint();
    130
}

/// SIGINT at its default, raised in the calling thread: the process ends by it.
fn raise_sigint() {
    unsafe {
        libc::signal(libc::SIGINT, libc::SIG_DFL);
        let mut none = std::mem::zeroed();
        libc::sigemptyset(&mut none);
        libc::pthread_sigmask(libc::SIG_SETMASK, &none, std::ptr::null_mut());
        libc::raise(libc::SIGINT);
    }
}

/// SIGINT's handler: Ctrl-C noted, the Ctrl-C thread woken; errno kept for the code it interrupted.
extern "C" fn on_ctrl_c(_: libc::c_int) {
    INTERRUPTED.store(true, Ordering::SeqCst);
    let byte = 0u8;
    unsafe {
        let errno = *libc::__errno_location();
        libc::write(WAKE.load(Ordering::SeqCst), (&raw const byte).cast(), 1);
        *libc::__errno_location() = errno;
    }
}

/// Ctrl-C for the whole run: a handler notes it and wakes a thread of its own, which, before a video's encoding, kills every child of the main thread and ends the run at once, whatever the main thread is doing (drawing, writing): nothing made then counts (row C10), and a child may outlive the terminal's SIGINT, or get none when it was sent to the converter alone; while a video's frames are read it kills the decoder's group, whose pipe the reader still holds open. A child starts with SIGINT as usual: exec resets a handler, and no signal is blocked. A SIGINT the parent left ignored stays so: a shell without job control starts a background job so, and the terminal's Ctrl-C is not that job's.
fn catch_ctrl_c() {
    let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
    let mut ends = [0; 2];
    unsafe {
        libc::sigaction(libc::SIGINT, std::ptr::null(), &mut action);
        if action.sa_sigaction == libc::SIG_IGN
            || libc::pipe2(ends.as_mut_ptr(), libc::O_CLOEXEC) != 0
        {
            return;
        }
        libc::fcntl(ends[1], libc::F_SETFL, libc::O_NONBLOCK);
    }
    WAKE.store(ends[1], Ordering::SeqCst);
    let mut woken = unsafe { std::fs::File::from_raw_fd(ends[0]) };
    let main = std::process::id();
    std::thread::spawn(move || {
        while woken.read_exact(&mut [0]).is_ok() {
            // a child between its fork and its exec runs the handler too
            if !INTERRUPTED.load(Ordering::SeqCst) {
                continue;
            }
            if ENCODING.load(Ordering::SeqCst) {
                let decoder = decoder_group();
                if *decoder != 0 {
                    kill(*decoder);
                }
                continue;
            }
            let children = std::fs::read_to_string(format!("/proc/{main}/task/{main}/children"))
                .unwrap_or_default();
            for child in children
                .split_whitespace()
                .filter_map(|pid| pid.parse().ok())
            {
                unsafe { libc::kill(child, libc::SIGKILL) };
            }
            raise_sigint();
        }
    });
    // SA_RESTART: a read, write or wait that SIGINT lands in goes on, in libchafa's threads too
    action.sa_sigaction = on_ctrl_c as extern "C" fn(libc::c_int) as libc::sighandler_t;
    action.sa_flags = libc::SA_RESTART;
    unsafe {
        libc::sigemptyset(&mut action.sa_mask);
        libc::sigaction(libc::SIGINT, &action, std::ptr::null_mut());
    }
}

fn number<T: std::str::FromStr + PartialOrd + std::fmt::Display>(
    name: &str,
    text: &str,
    (low, high): (T, T),
) -> std::result::Result<T, (u8, String)> {
    let Ok(n) = text.parse::<T>() else {
        return Err((2, format!("argument {name}: not a number: '{text}'")));
    };
    if n >= low && n <= high {
        Ok(n)
    } else {
        Err((
            2,
            format!("argument {name}: {text} is outside {low}..{high}"),
        ))
    }
}

fn choice<T: Copy>(
    name: &str,
    text: &str,
    choices: &[(&str, T)],
) -> std::result::Result<T, (u8, String)> {
    match choices.iter().find(|(word, _)| *word == text) {
        Some((_, value)) => Ok(*value),
        None => {
            let words: Vec<String> = choices
                .iter()
                .map(|(word, _)| format!("'{word}'"))
                .collect();
            Err((
                2,
                format!(
                    "argument {name}: invalid choice: '{text}' (choose from {})",
                    words.join(", ")
                ),
            ))
        }
    }
}

fn crop_spec(text: &str) -> std::result::Result<[f64; 4], (u8, String)> {
    let parts: Vec<f64> = text
        .split(',')
        .filter_map(|n| n.trim().parse().ok())
        .collect();
    let Ok(spec) = <[f64; 4]>::try_from(parts) else {
        return Err((2, format!("argument --crop: not X,Y,W,H: '{text}'")));
    };
    if text.split(',').count() != 4 || spec.iter().any(|n| !n.is_finite()) {
        return Err((2, format!("argument --crop: not X,Y,W,H: '{text}'")));
    }
    if spec[2] <= 0.0 || spec[3] <= 0.0 {
        Err((
            2,
            format!("argument --crop: W and H must be above 0: '{text}'"),
        ))
    } else {
        Ok(spec)
    }
}

/// The options from the arguments; the `Err` is the exit code and its text: 2 with a usage error, 0 with the help text.
pub fn parse(args: impl Iterator<Item = OsString>) -> std::result::Result<Options, (u8, String)> {
    use lexopt::prelude::*;
    let mut options = Options::default();
    let mut input = None;
    let mut parser = lexopt::Parser::from_args(args);
    loop {
        let arg = match parser.next() {
            Ok(Some(arg)) => arg,
            Ok(None) => break,
            Err(e) => return Err((2, e.to_string())),
        };
        let name = match &arg {
            Short('l') | Long("level") => "-l/--level",
            Long("contrast") => "--contrast",
            Short('s') | Long("saturation") => "-s/--saturation",
            Long("skin") => "--skin",
            Short('g') | Long("glyphs") => "-g/--glyphs",
            Short('t') | Long("theme") => "-t/--theme",
            Long("crop") => "--crop",
            Short('o') | Long("output") => "-o/--output",
            Short('c') | Long("columns") => "-c/--columns",
            Short('W') | Long("width") => "-W/--width",
            Short('H') | Long("height") => "-H/--height",
            Long("fit") => "--fit",
            Short('a') | Long("at") => "-a/--at",
            Short('m') | Long("mono") => {
                options.look.mono = true;
                continue;
            }
            Short('b') | Long("bold") => {
                options.look.bold = true;
                continue;
            }
            Short('h') | Long("help") => return Err((0, HELP.into())),
            Value(value) if input.is_none() => {
                input = Some(PathBuf::from(value));
                continue;
            }
            other => {
                return Err((
                    2,
                    format!("unrecognized arguments: {}", other.clone().unexpected())
                        .replace("invalid option ", "")
                        .replace("unexpected argument ", ""),
                ));
            }
        };
        let Ok(value) = parser.value() else {
            return Err((2, format!("argument {name}: expected one argument")));
        };
        let text = value.to_string_lossy();
        match name {
            "-l/--level" => options.look.level = number(name, &text, look::LEVEL)?,
            "--contrast" => options.look.contrast = number(name, &text, look::CONTRAST)?,
            "-s/--saturation" => options.look.saturation = number(name, &text, look::SATURATION)?,
            "--skin" => {
                options.look.skin =
                    choice(name, &text, &[("grey", Skin::Grey), ("theme", Skin::Theme)])?
            }
            "-g/--glyphs" => {
                options.look.glyphs = choice(name, &text, &Glyphs::ALL.map(|g| (g.name(), g)))?
            }
            "-t/--theme" => options.theme = Some(value),
            "--crop" => options.crop = crop_spec(&text)?,
            "-o/--output" => options.output = Some(PathBuf::from(value)),
            "-c/--columns" => options.columns = Some(number(name, &text, COLUMNS)?),
            "-W/--width" => options.width = Some(number(name, &text, PX)?),
            "-H/--height" => options.height = Some(number(name, &text, PX)?),
            "--fit" => {
                options.fit = choice(
                    name,
                    &text,
                    &[("contain", Fit::Contain), ("cover", Fit::Cover)],
                )?
            }
            _ => {
                let at: f64 = number(name, &text, (0.0, f64::INFINITY))?;
                options.at = Some(at);
            }
        }
    }
    match input {
        Some(input) => Ok(Options { input, ..options }),
        None => Err((2, "the following arguments are required: INPUT".into())),
    }
}

/// A float as C's `%g` prints it: six significant digits, no trailing zeros.
fn g(x: f64) -> String {
    if x == 0.0 || !x.is_finite() {
        return format!("{x}");
    }
    let exponent = x.abs().log10().floor() as i32;
    if !(-4..6).contains(&exponent) {
        let text = format!("{:.5e}", x);
        let (mantissa, power) = text.split_once('e').unwrap_or((&text, "0"));
        let mantissa = mantissa.trim_end_matches('0').trim_end_matches('.');
        let power: i32 = power.parse().unwrap_or(0);
        return format!(
            "{mantissa}e{}{:02}",
            if power < 0 { '-' } else { '+' },
            power.abs()
        );
    }
    let text = format!("{:.*}", (5 - exponent).max(0) as usize, x);
    if text.contains('.') {
        text.trim_end_matches('0').trim_end_matches('.').to_string()
    } else {
        text
    }
}

/// The output file beside `source`: `<stem>-ascii.png`, `<stem>-ascii.mp4` for a video, `<stem>-ascii-<at>s.png` for a frame (row C3).
pub fn out_name(source: &Path, at: Option<f64>, video: bool) -> PathBuf {
    let tail = match at {
        Some(at) => format!("-ascii-{}s.png", g(at)),
        None if video => "-ascii.mp4".into(),
        None => "-ascii.png".into(),
    };
    let mut name = source.file_stem().unwrap_or_default().to_os_string();
    name.push(tail);
    source.with_file_name(name)
}

/// 1920 px on the long side in the frame's shape, even numbers (row L5).
pub fn web_size((w, h): (u32, u32)) -> (u32, u32) {
    let short = |long: u32, short: u32| {
        ((960.0 * short as f64 / long as f64).round_ties_even() as u32).max(1) * 2
    };
    if w >= h {
        (1920, short(w, h))
    } else {
        (short(h, w), 1920)
    }
}

/// `--crop` in px: left, top, right, bottom (row L9); the window may reach past the picture.
pub fn crop_rect((w, h): (u32, u32), [x, y, cw, ch]: [f64; 4]) -> [i64; 4] {
    let px = |size: u32, percent: f64| (size as f64 * percent / 100.0).round_ties_even() as i64;
    // Dimensions beyond u32 already exceed the pixel limit. Once a window lies
    // wholly outside an edge, shifting it farther only increases ffmpeg's pad.
    let axis = |size, origin, extent| {
        let span = px(size, extent).clamp(1, i64::from(u32::MAX));
        let start = px(size, origin).clamp(-span, i64::from(size));
        (start, start + span)
    };
    let (left, right) = axis(w, x, cw);
    let (top, bottom) = axis(h, y, ch);
    [left, top, right, bottom]
}

/// The picture through `--crop`; outside it is black. A window over `picture::MAX_PIXELS` is an error: it is allocated whole.
pub fn crop(picture: &RgbImage, spec: [f64; 4]) -> Result<Cow<'_, RgbImage>> {
    if spec == FULL {
        return Ok(Cow::Borrowed(picture));
    }
    let [left, top, right, bottom] = crop_rect(picture.dimensions(), spec);
    let most = picture::MAX_PIXELS;
    if (right - left) as u64 * (bottom - top) as u64 > most {
        err!("crop over {:.0} megapixels", most as f64 / 1e6);
    }
    let mut out = RgbImage::new((right - left) as u32, (bottom - top) as u32);
    for (x, y, pixel) in out.enumerate_pixels_mut() {
        let (sx, sy) = (x as i64 + left, y as i64 + top);
        if sx >= 0 && sy >= 0 && (sx as u32) < picture.width() && (sy as u32) < picture.height() {
            *pixel = *picture.get_pixel(sx as u32, sy as u32);
        }
    }
    Ok(Cow::Owned(out))
}

/// `--crop` as ffmpeg filters (with a trailing comma) for frames of `size`, or nothing: pad with black to take in the window, then crop it.
fn crop_filter(size: (u32, u32), spec: [f64; 4]) -> String {
    if spec == FULL {
        return String::new();
    }
    let [left, top, right, bottom] = crop_rect(size, spec);
    // A subsampled input rounds padding down to its chroma grid. Multiples
    // of four cover the 4:1:1 and 4:1:0 grids as well as 4:2:0.
    let aligned = |px: i64| (px + 3) / 4 * 4;
    let (pad_l, pad_t) = (aligned((-left).max(0)), aligned((-top).max(0)));
    let (pad_w, pad_h) = (
        aligned((size.0 as i64).max(right) + pad_l),
        aligned((size.1 as i64).max(bottom) + pad_t),
    );
    format!(
        "pad={pad_w}:{pad_h}:{pad_l}:{pad_t}:black,crop={}:{}:{}:{}:exact=1,",
        right - left,
        bottom - top,
        left + pad_l,
        top + pad_t
    )
}

/// The colours to draw in: the current kitty theme, `--theme` over it, mono's two (rows C7, C8).
pub fn colours(options: &Options) -> Result<Theme> {
    let mut colours = Theme::current()?;
    if let Some(name) = &options.theme {
        let text = if Path::new(name).is_file() {
            std::fs::read_to_string(name)?
        } else {
            theme::by_name(&name.to_string_lossy())?
        };
        colours.overlay(&text);
    }
    if options.look.mono {
        (colours.background, colours.foreground) = colours.extremes();
    }
    Ok(colours)
}

/// A picture (already turned and flattened by the caller) drawn at full size: what a screenshot, the page's export and the converter's still share; `frame`: a video's frame, 1920 px on the long side unless a size is given.
pub fn draw(picture: &RgbImage, options: &Options, frame: bool) -> Result<RgbImage> {
    let picture = crop(picture, options.crop)?;
    let mut options = options.clone();
    if frame {
        options.set_frame_size(picture.dimensions());
    }
    let mut style = Style::new(&options, colours(&options)?, picture.dimensions())?;
    let art = style.art.cells(&picture)?;
    style.layout(art.rows as u32);
    Ok(style.paint(&art, false))
}

/// The next PPM frame of an ffmpeg stream; `None` at its end.
fn ppm_frame(stream: &mut impl BufRead) -> Option<RgbImage> {
    let mut line = String::new();
    let mut header = |line: &mut String| {
        line.clear();
        stream
            .read_line(line)
            .ok()
            .filter(|&n| n > 0)
            .map(|_| line.trim().to_string())
    };
    header(&mut line).filter(|magic| magic == "P6")?;
    let size = header(&mut line)?;
    let (w, h) = size
        .split_once(' ')
        .and_then(|(w, h)| Some((w.parse::<u32>().ok()?, h.parse::<u32>().ok()?)))?;
    header(&mut line)?;
    let mut data = vec![0; w as usize * h as usize * 3];
    stream.read_exact(&mut data).ok()?;
    RgbImage::from_raw(w, h, data)
}

fn ffmpeg() -> Command {
    let mut command = Command::new("ffmpeg");
    command
        .args(["-v", "error", "-nostdin"])
        .stdin(Stdio::null());
    command
}

/// Disambiguate local names that ffmpeg or ffprobe would parse as a switch or protocol.
fn media_path(path: &Path) -> Cow<'_, Path> {
    let bytes = path.as_os_str().as_bytes();
    let first = bytes.split(|&byte| byte == b'/').next().unwrap_or_default();
    if first.starts_with(b"-") || first.contains(&b':') {
        Cow::Owned(Path::new(".").join(path))
    } else {
        Cow::Borrowed(path)
    }
}

/// The source's frame at `seconds`, at full size, upright: ffmpeg seeks there before it reads the source, a seek after which ffmpeg n9.0.2 finds no frame of an animated WebP or a raw H.264 stream, not even at 0 s; at 0 s the first frame, taken without a seek; where the seek finds none though the source may run on past `seconds` (ffprobe tells no length, or a longer one) the source is decoded from its start to there.
fn frame_at(source: &Path, seconds: f64) -> Result<RgbImage> {
    let at = format!("{seconds:?}");
    let seek = ["-ss", at.as_str()];
    let grab = |before: &[&str], after: &[&str]| -> Result<(Option<RgbImage>, String)> {
        let output = ffmpeg()
            .args(before)
            .arg("-i")
            .arg(source)
            .args(after)
            .args(["-frames:v", "1", "-f", "image2pipe", "-c:v", "ppm", "-"])
            .output()
            .map_err(|e| Error(format!("cannot run ffmpeg: {e}")))?;
        let said = String::from_utf8_lossy(&output.stderr).trim().to_string();
        Ok((ppm_frame(&mut BufReader::new(&output.stdout[..])), said))
    };
    let first: &[&str] = if seconds > 0.0 { &seek } else { &[] };
    let (mut frame, mut said) = grab(first, &[])?;
    if frame.is_none()
        && said.is_empty()
        && seconds > 0.0
        && length(source).is_none_or(|length| seconds < length)
    {
        (frame, said) = grab(&[], &seek)?;
    }
    frame.ok_or_else(|| {
        Error(if said.is_empty() {
            format!("no frame at {at} s in {}", source.display())
        } else {
            said
        })
    })
}

/// The source's length in seconds, if ffprobe tells it.
fn length(source: &Path) -> Option<f64> {
    let probe = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_entries",
            "format=duration",
            "-of",
            "csv=p=0",
        ])
        .arg(source)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    String::from_utf8_lossy(&probe.stdout).trim().parse().ok()
}

/// Whether the GPU encodes H.264 at this size here: a render node, and a one-frame trial on it that works; libx264 otherwise.
fn vaapi(width: u32, height: u32) -> bool {
    let source = format!("color=s={width}x{height}:d=0.04");
    Path::new(VAAPI).exists()
        && ffmpeg()
            .args([
                "-vaapi_device",
                VAAPI,
                "-f",
                "lavfi",
                "-i",
                &source,
                "-vf",
                "format=nv12,hwupload",
                "-c:v",
                "h264_vaapi",
                "-f",
                "null",
                "-",
            ])
            .output()
            .is_ok_and(|output| output.status.success())
}

fn gcd(a: u64, b: u64) -> u64 {
    if b == 0 { a } else { gcd(b, a % b) }
}

/// The input stream mapped to the sole PPM output, as in the frame grab and decoder.
fn selected_video(source: &Path) -> Result<u64> {
    let report = ffmpeg()
        .args(["-hide_banner", "-nostats", "-v", "info", "-i"])
        .arg(source)
        .args(["-frames:v", "0", "-f", "image2pipe", "-c:v", "ppm", "-"])
        .env("AV_LOG_FORCE_NOCOLOR", "1")
        .output()
        .map_err(|e| Error(format!("cannot run ffmpeg: {e}")))?;
    let log = String::from_utf8_lossy(&report.stderr);
    let index = log
        .lines()
        .skip_while(|&line| line != "Stream mapping:")
        .nth(1)
        .and_then(|line| {
            let line = line.trim_start().strip_prefix("Stream #0:")?;
            line.split_once(" -> #0:0 ")?.0.parse().ok()
        });
    index.ok_or_else(|| {
        Error(format!(
            "cannot identify the selected video in {}",
            source.display()
        ))
    })
}

/// The selected video stream's average rate as a reduced fraction (25 if unknown) and the frames it makes at that rate (0: unknown).
fn rate_and_total(source: &Path) -> Result<((u64, u64), u64)> {
    let probe = match Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            "v",
            "-show_entries",
            "stream=index,avg_frame_rate,nb_frames,duration:format=duration",
            "-of",
            "json",
        ])
        .arg(source)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
    {
        Ok(probe) => probe,
        Err(e) => err!("cannot run ffprobe: {e}"),
    };
    let info: serde_json::Value = serde_json::from_slice(&probe.stdout).unwrap_or_default();
    let streams = &info["streams"];
    let stream = match streams.as_array() {
        Some(videos) if videos.len() > 1 => {
            let index = selected_video(source)?;
            videos
                .iter()
                .find(|video| video["index"].as_u64() == Some(index))
                .ok_or_else(|| Error(format!("no probe data for video stream {index}")))?
        }
        _ => &streams[0],
    };
    let rate = stream["avg_frame_rate"].as_str().and_then(|rate| {
        let (num, den) = rate.split_once('/').unwrap_or((rate, "1"));
        let (num, den) = (num.parse::<u64>().ok()?, den.parse::<u64>().ok()?);
        (num > 0 && den > 0).then(|| (num / gcd(num, den), den / gcd(num, den)))
    });
    let (num, den) = rate.unwrap_or((25, 1));
    let seconds =
        |value: &serde_json::Value| value.as_str().and_then(|text| text.parse::<f64>().ok());
    let total = match seconds(&stream["duration"]).or_else(|| seconds(&info["format"]["duration"]))
    {
        Some(length) => (length * num as f64 / den as f64).round_ties_even() as u64,
        None => stream["nb_frames"]
            .as_str()
            .and_then(|n| n.parse().ok())
            .unwrap_or(0),
    };
    Ok(((num, den), total))
}

/// A float as Python's `.3g` prints it.
fn g3(x: f64) -> String {
    let digits = (2 - x.abs().log10().floor() as i32).max(0) as usize;
    let text = format!("{x:.digits$}");
    if text.contains('.') {
        text.trim_end_matches('0').trim_end_matches('.').to_string()
    } else {
        text
    }
}

fn own_session(command: &mut Command) -> &mut Command {
    // Ctrl-C reaches only this process, which then stops the children in order
    unsafe {
        command.pre_exec(|| {
            libc::setsid();
            Ok(())
        })
    }
}

/// Kill a child started in a session of its own, with every process of its group.
fn kill(leader: libc::pid_t) {
    unsafe { libc::kill(-leader, libc::SIGKILL) };
}

fn decoder_group() -> MutexGuard<'static, libc::pid_t> {
    DECODER.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A video's decoder and encoder, each in a session of its own; a decoder whose encoder cannot start is killed and reaped.
fn start(
    decoder: &mut Command,
    encoder: &mut Command,
) -> std::result::Result<(Child, Child), Stop> {
    let spawn = |command: &mut Command| {
        own_session(command)
            .spawn()
            .map_err(|e| Stop::Failed(format!("cannot run ffmpeg: {e}")))
    };
    let mut decode = spawn(decoder)?;
    match spawn(encoder) {
        Ok(encode) => Ok((decode, encode)),
        Err(stop) => {
            kill(decode.id() as libc::pid_t);
            let _ = decode.wait();
            Err(stop)
        }
    }
}

/// `source` to `out`: ffmpeg decodes at the output's detail, each frame is drawn, ffmpeg encodes; `first` is the frame at 0 s, uncropped.
fn video(
    options: &Options,
    source: &Path,
    out: &Path,
    first: &RgbImage,
) -> std::result::Result<(), Stop> {
    let ((num, den), total) = rate_and_total(source)?;
    let cropped = crop(first, options.crop)?;
    let mut options = options.clone();
    options.set_frame_size(cropped.dimensions());
    let mut style = Style::new(&options, colours(&options)?, cropped.dimensions())?;
    let rows = style.art.cells(&cropped)?.rows;
    style.layout(rows as u32);
    let (w, h) = style.size(true);
    let detail = style.art.cols * CELL_PX;
    let shrink = (cropped.width() as f64 / detail as f64).max(1.0);
    let filters = format!(
        "{}fps={num}/{den},scale='min(iw,{detail})':-2,hqdn3d={}:{}:{}:{}",
        crop_filter(first.dimensions(), options.crop),
        g3(DENOISE.0 / shrink),
        g3(DENOISE.1 / shrink),
        DENOISE.2,
        DENOISE.3
    );
    // VAAPI: about a fifth of libx264's CPU; qp 24 matches libx264 -crf 20 on ASCII frames
    let (device, codec): (&[&str], &[&str]) = if vaapi(w, h) {
        (
            &["-vaapi_device", VAAPI],
            &[
                "-vf",
                "format=nv12,hwupload",
                "-c:v",
                "h264_vaapi",
                "-qp",
                "24",
            ],
        )
    } else {
        (
            &[],
            &["-c:v", "libx264", "-crf", "20", "-pix_fmt", "yuv420p"],
        )
    };
    // audio shorter than the video is padded with silence to a second past the video's expected end; -shortest ends the file with the video
    let pad = if total > 0 {
        format!(
            "apad=whole_dur={:.3}",
            total as f64 * den as f64 / num as f64 + 1.0
        )
    } else {
        "apad".into()
    };
    let (size, rate) = (format!("{w}x{h}"), format!("{num}/{den}"));
    ENCODING.store(true, Ordering::SeqCst);
    if INTERRUPTED.load(Ordering::SeqCst) {
        return Err(Stop::Interrupted(None));
    }
    let (mut decode, mut encode) = start(
        ffmpeg()
            .arg("-i")
            .arg(source)
            .args([
                "-an",
                "-sn",
                "-vf",
                &filters,
                "-f",
                "image2pipe",
                "-c:v",
                "ppm",
                "-",
            ])
            .stdout(Stdio::piped()),
        Command::new("ffmpeg")
            .args(["-v", "error", "-y"])
            .args(device)
            .args([
                "-f", "rawvideo", "-pix_fmt", "rgb24", "-s", &size, "-r", &rate, "-i", "-", "-i",
            ])
            .arg(source)
            .args(["-map", "0:v", "-map", "1:a?"])
            .args(codec)
            .args(["-c:a", "aac", "-af", &pad, "-shortest"])
            .arg(media_path(out).as_os_str())
            .stdin(Stdio::piped()),
    )?;
    let shown = if total > 0 {
        total.to_string()
    } else {
        "?".into()
    };
    let mut frames = BufReader::with_capacity(1 << 20, decode.stdout.take().expect("piped"));
    let mut sink = encode.stdin.take().expect("piped");
    let (mut done, mut art, decoder) = (0u64, style.art.clone(), decode.id() as libc::pid_t);
    *decoder_group() = decoder;
    say(format!("\r0/{shown} frames"));
    // two stages side by side, frames in order: reading and cells on a thread, painting and writing here; the decoder is killed before its pipe closes, of which it would complain on stderr
    let (grids, painted) = std::sync::mpsc::sync_channel::<Grid>(8);
    // Ok(true): Ctrl-C stopped the reading
    let read = std::thread::scope(|scope| {
        let reader = scope.spawn(move || -> Result<bool> {
            let ended = loop {
                if INTERRUPTED.load(Ordering::Relaxed) {
                    break Ok(true);
                }
                let Some(frame) = ppm_frame(&mut frames) else {
                    // the stream's end: the decoder ends by itself, or Ctrl-C killed it
                    return Ok(INTERRUPTED.load(Ordering::SeqCst));
                };
                let grid = match art.cells(&frame) {
                    Ok(grid) => grid,
                    Err(e) => break Err(e),
                };
                if grids.send(grid).is_err() {
                    break Ok(false); // the writer stopped
                }
            };
            kill(decoder);
            ended
        });
        for grid in painted {
            if sink.write_all(style.paint(&grid, true).as_raw()).is_err() {
                kill(decoder); // the encoder quit: its exit code says why
                break;
            }
            done += 1;
            say(format!("\r{done}/{shown} frames"));
        }
        reader.join().expect("the reader")
    });
    say(format!("\r{done}/{shown} frames\n"));
    *decoder_group() = 0;
    // a Ctrl-C before a frame was written leaves the encoder nothing to finish: killed while it waits for its first input, it neither complains of an empty one nor opens its output, and the run ends as one before the encoding (N25 against row X9)
    let unmade = || done == 0 && INTERRUPTED.load(Ordering::SeqCst);
    if unmade() {
        kill(encode.id() as libc::pid_t);
    }
    let finish = |child: &mut Child| child.wait().map(|status| status.success()).unwrap_or(false);
    let decoded = finish(&mut decode);
    drop(sink);
    let encoded = finish(&mut encode);
    if unmade() {
        return Err(Stop::Interrupted(None));
    }
    let stopped = read?;
    // a Ctrl-C after the reading: the file as the encoder left it, nothing said, also of an encoder that failed (row C10)
    if INTERRUPTED.load(Ordering::SeqCst) && !stopped {
        return Err(Stop::Interrupted(None));
    }
    if !encoded {
        return Err(Stop::Failed("ffmpeg could not encode the video".into()));
    }
    if stopped {
        say(format!(
            "stopped at frame {done}: {} holds the part converted\n",
            out.display()
        ));
        return Err(Stop::Interrupted(Some(out.to_path_buf())));
    }
    if !decoded {
        return Err(Stop::Failed(format!(
            "ffmpeg stopped decoding {}: {} holds the part converted",
            source.display(),
            out.display()
        )));
    }
    Ok(())
}

fn png_only(out: &Path) -> std::result::Result<(), Stop> {
    if out
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("png"))
    {
        Ok(())
    } else {
        Err(Stop::Usage(format!(
            "argument -o/--output: a picture is written as PNG: '{}'",
            out.display()
        )))
    }
}

fn save(picture: &RgbImage, out: &Path) -> std::result::Result<(), Stop> {
    let bytes = picture::png(picture)?;
    if INTERRUPTED.load(Ordering::SeqCst) {
        return Err(Stop::Interrupted(None)); // a file in the way stays as it was
    }
    std::fs::write(out, bytes)
        .map_err(|e| Stop::Failed(format!("cannot write {}: {e}", out.display())))
}

/// Convert as the options say; the output's path.
fn run(options: &Options) -> std::result::Result<PathBuf, Stop> {
    let source = &options.input;
    let still: Option<Still> = match picture::open(source) {
        Opened::Still(still) => Some(still),
        Opened::TooLarge(message) => return Err(Stop::Failed(message)),
        Opened::Animation | Opened::NotImage => None, // a video, an animation, or no picture: ffmpeg tries
    };
    let media = if source.is_file() {
        media_path(source)
    } else {
        Cow::Borrowed(source.as_path())
    };
    if let Some(at) = options.at {
        if still.is_some() {
            return Err(Stop::Usage(
                "--at takes a video's frame; this is an image".into(),
            ));
        }
        let out = options
            .output
            .clone()
            .unwrap_or_else(|| out_name(source, Some(at), false));
        png_only(&out)?;
        save(&draw(&frame_at(&media, at)?, options, true)?, &out)?;
        return Ok(out);
    }
    let Some(still) = still else {
        let out = options
            .output
            .clone()
            .unwrap_or_else(|| out_name(source, None, true));
        let first = frame_at(&media, 0.0)?;
        video(options, &media, &out, &first)?;
        return Ok(out);
    };
    let out = options
        .output
        .clone()
        .unwrap_or_else(|| out_name(source, None, false));
    png_only(&out)?;
    let colours = colours(options)?;
    let (w, h) = still.pic.size();
    let [left, top, right, bottom] = crop_rect((w, h), options.crop);
    let cropped = if options.crop == FULL {
        (w, h)
    } else {
        ((right - left) as u32, (bottom - top) as u32)
    };
    let mut style = Style::new(options, colours.clone(), cropped)?;
    // an SVG is drawn again at the detail the columns take
    let need = (style.art.cols as f64 * CELL_PX as f64 * 100.0 / options.crop[2]).ceil() as u32;
    let pic = match &still.svg {
        Some(svg) if need > w => svg
            .raster(Some(picture::raster_width(need, (w, h))))
            .map(picture::Pic::Rgba)
            .unwrap_or(still.pic),
        _ => still.pic,
    };
    let flattened = picture::flatten(&pic, colours.background);
    let picture = crop(&flattened, options.crop)?;
    let art = style.art.cells(&picture)?;
    style.layout(art.rows as u32);
    save(&style.paint(&art, false), &out)?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(flags: &[&str]) -> std::result::Result<Options, (u8, String)> {
        parse(
            flags
                .iter()
                .map(OsString::from)
                .chain([OsString::from("in.png")]),
        )
    }

    #[test]
    fn long_names_and_defaults() {
        let o = args(&[
            "--level",
            "-20",
            "--contrast",
            "10",
            "--saturation",
            "50",
            "--skin",
            "theme",
            "--glyphs",
            "shape",
            "--mono",
            "--bold",
            "--crop",
            "10,10,50,50",
            "--columns",
            "90",
            "--fit",
            "cover",
            "--at",
            "1.5",
        ])
        .unwrap();
        assert_eq!(
            (
                o.look.level,
                o.look.contrast,
                o.look.saturation,
                o.look.skin,
                o.look.glyphs
            ),
            (-20, 10, 50, Skin::Theme, Glyphs::Shape)
        );
        assert_eq!(
            (o.look.mono, o.look.bold, o.crop, o.columns, o.fit, o.at),
            (
                true,
                true,
                [10.0, 10.0, 50.0, 50.0],
                Some(90),
                Fit::Cover,
                Some(1.5)
            )
        );
        let d = args(&[]).unwrap();
        assert_eq!(
            (d.look, d.crop, d.input),
            (Look::default(), FULL, PathBuf::from("in.png"))
        );
    }

    #[test]
    fn bad_values_are_usage_errors() {
        for (flags, tail) in [
            (
                vec!["--level", "96"],
                "argument -l/--level: 96 is outside -95..95",
            ),
            (
                vec!["--saturation", "-1"],
                "argument -s/--saturation: -1 is outside 0..300",
            ),
            (
                vec!["--crop", "1,2,3"],
                "argument --crop: not X,Y,W,H: '1,2,3'",
            ),
            (
                vec!["--crop", "0,0,0,10"],
                "argument --crop: W and H must be above 0: '0,0,0,10'",
            ),
            (
                vec!["--columns", "x"],
                "argument -c/--columns: not a number: 'x'",
            ),
            (vec!["--at", "-1"], "argument -a/--at: -1 is outside 0..inf"),
            (
                vec!["-W", "4"],
                "argument -W/--width: 4 is outside 16..16384",
            ),
            (
                vec!["--skin", "red"],
                "argument --skin: invalid choice: 'red' (choose from 'grey', 'theme')",
            ),
        ] {
            assert_eq!(
                args(&flags).unwrap_err(),
                (2, tail.to_string()),
                "{flags:?}"
            );
        }
        assert_eq!(
            parse(std::iter::empty()).unwrap_err(),
            (2, "the following arguments are required: INPUT".to_string())
        );
        assert_eq!(args(&["--help"]).unwrap_err().0, 0);
    }

    #[test]
    fn short_flags_combine_and_take_negative_values() {
        let o = args(&["-mb", "-l", "-20", "--crop=-10,0,120,100", "-c80"]).unwrap();
        assert_eq!(
            (o.look.mono, o.look.bold, o.look.level, o.crop, o.columns),
            (true, true, -20, [-10.0, 0.0, 120.0, 100.0], Some(80))
        );
    }

    #[test]
    fn crops_outside_the_picture_stay_black_and_oversize_crops_are_refused() {
        let white = RgbImage::from_pixel(8, 8, image::Rgb([255; 3]));
        for offset in [1e30, -1e30, 53_687_091_200.0] {
            let cut = crop(&white, [offset, 0.0, 100.0, 100.0]).unwrap();
            assert_eq!(cut.as_ref(), &RgbImage::new(8, 8));
        }
        assert!(crop(&white, [0.0, 0.0, 1e30, 1e30]).is_err());
    }

    #[test]
    fn even_frame_dimensions_are_at_least_two_pixels() {
        assert_eq!(web_size((4000, 1)), (1920, 2));
        assert_eq!(web_size((1, 4000)), (2, 1920));
    }

    #[test]
    fn names_sizes_crops() {
        assert_eq!(
            out_name(Path::new("/d/a.b.png"), None, false),
            PathBuf::from("/d/a.b-ascii.png")
        );
        assert_eq!(
            out_name(Path::new("/d/v.mkv"), None, true),
            PathBuf::from("/d/v-ascii.mp4")
        );
        assert_eq!(
            out_name(Path::new("/d/v.mkv"), Some(1.5), false),
            PathBuf::from("/d/v-ascii-1.5s.png")
        );
        assert_eq!(
            out_name(Path::new("v.mkv"), Some(5.0), false),
            PathBuf::from("v-ascii-5s.png")
        );
        assert_eq!(
            [
                web_size((1920, 1080)),
                web_size((1080, 1920)),
                web_size((500, 500)),
                web_size((1000, 333))
            ],
            [(1920, 1080), (1080, 1920), (1920, 1920), (1920, 640)]
        );
        assert_eq!(crop_rect((200, 100), FULL), [0, 0, 200, 100]);
        assert_eq!(
            crop_rect((200, 100), [-10.0, 50.0, 50.0, 100.0]),
            [-20, 50, 80, 150]
        );
        assert_eq!(crop_rect((200, 100), [0.0, 0.0, 0.1, 0.1]), [0, 0, 1, 1]);
        assert_eq!(crop_rect((250, 100), [25.0, 0.0, 50.0, 100.0])[0], 62); // a tie rounds to even, as Python's round
        assert_eq!(crop_filter((200, 100), FULL), "");
        assert_eq!(
            crop_filter((200, 100), [-10.0, 0.0, 120.0, 100.0]),
            "pad=240:100:20:0:black,crop=240:100:0:0:exact=1,"
        );
        let white = RgbImage::from_pixel(100, 100, image::Rgb([255, 255, 255]));
        let cut = crop(&white, [50.0, 0.0, 100.0, 100.0]).unwrap();
        assert_eq!(
            (
                cut.dimensions(),
                cut.get_pixel(99, 50).0,
                cut.get_pixel(0, 50).0
            ),
            ((100, 100), [0, 0, 0], [255, 255, 255])
        );
        // 178956970 px at most: a window of 1 x 178956971 px of 100 x 100 is refused, nothing allocated
        let refused = crop(&white, [0.0, 0.0, 1.0, 178956971.0])
            .err()
            .map(|e| e.0);
        assert_eq!(refused.as_deref(), Some("crop over 179 megapixels"));
        assert!(crop(&white, [0.0, 0.0, 1e300, 1.0]).is_err());
    }

    #[test]
    fn a_decoder_whose_encoder_cannot_start_is_killed_and_reaped() {
        // a black-box run cannot fail the encoder's spawn alone: both run the same ffmpeg on PATH, the encoder started right after the decoder, before anything the decoder runs could change what PATH finds
        let children = || std::fs::read_to_string("/proc/thread-self/children").unwrap();
        let before = children();
        let said = match start(
            Command::new("sleep").arg("30"),
            &mut Command::new("/nonexistent/ffmpeg"),
        ) {
            Err(Stop::Failed(said)) => said,
            _ => panic!("no failure"),
        };
        assert_eq!(
            said,
            "cannot run ffmpeg: No such file or directory (os error 2)"
        );
        assert_eq!(children(), before, "the decoder left as a child");
    }

    #[test]
    fn the_vaapi_trial_is_at_the_output_size() {
        // row X6: tests/test_convert.py:344 at tag python-final: the one-frame trial runs at the size asked, not at one of its own (a 64 x 64 trial, under the 128 px a GPU's encoder may need, sent every video to libx264): vaapi(w, h) says what that trial at w x h says, 64 x 64 and 1920 x 1080; nothing to check without the render node
        if !Path::new(VAAPI).exists() {
            return;
        }
        let encodes = |w: u32, h: u32| {
            Command::new("ffmpeg")
                .args(["-v", "error", "-nostdin", "-vaapi_device", VAAPI])
                .args(["-f", "lavfi", "-i", &format!("color=s={w}x{h}:d=0.04")])
                .args(["-vf", "format=nv12,hwupload", "-c:v", "h264_vaapi"])
                .args(["-f", "null", "-"])
                .stdin(Stdio::null())
                .output()
                .is_ok_and(|output| output.status.success())
        };
        for (w, h) in [(64, 64), (1920, 1080)] {
            assert_eq!(vaapi(w, h), encodes(w, h), "{w} x {h}");
        }
    }

    #[test]
    fn floats_print_as_c_does() {
        assert_eq!(
            [g(1.5), g(5.0), g(0.1), g(12.30), g(1234567.0), g(0.00001)],
            ["1.5", "5", "0.1", "12.3", "1.23457e+06", "1e-05"]
        );
        assert_eq!(
            [g3(2.0), g3(1.5), g3(2.0 / 1.5), g3(0.25), g3(1.0 / 12.0)],
            ["2", "1.5", "1.33", "0.25", "0.0833"]
        );
    }
}
