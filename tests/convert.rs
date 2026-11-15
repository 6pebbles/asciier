//! The converter: its command line and messages, where it writes and what it prints, how cells are laid out and painted, and video: rate, decode filters, encoders, sound, progress, Ctrl-C, an ffmpeg that fails (inventory sections 12 to 14).

mod common;

use common::*;
use image::RgbImage;
use std::collections::HashSet;
use std::ffi::OsStr;
use std::io::Read;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Child, ExitStatus, Output, Stdio};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// bl1nk's background (tests/bl1nk.conf): blank cells, bands, the room around glyphs.
const GROUND: [u8; 3] = [0x11; 3];
/// bl1nk's color0 to color15, then its foreground: the inks a cell can take.
const INKS: [[u8; 3]; 17] = [
    [0x1a, 0x1c, 0x1d],
    [0xff, 0x58, 0x94],
    [0xb5, 0xe3, 0x54],
    [0xf5, 0x92, 0x1d],
    [0x4e, 0x81, 0xaa],
    [0x8b, 0x54, 0xfe],
    [0x46, 0x54, 0x56],
    [0xcb, 0xcb, 0xc5],
    [0x50, 0x53, 0x54],
    [0xf9, 0x25, 0x71],
    [0x81, 0xb3, 0x13],
    [0xfe, 0xed, 0x6b],
    [0x0b, 0x72, 0xc1],
    [0x9d, 0x6e, 0xfe],
    [0x88, 0x9b, 0xa1],
    [0xf8, 0xf8, 0xf2],
    [0xa0, 0xa0, 0xa0],
];
/// An encoder input frame at `-W 320 -H 180`: RGB, 3 bytes a px (the old converter fed rgb0, 4).
const FRAME: usize = 320 * 180 * 3;
/// A decoded 160 x 90 frame in the decoder's PPM stream: header and RGB.
const PPM_FRAME: usize = "P6\n160 90\n255\n".len() + 160 * 90 * 3;
/// A theme whose lightest colour is a vivid yellow and whose darkest is not the background.
const VIVID: &str =
    "background #303030\ncolor0 #000000\ncolor7 #F0F0F0\ncolor11 #FFFF00\ncolor15 #E0E0E0\n";
/// Shell lines that wait for `$DUMP/go`, 60 s at most: a test that fails before it lets go leaves nothing waiting for long.
const HELD: &str =
    r#"n=0; while [ ! -e "$DUMP/go" ] && [ $n -lt 1200 ]; do sleep 0.05; n=$((n + 1)); done"#;

/// ffmpeg as the converter runs it, first on PATH (tests/test_convert.py:28 at tag python-final): the decoder's PPM stream kept in $DUMP/decode.ppm and its arguments in $DUMP/decode.args, the encoder's input in $DUMP/encode.raw and its arguments in $DUMP/encode.args; $FAIL_AFTER: the encoder reads that many bytes and fails; $HOLD: it takes $HOLD frames of $FRAME bytes, then nothing until $DUMP/go; $HOLD_END: the encoder, its file written, ends only at $DUMP/go, `$DUMP/held` said first; $END_STATUS: the encoder ends with that status, whatever the real one's; $CHATTER: the decoder says that line on stderr again and again while it decodes; $DECODE_BYTES: the decoder fails after that many bytes; $STALL: the decoder gives that many bytes, then nothing, its pipe open, until $DUMP/go, `$DUMP/held` said first; each frame grab's arguments a line in $DUMP/grabs; $GRAB: a frame grab ignores Ctrl-C and waits for $DUMP/go, `$DUMP/held` said first.
const FFMPEG: &str = r#"#!/bin/sh
held() { HELD; }
case " $* " in
*" rawvideo "*)
    printf '%s\n' "$@" > "$DUMP/encode.args"
    if [ -n "$FAIL_AFTER" ]; then head -c "$FAIL_AFTER" > /dev/null; exit 1; fi
    if [ -n "$HOLD" ]; then
        { dd bs="$FRAME" count="$HOLD" iflag=fullblock status=none; held; cat; } | tee "$DUMP/encode.raw" | "$REAL" "$@"
    else
        tee "$DUMP/encode.raw" | "$REAL" "$@"
    fi
    status=$?
    if [ -n "$HOLD_END" ]; then touch "$DUMP/held"; held; fi
    exit "${END_STATUS:-$status}" ;;
*" -an "*)
    printf '%s\n' "$@" > "$DUMP/decode.args"
    if [ -n "$CHATTER" ]; then
        while :; do echo "$CHATTER"; done >&2 &
        "$REAL" "$@" | tee "$DUMP/decode.ppm"; status=$?; kill $!; exit $status
    fi
    if [ -n "$DECODE_BYTES" ]; then "$REAL" "$@" | head -c "$DECODE_BYTES" | tee "$DUMP/decode.ppm"; exit 1; fi
    if [ -n "$STALL" ]; then "$REAL" "$@" 2> /dev/null | head -c "$STALL" | tee "$DUMP/decode.ppm"; touch "$DUMP/held"; held; exit 0; fi
    "$REAL" "$@" | tee "$DUMP/decode.ppm" ;;
*" -frames:v "*)
    printf '%s\n' "$*" >> "$DUMP/grabs"
    if [ -n "$GRAB" ]; then trap '' INT; touch "$DUMP/held"; held; fi
    exec "$REAL" "$@" ;;
*) exec "$REAL" "$@" ;;
esac
"#;

/// ffprobe first on PATH: the converter's probe of a video answered with $PROBED (JSON) when it is set, as ffprobe answers for what it does not know (keys left out); all else asked of REAL, the real one.
const FFPROBE: &str = r#"#!/bin/sh
case " $* " in
*" stream=avg_frame_rate,nb_frames,duration:format=duration "*)
    if [ -n "$PROBED" ]; then printf '%s\n' "$PROBED"; exit 0; fi ;;
esac
exec REAL "$@"
"#;

/// A program the converter starts, first on PATH: notes its arguments and the signals it started with blocked and ignored, a line in $DUMP/masks (`NAME <args>: SigBlk:<hex> SigIgn:<hex>`), then runs REAL, the real one.
const MASKS: &str = r#"#!/bin/sh
masks=$(grep -E '^Sig(Blk|Ign)' /proc/self/status | tr -d '\t' | paste -sd ' ')
echo "NAME $*: $masks" >> "$DUMP/masks"
exec REAL "$@"
"#;

fn ran(output: Output) -> Ran {
    Ran {
        code: output.status.code(),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    }
}

/// `asciier convert ARGS` in the sandbox's folder, no terminal: how it ended and what it printed.
fn convert(sandbox: &Sandbox, args: &[&str]) -> Ran {
    ran(sandbox
        .command(BIN)
        .current_dir(&sandbox.dir)
        .arg("convert")
        .args(args)
        .output()
        .unwrap())
}

/// `asciier convert ARGS`, which must work: the file it printed.
fn converted(sandbox: &Sandbox, args: &[&str]) -> PathBuf {
    let ran = convert(sandbox, args);
    assert_eq!(ran.code, Some(0), "{args:?}: {ran:?}");
    sandbox.path(ran.stdout.strip_suffix('\n').unwrap())
}

/// `asciier convert` runs side by side, each as `convert` gives it.
fn converts<S: AsRef<OsStr>>(sandbox: &Sandbox, runs: &[Vec<S>]) -> Vec<Ran> {
    let children: Vec<Child> = runs
        .iter()
        .map(|args| {
            sandbox
                .command(BIN)
                .current_dir(&sandbox.dir)
                .arg("convert")
                .args(args)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap()
        })
        .collect();
    children
        .into_iter()
        .map(|child| ran(child.wait_with_output().unwrap()))
        .collect()
}

/// What a PNG's header says: width, height, bit depth, colour type (2: RGB), interlace.
fn png_info(path: &Path) -> (u32, u32, u8, u8, u8) {
    let bytes = std::fs::read(path).unwrap();
    assert!(
        bytes.starts_with(b"\x89PNG\r\n\x1a\n") && &bytes[12..16] == b"IHDR",
        "{path:?} is no PNG"
    );
    let number = |at: usize| u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap());
    (number(16), number(20), bytes[24], bytes[25], bytes[28])
}

fn pixels(path: &Path) -> RgbImage {
    image::open(path)
        .unwrap_or_else(|e| panic!("{path:?}: {e}"))
        .to_rgb8()
}

/// CRC-32 of a picture's RGB bytes: the old converter's pixels pinned in a number.
fn crc(picture: &RgbImage) -> u32 {
    let mut crc = flate2::Crc::new();
    crc.update(picture.as_raw());
    crc.sum()
}

/// Every colour a glyph tile can hold: each ink over `ground` through masks of 0 to 255, as Pillow's fill_mask_L blends (libImaging/Paste.c), with which the old converter drew its tiles.
fn blends(ground: [u8; 3], inks: &[[u8; 3]]) -> HashSet<[u8; 3]> {
    let mut all = HashSet::new();
    for ink in inks {
        for mask in 0..=255u32 {
            all.insert([0, 1, 2].map(|c| {
                let t = ground[c] as u32 * (255 - mask) + ink[c] as u32 * mask + 128;
                (((t >> 8) + t) >> 8) as u8
            }));
        }
    }
    all
}

/// The columns of a picture that hold anything but `ground`.
fn inked_columns(picture: &RgbImage, ground: [u8; 3]) -> Vec<u32> {
    (0..picture.width())
        .filter(|&x| (0..picture.height()).any(|y| picture.get_pixel(x, y).0 != ground))
        .collect()
}

/// ffprobe's JSON of `file` for `args`.
fn probed(sandbox: &Sandbox, file: &Path, args: &[&str]) -> serde_json::Value {
    let output = sandbox
        .command("ffprobe")
        .args(["-v", "error", "-of", "json"])
        .args(args)
        .arg(file)
        .output()
        .unwrap();
    assert!(output.status.success(), "ffprobe {file:?}");
    serde_json::from_slice(&output.stdout).unwrap()
}

/// The streams of a media file: `video <codec> <w>x<h>`, `audio <codec>`, `subtitle <codec>`.
fn streams(sandbox: &Sandbox, file: &Path) -> Vec<String> {
    let info = probed(
        sandbox,
        file,
        &["-show_entries", "stream=codec_type,codec_name,width,height"],
    );
    let text = |value: &serde_json::Value| value.as_str().unwrap_or("?").to_string();
    info["streams"]
        .as_array()
        .unwrap()
        .iter()
        .map(|stream| match text(&stream["codec_type"]).as_str() {
            "video" => format!(
                "video {} {}x{}",
                text(&stream["codec_name"]),
                stream["width"],
                stream["height"]
            ),
            kind => format!("{kind} {}", text(&stream["codec_name"])),
        })
        .collect()
}

/// A stream's entry (`v:0`, `a:0`): `duration`, `avg_frame_rate`.
fn entry(sandbox: &Sandbox, file: &Path, stream: &str, key: &str) -> String {
    let info = probed(
        sandbox,
        file,
        &[
            "-select_streams",
            stream,
            "-show_entries",
            &format!("stream={key}"),
        ],
    );
    info["streams"][0][key]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

fn length(sandbox: &Sandbox, file: &Path, stream: &str) -> f64 {
    entry(sandbox, file, stream, "duration").parse().unwrap()
}

/// The frames a file's video holds, counted by decoding it.
fn frames_in(sandbox: &Sandbox, file: &Path) -> usize {
    let info = probed(
        sandbox,
        file,
        &[
            "-count_frames",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=nb_read_frames",
        ],
    );
    info["streams"][0]["nb_read_frames"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap()
}

/// The frames of a PPM stream as ffmpeg writes it: each one's width, height and RGB; a frame cut short ends it.
fn ppm_frames(bytes: &[u8]) -> Vec<(u32, u32, Vec<u8>)> {
    let mut frames = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        let mut lines = Vec::new();
        for _ in 0..3 {
            let Some(end) = find(bytes, b"\n", at) else {
                return frames;
            };
            lines.push(String::from_utf8_lossy(&bytes[at..end]).into_owned());
            at = end + 1;
        }
        assert_eq!((lines[0].as_str(), lines[2].as_str()), ("P6", "255"));
        let (w, h) = lines[1].split_once(' ').unwrap();
        let (w, h): (u32, u32) = (w.parse().unwrap(), h.parse().unwrap());
        let size = w as usize * h as usize * 3;
        let Some(rgb) = bytes.get(at..at + size) else {
            return frames;
        };
        frames.push((w, h, rgb.to_vec()));
        at += size;
    }
    frames
}

/// The first frame of a video, as ffmpeg decodes it.
fn first_frame(sandbox: &Sandbox, file: &Path) -> RgbImage {
    let output = sandbox
        .command("ffmpeg")
        .args(["-v", "error", "-nostdin", "-i"])
        .arg(file)
        .args(["-frames:v", "1", "-f", "image2pipe", "-c:v", "ppm", "-"])
        .output()
        .unwrap();
    let (w, h, rgb) = ppm_frames(&output.stdout).remove(0);
    RgbImage::from_raw(w, h, rgb).unwrap()
}

/// Where the run environment finds `program` before any wrapper goes in its place: the real one a wrapper calls by its path.
fn real(sandbox: &Sandbox, program: &str) -> String {
    let output = sandbox
        .command("/bin/sh")
        .args(["-c", r#"command -v "$0""#, program])
        .output()
        .unwrap();
    let path = String::from_utf8(output.stdout).unwrap().trim().to_string();
    assert!(
        path.starts_with('/') && !Path::new(&path).starts_with(&sandbox.dir),
        "{program}: {path:?}"
    );
    path
}

/// The ffmpeg wrapper first on PATH, its dumps in the sandbox's folder; media made after it go through it.
fn wrap_ffmpeg(sandbox: &mut Sandbox) {
    let ffmpeg = real(sandbox, "ffmpeg");
    sandbox.program("ffmpeg", &FFMPEG.replace("HELD", HELD));
    sandbox.set("REAL", ffmpeg);
    let dump = sandbox.dir.clone();
    sandbox.set("DUMP", dump);
}

/// The ffprobe wrapper first on PATH.
fn wrap_ffprobe(sandbox: &Sandbox) {
    let ffprobe = real(sandbox, "ffprobe");
    sandbox.program("ffprobe", &FFPROBE.replace("REAL", &ffprobe));
}

/// The encoder's arguments in the ffmpeg wrapper's notes, a line each.
fn encoder_args(sandbox: &Sandbox) -> String {
    std::fs::read_to_string(sandbox.path("encode.args")).unwrap()
}

/// Wait until `file` exists; fails the test after `PATIENCE`.
fn await_file(file: &Path) {
    let end = Instant::now() + PATIENCE;
    while !file.exists() {
        assert!(Instant::now() < end, "no {file:?}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// The size of `file` once it holds `least` bytes and has stopped growing for 300 ms; fails the test after `PATIENCE`.
fn settled(file: &Path, least: usize) -> usize {
    let end = Instant::now() + PATIENCE;
    let (mut last, mut since) = (0, Instant::now());
    loop {
        let size = std::fs::metadata(file).map_or(0, |meta| meta.len() as usize);
        if size != last {
            (last, since) = (size, Instant::now());
        } else if size >= least && since.elapsed() >= Duration::from_millis(300) {
            return size;
        }
        assert!(Instant::now() < end, "{file:?}: {size} bytes");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Wait until no process works in the sandbox's folder: the converter, its ffmpeg and their wrappers all gone.
fn await_gone(sandbox: &Sandbox) {
    let (dir, end) = (
        sandbox.dir.canonicalize().unwrap(),
        Instant::now() + PATIENCE,
    );
    loop {
        let left: Vec<String> = std::fs::read_dir("/proc")
            .unwrap()
            .flatten()
            .filter(|entry| {
                std::fs::read_link(entry.path().join("cwd")).is_ok_and(|cwd| cwd == dir)
            })
            .map(|entry| {
                std::fs::read_to_string(entry.path().join("cmdline"))
                    .unwrap_or_default()
                    .replace('\0', " ")
            })
            .collect();
        if left.is_empty() {
            return;
        }
        assert!(Instant::now() < end, "still running: {left:?}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Whether SIGINT waits in process `pid` for a thread to take it, sent to the process (ShdPnd) or to one of its threads (SigPnd); false once it ended.
fn sigint_pending(pid: u32) -> bool {
    let status = |file: PathBuf| std::fs::read_to_string(file).unwrap_or_default();
    let holds = |status: &str, key: &str| {
        status
            .lines()
            .filter_map(|line| line.strip_prefix(key))
            .filter_map(|set| u64::from_str_radix(set.trim(), 16).ok())
            .any(|set| set & 1 << (libc::SIGINT - 1) != 0)
    };
    let process = status(PathBuf::from(format!("/proc/{pid}/status")));
    if process.contains("\nState:\tZ") {
        return false;
    }
    holds(&process, "ShdPnd:")
        || std::fs::read_dir(format!("/proc/{pid}/task"))
            .into_iter()
            .flatten()
            .flatten()
            .any(|task| holds(&status(task.path().join("status")), "SigPnd:"))
}

/// A converter run as a terminal's job: a process group of its own, which Ctrl-C reaches whole; what it prints read as it comes.
struct Running {
    child: Child,
    out: Arc<Mutex<Vec<u8>>>,
    err: Arc<Mutex<Vec<u8>>>,
    readers: Vec<JoinHandle<()>>,
    /// The sandbox's folder, its device and inode: the converter works in it, and so does every process it starts.
    folder: (u64, u64),
    /// Whether the converter has been reaped, by `ended` or `running`: its pid, the id of the group it led, may then be another's.
    reaped: bool,
}

/// A process held by a pidfd: a signal through it reaches that process alone, also once its pid is free for another.
struct Pinned {
    pid: libc::pid_t,
    fd: OwnedFd,
}

impl Pinned {
    /// Process `pid`, held, when it runs and works in `folder` (device and inode): its folder read once it is held, so the folder is its own.
    fn working_in(pid: libc::pid_t, folder: (u64, u64)) -> Option<Pinned> {
        let here = || {
            std::fs::metadata(format!("/proc/{pid}/cwd"))
                .is_ok_and(|cwd| (cwd.dev(), cwd.ino()) == folder)
        };
        if !here() {
            return None;
        }
        // SAFETY: pidfd_open takes a pid and flags, no memory.
        let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
        if fd < 0 {
            return None;
        }
        // SAFETY: the descriptor just opened, owned by nothing else.
        let fd = unsafe { OwnedFd::from_raw_fd(fd as RawFd) };
        let pinned = Pinned { pid, fd };
        (here() && pinned.running()).then_some(pinned)
    }

    /// Whether it has not ended: its pidfd turns readable at the end.
    fn running(&self) -> bool {
        let mut poll = libc::pollfd {
            fd: self.fd.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: one pollfd, owned here, no waiting.
        unsafe { libc::poll(&mut poll, 1, 0) != 1 }
    }

    fn signal(&self, signal: libc::c_int) {
        // SAFETY: a signal through the pidfd this holds, no siginfo.
        unsafe {
            libc::syscall(
                libc::SYS_pidfd_send_signal,
                self.fd.as_raw_fd(),
                signal,
                std::ptr::null::<libc::siginfo_t>(),
                0,
            )
        };
    }
}

impl Running {
    /// Started as a terminal starts a job, whatever the test runner inherited: SIGINT at its default, no signal blocked.
    fn start(sandbox: &Sandbox, args: &[&str]) -> Running {
        Running::launch(sandbox, args, libc::SIG_DFL)
    }

    /// Started as a shell without job control starts a background job: SIGINT ignored.
    fn ignoring_ctrl_c(sandbox: &Sandbox, args: &[&str]) -> Running {
        Running::launch(sandbox, args, libc::SIG_IGN)
    }

    fn launch(sandbox: &Sandbox, args: &[&str], sigint: libc::sighandler_t) -> Running {
        let mut command = sandbox.command(BIN);
        command
            .current_dir(&sandbox.dir)
            .arg("convert")
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0);
        // SAFETY: signal and sigprocmask alone, between fork and exec.
        unsafe {
            command.pre_exec(move || {
                libc::signal(libc::SIGINT, sigint);
                let mut none = std::mem::zeroed();
                libc::sigemptyset(&mut none);
                libc::sigprocmask(libc::SIG_SETMASK, &none, std::ptr::null_mut());
                Ok(())
            })
        };
        let mut child = command.spawn().unwrap();
        let (out, err) = (Arc::default(), Arc::default());
        let collect = |mut pipe: Box<dyn Read + Send>, into: &Arc<Mutex<Vec<u8>>>| {
            let into = into.clone();
            std::thread::spawn(move || {
                let mut buf = [0u8; 4096];
                while let Ok(n @ 1..) = pipe.read(&mut buf) {
                    into.lock().unwrap().extend_from_slice(&buf[..n]);
                }
            })
        };
        let readers = vec![
            collect(Box::new(child.stdout.take().unwrap()), &out),
            collect(Box::new(child.stderr.take().unwrap()), &err),
        ];
        let folder = std::fs::metadata(&sandbox.dir).unwrap();
        Running {
            child,
            out,
            err,
            readers,
            folder: (folder.dev(), folder.ino()),
            reaped: false,
        }
    }

    fn err(&self) -> String {
        String::from_utf8_lossy(&self.err.lock().unwrap()).into_owned()
    }

    /// Ctrl-C as a terminal sends it: SIGINT to the process group; back once the converter has taken it, or has ended. Refused once the converter is reaped.
    fn interrupt(&self) {
        assert!(!self.reaped, "Ctrl-C after the converter was reaped");
        let pid = self.child.id();
        // SAFETY: a signal to the group the child leads: not reaped (above), the child holds its pid, so the group is its own.
        unsafe { libc::kill(-(pid as libc::pid_t), libc::SIGINT) };
        let end = Instant::now() + PATIENCE;
        while sigint_pending(pid) {
            assert!(Instant::now() < end, "SIGINT never taken: {:?}", self.err());
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// Whether the converter runs on; one that has ended is reaped, as by `ended`.
    fn running(&mut self) -> bool {
        let ended = self.child.try_wait().unwrap().is_some();
        self.reaped |= ended;
        !ended
    }

    /// Its end within `PATIENCE`: how it ended.
    fn ended(&mut self) -> ExitStatus {
        let end = Instant::now() + PATIENCE;
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                self.reaped = true;
                return status;
            }
            assert!(Instant::now() < end, "still running: {:?}", self.err());
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Its end within `PATIENCE`, and its output to the end (children holding its pipes gone too): how it ended, stdout, stderr.
    fn finish(mut self) -> (ExitStatus, String, String) {
        let end = Instant::now() + PATIENCE;
        let status = self.ended();
        while !self.readers.iter().all(JoinHandle::is_finished) {
            assert!(
                Instant::now() < end,
                "its output still open: {:?}",
                self.err()
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        let out = String::from_utf8_lossy(&self.out.lock().unwrap()).into_owned();
        (status, out, self.err())
    }
}

impl Drop for Running {
    /// After a test that failed part-way, or a run dropped while it ran: every process that works in the sandbox's folder (the converter, the ffmpeg it started in sessions of their own, the wrappers, the old converter's pool) ended by SIGTERM, and by SIGKILL what is left of them after 5 s; each held by a pidfd once its folder is seen, so no signal reaches a process that took its pid. SIGTERM first lets the old tools' converter end its pool: its multiprocessing resource tracker ignores SIGTERM and unlinks the pool's semaphores in /dev/shm once the rest is gone, where SIGKILL left them behind. A run that ended while its test goes on is left as it is: what it left running is the test's to see (await_gone).
    fn drop(&mut self) {
        if !std::thread::panicking() && !matches!(self.child.try_wait(), Ok(None)) {
            return;
        }
        let end = Instant::now() + Duration::from_secs(5);
        let mut held: Vec<Pinned> = Vec::new();
        loop {
            let _ = self.child.try_wait(); // the converter reaped once it ends
            let pids = std::fs::read_dir("/proc").into_iter().flatten().flatten();
            for pid in pids.filter_map(|entry| entry.file_name().to_str()?.parse().ok()) {
                if held.iter().any(|seen| seen.pid == pid && seen.running()) {
                    continue;
                }
                if let Some(found) = Pinned::working_in(pid, self.folder) {
                    found.signal(libc::SIGTERM);
                    held.push(found);
                }
            }
            held.retain(Pinned::running);
            if held.is_empty() {
                break;
            }
            if Instant::now() >= end {
                held.iter().for_each(|left| left.signal(libc::SIGKILL));
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let _ = self.child.wait();
    }
}

/// The converter's test still: a hue sweep, skin tones, black and white bars, dim colours, a quarter each: every look option changes how it is drawn.
fn sampler() -> RgbImage {
    RgbImage::from_fn(320, 180, |x, y| {
        let (u, v) = (x % 80, y);
        let px = match x / 80 {
            0 => [
                (255 * u / 79) as u8,
                (255 * v / 179) as u8,
                (255 - 255 * u / 79) as u8,
            ],
            1 => [
                (150 + v / 2) as u8,
                (100 + v / 3 + u / 4) as u8,
                (70 + v / 4) as u8,
            ],
            2 if (u / 4 + v / 6) % 2 == 0 => [240, 240, 240],
            2 => [10, 10, 10],
            _ => [(40 + u) as u8, (30 + v / 4) as u8, (60 + u / 2) as u8],
        };
        image::Rgb(px)
    })
}

fn flat(w: u32, h: u32, colour: [u8; 3]) -> RgbImage {
    RgbImage::from_pixel(w, h, image::Rgb(colour))
}

#[test]
fn help_lists_the_options_in_two_groups() {
    // rows C1a, N9: the parser of bin/ascii-convert:82 at tag python-final: on stdout the usage, `Render an image or a video as ascii-viewer draws it.` (N1: asciier), the groups `look:` and `output:` with their options' help as it wrote them; N9: a layout of its own, an option a line; exit 0
    let sandbox = Sandbox::new();
    let groups: [(&str, &[(&str, &str)]); 2] = [
        (
            "look:",
            &[
                (
                    "-l, --level N",
                    "-95..95: up raises the black point (dark areas go blank), down lowers the white point",
                ),
                ("--contrast N", "-95..95, as mpv's contrast"),
                (
                    "-s, --saturation PERCENT",
                    "how much colour a cell needs to take a hue: lower, more grey (default 100)",
                ),
                (
                    "--skin grey|theme",
                    "skin tones grey (default) or in the theme's warm colours",
                ),
                (
                    "-g, --glyphs mixed|density|shape",
                    "mixed (shapes on edges, dithered density on flat areas; default), density, shape",
                ),
                ("-m, --mono", "the theme's brightest colour on its darkest"),
                ("-b, --bold", "the font's heaviest weight"),
                (
                    "-t, --theme NAME|FILE",
                    "a kitty theme by name or .conf file; colours it leaves out come from the current",
                ),
                (
                    "--crop X,Y,W,H",
                    "corner and size in percent of the picture; past its borders is background (a negative X: --crop=-10,0,120,100)",
                ),
            ],
        ),
        (
            "output:",
            &[
                (
                    "-o, --output PATH",
                    "the file to write (default: beside INPUT)",
                ),
                ("-c, --columns N", "text width in columns"),
                ("-W, --width PX", "output width"),
                ("-H, --height PX", "output height"),
                (
                    "--fit contain|cover",
                    "with --columns and a size: the whole grid inside (default) or the canvas filled",
                ),
                ("-a, --at SECONDS", "a video's frame at that time, as a PNG"),
                ("-h, --help", "this text"),
            ],
        ),
    ];
    for flag in ["-h", "--help"] {
        let ran = convert(&sandbox, &[flag]);
        assert_eq!((ran.code, ran.stderr.as_str()), (Some(0), ""), "{ran:?}");
        assert!(
            ran.stdout.starts_with(
                "usage: asciier convert [options] INPUT\n\nRender an image or a video as asciier draws it.\n\n"
            ),
            "{}",
            ran.stdout
        );
        let lines: Vec<&str> = ran.stdout.lines().collect();
        for (heading, options) in groups {
            let at = lines.iter().position(|line| *line == heading).unwrap();
            for (line, (option, help)) in lines[at + 1..].iter().zip(options) {
                assert!(
                    line.trim_start().starts_with(option) && line.ends_with(help),
                    "{line:?}"
                );
            }
        }
    }
}

#[test]
fn bad_values_are_usage_errors() {
    // rows C1, C2, N9: ranged and crop_spec of bin/ascii-convert:58 and :71 at tag python-final, the reasons as argparse gave them run here: exit 2, nothing on stdout, on stderr the usage and `asciier convert: error: <reason>` (argparse's preamble: A); level, contrast, saturation, columns, width and height take integers only; argparse's words for its own errors (A) checked by their start and what they name; N9: no abbreviated options
    let sandbox = Sandbox::new();
    let reasons: &[(&[&str], &str)] = &[
        (
            &["-l", "96", "in.png"],
            "argument -l/--level: 96 is outside -95..95",
        ),
        (
            &["--level=-96", "in.png"],
            "argument -l/--level: -96 is outside -95..95",
        ),
        (
            &["--contrast", "96", "in.png"],
            "argument --contrast: 96 is outside -95..95",
        ),
        (
            &["-s", "-1", "in.png"],
            "argument -s/--saturation: -1 is outside 0..300",
        ),
        (
            &["-s", "301", "in.png"],
            "argument -s/--saturation: 301 is outside 0..300",
        ),
        (
            &["-l", "1.5", "in.png"],
            "argument -l/--level: not a number: '1.5'",
        ),
        (
            &["--contrast", "1.5", "in.png"],
            "argument --contrast: not a number: '1.5'",
        ),
        (
            &["-s", "50.5", "in.png"],
            "argument -s/--saturation: not a number: '50.5'",
        ),
        (
            &["-W", "16.5", "in.png"],
            "argument -W/--width: not a number: '16.5'",
        ),
        (
            &["-H", "1e3", "in.png"],
            "argument -H/--height: not a number: '1e3'",
        ),
        (
            &["-c", "40.0", "in.png"],
            "argument -c/--columns: not a number: '40.0'",
        ),
        (
            &["-c", "x", "in.png"],
            "argument -c/--columns: not a number: 'x'",
        ),
        (
            &["-c", "7", "in.png"],
            "argument -c/--columns: 7 is outside 8..2000",
        ),
        (
            &["-c", "2001", "in.png"],
            "argument -c/--columns: 2001 is outside 8..2000",
        ),
        (
            &["-W", "4", "in.png"],
            "argument -W/--width: 4 is outside 16..16384",
        ),
        (
            &["-H", "16385", "in.png"],
            "argument -H/--height: 16385 is outside 16..16384",
        ),
        (
            &["--skin", "red", "in.png"],
            "argument --skin: invalid choice: 'red' (choose from 'grey', 'theme')",
        ),
        (
            &["-g", "x", "in.png"],
            "argument -g/--glyphs: invalid choice: 'x' (choose from 'mixed', 'density', 'shape')",
        ),
        (
            &["--fit", "x", "in.png"],
            "argument --fit: invalid choice: 'x' (choose from 'contain', 'cover')",
        ),
        (
            &["--crop", "1,2,3", "in.png"],
            "argument --crop: not X,Y,W,H: '1,2,3'",
        ),
        (
            &["--crop", "1,2,3,4,5", "in.png"],
            "argument --crop: not X,Y,W,H: '1,2,3,4,5'",
        ),
        (
            &["--crop", "a,b,c,d", "in.png"],
            "argument --crop: not X,Y,W,H: 'a,b,c,d'",
        ),
        (
            &["--crop", "0,0,0,10", "in.png"],
            "argument --crop: W and H must be above 0: '0,0,0,10'",
        ),
        (
            &["--at", "-1", "in.png"],
            "argument -a/--at: -1 is outside 0..inf",
        ),
        (
            &["-a", "x", "in.png"],
            "argument -a/--at: not a number: 'x'",
        ),
        (
            &["-a", "nan", "in.png"],
            "argument -a/--at: nan is outside 0..inf",
        ),
        (
            &["in.png", "-l"],
            "argument -l/--level: expected one argument",
        ),
        (&["-o"], "argument -o/--output: expected one argument"),
    ];
    for (args, reason) in reasons {
        let ran = convert(&sandbox, args);
        assert_eq!(
            (ran.code, ran.stdout.as_str(), ran.stderr),
            (
                Some(2),
                "",
                format!(
                    "usage: asciier convert [options] INPUT\nasciier convert: error: {reason}\n"
                )
            ),
            "{args:?}"
        );
    }
    let worded: [(&[&str], &str, &str); 4] = [
        (&[], "the following arguments are required: ", "INPUT"),
        (&["--nope", "in.png"], "unrecognized arguments: ", "--nope"),
        (&["a.png", "b.png"], "unrecognized arguments: ", "b.png"),
        (
            &["--col", "100", "in.png"],
            "unrecognized arguments: ",
            "--col",
        ),
    ];
    for (args, start, named) in worded {
        let ran = convert(&sandbox, args);
        let said = ran
            .stderr
            .strip_prefix("usage: asciier convert [options] INPUT\nasciier convert: error: ")
            .unwrap_or_else(|| panic!("{args:?}: {ran:?}"));
        assert_eq!((ran.code, ran.stdout.as_str()), (Some(2), ""), "{args:?}");
        assert!(
            said.starts_with(start)
                && said.to_lowercase().contains(&named.to_lowercase())
                && said.ends_with('\n'),
            "{args:?}: {said:?}"
        );
    }
}

#[test]
fn look_options_draw_as_the_old_converter_drew_them() {
    // rows C1, G1, G6, G8, G9: each look option in its spellings: `--opt value`, `--opt=value`, a short flag's value attached, a negative value without `=`, short flags combined; the pixels (CRC-32 of the RGB) as bin/ascii-convert at tag python-final drew the sampler at 40 columns in this harness, each option its own: G1 libchafa set as the old converter set it for what it draws (ASCII and space, foreground only, truecolour, mono's two colours, work factor 0.5, its geometry: 40 x 11 cells), `-g shape` its glyphs alone; G6 the installed face's ramp and its dither on flat cells (`-g density`, mixed); G8 the ramp and the glyphs of the bold face (`-b`, `-mb`); G9 the level and contrast table (`-l 30`, `--level -30`, `--contrast` 50 and -50)
    let sandbox = Sandbox::new();
    save(&sampler(), &sandbox.path("sampler.png"));
    let drawn: [(&[&str], u32); 22] = [
        (&["-c", "40"], 0xfc604bef),
        (&["--columns", "40"], 0xfc604bef),
        (&["--columns=40"], 0xfc604bef),
        (&["-c40"], 0xfc604bef),
        (&["-c", "40", "-l", "30"], 0x14d4738d),
        (&["-c", "40", "--level=30"], 0x14d4738d),
        (&["-c", "40", "-l30"], 0x14d4738d),
        (&["-c", "40", "--level", "-30"], 0xc2b14356),
        (&["-c", "40", "--contrast", "50"], 0x19a5197e),
        (&["-c", "40", "--contrast=-50"], 0x7f654f13),
        (&["-c", "40", "-s", "0"], 0x3010e1fe),
        (&["-c", "40", "--saturation=0"], 0x3010e1fe),
        (&["-c", "40", "-s", "300"], 0x6a1beeca),
        (&["-c", "40", "--skin", "theme"], 0xb1d82526),
        (&["-c", "40", "-g", "density"], 0x43896a9e),
        (&["-c", "40", "--glyphs=density"], 0x43896a9e),
        (&["-c", "40", "-g", "shape"], 0x49c4b830),
        (&["-c", "40", "-m"], 0xa73910eb),
        (&["-c", "40", "-b"], 0x4b1c1f78),
        (&["-c", "40", "-mb"], 0x01ff6bdb),
        (&["-c", "40", "-bm"], 0x01ff6bdb),
        (&["-c", "40", "--mono", "--bold"], 0x01ff6bdb),
    ];
    let runs: Vec<Vec<String>> = drawn
        .iter()
        .enumerate()
        .map(|(i, (args, _))| {
            let mut run: Vec<String> = args.iter().map(|arg| arg.to_string()).collect();
            run.extend(["-o".into(), format!("out{i}.png"), "sampler.png".into()]);
            run
        })
        .collect();
    for ((i, (args, expected)), ran) in drawn.iter().enumerate().zip(converts(&sandbox, &runs)) {
        assert_eq!(ran.code, Some(0), "{args:?}: {ran:?}");
        let picture = pixels(&sandbox.path(format!("out{i}.png")));
        assert_eq!(picture.dimensions(), (480, 297), "{args:?}");
        assert_eq!(crc(&picture), *expected, "{args:?}");
    }
}

#[test]
fn the_output_goes_beside_the_input_and_its_path_is_printed() {
    // rows C3, C4, N12: out_name and main of bin/ascii-convert:297 and :523 at tag python-final (tests/test_convert.py:138, :147 and :165): `<stem>-ascii.png` beside the input, its path printed as given or derived, never made absolute, exit 0; `--output` instead; a file in the way replaced; a missing folder is not made: exit 1 (the old converter's traceback; N12: a message)
    let sandbox = Sandbox::new();
    sandbox.folder("sub");
    for name in ["pic.png", "other.png", "sub/a.b.png"] {
        save(&gradient(80, 40), &sandbox.path(name));
    }
    std::fs::write(sandbox.path("pic-ascii.png"), "in the way").unwrap();
    let absolute = sandbox.path("sub/a.b.png");
    let absolute_out = sandbox.path("sub/a.b-ascii.png");
    let runs: [(&[&str], &str); 6] = [
        (&["pic.png"], "pic-ascii.png"),
        (&["sub/a.b.png"], "sub/a.b-ascii.png"),
        (&["sub/../pic.png"], "sub/../pic-ascii.png"),
        (
            &[absolute.to_str().unwrap()],
            absolute_out.to_str().unwrap(),
        ),
        (&["-o", "named.png", "other.png"], "named.png"),
        (&["--output=sub/kept.PNG", "other.png"], "sub/kept.PNG"),
    ];
    for (args, printed) in runs {
        let ran = convert(&sandbox, &[&["-c", "20"][..], args].concat());
        assert_eq!(
            (ran.code, ran.stdout, ran.stderr),
            (Some(0), format!("{printed}\n"), String::new()),
            "{args:?}"
        );
        assert_eq!(png_info(&sandbox.path(printed)).0, 240, "{printed}");
    }
    assert!(!sandbox.path("other-ascii.png").exists());
    let ran = convert(&sandbox, &["-c", "20", "-o", "nowhere/out.png", "pic.png"]);
    assert_eq!(
        (ran.code, ran.stdout.as_str(), ran.stderr.as_str()),
        (
            Some(1),
            "",
            "asciier: cannot write nowhere/out.png: No such file or directory (os error 2)\n"
        )
    );
    assert!(!sandbox.path("nowhere").exists());
}

#[test]
fn at_on_an_image_is_a_usage_error() {
    // row C5: bin/ascii-convert:530 at tag python-final: exit 2, `--at takes a video's frame; this is an image` after the usage, for an SVG too, and nothing written
    let sandbox = Sandbox::new();
    save(&gradient(80, 40), &sandbox.path("pic.png"));
    std::fs::write(
        sandbox.path("icon.svg"),
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24"><circle cx="12" cy="12" r="10"/></svg>"#,
    )
    .unwrap();
    for input in ["pic.png", "icon.svg"] {
        let ran = convert(&sandbox, &["--at", "1", input]);
        assert_eq!(
            (ran.code, ran.stdout.as_str(), ran.stderr.as_str()),
            (
                Some(2),
                "",
                "usage: asciier convert [options] INPUT\nasciier convert: error: --at takes a video's frame; this is an image\n"
            ),
            "{input}"
        );
    }
    assert!(
        !sandbox.path("pic-ascii-1s.png").exists() && !sandbox.path("icon-ascii-1s.png").exists()
    );
}

/// `asciier convert ARGS` on a terminal of its own, which must work: what `kitten` noted meanwhile, and what the terminal showed.
fn on_a_terminal(sandbox: &Sandbox, args: &[&str]) -> (String, Vec<u8>) {
    let _ = std::fs::remove_file(sandbox.path("bin/kitten.calls"));
    let mut launch = sandbox.launch().arg("convert");
    for arg in args {
        launch = launch.arg(arg);
    }
    let mut tty = launch.start();
    assert_eq!(tty.exit(), 0, "{args:?}: {}", shown(&tty.out()));
    (sandbox.noted("kitten.calls"), tty.out())
}

#[test]
fn at_on_a_kitty_terminal_shows_the_frame_there_too() {
    // row C6: bin/ascii-convert:557 at tag python-final: after an `--at` frame, `kitten icat <out>` when stdout is a terminal and the environment says kitty (KITTY_WINDOW_ID, or TERM=xterm-kitty: kitty_terminal of lib/ascii_term.py:51), its status ignored; not without kitty, nor with stdout no terminal, nor for an image
    let mut sandbox = Sandbox::new();
    clip(&sandbox, "clip.mp4", 0.2, "160x90", 25);
    save(&gradient(80, 40), &sandbox.path("pic.png"));
    let at = ["--at", "0", "-W", "160", "clip.mp4"];
    let icat = "icat\nclip-ascii-0s.png\n--\n";
    let (noted, out) = on_a_terminal(&sandbox, &at);
    assert_eq!(noted, "");
    assert!(
        find(&out, b"clip-ascii-0s.png\r\n", 0).is_some(),
        "{}",
        shown(&out)
    );
    sandbox.set("KITTY_WINDOW_ID", "1");
    assert_eq!(on_a_terminal(&sandbox, &at).0, icat);
    assert_eq!(on_a_terminal(&sandbox, &["-c", "20", "pic.png"]).0, "");
    let _ = std::fs::remove_file(sandbox.path("bin/kitten.calls"));
    assert_eq!(convert(&sandbox, &at).code, Some(0));
    assert_eq!(sandbox.noted("kitten.calls"), "");
    sandbox.program(
        "kitten",
        "#!/bin/sh\n{ printf '%s\\n' \"$@\"; echo --; } >> \"$0.calls\"\nexit 3\n",
    );
    assert_eq!(on_a_terminal(&sandbox, &at).0, icat);
    sandbox.unset("KITTY_WINDOW_ID");
    sandbox.set("TERM", "xterm-kitty");
    assert_eq!(on_a_terminal(&sandbox, &at).0, icat);
}

/// The colour of the top left pixel of a conversion of `pic.png`, a grey ramp whose dark left end draws no glyph: the background.
fn ground_of(sandbox: &Sandbox, args: &[&str]) -> [u8; 3] {
    let out = converted(sandbox, &[&["-c", "20"][..], args, &["pic.png"]].concat());
    pixels(&out).get_pixel(0, 0).0
}

#[test]
fn the_theme_is_the_current_one_with_theme_over_it() {
    // rows C7, G11: theme_colours and theme_text of bin/ascii-convert:115 and :130 at tag python-final, the colours as it drew them in this harness: the current theme, read again in every run, from ~/.config/kitty/current-theme.conf whatever XDG_CONFIG_HOME says, a later line winning; `--theme` overrides the colours it defines: an existing file's text, else the cache entry of exactly that name (kitten not asked), else `kitten themes --dump-theme NAME`; unknown: exit 1, `no kitty theme '<name>': <kitten's stderr or "no colours in it">`
    let mut sandbox = Sandbox::new();
    save(&gradient(200, 100), &sandbox.path("pic.png"));
    let theme = sandbox.path("home/.config/kitty/current-theme.conf");
    let bl1nk = std::fs::read_to_string(&theme).unwrap();
    assert_eq!(ground_of(&sandbox, &[]), GROUND);
    std::fs::write(&theme, format!("{bl1nk}\nbackground #222222\n")).unwrap();
    assert_eq!(ground_of(&sandbox, &[]), [0x22; 3]);
    std::fs::write(&theme, &bl1nk).unwrap();
    let elsewhere = sandbox.folder("elsewhere/kitty");
    std::fs::write(
        elsewhere.join("current-theme.conf"),
        bl1nk.replace("#111111", "#333333"),
    )
    .unwrap();
    let config = sandbox.path("elsewhere");
    sandbox.set("XDG_CONFIG_HOME", config);
    assert_eq!(ground_of(&sandbox, &[]), GROUND);
    // a file: its background over bl1nk's palette
    std::fs::write(sandbox.path("mine.conf"), "background #203040\n").unwrap();
    let out = converted(&sandbox, &["-c", "20", "-t", "mine.conf", "pic.png"]);
    let picture = pixels(&out);
    assert_eq!(
        (picture.get_pixel(0, 0).0, crc(&picture)),
        ([0x20, 0x30, 0x40], 0xc9571021)
    );
    // kitty's cache: exact names
    sandbox.folder("home/.cache/kitty");
    std::fs::copy(
        Path::new(CRATE).join("tests/golden/theme-collection.zip"),
        sandbox.path("home/.cache/kitty/kitty-themes.zip"),
    )
    .unwrap();
    let out = converted(&sandbox, &["-c", "20", "-t", "Day", "pic.png"]);
    let picture = pixels(&out);
    assert_eq!(
        (picture.get_pixel(0, 0).0, crc(&picture)),
        ([0xfa; 3], 0x79291215)
    );
    let out = converted(&sandbox, &["-c", "20", "--theme=Night", "pic.png"]);
    let picture = pixels(&out);
    assert_eq!(
        (picture.get_pixel(0, 0).0, crc(&picture)),
        ([0x10, 0x10, 0x14], 0xe3839ba7)
    );
    assert_eq!(sandbox.noted("kitten.calls"), "");
    // a file of that name comes first
    std::fs::write(sandbox.path("Day"), "background #605040\n").unwrap();
    assert_eq!(ground_of(&sandbox, &["-t", "Day"]), [0x60, 0x50, 0x40]);
    // kitten
    sandbox.stub_data("kitten.themes", "background #405060\n");
    let out = converted(&sandbox, &["-c", "20", "-t", "Known", "pic.png"]);
    let picture = pixels(&out);
    assert_eq!(
        (picture.get_pixel(0, 0).0, crc(&picture)),
        ([0x40, 0x50, 0x60], 0x7a62e1d9)
    );
    assert_eq!(
        sandbox.noted("kitten.calls"),
        "themes\n--dump-theme\nKnown\n--\n"
    );
    sandbox.stub_data("kitten.themes", "# no colours\n");
    let ran = convert(&sandbox, &["-c", "20", "-t", "Quiet", "pic.png"]);
    assert_eq!(
        (ran.code, ran.stderr.as_str()),
        (
            Some(1),
            "asciier: no kitty theme 'Quiet': no colours in it\n"
        )
    );
    std::fs::remove_file(sandbox.path("bin/kitten.themes")).unwrap();
    for name in ["nope", "day"] {
        let ran = convert(&sandbox, &["-c", "20", "-t", name, "pic.png"]);
        assert_eq!(
            (ran.code, ran.stdout, ran.stderr),
            (
                Some(1),
                String::new(),
                format!("asciier: no kitty theme '{name}': No theme named: {name}\n")
            )
        );
    }
}

#[test]
fn mono_draws_the_brightest_colour_on_the_darkest() {
    // rows C8, G5, L7: theme_colours of bin/ascii-convert:136 at tag python-final with extremes and brightest of lib/ascii_theme.py:161 and :171: every glyph in the theme's brightest colour (of those within 0.05 Oklab lightness of the lightest, the least colourful) on its darkest (least linear luminance), nothing blanked; bl1nk: color15 #F8F8F2 on the background #111111; a theme whose lightest is a vivid yellow: the near white #F0F0F0 under it, on color0 #000000, darker than its background; the pixels as the old converter drew them in this harness
    let sandbox = Sandbox::new();
    save(&sampler(), &sandbox.path("sampler.png"));
    std::fs::write(sandbox.path("vivid.conf"), VIVID).unwrap();
    for (args, ground, ink, expected) in [
        (&["-m"][..], GROUND, [0xf8, 0xf8, 0xf2], 0xa73910eb),
        (
            &["-m", "-t", "vivid.conf"][..],
            [0; 3],
            [0xf0; 3],
            0xc9390abb,
        ),
    ] {
        let out = converted(
            &sandbox,
            &[&["-c", "40"][..], args, &["sampler.png"]].concat(),
        );
        let picture = pixels(&out);
        let painted = blends(ground, &[ink]);
        assert!(
            picture.pixels().all(|px| painted.contains(&px.0)),
            "{args:?}: a pixel of another colour"
        );
        assert!(
            picture.pixels().any(|px| px.0 == ink) && picture.pixels().any(|px| px.0 == ground)
        );
        assert_eq!(crc(&picture), expected, "{args:?}");
    }
}

#[test]
fn cells_in_color0_and_spaces_stay_blank() {
    // rows G3, L7: Style.tile_rows of bin/ascii-convert:208 at tag python-final: each cell's glyph in its slot's colour on the theme's background, blank for spaces and for a slot in color0's colour; a picture in color0's own #1A1C1D draws nothing, with chafa's glyphs too (`-g shape`); one in color8's #505354 draws glyphs in it; every pixel of a drawing is a blend of the background with one of the 17 inks (Pillow's, as the old converter painted); the pixels as it drew them in this harness
    let sandbox = Sandbox::new();
    save(&flat(80, 40, INKS[0]), &sandbox.path("dark.png"));
    save(&flat(80, 40, INKS[8]), &sandbox.path("grey.png"));
    save(&sampler(), &sandbox.path("sampler.png"));
    let runs = [
        vec!["-c", "20", "-o", "dark-mixed.png", "dark.png"],
        vec![
            "-c",
            "20",
            "-g",
            "shape",
            "-o",
            "dark-shape.png",
            "dark.png",
        ],
        vec!["-c", "20", "grey.png"],
        vec!["-c", "40", "sampler.png"],
    ];
    for ran in converts(&sandbox, &runs) {
        assert_eq!(ran.code, Some(0), "{ran:?}");
    }
    for name in ["dark-mixed.png", "dark-shape.png"] {
        let picture = pixels(&sandbox.path(name));
        assert_eq!(picture.dimensions(), (240, 135));
        assert!(picture.pixels().all(|px| px.0 == GROUND), "{name}");
    }
    let grey = pixels(&sandbox.path("grey-ascii.png"));
    let painted = blends(GROUND, &[INKS[8]]);
    assert!(grey.pixels().all(|px| painted.contains(&px.0)));
    assert!(grey.pixels().any(|px| px.0 == INKS[8]));
    assert_eq!(crc(&grey), 0x541d815a);
    let drawing = pixels(&sandbox.path("sampler-ascii.png"));
    let painted = blends(GROUND, &INKS);
    assert!(drawing.pixels().all(|px| painted.contains(&px.0)));
    assert_eq!(crc(&drawing), 0xfc604bef);
}

#[test]
fn a_still_over_the_pixel_limit_is_refused() {
    // rows C9, N1: bin/ascii-convert:526 at tag python-final (tests/test_convert.py:156): exit 1, `<name> is too large to open (over 179 megapixels)` (twice Pillow's limit, lib/ascii_image.py:23), no picture and no video made of it; so too for a PNG of the largest sides a PNG's header holds and a QOI over 400 megapixels, whose decoders give up on such headers, as Pillow measures them (seen 2026-11-15 with the old converter, Pillow 13.0.0); N1: after `asciier: `, the old converter's `ascii-convert: `
    let sandbox = Sandbox::new();
    png_header(&sandbox.path("huge.png"), 20_000, 10_000);
    png_header(&sandbox.path("side.png"), 0x7FFF_FFFF, 0x7FFF_FFFF);
    std::fs::write(sandbox.path("over.qoi"), qoi_header(20025, 20025)).unwrap();
    for name in ["huge.png", "side.png", "over.qoi"] {
        let ran = convert(&sandbox, &[name]);
        let message = format!("asciier: {name} is too large to open (over 179 megapixels)\n");
        assert_eq!(
            (ran.code, ran.stdout.as_str(), ran.stderr.as_str()),
            (Some(1), "", message.as_str())
        );
        let stem = name.split_once('.').unwrap().0;
        let made = ["png", "mp4"].map(|kind| sandbox.path(format!("{stem}-ascii.{kind}")).exists());
        assert_eq!(made, [false, false], "{name}");
    }
}

/// A QOI's header of `w` x `h` px of RGBA, then the end marker: no pixels.
fn qoi_header(w: u32, h: u32) -> Vec<u8> {
    let sides = [w.to_be_bytes(), h.to_be_bytes()].concat();
    [&b"qoif"[..], &sides, &[4, 0], &[0, 0, 0, 0, 0, 0, 0, 1]].concat()
}

#[test]
fn ctrl_c_on_a_still_or_a_frame_ends_it_quietly() {
    // rows C10, N25: bin/ascii-convert:561 at tag python-final: Ctrl-C on a still, on an `--at` frame, or in a video run before its read loop: no message, a file in the way as it was, nothing left running, then death by SIGINT (N25; the old converter exited 130, as a shell shows both); each run held where it waits for fc-match or for the first frame by a child that ignores Ctrl-C, then SIGINT to its process group: the old converter killed the child it waited on (subprocess.run)
    let mut sandbox = Sandbox::new();
    save(&gradient(400, 300), &sandbox.path("pic.png"));
    clip(&sandbox, "clip.mp4", 0.4, "160x90", 25);
    let fc_match = real(&sandbox, "fc-match");
    wrap_ffmpeg(&mut sandbox);
    sandbox.set("GRAB", "1");
    sandbox.program(
        "fc-match",
        &format!("#!/bin/sh\ntrap '' INT\ntouch \"$DUMP/held\"\n{HELD}\nexec {fc_match} \"$@\"\n"),
    );
    std::fs::write(sandbox.path("pic-ascii.png"), "in the way").unwrap();
    // what each would make; the still's is in the way and checked last
    let runs: [(&[&str], Option<&str>); 3] = [
        (&["pic.png"], None),
        (&["--at", "0.2", "clip.mp4"], Some("clip-ascii-0.2s.png")),
        (&["clip.mp4"], Some("clip-ascii.mp4")),
    ];
    for (args, made) in runs {
        let _ = std::fs::remove_file(sandbox.path("held"));
        let run = Running::start(&sandbox, args);
        await_file(&sandbox.path("held"));
        run.interrupt();
        let (status, out, err) = run.finish();
        assert_eq!(status.signal(), Some(libc::SIGINT), "{args:?}: {status:?}");
        assert_eq!((out.as_str(), err.as_str()), ("", ""), "{args:?}");
        assert!(
            made.is_none_or(|made| !sandbox.path(made).exists()),
            "{args:?}"
        );
        await_gone(&sandbox);
    }
    assert_eq!(
        std::fs::read_to_string(sandbox.path("pic-ascii.png")).unwrap(),
        "in the way"
    );
}

#[test]
fn ctrl_c_while_the_encoder_finishes_ends_it_quietly_too() {
    // rows C10, N25: main of bin/ascii-convert:561 at tag python-final: Ctrl-C in a video run after its read loop, while it waits for the encoder (held through the wrapper once the file is written): no message, then death by SIGINT, also when the encoder then fails: the old converter's KeyboardInterrupt in encode.wait() (bin/ascii-convert:490) came before the encoder's status was read; run in this harness the old converter exited 130 with nothing printed, either way (3 of 3); the file the encoder wrote stays
    let mut sandbox = Sandbox::new();
    clip(&sandbox, "clip.mp4", 0.4, "160x90", 25);
    wrap_ffmpeg(&mut sandbox);
    sandbox.set("HOLD_END", "1");
    for (made, fails) in [("clip-out.mp4", false), ("fail-out.mp4", true)] {
        for file in ["held", "go"] {
            let _ = std::fs::remove_file(sandbox.path(file));
        }
        if fails {
            sandbox.set("END_STATUS", "1");
        }
        let run = Running::start(
            &sandbox,
            &["-W", "320", "-H", "180", "-o", made, "clip.mp4"],
        );
        await_file(&sandbox.path("held"));
        run.interrupt();
        std::fs::write(sandbox.path("go"), "").unwrap();
        let (status, out, err) = run.finish();
        assert_eq!(
            (status.signal(), out.as_str()),
            (Some(libc::SIGINT), ""),
            "{made}: {err:?}"
        );
        assert!(err.ends_with("\r10/10 frames\n"), "{made}: {err:?}");
        assert_eq!(frames_in(&sandbox, &sandbox.path(made)), 10);
        await_gone(&sandbox);
    }
}

#[test]
fn ctrl_c_while_a_picture_is_written_prints_no_path() {
    // rows C10, N25: main of bin/ascii-convert:561 at tag python-final: Ctrl-C ends a still at once, whatever its run is doing, here waiting in the write of its PNG: no message, no path (it goes with a done run), the file as far as it got (C10), then death by SIGINT (N25); `-o` names a FIFO whose pipe of one page is full, so the write waits, then SIGINT: the run ends with the pipe still full, whose page is then all that went through, a PNG cut short; the old converter, which could not open a FIFO (Pillow opens its output `w+b`), ended at once on a regular file: SIGINT during its save gave exit 130 and the PNG as far as it got (983253 of 1286502 bytes, seen here); a handler that only notes the Ctrl-C lets the run end once the FIFO is read, the whole PNG written
    let sandbox = Sandbox::new();
    save(&gradient(80, 40), &sandbox.path("pic.png"));
    let fifo = sandbox.path("out.png");
    let name = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
    // SAFETY: a C string that outlives the call.
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
    // the reading end opened first, so that the converter's opening goes through
    let mut png = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(&fifo)
        .unwrap();
    let fd = png.as_raw_fd();
    // SAFETY: fcntl on a descriptor this test owns.
    assert_eq!(unsafe { libc::fcntl(fd, libc::F_SETPIPE_SZ, 4096) }, 4096);
    let mut run = Running::start(&sandbox, &["-c", "40", "-o", "out.png", "pic.png"]);
    let end = Instant::now() + PATIENCE;
    loop {
        let mut held: libc::c_int = 0;
        // SAFETY: FIONREAD into an int.
        unsafe { libc::ioctl(fd, libc::FIONREAD, &mut held) };
        if held == 4096 {
            break;
        }
        assert!(run.running(), "ended, {held} bytes: {:?}", run.err());
        assert!(Instant::now() < end, "{held} bytes: {:?}", run.err());
        std::thread::sleep(Duration::from_millis(10));
    }
    run.interrupt();
    // the end comes with the pipe still full: nothing here reads it before
    let (status, out, err) = run.finish();
    assert_eq!(
        (status.signal(), out.as_str(), err.as_str()),
        (Some(libc::SIGINT), "", "")
    );
    // SAFETY: fcntl on a descriptor this test owns: reads wait again.
    unsafe { libc::fcntl(fd, libc::F_SETFL, 0) };
    let mut bytes = Vec::new();
    png.read_to_end(&mut bytes).unwrap();
    assert!(
        bytes.len() == 4096
            && bytes.starts_with(b"\x89PNG\r\n\x1a\n")
            && !bytes.ends_with(b"IEND\xae\x42\x60\x82"),
        "{} bytes",
        bytes.len()
    );
}

#[test]
fn ctrl_c_stays_ignored_when_the_parent_ignored_it() {
    // rows C10, X9, C4: a shell without job control starts a background job with SIGINT ignored (POSIX; `/bin/sh -c 'grep SigIgn /proc/self/status & wait'` gives 0x6 here), so the terminal's Ctrl-C is not that job's: Python keeps an ignored SIGINT ignored, and the old converter ran on to its end, exit 0 with the file (reviewer's probe); a still held where it waits for fc-match and a video held after 5 of its 10 frames (seen in the encoder's input: the old converter printed its count only as it read a frame, bin/ascii-convert:475), each given SIGINT then let go: exit 0, nothing on stderr but the progress, the path printed, the whole file made
    let mut sandbox = Sandbox::new();
    save(&gradient(80, 40), &sandbox.path("pic.png"));
    clip(&sandbox, "clip.mp4", 0.4, "160x90", 25);
    let fc_match = real(&sandbox, "fc-match");
    wrap_ffmpeg(&mut sandbox);
    sandbox.program(
        "fc-match",
        &format!(
            "#!/bin/sh\nif [ -n \"$HOLD_FONT\" ]; then touch \"$DUMP/held\"; {HELD}; fi\nexec {fc_match} \"$@\"\n"
        ),
    );
    sandbox.set("HOLD_FONT", "1");
    let run = Running::ignoring_ctrl_c(&sandbox, &["-c", "20", "pic.png"]);
    await_file(&sandbox.path("held"));
    run.interrupt();
    std::fs::write(sandbox.path("go"), "").unwrap();
    let (status, out, err) = run.finish();
    assert_eq!(
        (status.code(), out.as_str(), err.as_str()),
        (Some(0), "pic-ascii.png\n", "")
    );
    assert_eq!(png_info(&sandbox.path("pic-ascii.png")).0, 240);
    for file in ["held", "go"] {
        std::fs::remove_file(sandbox.path(file)).unwrap();
    }
    sandbox.unset("HOLD_FONT");
    sandbox.set("HOLD", "5");
    sandbox.set("FRAME", FRAME.to_string());
    let run = Running::ignoring_ctrl_c(
        &sandbox,
        &["-W", "320", "-H", "180", "-o", "clip-out.mp4", "clip.mp4"],
    );
    settled(&sandbox.path("encode.raw"), 5 * FRAME);
    run.interrupt();
    std::fs::write(sandbox.path("go"), "").unwrap();
    let (status, out, err) = run.finish();
    assert_eq!(
        (status.code(), out.as_str()),
        (Some(0), "clip-out.mp4\n"),
        "{err:?}"
    );
    assert_eq!(progress(&err).0.last(), Some(&10), "{err:?}");
    assert_eq!(frames_in(&sandbox, &sandbox.path("clip-out.mp4")), 10);
    await_gone(&sandbox);
}

#[test]
fn children_start_with_no_signal_blocked() {
    // rows C10, X9: every child of a video run, started either way (fc-match, ffprobe, the frame grab and the VAAPI trial where the render node is, as they are; the decoder and the encoder in sessions of their own): no signal blocked and SIGINT not ignored, as the converter itself started, so a Ctrl-C reaches what they run as usual; so before the converter caught Ctrl-C (reviewer's probe); seen while a converter blocked SIGINT for its Ctrl-C thread: SigBlk 0x2 in each
    let mut sandbox = Sandbox::new();
    clip(&sandbox, "clip.mp4", 0.2, "160x90", 25);
    for program in ["ffmpeg", "ffprobe", "fc-match"] {
        let path = real(&sandbox, program);
        sandbox.program(
            program,
            &MASKS.replace("NAME", program).replace("REAL", &path),
        );
    }
    let dump = sandbox.dir.clone();
    sandbox.set("DUMP", dump);
    let run = Running::start(&sandbox, &["-W", "320", "-o", "clip-out.mp4", "clip.mp4"]);
    let (status, out, err) = run.finish();
    assert_eq!(
        (status.code(), out.as_str()),
        (Some(0), "clip-out.mp4\n"),
        "{err:?}"
    );
    let noted = std::fs::read_to_string(sandbox.path("masks")).unwrap();
    for kind in [
        "fc-match ",
        "ffprobe ",
        " -frames:v ",
        " -an ",
        " rawvideo ",
    ] {
        assert!(
            noted.lines().any(|line| line.contains(kind)),
            "no {kind:?} in {noted}"
        );
    }
    for line in noted.lines() {
        let masks = line.rsplit_once(": ").map_or("", |(_, masks)| masks);
        let mask = |key: &str| {
            masks
                .split(' ')
                .find_map(|mask| mask.strip_prefix(key))
                .and_then(|hex| u64::from_str_radix(hex, 16).ok())
                .unwrap_or_else(|| panic!("{line}"))
        };
        assert_eq!(
            (mask("SigBlk:"), mask("SigIgn:") & 1 << (libc::SIGINT - 1)),
            (0, 0),
            "{line} in {noted}"
        );
    }
}

#[test]
fn animations_and_pictures_only_ffmpeg_reads_convert_as_video() {
    // rows C11, N7, C3, N38: bin/ascii-convert:527 and :538 at tag python-final: an animation is no still: a GIF, an animated WebP or an APNG converts as a video, `<stem>-ascii.mp4`, at its own rate, every frame; N7: so does a picture the built-in decoders do not take (PCX, which the old converter opened as a still and wrote as `pic-ascii.png`); N38: the WebP's first frame taken without a seek, after which ffmpeg's webp_anim demuxer gives no frame (with ffmpeg n9.0.2 the old converter, seeking to 0 s, ended `no frame at 0 s in spin.webp`, exit 1)
    let sandbox = Sandbox::new();
    let moving = "testsrc2=s=80x44:r=10:d=0.3";
    for (out, extra) in [
        ("anim.gif", &[][..]),
        ("spin.webp", &["-c:v", "libwebp_anim", "-loop", "0"][..]),
        ("moving.png", &["-f", "apng"][..]),
    ] {
        let path = sandbox.path(out);
        ffmpeg(
            &sandbox,
            &[
                &["-f", "lavfi", "-i", moving][..],
                extra,
                &[path.to_str().unwrap()],
            ]
            .concat(),
        );
    }
    let pcx = sandbox.path("pic.pcx");
    ffmpeg(
        &sandbox,
        &[
            "-f",
            "lavfi",
            "-i",
            "testsrc2=s=160x90:d=1",
            "-frames:v",
            "1",
            pcx.to_str().unwrap(),
        ],
    );
    let runs = [
        vec!["-W", "320", "anim.gif"],
        vec!["-W", "320", "spin.webp"],
        vec!["-W", "320", "moving.png"],
        vec!["-W", "320", "pic.pcx"],
    ];
    let made = converts(&sandbox, &runs);
    for ((ran, out), frames) in made
        .iter()
        .zip([
            "anim-ascii.mp4",
            "spin-ascii.mp4",
            "moving-ascii.mp4",
            "pic-ascii.mp4",
        ])
        .zip([3, 3, 3, 1])
    {
        assert_eq!(
            (ran.code, ran.stdout.clone()),
            (Some(0), format!("{out}\n")),
            "{ran:?}"
        );
        let out = sandbox.path(out);
        assert_eq!(streams(&sandbox, &out), ["video h264 320x190"]);
        assert_eq!(frames_in(&sandbox, &out), frames);
    }
    for out in ["anim-ascii.mp4", "spin-ascii.mp4"] {
        assert_eq!(
            entry(&sandbox, &sandbox.path(out), "v:0", "avg_frame_rate"),
            "10/1",
            "{out}"
        );
    }
    assert!(!sandbox.path("pic-ascii.png").exists());
}

#[test]
fn input_ffmpeg_cannot_read_is_exit_1_with_its_words() {
    // row C12: frame_at of bin/ascii-convert:350 at tag python-final: a file ffmpeg cannot read: exit 1 with ffmpeg's stderr (its words: A; N1: after `asciier: `); no frame at the time: `no frame at <s> s in <path>`; nothing written
    let sandbox = Sandbox::new();
    std::fs::write(sandbox.path("words.txt"), "not media\n").unwrap();
    clip(&sandbox, "clip.mp4", 0.2, "160x90", 25);
    for (args, said) in [
        (
            &["words.txt"][..],
            "Invalid data found when processing input",
        ),
        (&["missing.mp4"][..], "No such file or directory"),
    ] {
        let ran = convert(&sandbox, args);
        assert_eq!((ran.code, ran.stdout.as_str()), (Some(1), ""), "{ran:?}");
        assert!(
            ran.stderr.starts_with("asciier: ") && ran.stderr.contains(said),
            "{ran:?}"
        );
    }
    let ran = convert(&sandbox, &["--at", "99", "clip.mp4"]);
    assert_eq!(
        (ran.code, ran.stdout.as_str(), ran.stderr.as_str()),
        (Some(1), "", "asciier: no frame at 99.0 s in clip.mp4\n")
    );
    let mut made: Vec<String> = std::fs::read_dir(&sandbox.dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .filter(|name| name.contains("-ascii"))
        .collect();
    made.sort();
    assert_eq!(made, Vec::<String>::new());
}

#[test]
fn a_frame_is_found_at_its_time_where_ffmpeg_cannot_seek() {
    // rows C11, C12, N38: frame_at of bin/ascii-convert:350 at tag python-final seeked before ffmpeg read the source: an animation or anything else ffmpeg decodes is a video (C11), whose `--at` frame is the one at that time, and a time without one is `no frame at <s> s in <path>` (C12); with ffmpeg n9.0.2 that seek finds no frame in an animated WebP or a raw H.264 stream, whose length ffprobe cannot tell, so the old converter ended `no frame at 0.1 s in spin.webp`, exit 1 (seen here): N38: the frame is now decoded from the start, a second grab, and is the one a twin of the same frames that ffmpeg can seek gives at that time (a lossless MKV; the MP4 the stream came out of), not the first; a time past the end of a source whose length ffprobe tells fails at once, one grab, never decoded from the start
    let mut sandbox = Sandbox::new();
    let moving = "testsrc2=s=80x44:r=10:d=0.3,format=rgb24";
    for (out, codec) in [
        (
            "spin.webp",
            &["-c:v", "libwebp_anim", "-lossless", "1", "-loop", "0"][..],
        ),
        ("spin.mkv", &["-c:v", "ffv1"][..]),
    ] {
        let path = sandbox.path(out);
        ffmpeg(
            &sandbox,
            &[
                &["-f", "lavfi", "-i", moving][..],
                codec,
                &[path.to_str().unwrap()],
            ]
            .concat(),
        );
    }
    let mp4 = silent_clip(&sandbox, "clip.mp4", 2.0, "160x90", 25);
    let raw = sandbox.path("clip.264");
    ffmpeg(
        &sandbox,
        &[
            "-i",
            mp4.to_str().unwrap(),
            "-c:v",
            "copy",
            "-f",
            "h264",
            raw.to_str().unwrap(),
        ],
    );
    wrap_ffmpeg(&mut sandbox);
    let grabbed = |args: &[&str]| {
        let _ = std::fs::remove_file(sandbox.path("grabs"));
        let ran = convert(&sandbox, args);
        let grabs = std::fs::read_to_string(sandbox.path("grabs")).unwrap_or_default();
        (ran, grabs.lines().count())
    };
    for (source, twin, at) in [
        ("spin.webp", "spin.mkv", "0.1"),
        ("clip.264", "clip.mp4", "1.2"),
    ] {
        let (ran, grabs) = grabbed(&["--at", at, "-W", "320", "-o", "seek.png", source]);
        assert_eq!((ran.code, grabs), (Some(0), 2), "{source}: {ran:?}");
        let twin = converted(&sandbox, &["--at", at, "-W", "320", "-o", "twin.png", twin]);
        let first = converted(
            &sandbox,
            &["--at", "0", "-W", "320", "-o", "first.png", source],
        );
        let drawn = pixels(&sandbox.path("seek.png"));
        assert!(
            drawn == pixels(&twin) && drawn != pixels(&first),
            "{source}"
        );
    }
    for (source, at, grabs) in [("clip.mp4", "99", 1), ("clip.264", "9", 2)] {
        let (ran, made) = grabbed(&["--at", at, source]);
        assert_eq!(
            (ran.code, ran.stdout.as_str(), ran.stderr, made),
            (
                Some(1),
                "",
                format!("asciier: no frame at {at}.0 s in {source}\n"),
                grabs
            )
        );
    }
}

#[test]
fn failures_are_messages_with_exit_1() {
    // rows C13, N12, G10: exit 1 with `asciier: <why>` on stderr and nothing on stdout where bin/ascii-convert at tag python-final ended with a traceback (bin/ascii-convert:126, :355 and :555): the current theme missing even with a complete `--theme`, a colour missing from it, no font from fc-match, no ffmpeg; G10: the font is the file `fc-match -f %{file} 'JetBrainsMono Nerd Font Mono'` names, as font_path of lib/ascii_term.py:21 asked
    let mut sandbox = Sandbox::new();
    save(&gradient(80, 40), &sandbox.path("pic.png"));
    silent_clip(&sandbox, "mute.mp4", 0.2, "160x90", 25);
    let theme = sandbox.path("home/.config/kitty/current-theme.conf");
    let bl1nk = std::fs::read_to_string(&theme).unwrap();
    let complete = Path::new(CRATE).join("tests/bl1nk.conf");
    let said = |sandbox: &Sandbox, args: &[&str], why: String| {
        let ran = convert(sandbox, args);
        assert_eq!(
            (ran.code, ran.stdout, ran.stderr),
            (Some(1), String::new(), format!("asciier: {why}\n")),
            "{args:?}"
        );
    };
    std::fs::remove_file(&theme).unwrap();
    said(
        &sandbox,
        &["-c", "20", "-t", complete.to_str().unwrap(), "pic.png"],
        format!(
            "cannot read the kitty theme {}: No such file or directory (os error 2)",
            theme.display()
        ),
    );
    std::fs::write(&theme, bl1nk.replace("color5 ", "#color5 ")).unwrap();
    said(
        &sandbox,
        &["-c", "20", "pic.png"],
        format!("no color5 in the kitty theme {}", theme.display()),
    );
    std::fs::write(&theme, &bl1nk).unwrap();
    sandbox.program(
        "fc-match",
        "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$0.args\"\nexit 1\n",
    );
    said(
        &sandbox,
        &["-c", "20", "pic.png"],
        "no font file from fc-match for JetBrainsMono Nerd Font Mono".into(),
    );
    assert_eq!(
        sandbox.noted("fc-match.args"),
        "-f\n%{file}\nJetBrainsMono Nerd Font Mono\n"
    );
    sandbox.remove_program("fc-match");
    // the stubs alone on PATH: no ffmpeg
    let stubs = sandbox.path("bin");
    sandbox.set("PATH", stubs);
    said(
        &sandbox,
        &["mute.mp4"],
        "cannot run ffmpeg: No such file or directory (os error 2)".into(),
    );
    assert!(!sandbox.path("pic-ascii.png").exists() && !sandbox.path("mute-ascii.mp4").exists());
}

#[test]
fn the_grid_is_160_columns_of_12_by_27_px_unless_a_size_says_otherwise() {
    // rows L1, L2, L4, G8: tests/test_convert.py:105 and :117 at tag python-final, the pixels as bin/ascii-convert drew them in this harness: 400 x 300 at the defaults is 160 x 54 cells of 12 x 27 px, 1920 x 1458, in ExtraBold too (G8: `-b` draws the bold face's glyphs in its cells); a width or a height takes the columns that cover it, not held to 8..2000: `-W 16` 2 columns and 1 row, 16 x 27; `-H 16` 2 columns, 24 x 16; `-W 1000` 84 columns, 1000 x 783; a still's size is exact, odd numbers too: 1001 x 501
    let sandbox = Sandbox::new();
    save(&gradient(400, 300), &sandbox.path("pic.png"));
    let drawn: [(&[&str], (u32, u32), u32); 6] = [
        (&[], (1920, 1458), 0xb613976e),
        (&["-b"], (1920, 1458), 0x06aa919e),
        (&["-W", "16"], (16, 27), 0x3ac25fc9),
        (&["-H", "16"], (24, 16), 0x7f769a7c),
        (&["-W", "1000"], (1000, 783), 0xe547ea20),
        (&["-W", "1001", "-H", "501"], (1001, 501), 0x8fa856bc),
    ];
    let runs: Vec<Vec<String>> = drawn
        .iter()
        .enumerate()
        .map(|(i, (args, ..))| {
            let mut run: Vec<String> = args.iter().map(|arg| arg.to_string()).collect();
            run.extend(["-o".into(), format!("out{i}.png"), "pic.png".into()]);
            run
        })
        .collect();
    for ((i, (args, size, expected)), ran) in
        drawn.iter().enumerate().zip(converts(&sandbox, &runs))
    {
        assert_eq!(ran.code, Some(0), "{args:?}: {ran:?}");
        let picture = pixels(&sandbox.path(format!("out{i}.png")));
        assert_eq!(
            (picture.dimensions(), crc(&picture)),
            (*size, *expected),
            "{args:?}"
        );
    }
}

#[test]
fn columns_with_a_size_scale_the_font_to_fit() {
    // row L3: Style.layout of bin/ascii-convert:178 at tag python-final (tests/test_convert.py:111), the pixels as it drew them in this harness: `-c 100 -W 1920 -H 1080` of 400 x 300 (34 rows): contain takes the smaller fit, a 23.53 px font of 14.109375 x 32 px cells, the grid 1411 px wide in the middle; cover the larger, 32 px of 19.203125 x 43, the grid the full width, cut at top and bottom; one size alone: `-c 100 -W 1920` 1920 x 1462, `-c 100 -H 540` 705 x 540
    let sandbox = Sandbox::new();
    save(&gradient(400, 300), &sandbox.path("pic.png"));
    let drawn: [(&[&str], (u32, u32), u32); 4] = [
        (&["-W", "1920", "-H", "1080"], (1920, 1080), 0x8fdae120),
        (
            &["-W", "1920", "-H", "1080", "--fit", "cover"],
            (1920, 1080),
            0xfd8cb1cb,
        ),
        (&["-W", "1920"], (1920, 1462), 0x2a522990),
        (&["-H", "540"], (705, 540), 0x591a4ad3),
    ];
    let runs: Vec<Vec<String>> = drawn
        .iter()
        .enumerate()
        .map(|(i, (args, ..))| {
            let mut run: Vec<String> = ["-c", "100"]
                .iter()
                .chain(*args)
                .map(|arg| arg.to_string())
                .collect();
            run.extend(["-o".into(), format!("out{i}.png"), "pic.png".into()]);
            run
        })
        .collect();
    let mut drawings = Vec::new();
    for ((i, (args, size, expected)), ran) in
        drawn.iter().enumerate().zip(converts(&sandbox, &runs))
    {
        assert_eq!(ran.code, Some(0), "{args:?}: {ran:?}");
        let picture = pixels(&sandbox.path(format!("out{i}.png")));
        assert_eq!(
            (picture.dimensions(), crc(&picture)),
            (*size, *expected),
            "{args:?}"
        );
        drawings.push(picture);
    }
    // the ramp's bright right end is inked to the grid's edge: x 1664 contained, the canvas's 1919 covered
    let right = |picture: &RgbImage| *inked_columns(picture, GROUND).last().unwrap();
    assert!(
        (1600..1665).contains(&right(&drawings[0])),
        "{}",
        right(&drawings[0])
    );
    assert!(right(&drawings[1]) > 1900, "{}", right(&drawings[1]));
}

#[test]
fn video_and_frames_get_1920_px_on_the_long_side() {
    // rows L5, L4, C3, C4: web_size and main of bin/ascii-convert:514 and :523 at tag python-final (tests/test_convert.py:224 and :233): without a size a video or an `--at` frame is 1920 px on its long side in the frame's shape after `--crop`, the short side round(1920 x short / long / 2) x 2: 16:9 1920 x 1080, 9:16 1080 x 1920, 4:3 1920 x 1440, the left half of 16:9 (80 x 90) 1706 x 1920, as a frame and as a video (bin/ascii-convert:542); a video's size rounded up to even numbers: `-W 1001 -H 501` 1002 x 502; names `<stem>-ascii.mp4` and `<stem>-ascii-<%g of the time>s.png`; a file in the way of either replaced (the encoder's -y, bin/ascii-convert:425, and the PNG's save)
    let sandbox = Sandbox::new();
    clip(&sandbox, "clip.mp4", 0.08, "160x90", 25);
    silent_clip(&sandbox, "tall.mp4", 0.08, "90x160", 25);
    silent_clip(&sandbox, "four.mp4", 0.08, "160x120", 25);
    for name in ["clip-ascii.mp4", "clip-ascii-0.04s.png"] {
        std::fs::write(sandbox.path(name), "in the way").unwrap();
    }
    let runs = [
        vec!["clip.mp4"],
        vec!["-W", "1001", "-H", "501", "-o", "odd.mp4", "clip.mp4"],
        vec!["--crop", "0,0,50,100", "-o", "halfclip.mp4", "clip.mp4"],
        vec!["--at", "0.040", "clip.mp4"],
        vec!["--at", "0.01234567", "-W", "160", "clip.mp4"],
        vec!["--at", "0", "tall.mp4"],
        vec!["--at", "0", "four.mp4"],
        vec![
            "--at",
            "0",
            "--crop",
            "0,0,50,100",
            "-o",
            "half.png",
            "clip.mp4",
        ],
    ];
    let printed = [
        "clip-ascii.mp4",
        "odd.mp4",
        "halfclip.mp4",
        "clip-ascii-0.04s.png",
        "clip-ascii-0.0123457s.png",
        "tall-ascii-0s.png",
        "four-ascii-0s.png",
        "half.png",
    ];
    for (ran, printed) in converts(&sandbox, &runs).iter().zip(printed) {
        assert_eq!(
            (ran.code, ran.stdout.clone()),
            (Some(0), format!("{printed}\n")),
            "{ran:?}"
        );
    }
    assert_eq!(
        streams(&sandbox, &sandbox.path("clip-ascii.mp4")),
        ["video h264 1920x1080", "audio aac"]
    );
    assert_eq!(
        streams(&sandbox, &sandbox.path("odd.mp4")),
        ["video h264 1002x502", "audio aac"]
    );
    assert_eq!(
        streams(&sandbox, &sandbox.path("halfclip.mp4")),
        ["video h264 1706x1920", "audio aac"]
    );
    for (name, w, h) in [
        ("clip-ascii-0.04s.png", 1920, 1080),
        ("tall-ascii-0s.png", 1080, 1920),
        ("four-ascii-0s.png", 1920, 1440),
        ("half.png", 1706, 1920),
    ] {
        assert_eq!(png_info(&sandbox.path(name)), (w, h, 8, 2, 0), "{name}");
    }
    assert_eq!(png_info(&sandbox.path("clip-ascii-0.0123457s.png")).0, 160);
}

#[test]
fn the_picture_is_reduced_before_the_tone_map() {
    // rows L6, G9: Style.art of bin/ascii-convert:195 at tag python-final: a picture wider than 8 px a column is box-reduced to that first, then the tone table (G9: before the cell stage, on the picture's pixels), then chafa: a 1 px checkerboard of black and white 640 px wide at 40 columns is mid grey before `-l 50` takes it to black: nothing drawn; at 320 px (8 a column, no reduction) the level keeps black and white, and glyphs are drawn
    let sandbox = Sandbox::new();
    let checker = |w: u32, h: u32| {
        RgbImage::from_fn(w, h, |x, y| {
            image::Rgb([if (x + y) % 2 == 0 { 0 } else { 255 }; 3])
        })
    };
    save(&checker(640, 360), &sandbox.path("fine.png"));
    save(&checker(320, 180), &sandbox.path("exact.png"));
    let runs = [
        vec!["-c", "40", "-l", "50", "fine.png"],
        vec!["-c", "40", "-l", "50", "exact.png"],
    ];
    for ran in converts(&sandbox, &runs) {
        assert_eq!(ran.code, Some(0), "{ran:?}");
    }
    let fine = pixels(&sandbox.path("fine-ascii.png"));
    assert_eq!(fine.dimensions(), (480, 297));
    assert!(fine.pixels().all(|px| px.0 == GROUND));
    let exact = pixels(&sandbox.path("exact-ascii.png"));
    assert!(exact.pixels().filter(|px| px.0 != GROUND).count() > 10_000);
}

#[test]
fn crop_takes_a_window_black_past_the_borders() {
    // row L9: crop_rect and crop of bin/ascii-convert:321 and :330 at tag python-final (tests/test_convert.py:56): left round(w X / 100), top round(h Y / 100), width and height max(1, round(w W / 100)); outside the picture black, so blank: `--crop=-10,0,120,100` of a white 200 x 100 is the window -20..220: 30 columns, the outer 2.5 of each side blank (360 x 162, the pixels as the old converter drew them in this harness); `--crop 50,0,100,100` takes the right half and as much black: 10 columns inked, 10 blank
    let sandbox = Sandbox::new();
    save(&flat(200, 100, [255; 3]), &sandbox.path("white.png"));
    let runs = [
        vec![
            "-c",
            "30",
            "--crop=-10,0,120,100",
            "-o",
            "wide.png",
            "white.png",
        ],
        vec![
            "-c",
            "20",
            "--crop",
            "50,0,100,100",
            "-o",
            "right.png",
            "white.png",
        ],
    ];
    for ran in converts(&sandbox, &runs) {
        assert_eq!(ran.code, Some(0), "{ran:?}");
    }
    let wide = pixels(&sandbox.path("wide.png"));
    assert_eq!((wide.dimensions(), crc(&wide)), ((360, 162), 0xb984f3e0));
    let inked = inked_columns(&wide, GROUND);
    assert!(inked[0] >= 24 && *inked.last().unwrap() < 336, "{inked:?}");
    let right = pixels(&sandbox.path("right.png"));
    assert_eq!(right.dimensions(), (240, 135));
    let inked = inked_columns(&right, GROUND);
    assert!(inked[0] < 12 && *inked.last().unwrap() < 120, "{inked:?}");
}

#[test]
fn transparency_is_drawn_over_the_theme_background() {
    // row L10: main of bin/ascii-convert:551 at tag python-final: a still's transparency is put over the theme's background before the cells: white at alpha 0 is the background, nothing drawn; at alpha 128 a mid grey, drawn as the old converter drew it in this harness; the same white without alpha draws brighter; an animation's frames are not put over it (convert_video, bin/ascii-convert:405, decodes them as they are): an APNG of white at alpha 0 converts as a video whose first frame is a tenth or more ink (white's glyphs), where the background alone would be none
    let sandbox = Sandbox::new();
    for (name, alpha) in [("clear.png", 0), ("half.png", 128)] {
        image::RgbaImage::from_pixel(80, 40, image::Rgba([255, 255, 255, alpha]))
            .save(sandbox.path(name))
            .unwrap();
    }
    save(&flat(80, 40, [255; 3]), &sandbox.path("white.png"));
    let moving = sandbox.path("clear-moving.png");
    ffmpeg(
        &sandbox,
        &[
            "-f",
            "lavfi",
            "-i",
            "color=c=white@0:s=80x44:r=10:d=0.3,format=rgba",
            "-f",
            "apng",
            moving.to_str().unwrap(),
        ],
    );
    let runs = [
        vec!["-c", "20", "clear.png"],
        vec!["-c", "20", "half.png"],
        vec!["-c", "20", "white.png"],
        vec!["-W", "320", "clear-moving.png"],
    ];
    for ran in converts(&sandbox, &runs) {
        assert_eq!(ran.code, Some(0), "{ran:?}");
    }
    let clear = pixels(&sandbox.path("clear-ascii.png"));
    assert!(clear.pixels().all(|px| px.0 == GROUND));
    let half = pixels(&sandbox.path("half-ascii.png"));
    assert_eq!(crc(&half), 0xb8a1c77a);
    let white = pixels(&sandbox.path("white-ascii.png"));
    let light = |picture: &RgbImage| picture.pixels().map(|px| px.0[1] as u64).sum::<u64>();
    assert!(light(&white) > light(&half) && light(&half) > light(&clear));
    let video = sandbox.path("clear-moving-ascii.mp4");
    assert_eq!(streams(&sandbox, &video), ["video h264 320x190"]);
    let frame = first_frame(&sandbox, &video);
    let inked = frame
        .pixels()
        .filter(|px| px.0.iter().zip(GROUND).any(|(&v, g)| v.abs_diff(g) > 24))
        .count();
    let all = (frame.width() * frame.height()) as usize;
    assert!(inked * 10 > all, "{inked} of {all} px inked");
}

#[test]
fn an_svg_is_drawn_again_at_the_detail_the_columns_take() {
    // row L11: main of bin/ascii-convert:546 at tag python-final with raster_width of lib/ascii_image.py:30 (tests/test_convert.py:170): an SVG is a still, its first raster 1600 px wide; drawn again at ceil(columns x 8 x 100 / crop W) px when that is more, at most 8192 px on the long side: 100 columns of a quarter of a ring take 3200 px; 160 columns of a tenth of a 100:1 strip of bars want 12800 and get 8192; the pixels as the old converter drew them in this harness
    let sandbox = Sandbox::new();
    std::fs::write(
        sandbox.path("ring.svg"),
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24"><circle cx="12" cy="12" r="10" fill="none" stroke="#fff" stroke-width="0.6"/><circle cx="12" cy="12" r="4" fill="#f80"/></svg>"##,
    )
    .unwrap();
    let bars: String = (0..1000)
        .map(|x| format!(r##"<rect x="{x}" y="0" width="0.4" height="10" fill="#fff"/>"##))
        .collect();
    std::fs::write(
        sandbox.path("bars.svg"),
        format!(r#"<svg xmlns="http://www.w3.org/2000/svg" width="1000" height="10">{bars}</svg>"#),
    )
    .unwrap();
    let runs = [
        vec!["-o", "whole.png", "ring.svg"],
        vec![
            "-c",
            "100",
            "--crop",
            "0,0,25,25",
            "-o",
            "quarter.png",
            "ring.svg",
        ],
        vec!["-c", "160", "--crop", "0,0,10,100", "bars.svg"],
    ];
    for ran in converts(&sandbox, &runs) {
        assert_eq!(ran.code, Some(0), "{ran:?}");
    }
    for (name, size, expected) in [
        ("whole.png", (1920, 1944), 0x44a4872e),
        ("quarter.png", (1200, 1215), 0x7a3fa1e7),
        ("bars-ascii.png", (1920, 216), 0x95efefcf),
    ] {
        let picture = pixels(&sandbox.path(name));
        assert_eq!(
            (picture.dimensions(), crc(&picture)),
            (size, expected),
            "{name}"
        );
    }
}

#[test]
fn a_picture_is_written_as_8_bit_rgb_png() {
    // rows L12, L14, N10, N20: main of bin/ascii-convert:555 at tag python-final: a still or `--at` frame as 8-bit RGB PNG (L14: a wallpaper at the screen's 1920 x 1200 that hyprpaper and hyprlock load: here another decoder, ffmpeg's, reads it; the pixels as the old converter drew them in this harness); N20: another encoder, the same pixels; N10: PNG only, the extension in any case, another one a usage error (the old converter wrote it in that format)
    let sandbox = Sandbox::new();
    let wall = RgbImage::from_fn(160, 100, |x, y| {
        image::Rgb([(x * 255 / 159) as u8, (y * 255 / 99) as u8, 128])
    });
    save(&wall, &sandbox.path("wall.png"));
    clip(&sandbox, "clip.mp4", 0.2, "160x90", 25);
    let runs = [
        vec!["-W", "1920", "-H", "1200", "wall.png"],
        vec!["--at", "0", "-W", "160", "-o", "frame.PNG", "clip.mp4"],
    ];
    for ran in converts(&sandbox, &runs) {
        assert_eq!(ran.code, Some(0), "{ran:?}");
    }
    let out = sandbox.path("wall-ascii.png");
    assert_eq!(png_info(&out), (1920, 1200, 8, 2, 0));
    assert_eq!(crc(&pixels(&out)), 0x11cac5a7);
    assert_eq!(streams(&sandbox, &out), ["video png 1920x1200"]);
    let (_, _, depth, colour, interlace) = png_info(&sandbox.path("frame.PNG"));
    assert_eq!((depth, colour, interlace), (8, 2, 0));
    for (args, out) in [
        (&["-o", "wall.jpg", "wall.png"][..], "wall.jpg"),
        (
            &["--at", "0", "-o", "frame.bmp", "clip.mp4"][..],
            "frame.bmp",
        ),
    ] {
        let ran = convert(&sandbox, args);
        assert_eq!(
            (ran.code, ran.stderr),
            (
                Some(2),
                format!(
                    "usage: asciier convert [options] INPUT\nasciier convert: error: argument -o/--output: a picture is written as PNG: '{out}'\n"
                )
            )
        );
        assert!(!sandbox.path(out).exists());
    }
}

#[test]
fn the_rate_is_the_sources_average_so_variable_rate_keeps_its_speed() {
    // row X1: convert_video of bin/ascii-convert:376 at tag python-final (tests/test_convert.py:189): 1 s at 30 fps, then 1 s with two frames of three dropped (r_frame_rate 30, avg_frame_rate 200/9 here), made at the average rate, keeps its length within 0.2 s (at 30 it took 2.7 s for 1.8); a stream without an average (NUT at 10 fps) is made at 25
    let sandbox = Sandbox::new();
    let vfr = sandbox.path("vfr.mp4");
    ffmpeg(
        &sandbox,
        &[
            "-f",
            "lavfi",
            "-i",
            "testsrc2=d=2:r=30:s=160x90",
            "-vf",
            "select='lt(t,1)+not(mod(n,3))'",
            "-fps_mode",
            "vfr",
            "-c:v",
            "libx264",
            vfr.to_str().unwrap(),
        ],
    );
    let nut = sandbox.path("slow.nut");
    ffmpeg(
        &sandbox,
        &[
            "-f",
            "lavfi",
            "-i",
            "testsrc2=d=0.4:r=10:s=160x90",
            nut.to_str().unwrap(),
        ],
    );
    assert_eq!(entry(&sandbox, &nut, "v:0", "avg_frame_rate"), "0/0");
    let runs = [vec!["-W", "320", "vfr.mp4"], vec!["-W", "320", "slow.nut"]];
    for ran in converts(&sandbox, &runs) {
        assert_eq!(ran.code, Some(0), "{ran:?}");
    }
    let out = sandbox.path("vfr-ascii.mp4");
    let (made, source) = (length(&sandbox, &out, "v:0"), length(&sandbox, &vfr, "v:0"));
    assert!((made - source).abs() < 0.2, "{made} s for {source}");
    assert_eq!(
        entry(&sandbox, &out, "v:0", "avg_frame_rate"),
        entry(&sandbox, &vfr, "v:0", "avg_frame_rate")
    );
    assert_eq!(
        entry(
            &sandbox,
            &sandbox.path("slow-ascii.mp4"),
            "v:0",
            "avg_frame_rate"
        ),
        "25/1"
    );
}

/// The decoder's `-vf` in the wrapper's notes.
fn decode_filter(sandbox: &Sandbox) -> String {
    let args = std::fs::read_to_string(sandbox.path("decode.args")).unwrap();
    let mut lines = args.lines();
    lines.find(|line| *line == "-vf").unwrap();
    lines.next().unwrap().to_string()
}

#[test]
fn frames_are_decoded_cropped_at_the_detail_the_columns_take() {
    // row X2: convert_video of bin/ascii-convert:401 at tag python-final builds the decoder's filters: the crop window, padded with black first; the source's average rate; a scale down to 8 px a column, never up, to an even height; hqdn3d 2 and 1.5 over the downscale, then 3 and 2.25; seen through the ffmpeg wrapper, its arguments and its frames
    let mut sandbox = Sandbox::new();
    clip(&sandbox, "wide.mp4", 0.2, "160x90", 25);
    clip(&sandbox, "big.mp4", 0.2, "640x360", 25);
    clip(&sandbox, "small.mp4", 0.2, "80x44", 25);
    wrap_ffmpeg(&mut sandbox);
    let decoded = |sandbox: &Sandbox, args: &[&str]| {
        converted(sandbox, args);
        let frames = ppm_frames(&std::fs::read(sandbox.path("decode.ppm")).unwrap());
        assert_eq!(frames.len(), 5, "{args:?}");
        (decode_filter(sandbox), frames)
    };
    // 20 px left of the picture: 16 px of black, then the 160 x 90; 27 columns for 320 px want 216 px, more than there is
    let (filter, frames) = decoded(
        &sandbox,
        &[
            "--crop=-10,0,120,100",
            "-W",
            "320",
            "-o",
            "a.mp4",
            "wide.mp4",
        ],
    );
    assert_eq!(
        filter,
        "pad=192:90:16:0:black,crop=192:90:0:0,fps=25/1,scale='min(iw,216)':-2,hqdn3d=2:1.5:3:2.25"
    );
    for (w, h, rgb) in &frames {
        assert_eq!((*w, *h), (192, 90));
        let band = rgb.chunks(3).enumerate().filter(|(at, _)| at % 192 < 8);
        assert!(
            band.clone().all(|(_, px)| px.iter().all(|&v| v < 16)),
            "{:?}",
            band.take(4).collect::<Vec<_>>()
        );
    }
    // 640 px down to 216, the denoise over 640 / 216
    let (filter, frames) = decoded(&sandbox, &["-W", "320", "-o", "b.mp4", "big.mp4"]);
    assert_eq!(
        filter,
        "fps=25/1,scale='min(iw,216)':-2,hqdn3d=0.675:0.506:3:2.25"
    );
    assert!(
        frames
            .iter()
            .all(|(w, h, _)| *w == 216 && h % 2 == 0 && (120..=122).contains(h))
    );
    let (filter, frames) = decoded(&sandbox, &["-W", "320", "-o", "c.mp4", "small.mp4"]);
    assert_eq!(
        filter,
        "fps=25/1,scale='min(iw,216)':-2,hqdn3d=2:1.5:3:2.25"
    );
    assert!(frames.iter().all(|(w, h, _)| (*w, *h) == (80, 44)));
}

/// The hue of the ink in a quarter of a picture (0 top left, 1 top right, 2 bottom left, 3 bottom right): the mean of the pixels in the middle of the quarter that stand out from the background.
fn hue(picture: &RgbImage, quarter: u32) -> &'static str {
    let (w, h) = picture.dimensions();
    let (left, top) = (quarter % 2 * w / 2, quarter / 2 * h / 2);
    let (mut sum, mut n) = ([0u64; 3], 0u64);
    for y in top + h / 8..top + h * 3 / 8 {
        for x in left + w / 8..left + w * 3 / 8 {
            let px = picture.get_pixel(x, y).0;
            if px.iter().zip(GROUND).any(|(&v, g)| v.abs_diff(g) > 24) {
                (0..3).for_each(|c| sum[c] += px[c] as u64);
                n += 1;
            }
        }
    }
    let [r, g, b] = sum.map(|channel| channel / n.max(1));
    if b > r && b > g {
        "blue"
    } else if g > r && g > b {
        "green"
    } else if r > g && g > b && g * 10 > r * 7 {
        "yellow"
    } else if r > g && r > b {
        "red"
    } else {
        "none"
    }
}

#[test]
fn turned_video_comes_out_upright() {
    // row X3: frame_at and convert_video of bin/ascii-convert:350 and :404 at tag python-final: ffmpeg turns the frames by the stream's display matrix: 160 x 90 turned a quarter left converts at 360 px wide as 360 x 648, the frame and the video alike, its quarters green and blue over yellow and red (tests/test_viewer.py:68 at tag python-final; the stored clip has yellow and green over red and blue)
    let sandbox = Sandbox::new();
    let quadrants = "nullsrc=s=160x90:r=25:d=0.2,format=gbrp,geq=r='if(lt(X,W/2),255,0)':g='if(lt(Y,H/2),255,0)':b=128";
    let (flat, turned) = (sandbox.path("flat.mp4"), sandbox.path("turned.mp4"));
    ffmpeg(
        &sandbox,
        &[
            "-f",
            "lavfi",
            "-i",
            quadrants,
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
            flat.to_str().unwrap(),
        ],
    );
    ffmpeg(
        &sandbox,
        &[
            "-display_rotation",
            "90",
            "-i",
            flat.to_str().unwrap(),
            "-c",
            "copy",
            turned.to_str().unwrap(),
        ],
    );
    let runs = [
        vec!["--at", "0", "-W", "360", "turned.mp4"],
        vec!["-W", "360", "turned.mp4"],
    ];
    for ran in converts(&sandbox, &runs) {
        assert_eq!(ran.code, Some(0), "{ran:?}");
    }
    let frame = pixels(&sandbox.path("turned-ascii-0s.png"));
    let video = sandbox.path("turned-ascii.mp4");
    assert_eq!(streams(&sandbox, &video), ["video h264 360x648"]);
    for picture in [frame, first_frame(&sandbox, &video)] {
        assert_eq!(picture.dimensions(), (360, 648));
        assert_eq!(
            (0..4)
                .map(|quarter| hue(&picture, quarter))
                .collect::<Vec<_>>(),
            ["green", "blue", "yellow", "red"]
        );
    }
}

#[test]
fn every_frame_is_drawn_as_that_frame_alone() {
    // rows X4, N31: tests/test_convert.py:281 at tag python-final: each frame fed to the encoder is byte for byte the serial drawing of the frame decoded, in decode order, as many as decoded, and the file holds them all; the serial drawing here is the converter's own of that frame as a still of the same size; N31: cells on a thread of their own, painting and writing on the main one (AGENTS.md, 2026-11-11)
    let mut sandbox = Sandbox::new();
    clip(&sandbox, "clip.mp4", 0.4, "160x90", 25);
    wrap_ffmpeg(&mut sandbox);
    let out = converted(
        &sandbox,
        &["-W", "320", "-H", "180", "-o", "clip-out.mp4", "clip.mp4"],
    );
    let decoded = ppm_frames(&std::fs::read(sandbox.path("decode.ppm")).unwrap());
    let fed = std::fs::read(sandbox.path("encode.raw")).unwrap();
    assert_eq!((decoded.len(), fed.len()), (10, 10 * FRAME));
    let mut runs = Vec::new();
    for (i, (w, h, rgb)) in decoded.into_iter().enumerate() {
        save(
            &RgbImage::from_raw(w, h, rgb).unwrap(),
            &sandbox.path(format!("frame{i}.png")),
        );
        runs.push(vec![
            "-W".to_string(),
            "320".into(),
            "-H".into(),
            "180".into(),
            format!("frame{i}.png"),
        ]);
    }
    for (i, ran) in converts(&sandbox, &runs).iter().enumerate() {
        assert_eq!(ran.code, Some(0), "{ran:?}");
        let alone = pixels(&sandbox.path(format!("frame{i}-ascii.png")));
        assert!(
            alone.as_raw()[..] == fed[i * FRAME..(i + 1) * FRAME],
            "frame {i} differs"
        );
    }
    assert_eq!(frames_in(&sandbox, &out), 10);
}

#[test]
fn h264_with_every_sound_stream_as_aac_and_nothing_else() {
    // row X6: convert_video and vaapi of bin/ascii-convert:413 and :501 at tag python-final (tests/test_convert.py:215 and :220): H.264, by VAAPI only when a trial encode at the output size works, else by libx264 (which leaves its settings in the stream: `x264 - core`); whether this machine's VAAPI works is not assumed: the trial run here at 320 x 190 says which the files hold, and the encoder's arguments seen through the ffmpeg wrapper, `-vaapi_device /dev/dri/renderD128`, `-vf format=nv12,hwupload -c:v h264_vaapi -qp 24` or `-c:v libx264 -crf 20 -pix_fmt yuv420p`; a VAAPI that fails (LIBVA_DRIVER_NAME=bogus) falls back to libx264: `crf=20.0` in its settings, yuv420p; every sound stream of the source as AAC, none needed, subtitles left out
    let mut sandbox = Sandbox::new();
    std::fs::write(
        sandbox.path("words.srt"),
        "1\n00:00:00,000 --> 00:00:01,000\nwords\n",
    )
    .unwrap();
    let many = sandbox.path("many.mkv");
    ffmpeg(
        &sandbox,
        &[
            "-f",
            "lavfi",
            "-i",
            "testsrc2=s=160x90:r=25:d=0.2",
            "-f",
            "lavfi",
            "-i",
            "sine=d=0.2",
            "-f",
            "lavfi",
            "-i",
            "sine=d=0.2:f=880",
            "-i",
            sandbox.path("words.srt").to_str().unwrap(),
            "-map",
            "0",
            "-map",
            "1",
            "-map",
            "2",
            "-map",
            "3",
            "-c:v",
            "libx264",
            "-c:a",
            "libopus",
            "-c:s",
            "srt",
            many.to_str().unwrap(),
        ],
    );
    silent_clip(&sandbox, "mute.mp4", 0.2, "160x90", 25);
    let runs = [vec!["-W", "320", "many.mkv"], vec!["-W", "320", "mute.mp4"]];
    for ran in converts(&sandbox, &runs) {
        assert_eq!(ran.code, Some(0), "{ran:?}");
    }
    assert_eq!(
        streams(&sandbox, &sandbox.path("many-ascii.mp4")),
        ["video h264 320x190", "audio aac", "audio aac"]
    );
    assert_eq!(
        streams(&sandbox, &sandbox.path("mute-ascii.mp4")),
        ["video h264 320x190"]
    );
    let vaapi = Path::new("/dev/dri/renderD128").exists()
        && sandbox
            .command("ffmpeg")
            .args([
                "-v",
                "error",
                "-nostdin",
                "-vaapi_device",
                "/dev/dri/renderD128",
            ])
            .args(["-f", "lavfi", "-i", "color=s=320x190:d=0.04"])
            .args([
                "-vf",
                "format=nv12,hwupload",
                "-c:v",
                "h264_vaapi",
                "-f",
                "null",
                "-",
            ])
            .output()
            .unwrap()
            .status
            .success();
    let x264 = |out: &Path| find(&std::fs::read(out).unwrap(), b"x264 - core", 0).is_some();
    for out in ["many-ascii.mp4", "mute-ascii.mp4"] {
        assert_eq!(x264(&sandbox.path(out)), !vaapi, "{out}, VAAPI {vaapi}");
    }
    let libx264 = "-c:v\nlibx264\n-crf\n20\n-pix_fmt\nyuv420p\n-c:a\n";
    wrap_ffmpeg(&mut sandbox);
    converted(&sandbox, &["-W", "320", "-o", "plain.mp4", "mute.mp4"]);
    let args = encoder_args(&sandbox);
    if vaapi {
        assert!(
            args.starts_with("-v\nerror\n-y\n-vaapi_device\n/dev/dri/renderD128\n-f\nrawvideo\n")
                && args.contains("\n-vf\nformat=nv12,hwupload\n-c:v\nh264_vaapi\n-qp\n24\n-c:a\n"),
            "{args}"
        );
    } else {
        assert!(args.contains(libx264) && !args.contains("vaapi"), "{args}");
    }
    sandbox.set("LIBVA_DRIVER_NAME", "bogus");
    let out = converted(&sandbox, &["-W", "320", "-o", "bogus.mp4", "mute.mp4"]);
    assert_eq!(streams(&sandbox, &out), ["video h264 320x190"]);
    let args = encoder_args(&sandbox);
    assert!(
        args.contains(libx264) && !args.contains("h264_vaapi"),
        "{args}"
    );
    let stream = std::fs::read(&out).unwrap();
    let settings = find(&stream, b"x264 - core", 0).expect("not libx264");
    let line = &stream[settings..find(&stream, b"\0", settings).unwrap()];
    assert!(
        String::from_utf8_lossy(line).contains(" crf=20.0 "),
        "{}",
        String::from_utf8_lossy(line)
    );
    assert_eq!(entry(&sandbox, &out, "v:0", "pix_fmt"), "yuv420p");
}

#[test]
fn short_sound_is_padded_to_the_video() {
    // row X7: convert_video of bin/ascii-convert:419 and :423 at tag python-final (tests/test_convert.py:197): 3 s of video with 1 s of sound: the sound padded with silence to a second past the video's 75 frames at 25/1, `-af apad=whole_dur=4.000 -shortest` in the encoder's arguments, the file ending with the video: both 3 s; with the total unknown (an ffprobe that knows neither duration nor nb_frames) plain `-af apad`
    let mut sandbox = Sandbox::new();
    let short = sandbox.path("short.mp4");
    ffmpeg(
        &sandbox,
        &[
            "-f",
            "lavfi",
            "-i",
            "testsrc2=d=3:r=25:s=160x90",
            "-f",
            "lavfi",
            "-i",
            "sine=d=1",
            "-c:v",
            "libx264",
            "-c:a",
            "aac",
            short.to_str().unwrap(),
        ],
    );
    wrap_ffmpeg(&mut sandbox);
    let out = converted(&sandbox, &["-W", "320", "short.mp4"]);
    let (video, sound) = (length(&sandbox, &out, "v:0"), length(&sandbox, &out, "a:0"));
    assert!(
        (video - 3.0).abs() < 0.1 && (sound - 3.0).abs() < 0.15,
        "{video} s, {sound} s"
    );
    let args = encoder_args(&sandbox);
    assert!(
        args.contains("\n-c:a\naac\n-af\napad=whole_dur=4.000\n-shortest\n"),
        "{args}"
    );
    wrap_ffprobe(&sandbox);
    sandbox.set(
        "PROBED",
        r#"{"streams": [{"avg_frame_rate": "25/1"}], "format": {}}"#,
    );
    let ran = convert(&sandbox, &["-W", "320", "-o", "unknown.mp4", "short.mp4"]);
    assert_eq!(ran.code, Some(0), "{ran:?}");
    assert_eq!(progress(&ran.stderr).1, "?");
    let args = encoder_args(&sandbox);
    assert!(
        args.contains("\n-c:a\naac\n-af\napad\n-shortest\n"),
        "{args}"
    );
}

/// The counts of the progress on stderr, `\r<done>/<total> frames` again and again, the last one ended by a newline: the counts, the total as written.
fn progress(err: &str) -> (Vec<u64>, String) {
    let parts: Vec<&str> = err.split('\r').collect();
    assert_eq!(parts[0], "", "{err:?}");
    let last = parts.len() - 1;
    let (mut counts, mut totals) = (Vec::new(), HashSet::new());
    for (at, part) in parts.iter().enumerate().skip(1) {
        let line = if at == last {
            part.strip_suffix(" frames\n")
        } else {
            part.strip_suffix(" frames")
        };
        let (done, total) = line
            .and_then(|line| line.split_once('/'))
            .unwrap_or_else(|| panic!("{part:?} in {err:?}"));
        counts.push(done.parse().unwrap());
        totals.insert(total.to_string());
    }
    assert_eq!(totals.len(), 1, "{err:?}");
    (counts, totals.into_iter().next().unwrap())
}

#[test]
fn progress_counts_the_frames_on_stderr() {
    // row X8: convert_video of bin/ascii-convert:387 and :469 at tag python-final (tests/test_convert.py:205): `\r<done>/<total> frames` on stderr, terminal or not, from 0 up, a last one ended by a newline; the total the video stream's length times the rate (3 s of video with 4 s of sound: 75/75), else the container's (FLV: 10), else the stream's nb_frames (an ffprobe that knows neither duration: 10 of a 0.4 s MP4 at 25), else `?` (a raw AV1 stream)
    let mut sandbox = Sandbox::new();
    let long = sandbox.path("long.mp4");
    ffmpeg(
        &sandbox,
        &[
            "-f",
            "lavfi",
            "-i",
            "testsrc2=d=3:r=25:s=160x90",
            "-f",
            "lavfi",
            "-i",
            "sine=d=4",
            "-c:v",
            "libx264",
            "-c:a",
            "aac",
            long.to_str().unwrap(),
        ],
    );
    for (name, extra) in [
        ("clip.flv", &[][..]),
        ("clip.obu", &["-c:v", "libaom-av1", "-cpu-used", "8"][..]),
    ] {
        let path = sandbox.path(name);
        let source = ["-f", "lavfi", "-i", "testsrc2=d=0.4:r=25:s=160x90"];
        ffmpeg(
            &sandbox,
            &[&source[..], extra, &[path.to_str().unwrap()]].concat(),
        );
    }
    let runs = [
        vec!["-W", "320", "long.mp4"],
        vec!["-W", "320", "clip.flv"],
        vec!["-W", "320", "clip.obu"],
    ];
    let made = converts(&sandbox, &runs);
    for (ran, (frames, total)) in made.iter().zip([(75, "75"), (10, "10"), (10, "?")]) {
        assert_eq!(ran.code, Some(0), "{ran:?}");
        let (counts, written) = progress(&ran.stderr);
        assert_eq!(written, total, "{ran:?}");
        assert!(
            counts[0] == 0 && counts.windows(2).all(|pair| pair[0] <= pair[1]),
            "{counts:?}"
        );
        assert_eq!(counts.last(), Some(&frames), "{ran:?}");
    }
    let video = length(&sandbox, &sandbox.path("long-ascii.mp4"), "v:0");
    assert!((video - 3.0).abs() < 0.1, "{video} s");
    // on a terminal too: the newline as the terminal's line discipline writes it
    let mut tty = sandbox
        .launch()
        .arg("convert")
        .arg("-W")
        .arg("160")
        .arg("-o")
        .arg("tty.mp4")
        .arg("clip.flv")
        .start();
    assert_eq!(tty.exit(), 0);
    let out = tty.out();
    assert!(
        find(&out, b"\r0/10 frames", 0).is_some() && find(&out, b"\r10/10 frames\r\n", 0).is_some(),
        "{}",
        shown(&out)
    );
    silent_clip(&sandbox, "ten.mp4", 0.4, "160x90", 25);
    wrap_ffprobe(&sandbox);
    sandbox.set(
        "PROBED",
        r#"{"streams": [{"avg_frame_rate": "25/1", "nb_frames": "10"}], "format": {}}"#,
    );
    let ran = convert(&sandbox, &["-W", "320", "ten.mp4"]);
    assert_eq!(ran.code, Some(0), "{ran:?}");
    let (counts, total) = progress(&ran.stderr);
    assert_eq!(
        (counts.last(), total.as_str()),
        (Some(&10), "10"),
        "{ran:?}"
    );
    assert!(ran.stderr.ends_with("\r10/10 frames\n"), "{ran:?}");
}

#[test]
fn a_childs_lines_on_stderr_fall_between_the_progress_lines() {
    // row X8: bin/ascii-convert:475 at tag python-final: each `\r<done>/<total> frames` goes to stderr in one write (print flushed it at its `\r`), so a line a child writes there meanwhile lands between two, never inside one, as it did while one `eprint!` wrote its pieces apart; a decoder that says a line on stderr again and again while it gives its 250 frames: every stretch of the converter's text between two of its lines is whole progress lines, the old converter's too
    let mut sandbox = Sandbox::new();
    clip(&sandbox, "long.mp4", 10.0, "160x90", 25);
    wrap_ffmpeg(&mut sandbox);
    sandbox.set("CHATTER", "decoder says");
    let ran = convert(
        &sandbox,
        &["-W", "320", "-H", "180", "-o", "long-out.mp4", "long.mp4"],
    );
    assert_eq!((ran.code, ran.stdout.as_str()), (Some(0), "long-out.mp4\n"));
    let stretches: Vec<&str> = ran.stderr.split("decoder says\n").collect();
    assert!(
        stretches.len() > 100,
        "{} lines of the decoder",
        stretches.len() - 1
    );
    let cut: Vec<&&str> = stretches
        .iter()
        .filter(|text| {
            !text.is_empty() && !(text.starts_with('\r') && text.trim_end().ends_with(" frames"))
        })
        .collect();
    assert_eq!(cut, Vec::<&&str>::new());
    let (counts, total) = progress(&stretches.concat());
    assert_eq!((counts.last(), total.as_str()), (Some(&250), "250"));
}

#[test]
fn ctrl_c_writes_the_frames_in_flight_and_finishes_the_file() {
    // rows X9, X8, X4, N31, N25: tests/test_convert.py:305 at tag python-final: the encoder held after 10 frames of RGB (seen in its input: the old converter printed its count only as it read a frame, bin/ascii-convert:475, and fed rgb0, 7.5 of its frames before the hold) until the reading stops, then Ctrl-C (SIGINT to the group): the frames read are written whole but those Ctrl-C found in the pipes (4 at most), the file is finished and plays them, on stderr the progress, its 10 held among the counts of 250, then `stopped at frame <n>: <out> holds the part converted` and nothing else (the old converter killed the decoder at once: no complaint of a broken pipe from it), the path printed, then death by SIGINT (N25; the old converter exited 130); nothing left running; X4 memory bounded, N31 8 in flight (AGENTS.md, 2026-11-11): at most 10 frames past the 10 held, 8 waiting and one each in the reader's and the painter's hands
    let mut sandbox = Sandbox::new();
    clip(&sandbox, "long.mp4", 10.0, "160x90", 25);
    wrap_ffmpeg(&mut sandbox);
    sandbox.set("HOLD", "10");
    sandbox.set("FRAME", FRAME.to_string());
    let run = Running::start(
        &sandbox,
        &["-W", "320", "-H", "180", "-o", "long-out.mp4", "long.mp4"],
    );
    settled(&sandbox.path("encode.raw"), 10 * FRAME);
    let read = settled(&sandbox.path("decode.ppm"), 20 * PPM_FRAME) / PPM_FRAME;
    run.interrupt();
    std::fs::write(sandbox.path("go"), "").unwrap();
    let (status, out, err) = run.finish();
    assert_eq!(
        (status.signal(), out.as_str()),
        (Some(libc::SIGINT), "long-out.mp4\n"),
        "{err}"
    );
    let (shown, said) = err
        .split_once("stopped at frame ")
        .unwrap_or_else(|| panic!("{err:?}"));
    let (counts, total) = progress(shown);
    assert!(counts.contains(&10) && total == "250", "{err:?}");
    let done: usize = said
        .strip_suffix(": long-out.mp4 holds the part converted\n")
        .unwrap_or_else(|| panic!("{err:?}"))
        .parse()
        .unwrap();
    assert!(
        read - 4 <= done && done > 10 && done < 250,
        "{read} read, {done} done: {err:?}"
    );
    assert!(done <= 20 && read <= 24, "{read} read, {done} done");
    let fed = std::fs::metadata(sandbox.path("encode.raw")).unwrap().len() as usize;
    assert_eq!(fed, done * FRAME);
    assert_eq!(frames_in(&sandbox, &sandbox.path("long-out.mp4")), done);
    await_gone(&sandbox);
}

#[test]
fn ctrl_c_while_the_decoder_writes_nothing_stops_the_reading() {
    // rows X9, N25: convert_video of bin/ascii-convert:461 at tag python-final: Ctrl-C reached the old converter in its read of the decoder's pipe (KeyboardInterrupt), so a decoder that writes nothing held no Ctrl-C up: it ended within 3 s of one SIGINT with X9's message (reviewer's probe); a decoder that gives 5 of 20 frames, then nothing with its pipe open, and one Ctrl-C once the 5 are written (seen in the encoder's input: the old converter printed its count only as it read a frame, bin/ascii-convert:475): the decoder killed, the file finished with the 5, on stderr the progress then `stopped at frame 5: <out> holds the part converted`, the path printed, death by SIGINT, nothing left running
    let mut sandbox = Sandbox::new();
    clip(&sandbox, "clip.mp4", 0.8, "160x90", 25);
    wrap_ffmpeg(&mut sandbox);
    sandbox.set("STALL", (5 * PPM_FRAME).to_string());
    let run = Running::start(
        &sandbox,
        &["-W", "320", "-H", "180", "-o", "clip-out.mp4", "clip.mp4"],
    );
    await_file(&sandbox.path("held"));
    settled(&sandbox.path("encode.raw"), 5 * FRAME);
    run.interrupt();
    let (status, out, err) = run.finish();
    assert_eq!(
        (status.signal(), out.as_str()),
        (Some(libc::SIGINT), "clip-out.mp4\n"),
        "{err:?}"
    );
    let shown = err
        .strip_suffix("stopped at frame 5: clip-out.mp4 holds the part converted\n")
        .unwrap_or_else(|| panic!("{err:?}"));
    let (counts, total) = progress(shown);
    assert_eq!((counts.last(), total.as_str()), (Some(&5), "20"), "{err:?}");
    assert_eq!(frames_in(&sandbox, &sandbox.path("clip-out.mp4")), 5);
    await_gone(&sandbox);
}

#[test]
fn ctrl_c_before_the_first_frame_ends_as_before_the_encoding() {
    // rows X9, N25: a Ctrl-C while the decoder has given no frame, its pipe open, with the encoder started (both seen in the wrapper's notes, which the old converter makes too: it started the encoder before its first frame, and printed no progress until then): N25 ends it as a Ctrl-C before a video's encoding: the encoder, still waiting for its first input, killed before that input closes, so it never opens its output and a file in the way stays as it was; on stderr the progress alone, no path, death by SIGINT; so with either encoder, the trial's pick and libx264 under a VA driver that cannot load; the old converter took this Ctrl-C where it landed (seen here, h264_vaapi, 14 runs alone: 7 and 7): in X9's read loop of bin/ascii-convert:471 at tag python-final, which killed the decoder and closed the encoder's empty input, so the file in the way was made over: where h264_vaapi encodes, exit 1, ffmpeg's errors and `ffmpeg could not encode the video`, the file 0 bytes; where libx264 does, `stopped at frame 0: <out> holds the part converted`, the path, exit 130, a file of 261 bytes and no frame; or before that loop, no progress printed: its main's handler (:564-565) exited 130, nothing said, the decoder left running and holding the output open, so the status is asserted before the output's end
    let mut sandbox = Sandbox::new();
    clip(&sandbox, "clip.mp4", 0.8, "160x90", 25);
    wrap_ffmpeg(&mut sandbox);
    sandbox.set("STALL", "0");
    for driver in ["", "bogus"] {
        if !driver.is_empty() {
            sandbox.set("LIBVA_DRIVER_NAME", driver);
        }
        for note in ["held", "encode.args"] {
            let _ = std::fs::remove_file(sandbox.path(note));
        }
        std::fs::write(sandbox.path("clip-out.mp4"), "in the way").unwrap();
        let mut run = Running::start(
            &sandbox,
            &["-W", "320", "-H", "180", "-o", "clip-out.mp4", "clip.mp4"],
        );
        await_file(&sandbox.path("held"));
        await_file(&sandbox.path("encode.args"));
        run.interrupt();
        let status = run.ended();
        let ended = (status.code(), status.signal());
        assert_eq!(
            ended,
            (None, Some(libc::SIGINT)),
            "{driver:?}: {:?}",
            run.err()
        );
        let (_, out, err) = run.finish();
        assert_eq!(out, "", "{driver:?}: {err:?}");
        let (counts, total) = progress(&err);
        assert_eq!(
            (counts.last(), total.as_str()),
            (Some(&0), "20"),
            "{driver:?}: {err:?}"
        );
        assert_eq!(
            std::fs::read_to_string(sandbox.path("clip-out.mp4")).unwrap(),
            "in the way",
            "{driver:?}"
        );
        await_gone(&sandbox);
    }
}

#[test]
fn an_encoder_that_quits_is_exit_1_not_a_hang() {
    // rows X10, N1: tests/test_convert.py:335 at tag python-final: the encoder quits after reading three frames, or at once: exit 1, on stderr the progress then `asciier: ffmpeg could not encode the video` (N1; the old converter's `ascii-convert: `) and nothing else (no traceback, no complaint of a broken pipe from the decoder), nothing printed, no hang, nothing left running; the decoder here is a process group, the wrapper's shell and the ffmpeg it pipes through tee, killed whole before its pipe closes; the old converter killed the decoder's process alone (bin/ascii-convert:484), so in this harness that ffmpeg ran on and complained of the broken pipe after the message, and did not where it was the decoder itself (seen here, 3 of 3 each)
    let mut sandbox = Sandbox::new();
    clip(&sandbox, "quit.mp4", 2.0, "160x90", 25);
    wrap_ffmpeg(&mut sandbox);
    for frames in [3, 0] {
        sandbox.set("FAIL_AFTER", (frames * FRAME).to_string());
        let run = Running::start(
            &sandbox,
            &["-W", "320", "-H", "180", "-o", "quit-out.mp4", "quit.mp4"],
        );
        let (status, out, err) = run.finish();
        assert_eq!(
            (status.code(), out.as_str()),
            (Some(1), ""),
            "{frames}: {err:?}"
        );
        let shown = err
            .strip_suffix("asciier: ffmpeg could not encode the video\n")
            .unwrap_or_else(|| panic!("{frames}: {err:?}"));
        progress(shown);
        await_gone(&sandbox);
    }
}

#[test]
fn a_decoder_that_fails_part_way_is_exit_1_after_the_file() {
    // row N17: the decoder ending with an error after 5 of 20 frames: the frames it gave are converted and the file finished, then exit 1, `ffmpeg stopped decoding <input>: <out> holds the part converted` (the old converter never read the decoder's status: exit 0, X10a)
    let mut sandbox = Sandbox::new();
    clip(&sandbox, "clip.mp4", 0.8, "160x90", 25);
    wrap_ffmpeg(&mut sandbox);
    sandbox.set("DECODE_BYTES", (5 * PPM_FRAME).to_string());
    let ran = convert(
        &sandbox,
        &["-W", "320", "-H", "180", "-o", "clip-out.mp4", "clip.mp4"],
    );
    assert_eq!((ran.code, ran.stdout.as_str()), (Some(1), ""), "{ran:?}");
    assert!(
        ran.stderr.contains("\r5/20 frames\n")
            && ran.stderr.ends_with(
                "asciier: ffmpeg stopped decoding clip.mp4: clip-out.mp4 holds the part converted\n"
            ),
        "{ran:?}"
    );
    assert_eq!(frames_in(&sandbox, &sandbox.path("clip-out.mp4")), 5);
    await_gone(&sandbox);
}

#[test]
fn anamorphic_video_is_drawn_in_ffmpegs_shape() {
    // rows N22, L2: the converter keeps ffmpeg's reading of a frame, as the old one did (seen in this harness): a clip stored 160 x 180 and shown 2:1 wide (320 x 180) is drawn in its stored shape: at 192 px tall 15 columns, 180 x 192 (its shown shape would take 29, 348 px)
    let sandbox = Sandbox::new();
    let stretched = sandbox.path("stretched.mp4");
    ffmpeg(
        &sandbox,
        &[
            "-f",
            "lavfi",
            "-i",
            "testsrc2=s=160x180:r=25:d=0.08,setsar=2",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
            stretched.to_str().unwrap(),
        ],
    );
    let out = converted(&sandbox, &["--at", "0", "-H", "192", "stretched.mp4"]);
    let (w, h, ..) = png_info(&out);
    assert_eq!((w, h), (180, 192));
}
