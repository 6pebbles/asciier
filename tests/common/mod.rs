//! The black-box harness: asciier in a pseudo-terminal of a set size, in a sandbox of its own (home with the bl1nk theme and `ao=null`, XDG directories, stub programs first on PATH, a dead session bus), keys in, screen out.

// each test file uses a part of it
#![allow(dead_code)]

pub mod screen;

use screen::Screen;
use std::ffi::{OsStr, OsString};
use std::io::{BufRead, BufReader, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

pub const BIN: &str = env!("CARGO_BIN_EXE_asciier");
pub const CRATE: &str = env!("CARGO_MANIFEST_DIR");
/// How long a wait lasts before the test fails: generous, for a debug build among 16 tests at once.
pub const PATIENCE: Duration = Duration::from_secs(40);
/// The system's programs, never the user's: no `scope` there, and the stubs go first.
const SYSTEM_PATH: &str = "/usr/local/bin:/usr/bin:/bin";

pub const UP: &[u8] = b"\x1b[A";
pub const DOWN: &[u8] = b"\x1b[B";
pub const RIGHT: &[u8] = b"\x1b[C";
pub const LEFT: &[u8] = b"\x1b[D";
pub const F1: &[u8] = b"\x1bOP";
pub const ESC: &[u8] = b"\x1b";
pub const ENTER: &[u8] = b"\r";
pub const BACKSPACE: &[u8] = b"\x7f";
/// kitty's answer to the viewer's file probe, then to device attributes (tests/test_viewer_tty.py:20).
pub const KITTY_OK: &[u8] = b"\x1b_Gi=31;OK\x1b\\\x1b[?62;22c";
/// Deletes every kitty image of the window.
pub const KITTY_CLEAR: &[u8] = b"\x1b_Ga=d,d=A,q=2\x1b\\";
/// What leaving writes: images gone, cursor shown, the normal screen, the title popped.
pub const RESTORE: &[u8] = b"\x1b_Ga=d,d=A,q=2\x1b\\\x1b[?25h\x1b[?1049l\x1b[23;2t";

/// Notes its arguments in `<itself>.calls`, a line each and `--` after a call, then prints `<itself>.<first argument>` if there is one: hyprctl.
const HYPRCTL: &str = r#"#!/bin/sh
{ printf '%s\n' "$@"; echo --; } >> "$0.calls"
[ ! -e "$0.$1" ] || cat "$0.$1"
"#;

/// wl-copy: notes as above, keeps its input in `<itself>.stdin`, exits with the status in `<itself>.exit`.
const WL_COPY: &str = r#"#!/bin/sh
{ printf '%s\n' "$@"; echo --; } >> "$0.calls"
cat > "$0.stdin"
exit "$(cat "$0.exit" 2>/dev/null || echo 0)"
"#;

/// kitten: notes as above; `themes --dump-theme NAME` prints `<itself>.themes` if there is one, else fails as kitten does on an unknown name.
const KITTEN: &str = r#"#!/bin/sh
{ printf '%s\n' "$@"; echo --; } >> "$0.calls"
if [ "$1" = themes ]; then
    [ -e "$0.themes" ] && exec cat "$0.themes"
    echo "No theme named: $3" >&2
    exit 1
fi
"#;

/// The oscilloscope: notes its pid, environment and descriptors in `<itself>.out`, then runs until SIGTERM, which it notes too.
const SCOPE: &str = r#"#!/bin/sh
trap 'echo term >> "$0.out"; exit 0' TERM
in=$(readlink /proc/$$/fd/0) out=$(readlink /proc/$$/fd/1) err=$(readlink /proc/$$/fd/2)
{ echo "pid=$$"; echo "rows=$SCOPE_SPARE_ROWS"; echo "target=$SCOPE_TARGET"; echo "props=${PIPEWIRE_PROPS-none}"; echo "in=$in"; echo "out=$out"; echo "err=$err"; echo ready; } > "$0.out"
while :; do sleep 0.05; done
"#;

/// A directory of its own for one test, removed with it: the run's home, XDG directories, stub programs.
pub struct Sandbox {
    pub dir: PathBuf,
    /// Variables set over the run environment, or removed (`None`).
    vars: Vec<(String, Option<OsString>)>,
}

impl Sandbox {
    pub fn new() -> Sandbox {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "asciier-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        for sub in [
            "home/.config/kitty",
            "home/.config/mpv",
            "home/.cache",
            "data",
            "state",
            "runtime",
            "tmp",
            "bin",
        ] {
            std::fs::create_dir_all(dir.join(sub)).unwrap();
        }
        std::fs::set_permissions(dir.join("runtime"), std::fs::Permissions::from_mode(0o700))
            .unwrap();
        std::fs::copy(
            Path::new(CRATE).join("tests/bl1nk.conf"),
            dir.join("home/.config/kitty/current-theme.conf"),
        )
        .unwrap();
        std::fs::write(dir.join("home/.config/mpv/mpv.conf"), "ao=null\n").unwrap();
        let sandbox = Sandbox {
            dir,
            vars: Vec::new(),
        };
        for (name, script) in [
            ("hyprctl", HYPRCTL),
            ("wl-copy", WL_COPY),
            ("kitten", KITTEN),
            ("scope", SCOPE),
        ] {
            sandbox.program(name, script);
        }
        sandbox
    }

    pub fn path(&self, relative: impl AsRef<Path>) -> PathBuf {
        self.dir.join(relative)
    }

    pub fn home(&self) -> PathBuf {
        self.dir.join("home")
    }

    /// A folder of the sandbox, made.
    pub fn folder(&self, relative: &str) -> PathBuf {
        let folder = self.dir.join(relative);
        std::fs::create_dir_all(&folder).unwrap();
        folder
    }

    /// Set a variable over the run environment.
    pub fn set(&mut self, name: &str, value: impl AsRef<OsStr>) {
        self.vars
            .push((name.to_string(), Some(value.as_ref().to_os_string())));
    }

    /// Remove a variable from the run environment.
    pub fn unset(&mut self, name: &str) {
        self.vars.push((name.to_string(), None));
    }

    /// The environment of a run: nothing of the desktop's, the sandbox's directories, the stubs first on PATH, a session bus nobody listens on.
    pub fn env(&self) -> Vec<(String, OsString)> {
        let at = |relative: &str| self.dir.join(relative).into_os_string();
        let mut path = at("bin");
        path.push(":");
        path.push(SYSTEM_PATH);
        let mut bus = OsString::from("unix:path=");
        bus.push(at("no-bus"));
        let mut env: Vec<(String, OsString)> = vec![
            ("HOME".into(), at("home")),
            ("XDG_CONFIG_HOME".into(), at("home/.config")),
            ("XDG_CACHE_HOME".into(), at("home/.cache")),
            ("XDG_DATA_HOME".into(), at("data")),
            ("XDG_STATE_HOME".into(), at("state")),
            ("XDG_RUNTIME_DIR".into(), at("runtime")),
            ("TMPDIR".into(), at("tmp")),
            ("PATH".into(), path),
            ("TERM".into(), "xterm-256color".into()),
            ("LANG".into(), "C.UTF-8".into()),
            ("DBUS_SESSION_BUS_ADDRESS".into(), bus),
        ];
        for (name, value) in &self.vars {
            env.retain(|(known, _)| known != name);
            if let Some(value) = value {
                env.push((name.clone(), value.clone()));
            }
        }
        env
    }

    /// An executable script in the sandbox's bin/, first on PATH.
    pub fn program(&self, name: &str, script: &str) {
        write_program(&self.dir.join("bin").join(name), script);
    }

    /// Take a stub off PATH.
    pub fn remove_program(&self, name: &str) {
        std::fs::remove_file(self.dir.join("bin").join(name)).unwrap();
    }

    /// A file beside the stubs: what one prints, or its exit status.
    pub fn stub_data(&self, name: &str, text: &str) {
        std::fs::write(self.dir.join("bin").join(name), text).unwrap();
    }

    /// What a stub noted in `bin/<name>`; empty when nothing.
    pub fn noted(&self, name: &str) -> String {
        std::fs::read_to_string(self.dir.join("bin").join(name)).unwrap_or_default()
    }

    /// Wait until the stub's file `name` holds `text`; what it holds then.
    pub fn await_noted(&self, name: &str, text: &str) -> String {
        let end = Instant::now() + PATIENCE;
        loop {
            let noted = self.noted(name);
            if noted.contains(text) {
                return noted;
            }
            assert!(Instant::now() < end, "no {text:?} in {name}: {noted:?}");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// A command for a program with the run environment: asciier convert, ffprobe, a D-Bus client.
    pub fn command(&self, program: impl AsRef<OsStr>) -> Command {
        let mut command = Command::new(program);
        command.env_clear().envs(self.env()).stdin(Stdio::null());
        no_core(&mut command);
        command
    }

    /// `asciier ARGS` without a terminal: its output once it ended.
    pub fn run(&self, args: &[&OsStr]) -> Ran {
        let output = self.command(BIN).args(args).output().unwrap();
        Ran {
            code: output.status.code(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        }
    }

    /// The viewer on `file` in a 200 x 60 window of 9 x 20 px cells.
    pub fn view(&self, file: impl AsRef<OsStr>) -> Tty {
        self.launch().arg(file).start()
    }

    pub fn launch(&self) -> Launch<'_> {
        Launch {
            sandbox: self,
            args: Vec::new(),
            size: WIDE,
            reply: None,
            typed: Vec::new(),
        }
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// An executable script at `file`, written by a child process: written from here, a child forked meanwhile by another test would hold it open for writing, and running it would fail as busy.
pub fn write_program(file: &Path, script: &str) {
    let mut writer = Command::new("/bin/sh")
        .args(["-c", r#"cat > "$0" && chmod 755 "$0""#])
        .arg(file)
        .stdin(Stdio::piped())
        .spawn()
        .unwrap();
    writer
        .stdin
        .take()
        .unwrap()
        .write_all(script.as_bytes())
        .unwrap();
    assert!(writer.wait().unwrap().success());
}

/// No core file of a crash: systemd-coredump honours the limit.
fn no_core(command: &mut Command) {
    // SAFETY: setrlimit alone, between fork and exec.
    unsafe {
        command.pre_exec(|| {
            let none = libc::rlimit {
                rlim_cur: 0,
                rlim_max: 0,
            };
            libc::setrlimit(libc::RLIMIT_CORE, &none);
            Ok(())
        })
    };
}

/// A run that ended: its exit code and what it printed.
#[derive(Debug)]
pub struct Ran {
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

/// A window: cells and px.
#[derive(Clone, Copy, Debug)]
pub struct Size {
    pub rows: u16,
    pub cols: u16,
    pub xpx: u16,
    pub ypx: u16,
}

/// The window of the old suite (tests/test_viewer_tty.py:27): 200 x 60 cells of 9 x 20 px.
pub const WIDE: Size = Size {
    rows: 60,
    cols: 200,
    xpx: 1800,
    ypx: 1200,
};
/// 80 x 24 cells of 9 x 20 px.
pub const SMALL: Size = Size {
    rows: 24,
    cols: 80,
    xpx: 720,
    ypx: 480,
};

pub struct Launch<'a> {
    sandbox: &'a Sandbox,
    args: Vec<OsString>,
    size: Size,
    reply: Option<(Vec<u8>, Duration)>,
    typed: Vec<u8>,
}

impl Launch<'_> {
    pub fn arg(mut self, arg: impl AsRef<OsStr>) -> Self {
        self.args.push(arg.as_ref().to_os_string());
        self
    }

    pub fn size(mut self, size: Size) -> Self {
        self.size = size;
        self
    }

    /// Answer the kitty probe with `reply`, `delay` after it shows.
    pub fn kitty(mut self, reply: &[u8], delay: Duration) -> Self {
        self.reply = Some((reply.to_vec(), delay));
        self
    }

    /// Bytes typed before the program starts.
    pub fn typed(mut self, keys: &[u8]) -> Self {
        self.typed = keys.to_vec();
        self
    }

    pub fn start(self) -> Tty {
        Tty::start(self)
    }
}

struct Shared {
    out: Mutex<Vec<u8>>,
    grew: Condvar,
    stop: AtomicBool,
}

/// asciier in a pseudo-terminal: it is the session leader, the terminal its controlling one; what it writes is collected by a thread, which also answers the kitty probe when asked to.
pub struct Tty {
    master: Option<OwnedFd>,
    /// Held open so that what the program wrote stays readable after it ended.
    slave: Option<OwnedFd>,
    pub slave_name: String,
    child: Child,
    pub pid: u32,
    shared: Arc<Shared>,
    reader: Option<std::thread::JoinHandle<()>>,
    status: Option<ExitStatus>,
    pub size: Size,
    screen: Screen,
    seen: usize,
}

fn winsize(size: Size) -> libc::winsize {
    libc::winsize {
        ws_row: size.rows,
        ws_col: size.cols,
        ws_xpixel: size.xpx,
        ws_ypixel: size.ypx,
    }
}

/// A pseudo-terminal pair, both ends close-on-exec from the start: no other test's child inherits them.
fn pty(size: Size) -> (OwnedFd, OwnedFd, String) {
    // SAFETY: plain calls on descriptors this function owns.
    unsafe {
        let master = libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY | libc::O_CLOEXEC);
        assert!(
            master >= 0,
            "posix_openpt: {}",
            std::io::Error::last_os_error()
        );
        let master = OwnedFd::from_raw_fd(master);
        assert_eq!(libc::grantpt(master.as_raw_fd()), 0);
        assert_eq!(libc::unlockpt(master.as_raw_fd()), 0);
        let mut name = [0 as libc::c_char; 128];
        assert_eq!(
            libc::ptsname_r(master.as_raw_fd(), name.as_mut_ptr(), name.len()),
            0
        );
        let slave = libc::open(
            name.as_ptr(),
            libc::O_RDWR | libc::O_NOCTTY | libc::O_CLOEXEC,
        );
        assert!(
            slave >= 0,
            "open the slave: {}",
            std::io::Error::last_os_error()
        );
        let ws = winsize(size);
        assert_eq!(libc::ioctl(master.as_raw_fd(), libc::TIOCSWINSZ, &ws), 0);
        let name = std::ffi::CStr::from_ptr(name.as_ptr())
            .to_string_lossy()
            .into_owned();
        (master, OwnedFd::from_raw_fd(slave), name)
    }
}

fn reader(
    fd: OwnedFd,
    shared: Arc<Shared>,
    mut reply: Option<(Vec<u8>, Duration)>,
    answer: OwnedFd,
) {
    let mut buf = vec![0u8; 1 << 16];
    let mut due: Option<Instant> = None;
    while !shared.stop.load(Ordering::Relaxed) {
        if let (Some(at), Some((bytes, _))) = (due, &reply)
            && Instant::now() >= at
        {
            put(&answer, bytes);
            reply = None;
        }
        let mut wait = libc::pollfd {
            fd: fd.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: one descriptor, 10 ms at most.
        if unsafe { libc::poll(&mut wait, 1, 10) } <= 0 || wait.revents & libc::POLLIN == 0 {
            continue;
        }
        // SAFETY: a read into a buffer of that size.
        let n = unsafe { libc::read(fd.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len()) };
        if n <= 0 {
            std::thread::sleep(Duration::from_millis(5));
            continue;
        }
        let mut out = shared.out.lock().unwrap();
        let before = out.len().saturating_sub(16);
        out.extend_from_slice(&buf[..n as usize]);
        if due.is_none()
            && let Some((_, delay)) = &reply
            && out[before..].windows(8).any(|w| w == b"\x1b_Gi=31,")
        {
            due = Some(Instant::now() + *delay);
        }
        shared.grew.notify_all();
    }
}

fn put(fd: &OwnedFd, mut bytes: &[u8]) {
    while !bytes.is_empty() {
        // SAFETY: a write of that slice.
        let n = unsafe { libc::write(fd.as_raw_fd(), bytes.as_ptr().cast(), bytes.len()) };
        if n < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            panic!("write to the terminal: {error}");
        }
        bytes = &bytes[n as usize..];
    }
}

/// Bytes as a reader can tell them: escapes shown as `\e`, controls as `\xNN`.
pub fn shown(bytes: &[u8]) -> String {
    let mut text = String::new();
    for chunk in bytes.utf8_chunks() {
        for ch in chunk.valid().chars() {
            match ch {
                '\x1b' => text.push_str("\\e"),
                '\n' => text.push_str("\\n"),
                '\r' => text.push_str("\\r"),
                c if (c as u32) < 0x20 || c == '\x7f' => {
                    text.push_str(&format!("\\x{:02x}", c as u32))
                }
                c => text.push(c),
            }
        }
        for byte in chunk.invalid() {
            text.push_str(&format!("\\x{byte:02x}"));
        }
    }
    text
}

/// Where `needle` first occurs in `hay` at or after `from`.
pub fn find(hay: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if needle.is_empty() {
        return Some(from);
    }
    hay.get(from..)?
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|at| at + from)
}

pub fn count(hay: &[u8], needle: &[u8]) -> usize {
    hay.windows(needle.len()).filter(|w| *w == needle).count()
}

/// A toast as it is written: reverse video, a space each side.
pub fn toast(text: &str) -> Vec<u8> {
    format!("\x1b[7m {text} \x1b[0m").into_bytes()
}

impl Tty {
    fn start(launch: Launch) -> Tty {
        let (master, slave, slave_name) = pty(launch.size);
        if !launch.typed.is_empty() {
            put(&master, &launch.typed);
        }
        let mut command = Command::new(BIN);
        command
            .args(&launch.args)
            .env_clear()
            .envs(launch.sandbox.env())
            .current_dir(&launch.sandbox.dir)
            .stdin(Stdio::from(slave.try_clone().unwrap()))
            .stdout(Stdio::from(slave.try_clone().unwrap()))
            .stderr(Stdio::from(slave.try_clone().unwrap()));
        // SAFETY: setsid, ioctl and setrlimit alone, between fork and exec: a session of its own with the terminal as its controlling one, and no core file.
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() < 0 || libc::ioctl(0, libc::TIOCSCTTY, 0) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                let none = libc::rlimit {
                    rlim_cur: 0,
                    rlim_max: 0,
                };
                libc::setrlimit(libc::RLIMIT_CORE, &none);
                Ok(())
            })
        };
        let child = command.spawn().unwrap();
        drop(command);
        let shared = Arc::new(Shared {
            out: Mutex::new(Vec::new()),
            grew: Condvar::new(),
            stop: AtomicBool::new(false),
        });
        let (fd, answer, reply) = (
            master.try_clone().unwrap(),
            master.try_clone().unwrap(),
            launch.reply,
        );
        let thread = {
            let shared = shared.clone();
            std::thread::spawn(move || reader(fd, shared, reply, answer))
        };
        Tty {
            master: Some(master),
            slave: Some(slave),
            slave_name,
            pid: child.id(),
            child,
            shared,
            reader: Some(thread),
            status: None,
            size: launch.size,
            screen: Screen::new(launch.size.rows as usize, launch.size.cols as usize),
            seen: 0,
        }
    }

    /// Everything written so far.
    pub fn out(&self) -> Vec<u8> {
        self.shared.out.lock().unwrap().clone()
    }

    pub fn len(&self) -> usize {
        self.shared.out.lock().unwrap().len()
    }

    /// Type `keys` in one write; the output's length before them.
    pub fn send(&mut self, keys: &[u8]) -> usize {
        let at = self.len();
        put(self.master.as_ref().expect("the terminal is open"), keys);
        at
    }

    /// Wait until `test` holds for the output from `from` on; false after `within`.
    pub fn wait(&self, from: usize, within: Duration, test: impl Fn(&[u8]) -> bool) -> bool {
        let end = Instant::now() + within;
        let mut out = self.shared.out.lock().unwrap();
        loop {
            if test(out.get(from..).unwrap_or_default()) {
                return true;
            }
            let left = end.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return false;
            }
            out = self
                .shared
                .grew
                .wait_timeout(out, left.min(Duration::from_millis(50)))
                .unwrap()
                .0;
        }
    }

    /// Wait for `text` in the output from `from` on; the output's length past its end. Fails the test after `PATIENCE`.
    pub fn expect(&self, from: usize, text: impl AsRef<[u8]>) -> usize {
        let text = text.as_ref();
        if !self.wait(from, PATIENCE, |out| find(out, text, 0).is_some()) {
            self.fail(&format!("no {:?} after byte {from}", shown(text)));
        }
        let out = self.out();
        find(&out, text, from).unwrap() + text.len()
    }

    /// Fail the test, showing what was written last and the screen.
    pub fn fail(&self, why: &str) -> ! {
        let out = self.out();
        let tail = &out[out.len().saturating_sub(3000)..];
        let mut screen = Screen::new(self.size.rows as usize, self.size.cols as usize);
        screen.feed(&out);
        panic!(
            "{why}\n--- last bytes ---\n{}\n--- screen ---\n{}",
            shown(tail),
            screen.text()
        );
    }

    /// Wait until the status row (the bottom one) says playing, the length known, the clock past `00:00`: a `> 00:00` can come before mpv's late report of the open's `pause=yes`, which leaves the viewer believing it paused for a moment. Clips under 2 s never get there.
    pub fn await_playing(&mut self) {
        let rows = self.size.rows as usize;
        self.until("playing", |screen| {
            let row = screen.row(rows);
            row.starts_with("> ") && row.contains(" / ") && !row.starts_with("> 00:00 ")
        });
    }

    /// Wait until the status row starts with `text`.
    pub fn await_status(&mut self, text: &str) {
        let rows = self.size.rows as usize;
        self.until(&format!("the status {text:?}"), |screen| {
            screen.row(rows).starts_with(text)
        });
    }

    /// Output is quiet for `quiet`: nothing new written in that time (at most `PATIENCE`); for a still, whose screen settles.
    pub fn settle(&self, quiet: Duration) {
        let end = Instant::now() + PATIENCE;
        loop {
            let before = self.len();
            std::thread::sleep(quiet);
            if self.len() == before || Instant::now() > end {
                return;
            }
        }
    }

    /// The screen as all output so far leaves it.
    pub fn screen(&mut self) -> &Screen {
        let new = {
            let out = self.shared.out.lock().unwrap();
            out[self.seen..].to_vec()
        };
        self.seen += new.len();
        self.screen.feed(&new);
        &self.screen
    }

    /// Wait until `test` holds for the screen; fails the test after `PATIENCE`.
    pub fn until(&mut self, what: &str, test: impl Fn(&Screen) -> bool) {
        let end = Instant::now() + PATIENCE;
        loop {
            if test(self.screen()) {
                return;
            }
            if Instant::now() > end {
                self.fail(&format!("the screen never showed {what}"));
            }
            let at = self.len();
            self.wait(at, Duration::from_millis(100), |out| !out.is_empty());
        }
    }

    /// A new window size: SIGWINCH to the program.
    pub fn resize(&mut self, size: Size) {
        let ws = winsize(size);
        // SAFETY: the master's window size.
        assert_eq!(unsafe { libc::ioctl(self.fd(), libc::TIOCSWINSZ, &ws) }, 0);
        self.size = size;
        let _ = self.screen();
        self.screen.resize(size.rows as usize, size.cols as usize);
    }

    pub fn signal(&self, signal: libc::c_int) {
        // SAFETY: a signal to the child, which has not been reaped while self holds it.
        unsafe { libc::kill(self.pid as libc::pid_t, signal) };
    }

    /// The terminal's mode as the program left it: (ICANON, ECHO, ISIG) set.
    pub fn modes(&self) -> (bool, bool, bool) {
        // SAFETY: tcgetattr into a zeroed struct; the master reports the pair's mode.
        let mut mode: libc::termios = unsafe { std::mem::zeroed() };
        assert_eq!(unsafe { libc::tcgetattr(self.fd(), &mut mode) }, 0);
        let on = |flag| mode.c_lflag & flag != 0;
        (on(libc::ICANON), on(libc::ECHO), on(libc::ISIG))
    }

    /// Whether the program still runs.
    pub fn running(&mut self) -> bool {
        self.status.is_none()
            && self.child.try_wait().unwrap().is_none_or(|status| {
                self.status = Some(status);
                false
            })
    }

    /// Wait for the program's end, at most `within`; its exit status then, `None` when it still ran (it is killed).
    pub fn exit_within(&mut self, within: Duration) -> Option<ExitStatus> {
        let end = Instant::now() + within;
        while self.running() {
            if Instant::now() > end {
                self.kill();
                return None;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        self.drain();
        self.status
    }

    /// Wait for the program's end; its exit code. Fails the test when it does not end within `PATIENCE`.
    pub fn exit(&mut self) -> i32 {
        match self.exit_within(PATIENCE) {
            Some(status) => status
                .code()
                .unwrap_or_else(|| self.fail(&format!("ended by {status:?}"))),
            None => self.fail("still running"),
        }
    }

    /// Read what is still on its way from the terminal.
    fn drain(&self) {
        let end = Instant::now() + Duration::from_secs(2);
        loop {
            let mut waiting: libc::c_int = 0;
            // SAFETY: FIONREAD into an int.
            unsafe { libc::ioctl(self.fd(), libc::FIONREAD, &mut waiting) };
            if waiting == 0 || Instant::now() > end {
                std::thread::sleep(Duration::from_millis(20));
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// The program and what runs in its session, killed.
    pub fn kill(&mut self) {
        if self.running() {
            // SAFETY: the session's process group: the child leads it.
            unsafe { libc::kill(-(self.pid as libc::pid_t), libc::SIGKILL) };
            self.status = self.child.wait().ok();
        }
    }

    fn fd(&self) -> std::os::fd::RawFd {
        self.master
            .as_ref()
            .expect("the terminal is open")
            .as_raw_fd()
    }

    /// Close the terminal: every descriptor of its master side, as when the window goes. The exit status within `PATIENCE`, `None` when it still ran (it is killed).
    pub fn hang_up(&mut self) -> Option<ExitStatus> {
        self.shared.stop.store(true, Ordering::Relaxed);
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
        (self.master, self.slave) = (None, None);
        let end = Instant::now() + PATIENCE;
        while self.running() {
            if Instant::now() > end {
                self.kill();
                return None;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        self.status
    }
}

impl Drop for Tty {
    fn drop(&mut self) {
        self.kill();
        self.shared.stop.store(true, Ordering::Relaxed);
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

/// ffmpeg with the run environment of `sandbox`: `-v error -nostdin -y ARGS`, which must work.
pub fn ffmpeg(sandbox: &Sandbox, args: &[&str]) {
    let output = sandbox
        .command("ffmpeg")
        .args(["-v", "error", "-nostdin", "-y"])
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "ffmpeg {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// A test pattern clip with a sine tone (tests/support.py:32): `seconds` long, `size`, `rate` frames a second, keyframes every 5.
pub fn clip(sandbox: &Sandbox, name: &str, seconds: f64, size: &str, rate: u32) -> PathBuf {
    let out = sandbox.path(name);
    let (picture, tone) = (
        format!("testsrc2=d={seconds}:r={rate}:s={size}"),
        format!("sine=d={seconds}"),
    );
    ffmpeg(
        sandbox,
        &[
            "-f",
            "lavfi",
            "-i",
            &picture,
            "-f",
            "lavfi",
            "-i",
            &tone,
            "-c:v",
            "libx264",
            "-g",
            "5",
            "-pix_fmt",
            "yuv420p",
            "-c:a",
            "aac",
            out.to_str().unwrap(),
        ],
    );
    out
}

/// A test pattern clip without sound.
pub fn silent_clip(sandbox: &Sandbox, name: &str, seconds: f64, size: &str, rate: u32) -> PathBuf {
    let out = sandbox.path(name);
    let picture = format!("testsrc2=d={seconds}:r={rate}:s={size}");
    ffmpeg(
        sandbox,
        &[
            "-f",
            "lavfi",
            "-i",
            &picture,
            "-c:v",
            "libx264",
            "-g",
            "5",
            "-pix_fmt",
            "yuv420p",
            out.to_str().unwrap(),
        ],
    );
    out
}

/// A sine tone alone.
pub fn tone(sandbox: &Sandbox, name: &str, seconds: f64) -> PathBuf {
    let out = sandbox.path(name);
    let tone = format!("sine=d={seconds}");
    ffmpeg(
        sandbox,
        &[
            "-f",
            "lavfi",
            "-i",
            &tone,
            "-c:a",
            "aac",
            out.to_str().unwrap(),
        ],
    );
    out
}

/// A left-to-right grey ramp (tests/support.py:38).
pub fn gradient(w: u32, h: u32) -> image::RgbImage {
    image::RgbImage::from_fn(w, h, |x, _| {
        let v = (x as f64 * 255.0 / (w.max(2) - 1) as f64) as u8;
        image::Rgb([v, v, v])
    })
}

/// A picture saved at `path` (the format by its extension).
pub fn save(picture: &image::RgbImage, path: &Path) -> PathBuf {
    picture.save(path).unwrap();
    path.to_path_buf()
}

/// A PNG whose header says `w` x `h` and whose pixels are cut short: enough for a decoder to refuse it as too large by its header.
pub fn png_header(path: &Path, w: u32, h: u32) -> PathBuf {
    let mut ihdr = b"IHDR".to_vec();
    ihdr.extend(w.to_be_bytes());
    ihdr.extend(h.to_be_bytes());
    ihdr.extend([8, 2, 0, 0, 0]);
    let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
    for chunk in [ihdr, b"IDAT\x78\x9c\x63\x60".to_vec(), b"IEND".to_vec()] {
        png.extend((chunk.len() as u32 - 4).to_be_bytes());
        let mut crc = flate2::Crc::new();
        crc.update(&chunk);
        png.extend(&chunk);
        png.extend(crc.sum().to_be_bytes());
    }
    std::fs::write(path, png).unwrap();
    path.to_path_buf()
}

/// The file a kitty placement names, by its base64 payload.
pub fn placed_file(payload: &[u8]) -> PathBuf {
    const DIGITS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut bits = 0u32;
    let (mut held, mut out) = (0, Vec::new());
    for &byte in payload.iter().take_while(|&&b| b != b'=') {
        bits = bits << 6 | DIGITS.iter().position(|&d| d == byte).unwrap() as u32;
        held += 6;
        if held >= 8 {
            held -= 8;
            out.push((bits >> held) as u8);
        }
    }
    PathBuf::from(OsStr::from_bytes(&out))
}

/// The kitty placements (`a=T`) in `out`: their keys and the file each names.
pub fn placements(out: &[u8]) -> Vec<(String, PathBuf)> {
    let mut found = Vec::new();
    let mut at = 0;
    while let Some(start) = find(out, b"\x1b_Ga=T,", at) {
        let Some(end) = find(out, b"\x1b\\", start) else {
            break;
        };
        let body = &out[start + 3..end];
        let split = body.iter().position(|&b| b == b';').unwrap_or(body.len());
        found.push((
            String::from_utf8_lossy(&body[..split]).into_owned(),
            placed_file(body.get(split + 1..).unwrap_or_default()),
        ));
        at = end;
    }
    found
}

/// The files of /dev/shm named as kitty transfers are.
pub fn transfers() -> Vec<PathBuf> {
    std::fs::read_dir("/dev/shm")
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .is_some_and(|name| name.as_bytes().starts_with(b"tty-graphics-protocol-"))
        })
        .collect()
}

/// A session bus of its own for one test: no service files, so nothing can be started through it; the daemon dies with the test.
pub struct Bus {
    daemon: Child,
    pub address: String,
}

impl Bus {
    pub fn new(sandbox: &mut Sandbox) -> Bus {
        let socket = sandbox.path("bus");
        assert!(
            socket.as_os_str().len() < 100,
            "the temp dir is too deep for a socket: {socket:?}"
        );
        let conf = format!(
            "<busconfig><type>session</type><listen>unix:path={}</listen><policy context=\"default\">\
             <allow send_destination=\"*\" eavesdrop=\"true\"/><allow eavesdrop=\"true\"/><allow own=\"*\"/></policy></busconfig>",
            socket.display()
        );
        std::fs::write(sandbox.path("bus.conf"), conf).unwrap();
        let mut daemon = sandbox.command("dbus-daemon");
        daemon
            .arg(format!(
                "--config-file={}",
                sandbox.path("bus.conf").display()
            ))
            .args(["--nofork", "--print-address"])
            .stdout(Stdio::piped());
        // SAFETY: prctl alone, between fork and exec: the daemon dies with the test, however that ends.
        unsafe {
            daemon.pre_exec(|| {
                libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL);
                Ok(())
            })
        };
        let mut daemon = daemon.spawn().unwrap();
        let mut address = String::new();
        BufReader::new(daemon.stdout.take().unwrap())
            .read_line(&mut address)
            .unwrap();
        let address = address.trim().to_string();
        // never the session's own bus
        assert!(
            address.starts_with(&format!("unix:path={}", socket.display())),
            "no bus of its own: {address:?}"
        );
        sandbox.set("DBUS_SESSION_BUS_ADDRESS", &address);
        Bus { daemon, address }
    }
}

impl Drop for Bus {
    fn drop(&mut self) {
        let _ = self.daemon.kill();
        let _ = self.daemon.wait();
    }
}

/// ffprobe's `-show_entries stream=...` of a file as `key=value` lines.
pub fn probe(sandbox: &Sandbox, file: &Path, entries: &str, stream: Option<&str>) -> String {
    let mut command = sandbox.command("ffprobe");
    command.args(["-v", "error"]);
    if let Some(stream) = stream {
        command.args(["-select_streams", stream]);
    }
    let output = command
        .args([
            "-show_entries",
            entries,
            "-of",
            "default=noprint_wrappers=1",
        ])
        .arg(file)
        .output()
        .unwrap();
    assert!(output.status.success(), "ffprobe {file:?}");
    String::from_utf8_lossy(&output.stdout).into_owned()
}
