//! MPRIS on the session bus, served by the loop thread: `playerctl` and media keys drive the player.

use std::os::fd::RawFd;

/// What a caller asked for (row M2).
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Request {
    PlayPause,
    Play,
    Pause,
    /// Relative, microseconds.
    Seek(i64),
    /// Absolute, microseconds.
    SetPosition(i64),
    Previous,
    Stop,
    Quit,
}

/// What property reads answer from (row M3).
#[derive(Clone, PartialEq, Debug)]
pub struct State {
    /// `Playing`, `Paused` or `Stopped`.
    pub status: &'static str,
    pub position: f64,
    pub length: Option<f64>,
    pub rate: f64,
    pub volume: Option<f64>,
    pub looping: bool,
    pub file: std::path::PathBuf,
}

pub struct Mpris {
    connection: dbus::blocking::Connection,
    crossroads: dbus_crossroads::Crossroads,
}

impl Mpris {
    /// Own `org.mpris.MediaPlayer2.asciier.instance<pid>` and export `/org/mpris/MediaPlayer2`; `None` without a session bus.
    pub fn start() -> Option<Mpris> {
        todo!()
    }

    /// The bus connection's descriptor, for the loop's poll.
    pub fn fd(&self) -> RawFd {
        todo!()
    }

    /// Answer what is waiting: property reads from `state`, method calls at once with an empty reply; the calls come back as requests.
    pub fn serve(&mut self, state: &State) -> Vec<Request> {
        todo!("{state:?}")
    }
}
