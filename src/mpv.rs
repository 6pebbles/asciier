//! libmpv in-process: it decodes, keeps time, seeks, steps and loops; the viewer draws the frames it renders. Driven from one thread with the calls client.h marks safe beside rendering.

use crate::Result;
use image::RgbImage;
use std::path::Path;

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
    /// A new frame is due (`Mpv::render`).
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
    /// The end of the file was reached (mpv pauses there: `keep-open`).
    Ended(bool),
    /// The picture's display size and the clockwise turn (0, 90, 180, 270) the stream asks for.
    Shape {
        w: u32,
        h: u32,
        turn: u32,
    },
    /// The frame asked for with `grab`, upright, at source size; `None` when mpv had none.
    Grabbed(Option<RgbImage>),
}

pub struct Mpv {
    handle: *mut std::ffi::c_void,
    render: *mut std::ffi::c_void,
}

impl Mpv {
    /// libmpv with the viewer's options (design section 6), the audio options of `~/.config/mpv/mpv.conf`, a software render context, wake-ups through `term::wake(Wake::Mpv)`; `node`: the PipeWire node name (`PIPEWIRE_PROPS` in the own environment).
    pub fn new(node: &str) -> Result<Mpv> {
        todo!("{node}")
    }

    /// Open `file` (an absolute path) paused and wait for `Loaded` or `Failed`, at most `seconds`.
    pub fn open(&mut self, file: &Path, seconds: f64) -> Result<(bool, bool)> {
        todo!("{file:?} {seconds}")
    }

    /// Audio alone: give up the render context.
    pub fn no_video(&mut self) {
        todo!()
    }

    /// An mpv command, not waited for (`["seek", "5", "relative+exact"]`).
    pub fn command(&self, args: &[&str]) {
        todo!("{args:?}")
    }

    /// Set a property, not waited for.
    pub fn set(&self, name: &str, value: &str) {
        todo!("{name} {value}")
    }

    /// Everything mpv reported since the last call; never blocks.
    pub fn events(&mut self) -> Vec<Event> {
        todo!()
    }

    /// The current frame as packed RGB, upright, scaled to exactly `w` x `h` (aspect not kept: the caller chose the size).
    pub fn render(&mut self, w: u32, h: u32) -> Option<Vec<u8>> {
        todo!("{w} {h}")
    }

    /// Tell mpv the due frame was not drawn.
    pub fn skip(&mut self) {
        todo!()
    }

    /// Ask for the frame mpv holds, at source size; it arrives as `Event::Grabbed`.
    pub fn grab(&self) {
        todo!()
    }
}

impl Drop for Mpv {
    fn drop(&mut self) {
        // render context first, then the core
    }
}
