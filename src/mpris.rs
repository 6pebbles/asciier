//! MPRIS on the session bus, served by the loop thread: `playerctl` and media keys drive the player.

use dbus::arg::{PropMap, RefArg, Variant};
use dbus::blocking::stdintf::org_freedesktop_dbus::RequestNameReply;
use dbus::channel::{BusType, Channel};
use dbus_crossroads::{Crossroads, IfaceBuilder};
use std::os::fd::RawFd;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

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

const PATH: &str = "/org/mpris/MediaPlayer2";

/// What the object holds: the state its properties answer from, and the calls not handed on yet.
struct Served {
    state: State,
    requests: Vec<Request>,
}

/// `file:` and the path, every byte but letters, digits and `_ . - ~ /` as `%XX` (as the old viewer's URLs).
fn uri(file: &Path) -> String {
    let mut uri = String::from("file://");
    for &byte in file.as_os_str().as_bytes() {
        if byte.is_ascii_alphanumeric() || b"_.-~/".contains(&byte) {
            uri.push(byte as char);
        } else {
            uri += &format!("%{byte:02X}");
        }
    }
    uri
}

fn metadata(state: &State) -> PropMap {
    let name = state.file.file_name().unwrap_or_default().to_string_lossy();
    // the file itself, not a link to it
    let file = std::fs::canonicalize(&state.file).unwrap_or_else(|_| state.file.clone());
    let track = dbus::Path::from(format!("{PATH}/Track/1"));
    let mut data: Vec<(&str, Box<dyn RefArg>)> = vec![
        ("mpris:trackid", Box::new(track)),
        ("xesam:title", Box::new(name.into_owned())),
        ("xesam:url", Box::new(uri(&file))),
    ];
    if let Some(length) = state.length {
        data.push(("mpris:length", Box::new((length * 1e6) as i64)));
    }
    data.into_iter()
        .map(|(key, value)| (key.to_string(), Variant(value)))
        .collect()
}

/// The object: the two MPRIS interfaces (rows M2, M3) over what they answer from.
fn object() -> Crossroads {
    let mut crossroads = Crossroads::new();
    let root = crossroads.register("org.mpris.MediaPlayer2", |b: &mut IfaceBuilder<Served>| {
        b.method("Raise", (), (), |_, _, ()| Ok(()));
        b.method("Quit", (), (), |_, served, ()| {
            served.requests.push(Request::Quit);
            Ok(())
        });
        for (name, value) in [
            ("CanQuit", true),
            ("CanRaise", false),
            ("HasTrackList", false),
        ] {
            b.property(name).get(move |_, _| Ok(value));
        }
        b.property("Identity")
            .get(|_, _| Ok(crate::NAME.to_string()));
        b.property("SupportedUriSchemes")
            .get(|_, _| Ok(vec!["file".to_string()]));
        b.property("SupportedMimeTypes")
            .get(|_, _| Ok(Vec::<String>::new()));
    });
    let player = crossroads.register(
        "org.mpris.MediaPlayer2.Player",
        |b: &mut IfaceBuilder<Served>| {
            // Next is taken and does nothing
            let plain = [
                ("PlayPause", Some(Request::PlayPause)),
                ("Play", Some(Request::Play)),
                ("Pause", Some(Request::Pause)),
                ("Previous", Some(Request::Previous)),
                ("Stop", Some(Request::Stop)),
                ("Next", None),
            ];
            for (name, request) in plain {
                b.method(name, (), (), move |_, served, ()| {
                    served.requests.extend(request);
                    Ok(())
                });
            }
            b.method("Seek", ("Offset",), (), |_, served, (offset,): (i64,)| {
                served.requests.push(Request::Seek(offset));
                Ok(())
            });
            b.method(
                "SetPosition",
                ("TrackId", "Position"),
                (),
                |_, served, (_, position): (dbus::Path, i64)| {
                    served.requests.push(Request::SetPosition(position));
                    Ok(())
                },
            );
            b.method("OpenUri", ("Uri",), (), |_, _, (_,): (String,)| Ok(()));
            b.property("PlaybackStatus")
                .get(|_, served| Ok(served.state.status.to_string()));
            b.property("LoopStatus")
                .get(|_, served| Ok(["None", "Track"][served.state.looping as usize].to_string()));
            b.property("Rate").get(|_, served| Ok(served.state.rate));
            b.property("Metadata")
                .get(|_, served| Ok(metadata(&served.state)));
            // 1.0 while unknown or 0
            b.property("Volume").get(|_, served| {
                let volume = served.state.volume.filter(|volume| *volume != 0.0);
                Ok(volume.unwrap_or(100.0) / 100.0)
            });
            b.property("Position")
                .get(|_, served| Ok((served.state.position * 1e6) as i64));
            for (name, value) in [("MinimumRate", 0.25), ("MaximumRate", 4.0)] {
                b.property(name).get(move |_, _| Ok(value));
            }
            let fixed = [
                ("Shuffle", false),
                ("CanGoNext", false),
                ("CanGoPrevious", true),
                ("CanPlay", true),
                ("CanPause", true),
                ("CanSeek", true),
                ("CanControl", true),
            ];
            for (name, value) in fixed {
                b.property(name).get(move |_, _| Ok(value));
            }
        },
    );
    let state = State {
        status: "Stopped",
        position: 0.0,
        length: None,
        rate: 1.0,
        volume: None,
        looping: false,
        file: Default::default(),
    };
    let requests = Vec::new();
    crossroads.insert(PATH, &[root, player], Served { state, requests });
    crossroads
}

impl Mpris {
    /// Own `org.mpris.MediaPlayer2.asciier.instance<pid>` and export `/org/mpris/MediaPlayer2`; `None` without a session bus.
    pub fn start() -> Option<Mpris> {
        // the address of this moment: libdbus itself keeps the first one it read for the whole run
        let mut channel = match std::env::var("DBUS_SESSION_BUS_ADDRESS") {
            Ok(address) => Channel::open_private(&address).ok()?,
            Err(_) => Channel::get_private(BusType::Session).ok()?,
        };
        if channel.unique_name().is_none() {
            channel.register().ok()?;
        }
        channel.set_watch_enabled(true);
        let connection: dbus::blocking::Connection = channel.into();
        let (name, pid) = (crate::NAME, std::process::id());
        let name = format!("org.mpris.MediaPlayer2.{name}.instance{pid}");
        let owned = connection.request_name(name, false, false, true).ok()?;
        let crossroads = object();
        (owned == RequestNameReply::PrimaryOwner).then_some(Mpris {
            connection,
            crossroads,
        })
    }

    /// The bus connection's descriptor, for the loop's poll; -1 (which poll passes over) once the bus is gone.
    pub fn fd(&self) -> RawFd {
        let channel = self.connection.channel();
        if channel.is_connected() {
            channel.watch().fd
        } else {
            -1
        }
    }

    /// Answer what is waiting: property reads from `state`, method calls at once with an empty reply; the calls come back as requests.
    pub fn serve(&mut self, state: &State) -> Vec<Request> {
        let (channel, path) = (self.connection.channel(), dbus::Path::from(PATH));
        if let Some(served) = self.crossroads.data_mut::<Served>(&path) {
            served.state.clone_from(state);
        }
        // nothing to read is no error; an error is a bus that is gone
        let _ = channel.read_write(Some(std::time::Duration::ZERO));
        while let Some(message) = channel.pop_message() {
            // what is no call (the bus's own signals) needs no answer
            let _ = self.crossroads.handle_message(message, channel);
        }
        channel.flush();
        let served = self.crossroads.data_mut::<Served>(&path);
        served
            .map(|served| std::mem::take(&mut served.requests))
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mpv::tests::{Scratch, Var, env};
    use std::io::{BufRead, BufReader};
    use std::os::unix::process::CommandExt;
    use std::process::{Child, Command, Stdio};
    use std::sync::MutexGuard;
    use std::time::{Duration, Instant};

    const PLAYER: &str = "org.mpris.MediaPlayer2.Player";

    /// A bus of its own for one test: no service files, so nothing can be started through it.
    struct Bus {
        daemon: Child,
        address: String,
        /// The service, and how often its descriptor showed something to read.
        mpris: Option<Mpris>,
        readable: usize,
        dir: Scratch,
        _address: Var,
        _env: MutexGuard<'static, ()>,
    }

    impl Bus {
        fn new() -> Bus {
            let _env = env();
            let dir = Scratch::new();
            let socket = dir.0.join("bus");
            assert!(
                socket.as_os_str().len() < 100,
                "the temp dir is too deep for a socket: {socket:?}"
            );
            let conf = format!(
                "<busconfig><type>session</type><listen>unix:path={}</listen><policy context=\"default\">\
                 <allow send_destination=\"*\" eavesdrop=\"true\"/><allow eavesdrop=\"true\"/><allow own=\"*\"/></policy></busconfig>",
                socket.display()
            );
            std::fs::write(dir.0.join("bus.conf"), conf).unwrap();
            let mut daemon = Command::new("dbus-daemon");
            daemon.arg(format!(
                "--config-file={}",
                dir.0.join("bus.conf").display()
            ));
            daemon
                .args(["--nofork", "--print-address"])
                .stdin(Stdio::null())
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
            let _address = Var::set("DBUS_SESSION_BUS_ADDRESS", &address);
            let mpris = Mpris::start();
            Bus {
                daemon,
                address,
                mpris,
                readable: 0,
                dir,
                _address,
                _env,
            }
        }

        fn name() -> String {
            format!(
                "org.mpris.MediaPlayer2.asciier.instance{}",
                std::process::id()
            )
        }

        /// Run a client on this bus while the service serves `state`: what it printed, whether it succeeded, what was asked of the player.
        fn run(
            &mut self,
            state: &State,
            program: &str,
            args: &[&str],
        ) -> (String, bool, Vec<Request>) {
            let mut client = Command::new(program);
            client
                .args(args)
                .env("DBUS_SESSION_BUS_ADDRESS", &self.address)
                .stdin(Stdio::null());
            let mut client = client
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap();
            let mpris = self.mpris.as_mut().unwrap();
            let (start, mut requests) = (Instant::now(), Vec::new());
            while client.try_wait().unwrap().is_none() {
                assert!(
                    start.elapsed() < Duration::from_secs(10),
                    "{program} {args:?} does not end"
                );
                let mut bus = libc::pollfd {
                    fd: mpris.fd(),
                    events: libc::POLLIN,
                    revents: 0,
                };
                // SAFETY: one descriptor, 5 ms at most.
                self.readable += (unsafe { libc::poll(&mut bus, 1, 5) } > 0) as usize;
                requests.extend(mpris.serve(state));
            }
            requests.extend(mpris.serve(state));
            let output = client.wait_with_output().unwrap();
            (
                String::from_utf8_lossy(&output.stdout).trim().to_string(),
                output.status.success(),
                requests,
            )
        }

        fn playerctl(&mut self, state: &State, args: &[&str]) -> (String, Vec<Request>) {
            let (said, _, requests) = self.run(state, "playerctl", args);
            (said, requests)
        }

        /// A property as busctl prints it: its signature, then its value.
        fn property(&mut self, state: &State, interface: &str, name: &str) -> String {
            let address = format!("--address={}", self.address);
            let (said, ok, requests) = self.run(
                state,
                "busctl",
                &[
                    &address,
                    "get-property",
                    &Bus::name(),
                    PATH,
                    interface,
                    name,
                ],
            );
            assert!(ok && requests.is_empty(), "{name}: {said}");
            said
        }

        /// A call by dbus-send: the reply's lines and what was asked of the player.
        fn call(
            &mut self,
            state: &State,
            method: &str,
            args: &[&str],
        ) -> (Vec<String>, Vec<Request>) {
            let (bus, to) = (
                format!("--bus={}", self.address),
                format!("--dest={}", Bus::name()),
            );
            let mut all = vec![bus.as_str(), "--print-reply", to.as_str(), PATH, method];
            all.extend(args);
            let (said, ok, requests) = self.run(state, "dbus-send", &all);
            assert!(ok, "{method}: {said}");
            (said.lines().map(str::to_string).collect(), requests)
        }
    }

    impl Drop for Bus {
        fn drop(&mut self) {
            self.mpris = None;
            let _ = self.daemon.kill();
            let _ = self.daemon.wait();
        }
    }

    fn state(file: &str) -> State {
        State {
            status: "Playing",
            position: 5.5,
            length: Some(12.25),
            rate: 1.5,
            volume: Some(40.0),
            looping: false,
            file: file.into(),
        }
    }

    #[test]
    fn a_path_as_file_uri() {
        // the oracle: Python's Path(p).as_uri(), which the old viewer served
        for (path, expected) in [
            ("/tmp/clip.mp4", "file:///tmp/clip.mp4"),
            (
                "/media/a b/č#1 &'\".mp4",
                "file:///media/a%20b/%C4%8D%231%20%26%27%22.mp4",
            ),
            (
                "/x/100%_~.-/[v](2);=+,@!$*:?.mkv",
                "file:///x/100%25_~.-/%5Bv%5D%282%29%3B%3D%2B%2C%40%21%24%2A%3A%3F.mkv",
            ),
            ("/x/日本語.mp3", "file:///x/%E6%97%A5%E6%9C%AC%E8%AA%9E.mp3"),
        ] {
            assert_eq!(uri(Path::new(path)), expected);
        }
        let odd = std::ffi::OsStr::from_bytes(b"/x/\xff.mp4");
        assert_eq!(uri(Path::new(odd)), "file:///x/%FF.mp4");
    }

    #[test]
    fn without_a_bus_there_is_no_service() {
        // row M6
        let _env = env();
        let dir = Scratch::new();
        let _address = Var::set(
            "DBUS_SESSION_BUS_ADDRESS",
            format!("unix:path={}", dir.0.join("none").display()),
        );
        assert!(Mpris::start().is_none());
    }

    #[test]
    fn a_bus_that_goes_leaves_the_service_quiet() {
        let mut bus = Bus::new();
        let playing = state("/media/clip.mp4");
        assert!(bus.mpris.as_ref().unwrap().fd() >= 0);
        bus.daemon.kill().unwrap();
        bus.daemon.wait().unwrap();
        let mpris = bus.mpris.as_mut().unwrap();
        assert_eq!(mpris.serve(&playing), []);
        assert_eq!(mpris.fd(), -1, "poll passes over it");
        assert_eq!(mpris.serve(&playing), []);
    }

    #[test]
    fn playerctl_drives_the_player() {
        let mut bus = Bus::new();
        let playing = state("/media/clip one.mp4");
        // rows M1, M1a: what the media keys run
        assert_eq!(
            bus.playerctl(&playing, &["-l"]).0,
            format!("asciier.instance{}", std::process::id())
        );
        for (asked, expected) in [
            (&["play-pause"][..], vec![Request::PlayPause]),
            (&["position", "5+"], vec![Request::Seek(5_000_000)]),
            (&["position", "5-"], vec![Request::Seek(-5_000_000)]),
            (&["previous"], vec![Request::Previous]),
            (&["next"], vec![]),
            (&["stop"], vec![Request::Stop]),
            (&["play"], vec![Request::Play]),
            (&["pause"], vec![Request::Pause]),
            (&["position", "30"], vec![Request::SetPosition(30_000_000)]),
        ] {
            assert_eq!(
                bus.playerctl(&playing, asked),
                (String::new(), expected),
                "playerctl {asked:?}"
            );
        }
        assert!(
            bus.readable > 5,
            "the descriptor shows the calls: {}",
            bus.readable
        );
        // what it reads follows the state of the moment; playerctl itself counts on while playing and shows 0 when stopped
        for (status, position, shown) in [
            ("Paused", 71.123456, 71.123456),
            ("Playing", 5.5, 5.5),
            ("Stopped", 5.5, 0.0),
        ] {
            let state = State {
                status,
                position,
                ..playing.clone()
            };
            assert_eq!(bus.playerctl(&state, &["status"]).0, status);
            let read: f64 = bus.playerctl(&state, &["position"]).0.parse().unwrap();
            assert!(
                read >= shown && read < shown + if status == "Playing" { 0.1 } else { 1e-9 },
                "{status}: {read}"
            );
        }
        assert_eq!(
            bus.playerctl(&playing, &["metadata", "xesam:title"]).0,
            "clip one.mp4"
        );
        assert_eq!(
            bus.playerctl(&playing, &["metadata", "xesam:url"]).0,
            "file:///media/clip%20one.mp4"
        );
        assert_eq!(
            bus.playerctl(&playing, &["metadata", "mpris:length"]).0,
            "12250000"
        );
        assert_eq!(
            bus.playerctl(&playing, &["metadata", "mpris:trackid"]).0,
            "'/org/mpris/MediaPlayer2/Track/1'"
        );
        assert_eq!(bus.playerctl(&playing, &["volume"]).0, "0.400000");
        assert_eq!(bus.playerctl(&playing, &["loop"]).0, "None");
        let looping = State {
            looping: true,
            length: None,
            ..playing.clone()
        };
        assert_eq!(bus.playerctl(&looping, &["loop"]).0, "Track");
        assert_eq!(
            bus.playerctl(&looping, &["metadata", "mpris:length"]).0,
            "",
            "no length while unknown"
        );
    }

    #[test]
    fn properties_answer_from_the_state() {
        // row M3
        let mut bus = Bus::new();
        let playing = state("/media/clip one.mp4");
        for (name, expected) in [
            ("CanQuit", "b true"),
            ("CanRaise", "b false"),
            ("HasTrackList", "b false"),
            ("Identity", "s \"asciier\""),
            ("SupportedUriSchemes", "as 1 \"file\""),
            ("SupportedMimeTypes", "as 0"),
        ] {
            assert_eq!(
                bus.property(&playing, "org.mpris.MediaPlayer2", name),
                expected,
                "{name}"
            );
        }
        for (name, expected) in [
            ("PlaybackStatus", "s \"Playing\""),
            ("LoopStatus", "s \"None\""),
            ("Rate", "d 1.5"),
            ("Shuffle", "b false"),
            ("Volume", "d 0.4"),
            ("Position", "x 5500000"),
            ("MinimumRate", "d 0.25"),
            ("MaximumRate", "d 4"),
            ("CanGoNext", "b false"),
            ("CanGoPrevious", "b true"),
            ("CanPlay", "b true"),
            ("CanPause", "b true"),
            ("CanSeek", "b true"),
            ("CanControl", "b true"),
        ] {
            assert_eq!(bus.property(&playing, PLAYER, name), expected, "{name}");
        }
        let metadata = bus.property(&playing, PLAYER, "Metadata");
        assert!(metadata.starts_with("a{sv} 4 "), "{metadata}");
        for entry in [
            "\"mpris:trackid\" o \"/org/mpris/MediaPlayer2/Track/1\"",
            "\"xesam:title\" s \"clip one.mp4\"",
            "\"xesam:url\" s \"file:///media/clip%20one.mp4\"",
            "\"mpris:length\" x 12250000",
        ] {
            assert!(metadata.contains(entry), "{entry} in {metadata}");
        }
        // volume / 100, 1.0 while unknown or 0; no length while unknown; the loop as Track
        for (volume, expected) in [(None, "d 1"), (Some(0.0), "d 1"), (Some(130.0), "d 1.3")] {
            let state = State {
                volume,
                ..playing.clone()
            };
            assert_eq!(bus.property(&state, PLAYER, "Volume"), expected);
        }
        let other = State {
            length: None,
            looping: true,
            status: "Stopped",
            ..playing.clone()
        };
        assert!(
            bus.property(&other, PLAYER, "Metadata")
                .starts_with("a{sv} 3 ")
        );
        assert_eq!(bus.property(&other, PLAYER, "LoopStatus"), "s \"Track\"");
        assert_eq!(
            bus.property(&other, PLAYER, "PlaybackStatus"),
            "s \"Stopped\""
        );
        // the url names the file itself, not a link to it
        std::fs::write(bus.dir.0.join("real file.mp3"), "").unwrap();
        std::os::unix::fs::symlink(bus.dir.0.join("real file.mp3"), bus.dir.0.join("link.mp3"))
            .unwrap();
        let linked = State {
            file: bus.dir.0.join("link.mp3"),
            ..playing.clone()
        };
        let metadata = bus.property(&linked, PLAYER, "Metadata");
        let real = std::fs::canonicalize(&bus.dir.0).unwrap();
        assert!(
            metadata.contains(&format!("s \"file://{}/real%20file.mp3\"", real.display())),
            "{metadata}"
        );
        assert!(
            metadata.contains("\"xesam:title\" s \"link.mp3\""),
            "{metadata}"
        );
        // all read-only: nothing can be set, and nothing is asked of the player
        let address = format!("--address={}", bus.address);
        let set = [
            &address,
            "set-property",
            &Bus::name(),
            PATH,
            PLAYER,
            "Volume",
            "d",
            "0.5",
        ];
        let (_, ok, requests) = bus.run(&playing, "busctl", &set);
        assert_eq!((ok, requests), (false, vec![]));
    }

    #[test]
    fn calls_are_answered_at_once_with_nothing() {
        // row M2
        let mut bus = Bus::new();
        let playing = state("/media/clip.mp4");
        for (method, args, expected) in [
            ("org.mpris.MediaPlayer2.Quit", &[][..], vec![Request::Quit]),
            ("org.mpris.MediaPlayer2.Raise", &[], vec![]),
            (
                "org.mpris.MediaPlayer2.Player.PlayPause",
                &[],
                vec![Request::PlayPause],
            ),
            (
                "org.mpris.MediaPlayer2.Player.Play",
                &[],
                vec![Request::Play],
            ),
            (
                "org.mpris.MediaPlayer2.Player.Pause",
                &[],
                vec![Request::Pause],
            ),
            (
                "org.mpris.MediaPlayer2.Player.Stop",
                &[],
                vec![Request::Stop],
            ),
            (
                "org.mpris.MediaPlayer2.Player.Previous",
                &[],
                vec![Request::Previous],
            ),
            ("org.mpris.MediaPlayer2.Player.Next", &[], vec![]),
            (
                "org.mpris.MediaPlayer2.Player.Seek",
                &["int64:-7000000"],
                vec![Request::Seek(-7_000_000)],
            ),
            (
                "org.mpris.MediaPlayer2.Player.SetPosition",
                &["objpath:/not/a/track", "int64:9000000"],
                vec![Request::SetPosition(9_000_000)],
            ),
            (
                "org.mpris.MediaPlayer2.Player.OpenUri",
                &["string:file:///x.mp4"],
                vec![],
            ),
        ] {
            let (reply, requests) = bus.call(&playing, method, args);
            assert_eq!(requests, expected, "{method}");
            assert!(
                reply.len() == 1 && reply[0].starts_with("method return "),
                "{method}: an empty reply: {reply:?}"
            );
        }
        // every property in one answer, and what the object says of itself
        let all = ["string:org.mpris.MediaPlayer2.Player"];
        let (reply, requests) = bus.call(&playing, "org.freedesktop.DBus.Properties.GetAll", &all);
        assert_eq!(
            (
                reply
                    .iter()
                    .filter(|line| line.trim() == "dict entry(")
                    .count(),
                requests
            ),
            (15 + 4, vec![]),
            "{reply:?}"
        );
        let (reply, _) = bus.call(
            &playing,
            "org.freedesktop.DBus.Introspectable.Introspect",
            &[],
        );
        let described = reply.join("\n");
        for part in [
            "<method name=\"Seek\">\n      <arg name=\"Offset\" type=\"x\" direction=\"in\"/>",
            "<arg name=\"TrackId\" type=\"o\" direction=\"in\"/>\n      <arg name=\"Position\" type=\"x\" direction=\"in\"/>",
            "<arg name=\"Uri\" type=\"s\" direction=\"in\"/>",
            "<property name=\"Metadata\" type=\"a{sv}\" access=\"read\"/>",
            "<property name=\"Position\" type=\"x\" access=\"read\"/>",
            "<property name=\"SupportedMimeTypes\" type=\"as\" access=\"read\"/>",
        ] {
            assert!(described.contains(part), "{part} in {described}");
        }
        assert_eq!(described.matches("<property ").count(), 15 + 6);
        assert_eq!(
            described.matches("access=\"read\"").count(),
            15 + 6,
            "all read-only"
        );
        // row M4: no signal but the one the standard properties interface declares
        assert_eq!(
            (
                described.matches("<signal ").count(),
                described
                    .matches("<signal name=\"PropertiesChanged\">")
                    .count()
            ),
            (1, 1)
        );
    }
}
