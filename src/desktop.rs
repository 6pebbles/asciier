//! The desktop around the viewer: the oscilloscope child, the clipboard, Hyprland, kitty's remote control.

use std::path::Path;
use std::process::Child;

/// Toggle Hyprland fullscreen on the window owning this process (row V22); silent on any failure.
pub fn fullscreen() {
    todo!()
}

/// The focused monitor's size in px, from `hyprctl monitors -j` (row R9).
pub fn screen_size() -> Option<(u32, u32)> {
    todo!()
}

/// kitty ligatures for this window, off while the viewer draws (row V4); silent without kitty's socket.
pub fn ligatures(off: bool) {
    todo!("{off}")
}

/// PNG bytes to the Wayland clipboard: `wl-copy --type image/png`, in its own session so it outlives the viewer; whether it took them.
pub fn copy_png(png: &[u8]) -> bool {
    todo!("{}", png.len())
}

/// Show an image in kitty (`kitten icat`), the converter's `--at` on a kitty terminal.
pub fn icat(file: &Path) {
    todo!("{file:?}")
}

/// The oscilloscope that owns the screen for audio alone (rows A1, A6).
pub struct Scope {
    child: Option<Child>,
}

impl Scope {
    /// Start `scope` (beside the executable, else on PATH) recording the node `target`, one spare row, stdin /dev/null, `PIPEWIRE_PROPS` removed, dying with this process; a missing program gives a scope that does nothing.
    pub fn start(target: &str) -> Scope {
        todo!("{target}")
    }

    /// SIGSTOP it and wait until it is stopped (100 ms at most); false when it is not running.
    pub fn stop(&mut self) -> bool {
        todo!()
    }

    /// SIGCONT.
    pub fn resume(&mut self) {
        todo!()
    }

    /// SIGCONT, SIGTERM, reap.
    pub fn end(&mut self) {
        todo!()
    }
}
