//! The terminal: raw mode and its restore on every way out, geometry, key tokens, the wake pipe, kitty graphics.

use crate::Result;
use std::cell::Cell;
use std::os::fd::RawFd;
use std::sync::Once;
use std::sync::atomic::{AtomicPtr, AtomicU64, Ordering};

const STDIN: RawFd = libc::STDIN_FILENO;
const STDOUT: RawFd = libc::STDOUT_FILENO;
const ESC: u8 = 0x1b;
/// Milliseconds the rest of a sequence cut by a read's end may take; a lone Esc has no rest.
const REST: i32 = 50;
/// Undoes what `enter` wrote, in reverse: kitty's images, the cursor, the screen, the title.
const RESTORE: &[u8] = b"\x1b_Ga=d,d=A,q=2\x1b\\\x1b[?25h\x1b[?1049l\x1b[23;2t";

/// One key as the terminal sent it: a byte, a CSI or SS3 sequence, or a lone Esc.
pub type Key = Vec<u8>;

/// The window in cells and a cell's size in px (fractions kept); 9 x 20 px when the terminal reports no px.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Geometry {
    pub cols: u32,
    pub rows: u32,
    pub cell_w: f64,
    pub cell_h: f64,
}

/// Why the loop was woken through the pipe.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Wake {
    /// SIGWINCH: the window changed.
    Resize,
    /// SIGHUP, SIGTERM or SIGINT: leave cleanly.
    Quit,
    /// libmpv has events or a frame.
    Mpv,
    /// A background job finished.
    Job,
}

/// The reasons by the byte that stands for each in the pipe.
const WAKES: [Wake; 4] = [Wake::Resize, Wake::Quit, Wake::Mpv, Wake::Job];

/// The wake pipe's ends, read << 32 | write; 0 until its first use.
static PIPE: AtomicU64 = AtomicU64::new(0);
/// The terminal mode `enter` found, while it is entered; leaked, since a signal handler may be reading it.
static SAVED: AtomicPtr<libc::termios> = AtomicPtr::new(std::ptr::null_mut());

thread_local! {
    /// Set while `guard` runs its closure: the panic hook leaves such a panic to the caller.
    static GUARDED: Cell<bool> = const { Cell::new(false) };
}

/// Whether stdin and stdout are terminals.
pub fn is_tty() -> bool {
    unsafe { libc::isatty(STDIN) == 1 && libc::isatty(STDOUT) == 1 }
}

/// Wake the loop: one byte into the pipe. Async-signal-safe; callable from any thread and from signal handlers.
pub fn wake(why: Wake) {
    let Some((_, write)) = pipe() else { return };
    let byte = why as u8;
    // a full pipe wakes the loop anyway; errno is kept for the code a handler interrupted
    unsafe {
        let errno = *libc::__errno_location();
        libc::write(write, (&raw const byte).cast(), 1);
        *libc::__errno_location() = errno;
    }
}

/// The wake pipe's (read, write) ends, made at first use and never closed: non-blocking, close-on-exec. Async-signal-safe.
fn pipe() -> Option<(RawFd, RawFd)> {
    let ends = |both: u64| ((both >> 32) as RawFd, both as u32 as RawFd);
    let both = PIPE.load(Ordering::Acquire);
    if both != 0 {
        return Some(ends(both));
    }
    let mut made = [0; 2];
    if unsafe { libc::pipe2(made.as_mut_ptr(), libc::O_NONBLOCK | libc::O_CLOEXEC) } != 0 {
        return None;
    }
    let new = (made[0] as u64) << 32 | made[1] as u64;
    match PIPE.compare_exchange(0, new, Ordering::AcqRel, Ordering::Acquire) {
        Ok(_) => Some((made[0], made[1])),
        // another thread made it meanwhile
        Err(both) => {
            unsafe {
                libc::close(made[0]);
                libc::close(made[1]);
            }
            Some(ends(both))
        }
    }
}

fn errno() -> i32 {
    unsafe { *libc::__errno_location() }
}

/// Write all of `bytes` to `fd`, waiting where it would block; gives up when the terminal is gone. Async-signal-safe.
fn put(fd: RawFd, mut bytes: &[u8]) {
    while !bytes.is_empty() {
        let n = unsafe { libc::write(fd, bytes.as_ptr().cast(), bytes.len()) };
        if n >= 0 {
            bytes = &bytes[n as usize..];
            continue;
        }
        match errno() {
            libc::EINTR => {}
            libc::EAGAIN => {
                let mut room = libc::pollfd {
                    fd,
                    events: libc::POLLOUT,
                    revents: 0,
                };
                unsafe { libc::poll(&mut room, 1, -1) };
            }
            _ => return,
        }
    }
}

/// Whether `fd` has input, or its end, within `ms`.
fn ready(fd: RawFd, ms: i32) -> bool {
    let mut input = libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    };
    loop {
        let n = unsafe { libc::poll(&mut input, 1, ms) };
        if n >= 0 || errno() != libc::EINTR {
            return n > 0;
        }
    }
}

/// One read of what `fd` holds, 4096 bytes at most; `None` at the end of input or when the terminal is gone.
fn read(fd: RawFd) -> Option<Vec<u8>> {
    let mut data = vec![0; 4096];
    loop {
        let n = unsafe { libc::read(fd, data.as_mut_ptr().cast(), data.len()) };
        if n > 0 {
            data.truncate(n as usize);
            return Some(data);
        }
        match (n, errno()) {
            (0, _) => return None,
            (_, libc::EINTR) => {}
            (_, libc::EAGAIN) => return Some(Vec::new()),
            _ => return None,
        }
    }
}

/// Put the terminal back as `enter` found it, once however many ways out are taken. Async-signal-safe.
fn restore() {
    let saved = SAVED.swap(std::ptr::null_mut(), Ordering::AcqRel);
    if !saved.is_null() {
        put(STDOUT, RESTORE);
        unsafe { libc::tcsetattr(STDIN, libc::TCSADRAIN, saved) };
    }
}

/// The panic hook, set once: the terminal restored before the message prints; a panic under `guard` passes without either.
fn hook() {
    static SET: Once = Once::new();
    SET.call_once(|| {
        let print = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            if !GUARDED.get() {
                restore();
                print(info);
            }
        }));
    });
}

/// Run `f` and hand back its panic, if any, with nothing printed and the terminal left as it is: for decoders that may panic on a damaged file.
pub fn guard<T>(f: impl FnOnce() -> T + std::panic::UnwindSafe) -> std::thread::Result<T> {
    hook();
    let outer = GUARDED.replace(true);
    let result = std::panic::catch_unwind(f);
    GUARDED.set(outer);
    result
}

extern "C" fn on_resize(_: libc::c_int) {
    wake(Wake::Resize);
}

extern "C" fn on_quit(_: libc::c_int) {
    wake(Wake::Quit);
}

/// A fault (libmpv aborts on some input): the terminal back, then the signal's own end.
extern "C" fn on_fault(signal: libc::c_int) {
    restore();
    unsafe {
        libc::signal(signal, libc::SIG_DFL);
        libc::raise(signal);
    }
}

/// SA_RESTART: libmpv's condition on every handler. SA_ONSTACK: a stack overflow leaves the handler no room on the stack itself.
fn handle(signal: libc::c_int, handler: extern "C" fn(libc::c_int)) {
    unsafe {
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = handler as libc::sighandler_t;
        action.sa_flags = libc::SA_RESTART | libc::SA_ONSTACK;
        libc::sigemptyset(&mut action.sa_mask);
        libc::sigaction(signal, &action, std::ptr::null_mut());
    }
}

pub struct Term {
    geometry: Option<Geometry>,
}

impl Term {
    /// Enter: cbreak without ISIG, title pushed and set to `title`, alternate screen, cursor hidden; panic hook and handlers installed (SIGWINCH, SIGHUP, SIGTERM, SIGINT wake the loop; SIGABRT and SIGSEGV restore the terminal and re-raise). Input typed before is discarded.
    pub fn enter(title: &str) -> Result<Term> {
        if pipe().is_none() {
            crate::err!("no wake pipe: {}", std::io::Error::last_os_error());
        }
        let mut mode: libc::termios = unsafe { std::mem::zeroed() };
        if unsafe { libc::tcgetattr(STDIN, &mut mode) } != 0 {
            crate::err!("stdin is no terminal: {}", std::io::Error::last_os_error());
        }
        let saved = Box::into_raw(Box::new(mode));
        if SAVED
            .compare_exchange(
                std::ptr::null_mut(),
                saved,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_err()
        {
            drop(unsafe { Box::from_raw(saved) });
            crate::err!("the terminal is entered already");
        }
        hook();
        handle(libc::SIGWINCH, on_resize);
        for signal in [libc::SIGHUP, libc::SIGTERM, libc::SIGINT] {
            handle(signal, on_quit);
        }
        for signal in [libc::SIGABRT, libc::SIGSEGV] {
            handle(signal, on_fault);
        }
        mode.c_lflag &= !(libc::ICANON | libc::ECHO | libc::ISIG);
        mode.c_cc[libc::VMIN] = 1;
        mode.c_cc[libc::VTIME] = 0;
        if unsafe { libc::tcsetattr(STDIN, libc::TCSAFLUSH, &mode) } != 0 {
            let error = std::io::Error::last_os_error();
            SAVED.store(std::ptr::null_mut(), Ordering::Release);
            crate::err!("cannot set the terminal: {error}");
        }
        put(
            STDOUT,
            format!("\x1b[22;2t\x1b]2;{title}\x07\x1b[?1049h\x1b[?25l").as_bytes(),
        );
        Ok(Term { geometry: None })
    }

    /// A `Term` that entered nothing, for tests that draw without a terminal: leaving it does nothing.
    #[cfg(test)]
    pub fn detached() -> Term {
        Term { geometry: None }
    }

    /// Restore everything `enter` changed; safe to call twice.
    pub fn leave(&mut self) {
        // the handlers stay: a signal repeated during the caller's cleanup still only wakes
        restore();
    }

    /// The window's geometry (row V9 without the COLUMNS / LINES override): one snapshot until `resized`.
    pub fn geometry(&mut self) -> Geometry {
        *self.geometry.get_or_insert_with(|| {
            let mut size: libc::winsize = unsafe { std::mem::zeroed() };
            // a failure leaves the zeros, which read as a terminal that says nothing
            unsafe { libc::ioctl(STDOUT, libc::TIOCGWINSZ, &mut size) };
            measure(size.ws_row, size.ws_col, size.ws_xpixel, size.ws_ypixel)
        })
    }

    /// Forget the snapshot (after SIGWINCH).
    pub fn resized(&mut self) {
        self.geometry = None;
    }

    pub fn write(&mut self, bytes: &[u8]) {
        put(STDOUT, bytes);
    }

    /// The read end of the wake pipe, for the loop's poll.
    pub fn wake_fd(&self) -> RawFd {
        pipe().map_or(-1, |(read, _)| read)
    }

    /// The reasons waiting in the pipe, each once.
    pub fn wakes(&mut self) -> Vec<Wake> {
        let mut reasons = Vec::new();
        let mut bytes = [0u8; 4096];
        loop {
            let n = unsafe { libc::read(self.wake_fd(), bytes.as_mut_ptr().cast(), bytes.len()) };
            for byte in &bytes[..n.max(0) as usize] {
                match WAKES.get(*byte as usize) {
                    Some(why) if !reasons.contains(why) => reasons.push(*why),
                    _ => {}
                }
            }
            if n < bytes.len() as isize {
                return reasons;
            }
        }
    }

    /// The keys waiting on stdin (rows V11, V12): everything pending in one read, a sequence cut by the read completed within 50 ms, terminal replies dropped. `None`: the terminal is gone.
    pub fn keys(&mut self) -> Option<Vec<Key>> {
        if !ready(STDIN, 0) {
            return Some(Vec::new());
        }
        let data = read(STDIN)?;
        let rest = || {
            if ready(STDIN, REST) {
                read(STDIN)
            } else {
                None
            }
        };
        Some(gather(data, rest))
    }
}

impl Drop for Term {
    fn drop(&mut self) {
        self.leave();
    }
}

/// A window of `rows` x `cols` cells and `xpx` x `ypx` px as the terminal reports it, zeros where it does not say (row V9): 80 x 24 cells, 9 x 20 px a cell.
fn measure(rows: u16, cols: u16, xpx: u16, ypx: u16) -> Geometry {
    let cols = if cols > 0 { cols as u32 } else { 80 };
    let rows = if rows > 0 { rows as u32 } else { 24 };
    let (cell_w, cell_h) = if xpx > 0 && ypx > 0 {
        (xpx as f64 / cols as f64, ypx as f64 / rows as f64)
    } else {
        (9.0, 20.0)
    };
    Geometry {
        cols,
        rows,
        cell_w,
        cell_h,
    }
}

/// The keys of one batch: `data`, joined by what `more` reads while the input ends inside a sequence; once `more` has nothing, the cut sequence counts byte by byte (a lone Esc is a key).
fn gather(mut data: Vec<u8>, mut more: impl FnMut() -> Option<Vec<u8>>) -> Vec<Key> {
    let mut keys = Vec::new();
    loop {
        let (whole, tail) = split(&data, false);
        keys.extend(whole);
        if tail.is_empty() {
            return keys;
        }
        let mut tail = tail.to_vec();
        match more() {
            Some(rest) if !rest.is_empty() => tail.extend(rest),
            _ => {
                keys.extend(split(&tail, true).0);
                return keys;
            }
        }
        data = tail;
    }
}

/// Split input bytes into keys, dropping terminal replies (APC strings, `CSI ?` reports); the second value is the unfinished tail (an escape sequence cut short, or a lone Esc). With `last` no more input comes: a sequence cut short is read as its single bytes, no tail is left.
fn split(data: &[u8], last: bool) -> (Vec<Key>, &[u8]) {
    let mut keys = Vec::new();
    let mut at = 0;
    while at < data.len() {
        let len = match token(&data[at..]) {
            Some(len) => len,
            None if last => 1,
            None => break,
        };
        let key = &data[at..at + len];
        // a terminal's answer, no key: an APC string, a CSI with "?"
        if !(key.starts_with(b"\x1b_") || key.starts_with(b"\x1b[?")) {
            keys.push(key.to_vec());
        }
        at += len;
    }
    (keys, &data[at..])
}

/// The length of the token `data` (not empty) starts with: an APC string to `ESC \`, a CSI sequence (parameters `?0-9;`, then a letter or `~`), SS3 with one byte, else one byte. `None` while the end of `data` leaves a sequence open.
fn token(data: &[u8]) -> Option<usize> {
    if data[0] != ESC {
        return Some(1);
    }
    match *data.get(1)? {
        b'_' => {
            let end = 2 + data[2..].iter().position(|&byte| byte == ESC)?;
            Some(if *data.get(end + 1)? == b'\\' {
                end + 2
            } else {
                1
            })
        }
        b'[' => {
            let end = 2 + data[2..]
                .iter()
                .take_while(|byte| matches!(byte, b'?' | b'0'..=b'9' | b';'))
                .count();
            let last = *data.get(end)?;
            Some(if last.is_ascii_alphabetic() || last == b'~' {
                end + 1
            } else {
                1
            })
        }
        b'O' => data.get(2).map(|_| 3),
        _ => Some(1),
    }
}

pub mod kitty {
    //! The kitty graphics protocol as the viewer uses it (rows S4, K3-K5).

    use super::{Geometry, STDIN, STDOUT};
    use std::ffi::OsString;
    use std::io::Write;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::OpenOptionsExt;
    use std::path::PathBuf;
    use std::time::{Duration, Instant};

    /// Deletes every image this window shows.
    pub const CLEAR: &[u8] = b"\x1b_Ga=d,d=A,q=2\x1b\\";
    /// How long `probe` waits for the terminal; a later answer is dropped as input (`split`).
    const PROBE_WAIT: Duration = Duration::from_secs(1);

    /// Whether the environment says kitty: `KITTY_WINDOW_ID` non-empty or `TERM=xterm-kitty`.
    pub fn named() -> bool {
        says(
            std::env::var_os("KITTY_WINDOW_ID"),
            std::env::var_os("TERM"),
        )
    }

    fn says(window: Option<OsString>, term: Option<OsString>) -> bool {
        window.is_some_and(|id| !id.is_empty()) || term.is_some_and(|term| term == "xterm-kitty")
    }

    /// Whether the terminal reads a file from /dev/shm (`a=q,t=t`), asked on stdout, answered on stdin within 1 s (row S4). Needs cbreak mode.
    pub fn probe() -> bool {
        let Some(file) = shm(&[0; 3]) else {
            return false;
        };
        let mut ask = b"\x1b_Gi=31,s=1,v=1,a=q,t=t,f=24;".to_vec();
        ask.extend(base64(file.as_os_str().as_bytes()));
        // device attributes: every terminal answers them, and after the graphics query
        ask.extend(b"\x1b\\\x1b[c");
        super::put(STDOUT, &ask);
        let (mut reply, end) = (Vec::new(), Instant::now() + PROBE_WAIT);
        while !attributes(&reply) {
            let left = end.saturating_duration_since(Instant::now());
            if left.is_zero() || !super::ready(STDIN, left.as_millis() as i32 + 1) {
                break;
            }
            match super::read(STDIN) {
                Some(more) if !more.is_empty() => reply.extend(more),
                _ => break,
            }
        }
        let _ = std::fs::remove_file(file);
        reply.windows(12).any(|part| part == b"\x1b_Gi=31;OK\x1b\\")
    }

    /// Whether `reply` holds the answer to device attributes: `CSI ?`, digits and `;`, `c`.
    fn attributes(reply: &[u8]) -> bool {
        (3..reply.len()).any(|at| {
            reply[at - 3..at] == *b"\x1b[?"
                && reply[at..]
                    .iter()
                    .find(|byte| !matches!(byte, b'0'..=b'9' | b';'))
                    == Some(&b'c')
        })
    }

    /// A new file in /dev/shm holding `bytes`; the name kitty asks of a file it reads and removes (`t=t`).
    fn shm(bytes: &[u8]) -> Option<PathBuf> {
        for _ in 0..16 {
            let mut random = [0u8; 8];
            unsafe { libc::getrandom(random.as_mut_ptr().cast(), random.len(), 0) };
            let path = PathBuf::from(format!(
                "/dev/shm/tty-graphics-protocol-{:016x}",
                u64::from_ne_bytes(random)
            ));
            let made = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&path);
            match made {
                Ok(mut file) => {
                    if file.write_all(bytes).is_ok() {
                        return Some(path);
                    }
                    let _ = std::fs::remove_file(path);
                    return None;
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(_) => return None,
            }
        }
        None
    }

    fn base64(bytes: &[u8]) -> Vec<u8> {
        const DIGITS: &[u8; 64] =
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut text = Vec::with_capacity(bytes.len().div_ceil(3) * 4);
        for group in bytes.chunks(3) {
            let bits = group.iter().fold(0, |bits, &byte| bits << 8 | byte as u32)
                << (8 * (3 - group.len()));
            for digit in 0..4 {
                text.push(if digit <= group.len() {
                    DIGITS[(bits >> (18 - 6 * digit) & 63) as usize]
                } else {
                    b'='
                });
            }
        }
        text
    }

    /// The files pixels travel in: /dev/shm/tty-graphics-protocol-<random>, one at a time.
    #[derive(Default)]
    pub struct Transfer {
        last: Option<std::path::PathBuf>,
    }

    impl Transfer {
        /// The escape bytes that show `w` x `h` raw pixels (`depth` 24 or 32 bits) centred in the box of `cols` x `rows` cells whose top left cell is (`col`, `row`), under the text (row K3); the previous file is removed first. Empty when the file cannot be written.
        #[allow(clippy::too_many_arguments)]
        pub fn place(
            &mut self,
            w: u32,
            h: u32,
            pixels: &[u8],
            depth: u8,
            col: u32,
            row: u32,
            cols: u32,
            rows: u32,
            geometry: Geometry,
        ) -> Vec<u8> {
            self.clear();
            let Some(file) = shm(pixels) else {
                return Vec::new();
            };
            // the cells the picture covers
            let cells = |px: u32, cell: f64, room: u32| {
                ((px as f64 / cell).round_ties_even() as u32)
                    .max(1)
                    .min(room)
            };
            let (c, r) = (
                cells(w, geometry.cell_w, cols),
                cells(h, geometry.cell_h, rows),
            );
            let mut out = format!(
                "\x1b[{};{}H\x1b_Ga=T,f={depth},t=t,s={w},v={h},c={c},r={r},C=1,z=-1,q=2;",
                row + (rows - r) / 2,
                col + (cols - c) / 2
            )
            .into_bytes();
            out.extend(base64(file.as_os_str().as_bytes()));
            out.extend(b"\x1b\\");
            self.last = Some(file);
            out
        }

        /// Remove the last file (kitty has read it by now; another terminal never did).
        pub fn clear(&mut self) {
            if let Some(file) = self.last.take() {
                let _ = std::fs::remove_file(file);
            }
        }
    }

    impl Drop for Transfer {
        fn drop(&mut self) {
            self.clear();
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn named_by_window_id_or_term() {
            // KittyTerminalTest of tests/test_viewer.py:1295, and an empty id: kitty_terminal() of lib/ascii_term.py:51 at tag python-final, run by python3
            for (window, term, want) in [
                (Some("3"), Some("xterm-256color"), true),
                (None, Some("xterm-kitty"), true),
                (None, Some("xterm-256color"), false),
                (None, None, false),
                (Some(""), Some("xterm"), false),
                (Some(""), Some("xterm-kitty"), true),
            ] {
                let said = says(window.map(OsString::from), term.map(OsString::from));
                assert_eq!(said, want, "{window:?} {term:?}");
            }
        }

        #[test]
        fn base64_is_pythons() {
            // base64.b64encode, run by python3
            for (bytes, text) in [
                (&b""[..], ""),
                (b"f", "Zg=="),
                (b"fo", "Zm8="),
                (b"foo", "Zm9v"),
                (b"foob", "Zm9vYg=="),
                (b"fooba", "Zm9vYmE="),
                (b"foobar", "Zm9vYmFy"),
                (
                    b"/dev/shm/tty-graphics-protocol-abc123",
                    "L2Rldi9zaG0vdHR5LWdyYXBoaWNzLXByb3RvY29sLWFiYzEyMw==",
                ),
                (b"\xfa\xfb\xfc\xfd\xfe\xff\x00\x01", "+vv8/f7/AAE="),
            ] {
                assert_eq!(base64(bytes), text.as_bytes());
            }
        }

        #[test]
        fn the_answer_to_device_attributes_is_found() {
            // the pattern of bin/ascii-viewer:246 at tag python-final on these replies, run by python3
            for (reply, want) in [
                (&b""[..], false),
                (b"\x1b[?62;22c", true),
                (b"\x1b_Gi=31;OK\x1b\\", false),
                (b"\x1b_Gi=31;OK\x1b\\\x1b[?62;22c", true),
                (b"\x1b[?62;", false),
                (b"\x1b[?c", true),
                (b"\x1b[62c", false),
                (b"x\x1b[?1;2cq", true),
                (b"\x1b[?6x2c", false),
                (b"\x1b[?\x1b[?1c", true),
                (b"\x1b[?", false),
                (b"\x1b[?1;2", false),
                (b"\x1b[?1;2C", false),
            ] {
                assert_eq!(attributes(reply), want, "{reply:?}");
            }
        }

        #[test]
        fn place_writes_the_pixels_to_a_file_and_names_it_centred() {
            // (w, h, depth, col, row, cols, rows, cell w, cell h) -> the bytes before the file's name: Player.kitty_pixels of bin/ascii-viewer:2027 at tag python-final, run by python3
            #[rustfmt::skip]
            let cases = [
                ((800, 400, 24, 1, 1, 200, 60, 9.0, 20.0), "\x1b[21;56H\x1b_Ga=T,f=24,t=t,s=800,v=400,c=89,r=20,C=1,z=-1,q=2;"),
                ((450, 225, 24, 1, 1, 50, 20, 9.0, 20.0), "\x1b[5;1H\x1b_Ga=T,f=24,t=t,s=450,v=225,c=50,r=11,C=1,z=-1,q=2;"),
                ((400, 800, 24, 1, 1, 200, 60, 9.0, 20.0), "\x1b[11;79H\x1b_Ga=T,f=24,t=t,s=400,v=800,c=44,r=40,C=1,z=-1,q=2;"),
                ((180, 120, 24, 1, 1, 20, 6, 9.0, 20.0), "\x1b[1;1H\x1b_Ga=T,f=24,t=t,s=180,v=120,c=20,r=6,C=1,z=-1,q=2;"),
                ((500, 300, 24, 1, 1, 10, 5, 9.0, 20.0), "\x1b[1;1H\x1b_Ga=T,f=24,t=t,s=500,v=300,c=10,r=5,C=1,z=-1,q=2;"),
                ((3, 2, 24, 1, 1, 100, 30, 9.0, 20.0), "\x1b[15;50H\x1b_Ga=T,f=24,t=t,s=3,v=2,c=1,r=1,C=1,z=-1,q=2;"),
                ((2, 2, 32, 48, 2, 51, 28, 9.0, 20.0), "\x1b[15;73H\x1b_Ga=T,f=32,t=t,s=2,v=2,c=1,r=1,C=1,z=-1,q=2;"),
                // halves go to the even cell count
                ((45, 50, 24, 1, 1, 80, 24, 10.0, 20.0), "\x1b[12;39H\x1b_Ga=T,f=24,t=t,s=45,v=50,c=4,r=2,C=1,z=-1,q=2;"),
                ((25, 30, 24, 1, 1, 80, 24, 10.0, 20.0), "\x1b[12;40H\x1b_Ga=T,f=24,t=t,s=25,v=30,c=2,r=2,C=1,z=-1,q=2;"),
                ((35, 70, 24, 3, 4, 80, 24, 10.0, 20.0), "\x1b[14;41H\x1b_Ga=T,f=24,t=t,s=35,v=70,c=4,r=4,C=1,z=-1,q=2;"),
                ((640, 361, 24, 48, 2, 151, 58, 9.025, 20.06), "\x1b[22;88H\x1b_Ga=T,f=24,t=t,s=640,v=361,c=71,r=18,C=1,z=-1,q=2;"),
                ((100, 100, 24, 1, 1, 100, 30, 10.0, 22.0), "\x1b[13;46H\x1b_Ga=T,f=24,t=t,s=100,v=100,c=10,r=5,C=1,z=-1,q=2;"),
            ];
            let mut transfer = Transfer::default();
            let mut before: Option<PathBuf> = None;
            for ((w, h, depth, col, row, cols, rows, cell_w, cell_h), head) in cases {
                let pixels: Vec<u8> = (0..w * h * depth / 8).map(|i| i as u8).collect();
                let geometry = Geometry {
                    cols,
                    rows,
                    cell_w,
                    cell_h,
                };
                let out =
                    transfer.place(w, h, &pixels, depth as u8, col, row, cols, rows, geometry);
                let file = transfer.last.clone().unwrap();
                assert!(file.starts_with("/dev/shm/"));
                assert!(
                    file.file_name()
                        .unwrap()
                        .as_bytes()
                        .starts_with(b"tty-graphics-protocol-")
                );
                let name = base64(file.as_os_str().as_bytes());
                assert_eq!(out, [head.as_bytes(), &name, b"\x1b\\"].concat());
                assert_eq!(std::fs::read(&file).unwrap(), pixels);
                // one file at a time
                assert!(before.replace(file).is_none_or(|old| !old.exists()));
            }
            transfer.clear();
            assert!(transfer.last.is_none() && !before.unwrap().exists());
            transfer.clear();
        }

        #[test]
        fn a_dropped_transfer_takes_its_file_along() {
            let window = Geometry {
                cols: 80,
                rows: 24,
                cell_w: 9.0,
                cell_h: 20.0,
            };
            let mut transfer = Transfer::default();
            let shown = transfer.place(1, 1, &[1, 2, 3], 24, 1, 1, 80, 24, window);
            assert!(!shown.is_empty());
            let file = transfer.last.clone().unwrap();
            assert!(file.exists());
            drop(transfer);
            assert!(!file.exists());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::ffi::OsString;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::os::unix::ffi::OsStringExt;
    use std::os::unix::process::{CommandExt, ExitStatusExt};
    use std::path::PathBuf;
    use std::process::{Child, Command, ExitStatus, Stdio};
    use std::time::{Duration, Instant};

    type Keys = &'static [&'static [u8]];

    // Expected values of the three tables: KEY, REPLY, PARTIAL and the read loop of poll() in bin/ascii-viewer at tag python-final (lines 122-124, 2351-2361), run by python3 on these inputs.

    /// Input that ends on a whole token, and its keys.
    #[rustfmt::skip]
    const WHOLE: &[(&[u8], Keys)] = &[
        (b"", &[]),
        (b"q", &[b"q"]),
        (b"3q", &[b"3", b"q"]),
        (b"\x1b[A", &[b"\x1b[A"]),
        (b"\x1b[B\x1b[C\x1b[D", &[b"\x1b[B", b"\x1b[C", b"\x1b[D"]),
        (b"\x1b[1;2A\x1b[1;2B\x1b[1;2C\x1b[1;2D", &[b"\x1b[1;2A", b"\x1b[1;2B", b"\x1b[1;2C", b"\x1b[1;2D"]),
        (b"\x1bOP", &[b"\x1bOP"]),
        (b"\x1b[11~", &[b"\x1b[11~"]),
        (b"\x1b[P", &[b"\x1b[P"]),
        (b"l\x1b_Gi=31;EBADF:Failed to open file for graphics transmission: /dev/shm/x with error: [2] No such file or directory\x1b\\\x1b[?62;22ck", &[b"l", b"k"]),
        (b"\x1b_Gi=31;OK\x1b\\\x1b[?62;22c", &[]),
        (b"\x1b_Gi=31;EBADF:Failed to open file for graphics transmission: /dev/shm/tty-graphics-protocol-x with error: [2] No such file or directory\x1b\\\x1b[?62;22c", &[]),
        (b"\x1b_Gi=31;EBADF:could not read\x1b\\\x1b[?62;22c", &[]),
        (b"\x03", &[b"\x03"]),
        (b"\x1a\x1c", &[b"\x1a", b"\x1c"]),
        (b"\x7f\x08\\", &[b"\x7f", b"\x08", b"\\"]),
        (b"\x0d\x0a\x09 ", &[b"\x0d", b"\x0a", b"\x09", b" "]),
        (b"\xc3\xa9", &[b"\xc3", b"\xa9"]),
        (b"\xff\x00", &[b"\xff", b"\x00"]),
        (b"a\x1bb", &[b"a", b"\x1b", b"b"]),
        (b"\x1b\x1bq", &[b"\x1b", b"\x1b", b"q"]),
        (b"\x1b[<x", &[b"\x1b", b"[", b"<", b"x"]),
        (b"\x1b[1;x", &[b"\x1b[1;x"]),
        (b"\x1b[ A", &[b"\x1b", b"[", b" ", b"A"]),
        (b"\x1b_ab\x1bXc", &[b"\x1b", b"_", b"a", b"b", b"\x1b", b"X", b"c"]),
        (b"\x1b_\x1b\\q", &[b"q"]),
        (b"\x1bO\x1b[A", &[b"\x1bO\x1b", b"[", b"A"]),
        (b"\x1bOPq", &[b"\x1bOP", b"q"]),
        (b"\x1b[?25hq", &[b"q"]),
        (b"\x1b[?1;2cq", &[b"q"]),
        (b"\x1b[200~", &[b"\x1b[200~"]),
        (b"\x1b[;A", &[b"\x1b[;A"]),
        (b"\x1b[?A", &[]),
        (b"\x1b[1?2c", &[b"\x1b[1?2c"]),
        (b"x\x1b[A\x1b_G\x1b\\y", &[b"x", b"\x1b[A", b"y"]),
        (b"\x1b[\x1b[A", &[b"\x1b", b"[", b"\x1b[A"]),
        (b"\x1bO\x0a", &[b"\x1bO\x0a"]),
        (b"\x1b_a\x0ab\x1b\\z", &[b"z"]),
    ];

    /// Input that ends inside a sequence, and its keys were no more to come.
    #[rustfmt::skip]
    const CUT: &[(&[u8], Keys)] = &[
        (b"\x1b", &[b"\x1b"]),
        (b"q\x1b", &[b"q", b"\x1b"]),
        (b"\x1b[", &[b"\x1b", b"["]),
        (b"\x1b[1", &[b"\x1b", b"[", b"1"]),
        (b"\x1b[1;", &[b"\x1b", b"[", b"1", b";"]),
        (b"\x1b[1;2", &[b"\x1b", b"[", b"1", b";", b"2"]),
        (b"\x1b[?", &[b"\x1b", b"[", b"?"]),
        (b"\x1b[?62;", &[b"\x1b", b"[", b"?", b"6", b"2", b";"]),
        (b"\x1bO", &[b"\x1b", b"O"]),
        (b"\x1b_", &[b"\x1b", b"_"]),
        (b"\x1b_Gi=31;O", &[b"\x1b", b"_", b"G", b"i", b"=", b"3", b"1", b";", b"O"]),
        (b"\x1b_Gi=31;OK\x1b", &[b"\x1b", b"_", b"G", b"i", b"=", b"3", b"1", b";", b"O", b"K", b"\x1b"]),
        (b"l\x1b_Gi=31;O", &[b"l", b"\x1b", b"_", b"G", b"i", b"=", b"3", b"1", b";", b"O"]),
        (b"\x1b[A\x1b[", &[b"\x1b[A", b"\x1b", b"["]),
        (b"\x1b[A\x1b", &[b"\x1b[A", b"\x1b"]),
        (b"ab\x1bO", &[b"a", b"b", b"\x1b", b"O"]),
        (b"\x1b\x1b", &[b"\x1b", b"\x1b"]),
        (b"\x1b[\x1b", &[b"\x1b", b"[", b"\x1b"]),
        (b"\x1b_a\x1bb\x1b", &[b"\x1b", b"_", b"a", b"\x1b", b"b", b"\x1b"]),
        (b"\x1bO\x1b[", &[b"\x1bO\x1b", b"["]),
        (b"K\x1b\\\x1b[?62;", &[b"K", b"\x1b", b"\\", b"\x1b", b"[", b"?", b"6", b"2", b";"]),
        (b"\x1bO\x1b", &[b"\x1bO\x1b"]),
    ];

    /// Reads as they follow each other within 50 ms, and the keys of each poll().
    #[rustfmt::skip]
    const READS: &[(Keys, &[Keys])] = &[
        (&[b"\x1b[A\x1b[", b"B"], &[&[b"\x1b[A", b"\x1b[B"]]),
        (&[b"l\x1b_Gi=31;EBADF:Failed to open file for graphics transmission: /dev/shm/x with error: [2] No such file or directory\x1b\\\x1b[?62;22ck"], &[&[b"l", b"k"]]),
        (&[b"l\x1b_Gi=31;O", b"K\x1b\\\x1b[?62;", b"22ck"], &[&[b"l", b"k"]]),
        (&[b"\x1b"], &[&[b"\x1b"]]),
        (&[b"\x1b", b"[A"], &[&[b"\x1b[A"]]),
        (&[b"\x1b[", b"1;", b"2", b"Cq"], &[&[b"\x1b[1;2C", b"q"]]),
        (&[b"\x1b["], &[&[b"\x1b", b"["]]),
        (&[b"\x1b[1;2"], &[&[b"\x1b", b"[", b"1", b";", b"2"]]),
        (&[b"\x1bO"], &[&[b"\x1b", b"O"]]),
        (&[b"\x1bO", b"P"], &[&[b"\x1bOP"]]),
        (&[b"\x1b_Gi=31;O"], &[&[b"\x1b", b"_", b"G", b"i", b"=", b"3", b"1", b";", b"O"]]),
        (&[b"\x1b[?62;"], &[&[b"\x1b", b"[", b"?", b"6", b"2", b";"]]),
        (&[b"q\x1b_Gi=31;OK\x1b", b"\\k"], &[&[b"q", b"k"]]),
        (&[b"\x1bO\x1b", b"[A"], &[&[b"\x1bO\x1b", b"[", b"A"]]),
        (&[b"ab", b"cd"], &[&[b"a", b"b"], &[b"c", b"d"]]),
        (&[b"\x1b", b"\x1b"], &[&[b"\x1b", b"\x1b"]]),
        (&[b"\x1b_a\x1b", b"b\x1b", b"\\"], &[&[b"\x1b", b"_", b"a", b"\x1b", b"b", b"\x1b", b"\\"]]),
        (&[b"33333\x1b", b"[Bq"], &[&[b"3", b"3", b"3", b"3", b"3", b"\x1b[B", b"q"]]),
    ];

    fn keys(list: &[&[u8]]) -> Vec<Key> {
        list.iter().map(|key| key.to_vec()).collect()
    }

    /// The keys of each `Term::keys` call over scripted reads; a read that is not there is 50 ms without input.
    fn polls(reads: &[&[u8]]) -> Vec<Vec<Key>> {
        let mut reads: VecDeque<Vec<u8>> = reads.iter().map(|read| read.to_vec()).collect();
        let mut batches = Vec::new();
        while let Some(first) = reads.pop_front() {
            batches.push(gather(first, || reads.pop_front()));
        }
        batches
    }

    #[test]
    fn whole_input_is_split_into_keys_and_replies_are_dropped() {
        for (input, want) in WHOLE {
            assert_eq!(split(input, false), (keys(want), &b""[..]), "{input:?}");
        }
    }

    #[test]
    fn input_cut_inside_a_sequence_leaves_it_as_the_tail() {
        for (input, last) in CUT {
            let (whole, tail) = split(input, false);
            // ESC O takes any byte for its third, an ESC too: the keys are whole where the Python pattern still waits
            let waits = ![&b"\x1bO\x1b["[..], b"\x1bO\x1b"].contains(input);
            assert_eq!(!tail.is_empty(), waits, "{input:?}");
            assert!(tail.first().is_none_or(|&byte| byte == ESC), "{input:?}");
            assert_eq!(split(input, true), (keys(last), &b""[..]), "{input:?}");
            assert_eq!(
                [whole, split(tail, true).0].concat(),
                keys(last),
                "{input:?}"
            );
        }
    }

    #[test]
    fn reads_are_joined_while_a_sequence_is_open() {
        for (reads, want) in READS {
            let got = polls(reads);
            let want: Vec<Vec<Key>> = want.iter().map(|batch| keys(batch)).collect();
            assert_eq!(got.concat(), want.concat(), "{reads:?}");
            // the Python pattern waits after ESC O ESC and takes the next read into the same batch
            if reads[0] != b"\x1bO\x1b" {
                assert_eq!(got, want, "{reads:?}");
            }
        }
    }

    #[test]
    fn an_escape_sequence_split_across_reads_is_one_key() {
        assert_eq!(
            polls(&[b"\x1b[A\x1b[", b"B"]),
            [keys(&[b"\x1b[A", b"\x1b[B"])]
        );
    }

    #[test]
    fn terminal_replies_are_no_keys() {
        // kitty's answers to the start-up probe, come after it stopped waiting: whole, and cut by the reads' ends
        let whole: Keys = &[
            b"l\x1b_Gi=31;EBADF:Failed to open file for graphics transmission: /dev/shm/x with error: [2] No such file or directory\x1b\\\x1b[?62;22ck",
        ];
        let cut: Keys = &[b"l\x1b_Gi=31;O", b"K\x1b\\\x1b[?62;", b"22ck"];
        for reads in [whole, cut] {
            assert_eq!(polls(reads), [keys(&[b"l", b"k"])]);
        }
    }

    #[test]
    fn a_lone_escape_is_a_key() {
        assert_eq!(polls(&[b"\x1b"]), [keys(&[b"\x1b"])]);
    }

    #[test]
    fn a_backlog_of_held_keys_is_one_batch() {
        // tests/test_viewer_tty.py:88, :96: 300 level keys and q; zooms, pans and q (704 bytes, 404 keys by the Python pattern)
        let level = [b"3".repeat(300), b"q".to_vec()].concat();
        assert_eq!(polls(&[&level])[0].len(), 301);
        let held = [
            b"+++".to_vec(),
            b"l\x1b[B".repeat(150),
            b"+-".repeat(50),
            b"q".to_vec(),
        ]
        .concat();
        assert_eq!((held.len(), split(&held, false).0.len()), (704, 404));
    }

    #[test]
    fn geometry_from_what_the_terminal_reports() {
        // (rows, cols, x px, y px) of TIOCGWINSZ -> Player.window() of bin/ascii-viewer:1709 with COLUMNS and LINES unset, run by python3; all zeros is also what a failing ioctl leaves
        for ((rows, cols, xpx, ypx), (want_cols, want_rows, cell_w, cell_h)) in [
            ((60, 200, 1800, 1200), (200, 60, 9.0, 20.0)),
            ((30, 100, 1000, 660), (100, 30, 10.0, 22.0)),
            ((24, 80, 0, 0), (80, 24, 9.0, 20.0)),
            ((50, 211, 1905, 1003), (211, 50, 9.028436018957345, 20.06)),
            ((20, 50, 450, 400), (50, 20, 9.0, 20.0)),
            ((0, 0, 0, 0), (80, 24, 9.0, 20.0)),
            ((0, 0, 800, 600), (80, 24, 10.0, 25.0)),
            ((40, 0, 800, 600), (80, 40, 10.0, 15.0)),
            ((0, 120, 800, 600), (120, 24, 6.666666666666667, 25.0)),
            ((40, 120, 800, 0), (120, 40, 9.0, 20.0)),
            ((40, 120, 0, 600), (120, 40, 9.0, 20.0)),
        ] {
            let want = Geometry {
                cols: want_cols,
                rows: want_rows,
                cell_w,
                cell_h,
            };
            assert_eq!(measure(rows, cols, xpx, ypx), want);
        }
    }

    #[test]
    fn a_wake_is_its_place_in_the_table() {
        for (byte, why) in WAKES.into_iter().enumerate() {
            assert_eq!(why as usize, byte);
        }
    }

    #[test]
    fn the_restore_is_the_entry_backwards_after_kittys_clear() {
        assert_eq!(
            RESTORE,
            [kitty::CLEAR, b"\x1b[?25h\x1b[?1049l\x1b[23;2t"].concat()
        );
    }

    #[test]
    fn guard_hands_back_the_value_or_the_panic() {
        assert_eq!(guard(|| 7).ok(), Some(7));
        let inner = guard(|| guard(|| -> u8 { panic!("hush") }).is_err());
        assert_eq!(inner.ok(), Some(true));
        let caught = guard(|| -> u8 { panic!("hush") }).unwrap_err();
        assert_eq!(caught.downcast_ref::<&str>(), Some(&"hush"));
        assert!(!GUARDED.get());
    }

    // The tests below run this test binary again as a child on a pseudo-terminal and play the terminal: real descriptors, modes and signals.

    const SIZE: [u16; 4] = [60, 200, 1800, 1200];
    const ENTER: &[u8] = b"\x1b[22;2t\x1b]2;asciier-test\x07\x1b[?1049h\x1b[?25l";
    const AT_START: &str = "Geometry { cols: 200, rows: 60, cell_w: 9.0, cell_h: 20.0 }";
    /// How long a test waits for the child before it fails; the child answers within milliseconds.
    const PATIENCE: Duration = Duration::from_secs(10);

    fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
        hay.windows(needle.len()).position(|part| part == needle)
    }

    fn count(hay: &[u8], needle: &[u8]) -> usize {
        hay.windows(needle.len())
            .filter(|part| *part == needle)
            .count()
    }

    fn unbase64(text: &[u8]) -> Vec<u8> {
        let digits = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut bytes = Vec::new();
        for group in text.chunks(4) {
            let given = group.iter().filter(|&&digit| digit != b'=').count();
            let bits = group.iter().fold(0u32, |bits, digit| {
                bits << 6 | digits.iter().position(|d| d == digit).unwrap_or(0) as u32
            });
            bytes.extend(&bits.to_be_bytes()[1..given]);
        }
        bytes
    }

    fn window(size: [u16; 4]) -> libc::winsize {
        libc::winsize {
            ws_row: size[0],
            ws_col: size[1],
            ws_xpixel: size[2],
            ws_ypixel: size[3],
        }
    }

    /// The child on its pseudo-terminal; the test holds the terminal's side.
    struct Pty {
        master: Option<OwnedFd>,
        child: Child,
        /// What the child wrote to the terminal.
        seen: Vec<u8>,
        /// What it reported on stderr.
        said: Vec<u8>,
    }

    impl Pty {
        /// A window of `size` (rows, cols, x px, y px) with `typed` waiting as input; `aloud`: the child's stderr goes to the terminal too.
        fn start(size: [u16; 4], typed: &[u8], aloud: bool) -> Pty {
            // close-on-exec from birth: children of other tests must not hold the terminal open
            let open = libc::O_RDWR | libc::O_NOCTTY | libc::O_CLOEXEC;
            let (master, slave) = unsafe {
                let master = libc::posix_openpt(open);
                assert!(master >= 0 && libc::grantpt(master) == 0 && libc::unlockpt(master) == 0);
                let mut name = [0; 64];
                assert_eq!(libc::ptsname_r(master, name.as_mut_ptr(), name.len()), 0);
                let slave = libc::open(name.as_ptr(), open);
                assert!(slave >= 0);
                assert_eq!(libc::ioctl(master, libc::TIOCSWINSZ, &window(size)), 0);
                // before the child exists: it is there when the child enters, whatever the timing
                assert_eq!(
                    libc::write(master, typed.as_ptr().cast(), typed.len()),
                    typed.len() as isize
                );
                (OwnedFd::from_raw_fd(master), OwnedFd::from_raw_fd(slave))
            };
            let child = {
                let mut command = Command::new(std::env::current_exe().unwrap());
                command
                    .args(["--exact", "term::tests::pty_child", "--nocapture"])
                    .env("ASCIIER_PTY_CHILD", "1")
                    .stdin(slave.try_clone().unwrap())
                    .stdout(slave.try_clone().unwrap());
                if aloud {
                    command.stderr(slave);
                } else {
                    command.stderr(Stdio::piped());
                }
                // a session of its own with the pty as its terminal: the terminal's signals reach it
                unsafe {
                    command.pre_exec(|| {
                        libc::setsid();
                        libc::ioctl(0, libc::TIOCSCTTY, 0);
                        Ok(())
                    })
                };
                command.spawn().unwrap()
            };
            Pty {
                master: Some(master),
                child,
                seen: Vec::new(),
                said: Vec::new(),
            }
        }

        fn fd(&self) -> RawFd {
            self.master.as_ref().unwrap().as_raw_fd()
        }

        fn type_keys(&mut self, keys: &[u8]) {
            let n = unsafe { libc::write(self.fd(), keys.as_ptr().cast(), keys.len()) };
            assert_eq!(n, keys.len() as isize);
        }

        fn resize(&mut self, size: [u16; 4]) {
            assert_eq!(
                unsafe { libc::ioctl(self.fd(), libc::TIOCSWINSZ, &window(size)) },
                0
            );
        }

        fn signal(&mut self, signal: libc::c_int) {
            unsafe { libc::kill(self.child.id() as libc::pid_t, signal) };
        }

        /// Read what the child writes for `ms` at most; false once both its streams have ended.
        fn pump(&mut self, ms: i32) -> bool {
            let fds = [
                self.master.as_ref().map_or(-1, |fd| fd.as_raw_fd()),
                self.child.stderr.as_ref().map_or(-1, |fd| fd.as_raw_fd()),
            ];
            if fds == [-1, -1] {
                return false;
            }
            let mut polls = fds.map(|fd| libc::pollfd {
                fd,
                events: libc::POLLIN,
                revents: 0,
            });
            unsafe { libc::poll(polls.as_mut_ptr(), 2, ms) };
            let mut bytes = vec![0u8; 1 << 16];
            for (stream, poll) in polls.iter().enumerate() {
                if poll.revents == 0 {
                    continue;
                }
                let n = unsafe { libc::read(poll.fd, bytes.as_mut_ptr().cast(), bytes.len()) };
                match (n > 0, stream) {
                    (true, 0) => self.seen.extend(&bytes[..n as usize]),
                    (true, _) => self.said.extend(&bytes[..n as usize]),
                    // a pty whose other side is closed fails the read, a pipe ends
                    (false, 0) => self.master = None,
                    (false, _) => self.child.stderr = None,
                }
            }
            true
        }

        /// Wait for `needle` on the terminal (`on_tty`) or in the reports, from `from` on; where it ends.
        fn until(&mut self, on_tty: bool, needle: &[u8], from: usize) -> usize {
            let end = Instant::now() + PATIENCE;
            loop {
                let hay = if on_tty { &self.seen } else { &self.said };
                if let Some(at) = find(&hay[from.min(hay.len())..], needle) {
                    return from + at + needle.len();
                }
                assert!(
                    Instant::now() < end && self.pump(100),
                    "no {:?} in\n{:?}\n{:?}",
                    String::from_utf8_lossy(needle),
                    String::from_utf8_lossy(&self.seen),
                    String::from_utf8_lossy(&self.said)
                );
            }
        }

        /// Wait for a line of the child's report.
        fn says(&mut self, line: &str) -> usize {
            self.until(false, format!("{line}\n").as_bytes(), 0)
        }

        /// The child's end: its status, the terminal's bytes, the reports.
        fn end(mut self) -> (ExitStatus, Vec<u8>, Vec<u8>) {
            let end = Instant::now() + PATIENCE;
            while self.pump(100) {
                assert!(Instant::now() < end, "the child does not end");
            }
            let status = self.child.wait().unwrap();
            (
                status,
                std::mem::take(&mut self.seen),
                std::mem::take(&mut self.said),
            )
        }
    }

    impl Drop for Pty {
        fn drop(&mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }

    /// Not a test: the child of the tests below, with their pty as its terminal. It reports on stderr, acts on the keys it is sent and ends the process itself.
    #[test]
    fn pty_child() {
        if std::env::var_os("ASCIIER_PTY_CHILD").is_none() {
            return;
        }
        // the faults acted out leave no core dump
        unsafe { libc::prctl(libc::PR_SET_DUMPABLE, 0) };
        let mode = || {
            let mut mode: libc::termios = unsafe { std::mem::zeroed() };
            unsafe { libc::tcgetattr(STDIN, &mut mode) };
            mode
        };
        let flags = |mode: libc::termios| {
            let flag = |bit| (mode.c_lflag & bit != 0) as u8;
            format!(
                "icanon {} echo {} isig {} min {} time {}",
                flag(libc::ICANON),
                flag(libc::ECHO),
                flag(libc::ISIG),
                mode.c_cc[libc::VMIN],
                mode.c_cc[libc::VTIME]
            )
        };
        let all = |mode: libc::termios| {
            (
                mode.c_iflag,
                mode.c_oflag,
                mode.c_cflag,
                mode.c_lflag,
                mode.c_cc,
            )
        };
        let found = mode();
        eprintln!("tty {}", is_tty());
        for why in [Wake::Job, Wake::Mpv, Wake::Job] {
            wake(why);
        }
        let mut term = match Term::enter("asciier-test") {
            Ok(term) => term,
            Err(error) => {
                eprintln!("no entry: {}", error.0);
                std::process::exit(0);
            }
        };
        eprintln!("again {}", Term::enter("twice").is_err());
        eprintln!("woken {:?} {:?}", term.wakes(), term.wakes());
        eprintln!("{}", flags(mode()));
        eprintln!("geometry {:?}", term.geometry());
        'life: loop {
            let mut polls = [STDIN, term.wake_fd()].map(|fd| libc::pollfd {
                fd,
                events: libc::POLLIN,
                revents: 0,
            });
            unsafe { libc::poll(polls.as_mut_ptr(), 2, -1) };
            let Some(keys) = term.keys() else {
                eprintln!("gone");
                break;
            };
            if !keys.is_empty() {
                let hex = |key: &Key| key.iter().map(|byte| format!("{byte:02x}")).collect();
                eprintln!(
                    "keys {}",
                    keys.iter().map(hex).collect::<Vec<String>>().join(" ")
                );
            }
            for key in keys {
                match key.as_slice() {
                    b"q" => break 'life,
                    b"W" => term.write(b"written"),
                    b"P" => eprintln!("probe {}", kitty::probe()),
                    b"!" => panic!("boom"),
                    b"G" => {
                        let caught = guard(|| -> u8 { panic!("hush") }).is_err();
                        eprintln!("guard {caught} {}", flags(mode()));
                    }
                    b"S" => unsafe {
                        libc::raise(libc::SIGSEGV);
                    },
                    b"A" => std::process::abort(),
                    b"F" => {
                        (0..100_000).for_each(|_| wake(Wake::Mpv));
                        eprintln!("flood {:?}", term.wakes());
                    }
                    b"D" => {
                        drop(term);
                        eprintln!("dropped {}", flags(mode()));
                        std::process::exit(0);
                    }
                    b"T" => {
                        // keys typed while the program ends: leave once they are there, then count what the terminal holds
                        eprintln!("waiting");
                        ready(STDIN, PATIENCE.as_millis() as i32);
                        term.leave();
                        let mut held: libc::c_int = 0;
                        unsafe { libc::ioctl(STDIN, libc::FIONREAD, &mut held) };
                        eprintln!("held {held}");
                        std::process::exit(0);
                    }
                    _ => {}
                }
            }
            for why in term.wakes() {
                eprintln!("wake {why:?}");
                match why {
                    Wake::Resize => {
                        eprintln!("held {:?}", term.geometry());
                        term.resized();
                        eprintln!("geometry {:?}", term.geometry());
                    }
                    Wake::Quit => break 'life,
                    _ => {}
                }
            }
        }
        term.leave();
        eprintln!(
            "left {} as found {}",
            flags(mode()),
            all(mode()) == all(found)
        );
        term.leave();
        // a signal repeated during the cleanup must not end it
        std::thread::sleep(Duration::from_millis(100));
        eprintln!("alive");
        std::process::exit(0);
    }

    #[test]
    fn enter_and_leave_on_a_terminal() {
        // rows V2, V3, V11, N11
        let mut pty = Pty::start(SIZE, b"typed before", false);
        pty.says("tty true");
        pty.says("again true");
        pty.says("woken [Job, Mpv] []");
        pty.says("icanon 0 echo 0 isig 0 min 1 time 0");
        pty.says(&format!("geometry {AT_START}"));
        // Ctrl-C, Ctrl-Z and Ctrl-\ come as keys, with the rest of one write
        pty.type_keys(b"W3\x03\x1a\x1c\x1b[1;2Aq");
        let (status, seen, said) = pty.end();
        let said = String::from_utf8_lossy(&said);
        assert_eq!(said.matches("keys ").count(), 1, "{said}");
        assert!(
            said.contains("keys 57 33 03 1a 1c 1b5b313b3241 71\n"),
            "{said}"
        );
        assert!(
            said.contains("left icanon 1 echo 1 isig 1 min 1 time 0 as found true\nalive\n"),
            "{said}"
        );
        assert_eq!((count(&seen, ENTER), count(&seen, RESTORE)), (1, 1));
        let entered = find(&seen, ENTER).unwrap();
        assert!(find(&seen[entered..], b"written").is_some() && seen.ends_with(RESTORE));
        assert_eq!(status.code(), Some(0));
    }

    #[test]
    fn keys_typed_while_it_ends_are_left_to_the_shell() {
        // row V2: the mode goes back by TCSADRAIN, which keeps the input not read yet (bin/ascii-viewer:2469 at tag python-final); a flush would drop what was typed while the viewer ended
        let mut pty = Pty::start(SIZE, b"", false);
        pty.says(&format!("geometry {AT_START}"));
        pty.type_keys(b"T");
        pty.says("waiting");
        pty.type_keys(b"ls -l");
        pty.says("held 5");
        let (status, seen, _) = pty.end();
        assert!(count(&seen, RESTORE) == 1 && seen.ends_with(RESTORE));
        assert_eq!(status.code(), Some(0));
    }

    #[test]
    fn a_resize_wakes_and_is_measured_once_told() {
        // rows V9, V10; GeometryTest of tests/test_viewer.py:407: the same two windows
        let mut pty = Pty::start(SIZE, b"", false);
        pty.says(&format!("geometry {AT_START}"));
        pty.resize([30, 100, 1000, 660]);
        let at = pty.says(&format!(
            "wake Resize\nheld {AT_START}\ngeometry Geometry {{ cols: 100, rows: 30, cell_w: 10.0, cell_h: 22.0 }}"
        ));
        pty.resize([24, 80, 0, 0]);
        pty.until(
            false,
            b"geometry Geometry { cols: 80, rows: 24, cell_w: 9.0, cell_h: 20.0 }\n",
            at,
        );
        pty.type_keys(b"Fq");
        let (status, _, said) = pty.end();
        assert!(find(&said, b"flood [Mpv]\n").is_some());
        assert_eq!(status.code(), Some(0));
    }

    #[test]
    fn hup_term_and_int_wake_the_loop_to_quit() {
        // row V5
        for signal in [libc::SIGHUP, libc::SIGTERM, libc::SIGINT] {
            let mut pty = Pty::start(SIZE, b"", false);
            pty.says(&format!("geometry {AT_START}"));
            pty.signal(signal);
            pty.says("wake Quit");
            pty.until(false, b"left ", 0);
            pty.signal(signal);
            let (status, seen, said) = pty.end();
            assert!(find(&said, b"alive\n").is_some());
            assert_eq!((count(&seen, RESTORE), status.code()), (1, Some(0)));
        }
    }

    #[test]
    fn a_fault_restores_the_terminal_and_ends_by_its_signal() {
        for (key, signal) in [(b"S", libc::SIGSEGV), (b"A", libc::SIGABRT)] {
            let mut pty = Pty::start(SIZE, b"", false);
            pty.says(&format!("geometry {AT_START}"));
            pty.type_keys(key);
            let master = pty.master.as_ref().unwrap().try_clone().unwrap();
            let (status, seen, _) = pty.end();
            assert_eq!(status.signal(), Some(signal));
            assert!(count(&seen, RESTORE) == 1 && seen.ends_with(RESTORE));
            // the mode, asked of the terminal itself
            let mut mode: libc::termios = unsafe { std::mem::zeroed() };
            assert_eq!(unsafe { libc::tcgetattr(master.as_raw_fd(), &mut mode) }, 0);
            let cooked = libc::ICANON | libc::ECHO | libc::ISIG;
            assert_eq!(mode.c_lflag & cooked, cooked);
        }
    }

    #[test]
    fn a_panic_restores_the_terminal_before_its_message() {
        // stderr on the terminal: one stream, one order
        let mut pty = Pty::start(SIZE, b"", true);
        pty.until(true, AT_START.as_bytes(), 0);
        pty.type_keys(b"!");
        let (status, seen, _) = pty.end();
        assert_eq!(count(&seen, RESTORE), 1);
        assert!(find(&seen, RESTORE).unwrap() < find(&seen, b"boom").unwrap());
        assert_eq!(status.code(), Some(101));
    }

    #[test]
    fn a_guarded_panic_says_nothing_and_leaves_the_terminal_entered() {
        let mut pty = Pty::start(SIZE, b"", false);
        pty.says(&format!("geometry {AT_START}"));
        pty.type_keys(b"Gq");
        let (status, seen, said) = pty.end();
        assert!(find(&said, b"guard true icanon 0 echo 0 isig 0 ").is_some());
        assert!(find(&said, b"hush").is_none() && find(&seen, b"hush").is_none());
        assert!(count(&seen, RESTORE) == 1 && seen.ends_with(RESTORE));
        assert_eq!(status.code(), Some(0));
    }

    /// The probe's question as the terminal reads it: the file it names.
    fn probed(pty: &mut Pty) -> PathBuf {
        let start = pty.until(true, b"\x1b_Gi=31,s=1,v=1,a=q,t=t,f=24;", 0);
        let end = pty.until(true, b"\x1b\\\x1b[c", start) - 5;
        PathBuf::from(OsString::from_vec(unbase64(&pty.seen[start..end])))
    }

    #[test]
    fn the_probe_is_a_yes_on_ok_before_the_attributes() {
        // row S4; the answers of tests/test_viewer_tty.py:20, :159
        let answers: [(&[&[u8]], bool); 5] = [
            (&[b"\x1b_Gi=31;OK\x1b\\\x1b[?62;22c"], true),
            (
                &[b"\x1b_Gi=31;EBADF:could not read\x1b\\\x1b[?62;22c"],
                false,
            ),
            (&[b"\x1b[?62;22c"], false),
            (&[b"\x1b_Gi=31;O", b"K\x1b\\\x1b[?6", b"2;22c"], true),
            // a key typed meanwhile is lost
            (&[b"d\x1b_Gi=31;OK\x1b\\\x1b[?62;22c"], true),
        ];
        for (answer, want) in answers {
            let mut pty = Pty::start(SIZE, b"", false);
            pty.says(&format!("geometry {AT_START}"));
            pty.type_keys(b"P");
            let file = probed(&mut pty);
            assert!(file.starts_with("/dev/shm/"));
            assert!(
                file.file_name()
                    .unwrap()
                    .as_encoded_bytes()
                    .starts_with(b"tty-graphics-protocol-")
            );
            assert_eq!(std::fs::read(&file).unwrap(), [0, 0, 0]);
            let asked = Instant::now();
            for part in answer {
                pty.type_keys(part);
            }
            pty.says(&format!("probe {want}"));
            // the attributes end the wait
            assert!(asked.elapsed() < Duration::from_millis(900));
            assert!(!file.exists());
            pty.type_keys(b"q");
            let (status, _, said) = pty.end();
            let said = String::from_utf8_lossy(&said);
            assert_eq!(said.matches("keys ").count(), 2, "{said}");
            assert!(
                said.contains("keys 50\n") && said.contains("keys 71\n"),
                "{said}"
            );
            assert_eq!(status.code(), Some(0));
        }
    }

    #[test]
    fn the_probe_gives_up_after_a_second_and_a_late_answer_types_nothing() {
        // rows S4, V12; the late answer of tests/test_viewer_tty.py:22
        let mut pty = Pty::start(SIZE, b"", false);
        pty.says(&format!("geometry {AT_START}"));
        pty.type_keys(b"P");
        let file = probed(&mut pty);
        let asked = Instant::now();
        pty.says("probe false");
        let waited = asked.elapsed();
        assert!(
            waited > Duration::from_millis(900) && waited < Duration::from_millis(1900),
            "{waited:?}"
        );
        assert!(!file.exists());
        let late = b"l\x1b_Gi=31;EBADF:Failed to open file for graphics transmission: /dev/shm/tty-graphics-protocol-x with error: [2] No such file or directory\x1b\\\x1b[?62;22ck";
        pty.type_keys(late);
        pty.says("keys 6c 6b");
        pty.type_keys(b"q");
        assert_eq!(pty.end().0.code(), Some(0));
    }

    #[test]
    fn an_escape_waits_for_its_rest_then_counts_alone() {
        // row V11
        let mut pty = Pty::start(SIZE, b"", false);
        pty.says(&format!("geometry {AT_START}"));
        let typed = Instant::now();
        pty.type_keys(b"\x1b");
        pty.says("keys 1b");
        assert!(typed.elapsed() >= Duration::from_millis(REST as u64));
        // two writes, one key: joined by the same read or by the wait
        pty.type_keys(b"\x1b[");
        pty.type_keys(b"B");
        pty.says("keys 1b5b42");
        pty.type_keys(b"q");
        let (status, _, said) = pty.end();
        assert_eq!(count(&said, b"keys "), 3);
        assert_eq!(status.code(), Some(0));
    }

    #[test]
    fn a_dropped_term_has_left() {
        let mut pty = Pty::start(SIZE, b"", false);
        pty.says(&format!("geometry {AT_START}"));
        pty.type_keys(b"D");
        let (status, seen, said) = pty.end();
        assert!(find(&said, b"dropped icanon 1 echo 1 isig 1 ").is_some());
        assert!(count(&seen, RESTORE) == 1 && seen.ends_with(RESTORE));
        assert_eq!(status.code(), Some(0));
    }

    #[test]
    fn a_terminal_that_is_gone_ends_the_keys() {
        // row V13
        let mut pty = Pty::start(SIZE, b"", false);
        pty.says(&format!("geometry {AT_START}"));
        pty.master = None;
        let (status, _, said) = pty.end();
        assert!(find(&said, b"gone\n").is_some());
        assert_eq!(status.code(), Some(0));
    }

    #[test]
    fn without_a_terminal_nothing_is_entered() {
        let child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "term::tests::pty_child", "--nocapture"])
            .env("ASCIIER_PTY_CHILD", "1")
            .stdin(Stdio::null())
            .output()
            .unwrap();
        let said = String::from_utf8_lossy(&child.stderr);
        assert!(
            said.contains("tty false\n") && said.contains("no entry: stdin is no terminal"),
            "{said}"
        );
        assert!(!child.stdout.contains(&ESC));
        assert_eq!(child.status.code(), Some(0));
    }
}
