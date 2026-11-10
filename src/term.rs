//! The terminal: raw mode and its restore on every way out, geometry, key tokens, the wake pipe, kitty graphics.

use crate::Result;
use std::os::fd::RawFd;

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

/// Whether stdin and stdout are terminals.
pub fn is_tty() -> bool {
    todo!()
}

/// Wake the loop: one byte into the pipe. Async-signal-safe; callable from any thread and from signal handlers.
pub fn wake(why: Wake) {
    todo!("{why:?}")
}

pub struct Term {
    geometry: Option<Geometry>,
}

impl Term {
    /// Enter: cbreak without ISIG, title pushed and set to `title`, alternate screen, cursor hidden; panic hook and handlers installed (SIGWINCH, SIGHUP, SIGTERM, SIGINT wake the loop; SIGABRT and SIGSEGV restore the terminal and re-raise). Input typed before is discarded.
    pub fn enter(title: &str) -> Result<Term> {
        todo!("{title}")
    }

    /// Restore everything `enter` changed; safe to call twice.
    pub fn leave(&mut self) {
        todo!()
    }

    /// The window's geometry (row V9 without the COLUMNS / LINES override): one snapshot until `resized`.
    pub fn geometry(&mut self) -> Geometry {
        todo!()
    }

    /// Forget the snapshot (after SIGWINCH).
    pub fn resized(&mut self) {
        self.geometry = None;
    }

    pub fn write(&mut self, bytes: &[u8]) {
        todo!("{}", bytes.len())
    }

    /// The read end of the wake pipe, for the loop's poll.
    pub fn wake_fd(&self) -> RawFd {
        todo!()
    }

    /// The reasons waiting in the pipe, each once.
    pub fn wakes(&mut self) -> Vec<Wake> {
        todo!()
    }

    /// The keys waiting on stdin (rows V11, V12): everything pending in one read, a sequence cut by the read completed within 50 ms, terminal replies dropped. `None`: the terminal is gone.
    pub fn keys(&mut self) -> Option<Vec<Key>> {
        todo!()
    }
}

/// Split input bytes into keys, dropping terminal replies (APC strings, `CSI ?` reports); the second value is the unfinished tail (an escape sequence cut short, or a lone Esc).
pub fn tokens(data: &[u8]) -> (Vec<Key>, &[u8]) {
    todo!("{}", data.len())
}

pub mod kitty {
    //! The kitty graphics protocol as the viewer uses it (rows S4, K3-K5).

    use super::Geometry;

    /// Deletes every image this window shows.
    pub const CLEAR: &[u8] = b"\x1b_Ga=d,d=A,q=2\x1b\\";

    /// Whether the environment says kitty: `KITTY_WINDOW_ID` non-empty or `TERM=xterm-kitty`.
    pub fn named() -> bool {
        todo!()
    }

    /// Whether the terminal reads a file from /dev/shm (`a=q,t=t`), asked on stdout, answered on stdin within 1 s (row S4). Needs cbreak mode.
    pub fn probe() -> bool {
        todo!()
    }

    /// The files pixels travel in: /dev/shm/tty-graphics-protocol-<random>, one at a time.
    #[derive(Default)]
    pub struct Transfer {
        last: Option<std::path::PathBuf>,
    }

    impl Transfer {
        /// The escape bytes that show `w` x `h` raw pixels (`depth` 24 or 32 bits) centred in the box of `cols` x `rows` cells whose top left cell is (`col`, `row`), under the text (row K3); the previous file is removed first.
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
            todo!(
                "{w} {h} {} {depth} {col} {row} {cols} {rows} {geometry:?}",
                pixels.len()
            )
        }

        /// Remove the last file (kitty has read it by now; another terminal never did).
        pub fn clear(&mut self) {
            todo!()
        }
    }
}
