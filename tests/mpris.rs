//! MPRIS on a session bus of the test's own: the name, the properties, the methods, playerctl as the media keys run it, the stopped state, requests under the render page and under the help, no signals, no bus (inventory section 4, P8a).

mod common;

use common::screen::Screen;
use common::*;
use serde_json::{Value, json};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

const PATH: &str = "/org/mpris/MediaPlayer2";
const ROOT: &str = "org.mpris.MediaPlayer2";
const PLAYER: &str = "org.mpris.MediaPlayer2.Player";
const SHIFT_RIGHT: &[u8] = b"\x1b[1;2C";

/// The viewer as a client of the test's bus sees it.
struct Remote<'a> {
    sandbox: &'a Sandbox,
    bus: &'a Bus,
    /// Its name on the bus.
    name: String,
}

impl<'a> Remote<'a> {
    /// The viewer `pid` on `bus`, once its name is there.
    fn new(sandbox: &'a Sandbox, bus: &'a Bus, pid: u32) -> Remote<'a> {
        let remote = Remote {
            sandbox,
            bus,
            name: format!("{ROOT}.asciier.instance{pid}"),
        };
        remote.await_name(true);
        remote
    }

    /// Wait until the viewer's name is on the bus, or with `there` false, gone.
    fn await_name(&self, there: bool) {
        let end = Instant::now() + PATIENCE;
        while self.names().contains(&self.name) != there {
            assert!(Instant::now() < end, "{} on the bus: {}", self.name, !there);
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// A client program on the test's bus, the bus checked first: never the session's.
    fn client(&self, program: &str) -> Command {
        let own = format!("unix:path={}", self.sandbox.path("bus").display());
        assert!(
            self.bus.address == own || self.bus.address.starts_with(&format!("{own},")),
            "not the test's bus: {}",
            self.bus.address
        );
        let mut command = self.sandbox.command(program);
        command.env("DBUS_SESSION_BUS_ADDRESS", &self.bus.address);
        match program {
            "busctl" => command.arg(format!("--address={}", self.bus.address)),
            "dbus-send" => command.arg(format!("--bus={}", self.bus.address)),
            "dbus-monitor" => command.args(["--address", &self.bus.address]),
            _ => &mut command,
        };
        command
    }

    fn run(&self, program: &str, args: &[&str]) -> Output {
        self.client(program).args(args).output().unwrap()
    }

    /// busctl's answer as JSON; the call must work.
    fn busctl(&self, args: &[&str]) -> Value {
        let output = self.run("busctl", &[&["--json=short"], args].concat());
        assert!(
            output.status.success(),
            "busctl {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    }

    /// A call of the bus itself: its first answer.
    fn ask_bus(&self, method: &str, args: &[&str]) -> Value {
        let call = [
            &[
                "call",
                "org.freedesktop.DBus",
                "/org/freedesktop/DBus",
                "org.freedesktop.DBus",
                method,
            ],
            args,
        ]
        .concat();
        self.busctl(&call)["data"][0].clone()
    }

    /// The names on the bus.
    fn names(&self) -> Vec<String> {
        serde_json::from_value(self.ask_bus("ListNames", &[])).unwrap()
    }

    /// A property as busctl gives it: its type and value.
    fn get(&self, interface: &str, property: &str) -> Value {
        self.busctl(&["get-property", &self.name, PATH, interface, property])
    }

    /// Wait until `property` of the player is `value`.
    fn await_get(&self, property: &str, value: Value) {
        let end = Instant::now() + PATIENCE;
        loop {
            let now = self.get(PLAYER, property)["data"].clone();
            if now == value {
                return;
            }
            assert!(Instant::now() < end, "{property}: {now}, not {value}");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Every property of `interface` as GetAll gives them: by name, each with its type and value.
    fn all(&self, interface: &str) -> serde_json::Map<String, Value> {
        let all = self.busctl(&[
            "call",
            &self.name,
            PATH,
            "org.freedesktop.DBus.Properties",
            "GetAll",
            "s",
            interface,
        ]);
        all["data"][0].as_object().unwrap().clone()
    }

    /// What the object says of itself.
    fn introspect(&self) -> String {
        let said = self.busctl(&[
            "call",
            &self.name,
            PATH,
            "org.freedesktop.DBus.Introspectable",
            "Introspect",
        ]);
        said["data"][0].as_str().unwrap().to_string()
    }

    /// A method call by dbus-send, its reply waited for: the reply's lines and how long it took.
    fn call(&self, method: &str, args: &[&str]) -> (Vec<String>, Duration) {
        let dest = format!("--dest={}", self.name);
        let sent = Instant::now();
        let output = self.run(
            "dbus-send",
            &[&["--print-reply", &dest, PATH, method], args].concat(),
        );
        let took = sent.elapsed();
        assert!(
            output.status.success(),
            "{method}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let reply = String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(str::to_string)
            .collect();
        (reply, took)
    }

    /// A call of the player's that must be answered at once with an empty reply.
    fn send(&self, method: &str, args: &[&str]) {
        let (reply, took) = self.call(&format!("{PLAYER}.{method}"), args);
        assert!(
            reply.len() == 1 && reply[0].starts_with("method return "),
            "{method}: {reply:?}"
        );
        assert!(
            took < Duration::from_secs(5),
            "{method}: answered in {took:?}"
        );
    }

    /// playerctl ARGS with no player named, as the media keys run it: what it printed.
    fn playerctl(&self, args: &[&str]) -> String {
        let output = self.run("playerctl", args);
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }
}

/// The frame a 16:9 counter clip shows in an 80 x 24 window (rows 2 to 22, all 80 columns): read on row 12.
fn frame(screen: &Screen) -> u32 {
    counted(screen, 12, 1, 80)
}

/// The frames drawn in `out`, each once, in order.
fn frames(out: &[u8]) -> Vec<u32> {
    let mut drawn = replay(out, SMALL, frame);
    drawn.dedup();
    drawn
}

/// Send `keys`, then wait for the toast `text`.
fn says(tty: &mut Tty, keys: &[u8], text: &str) {
    let at = tty.send(keys);
    tty.expect(at, toast(text));
}

#[test]
fn video_and_audio_own_a_name_on_the_bus_and_stills_none() {
    // rows M1, N1: serve_mpris of bin/ascii-viewer:1512 at tag python-final: `org.mpris.MediaPlayer2.<name>.instance<pid>` on the session bus, the object /org/mpris/MediaPlayer2 with the interfaces org.mpris.MediaPlayer2 and org.mpris.MediaPlayer2.Player, for video and audio, not for a still (run of bin/ascii-viewer:2386 returns before it); gone with the viewer; N1: `asciier`
    let mut sandbox = Sandbox::new();
    let bus = Bus::new(&mut sandbox);
    let video = clip(&sandbox, "clip.mp4", 20.0, "160x90", 25);
    let tone = tone(&sandbox, "tone.m4a", 20.0);
    for media in [&video, &tone] {
        let mut tty = sandbox.launch().arg(media).size(SMALL).start();
        let remote = Remote::new(&sandbox, &bus, tty.pid);
        let described = remote.introspect();
        for interface in [ROOT, PLAYER] {
            assert!(
                described.contains(&format!("<interface name=\"{interface}\">")),
                "{interface} in {described}"
            );
        }
        tty.await_playing();
        tty.send(b"q");
        assert_eq!(tty.exit(), 0);
        remote.await_name(false);
    }
    let picture = save(&gradient(800, 400), &sandbox.path("pic.png"));
    let mut tty = sandbox.launch().arg(&picture).size(SMALL).start();
    tty.until("the picture", |screen| screen.inked() > 1000);
    tty.settle(Duration::from_millis(300));
    let remote = Remote {
        sandbox: &sandbox,
        bus: &bus,
        name: String::new(),
    };
    let names = remote.names();
    assert!(
        names.iter().all(|name| !name.starts_with(ROOT)),
        "{names:?}"
    );
    tty.send(b"q");
    assert_eq!(tty.exit(), 0);
}

#[test]
fn properties_answer_from_the_player() {
    // rows M3, N1: mpris_get of bin/ascii-viewer:1545 at tag python-final, all read-only: the fixed values, PlaybackStatus by the mark, Position the clock in microseconds, Metadata of the file (its URI of the file itself, percent-encoded), LoopStatus Track while `L`, Rate the speed, Volume mpv's volume / 100 (1.0 at 0 and while unknown), no length while unknown; N1: Identity `asciier`
    let mut sandbox = Sandbox::new();
    let bus = Bus::new(&mut sandbox);
    let video = clip(&sandbox, "a clip.mp4", 20.0, "160x90", 25);
    let mut tty = sandbox.launch().arg(&video).size(SMALL).start();
    let remote = Remote::new(&sandbox, &bus, tty.pid);
    tty.await_playing();
    let root = Value::Object(remote.all(ROOT));
    assert_eq!(
        root,
        json!({
            "CanQuit": {"type": "b", "data": true},
            "CanRaise": {"type": "b", "data": false},
            "HasTrackList": {"type": "b", "data": false},
            "Identity": {"type": "s", "data": "asciier"},
            "SupportedUriSchemes": {"type": "as", "data": ["file"]},
            "SupportedMimeTypes": {"type": "as", "data": []},
        })
    );
    let mut player = remote.all(PLAYER);
    let (position, metadata) = (
        player.remove("Position").unwrap(),
        player.remove("Metadata").unwrap(),
    );
    assert_eq!(
        Value::Object(player),
        json!({
            "PlaybackStatus": {"type": "s", "data": "Playing"},
            "LoopStatus": {"type": "s", "data": "None"},
            "Rate": {"type": "d", "data": 1.0},
            "Shuffle": {"type": "b", "data": false},
            "Volume": {"type": "d", "data": 1.0},
            "MinimumRate": {"type": "d", "data": 0.25},
            "MaximumRate": {"type": "d", "data": 4.0},
            "CanGoNext": {"type": "b", "data": false},
            "CanGoPrevious": {"type": "b", "data": true},
            "CanPlay": {"type": "b", "data": true},
            "CanPause": {"type": "b", "data": true},
            "CanSeek": {"type": "b", "data": true},
            "CanControl": {"type": "b", "data": true},
        })
    );
    assert_eq!(position["type"], "x");
    assert!(
        (1_000_000..20_000_000).contains(&position["data"].as_i64().unwrap()),
        "{position}"
    );
    let file = std::fs::canonicalize(&video).unwrap();
    let url = format!("file://{}", file.display().to_string().replace(' ', "%20"));
    let length = metadata["data"]["mpris:length"].clone();
    assert_eq!(length["type"], "x");
    assert!(
        (20_000_000..20_100_000).contains(&length["data"].as_i64().unwrap()),
        "{length}"
    );
    assert_eq!(
        metadata,
        json!({"type": "a{sv}", "data": {
            "mpris:trackid": {"type": "o", "data": "/org/mpris/MediaPlayer2/Track/1"},
            "xesam:title": {"type": "s", "data": "a clip.mp4"},
            "xesam:url": {"type": "s", "data": url},
            "mpris:length": length,
        }})
    );
    // the player's state as it changes: paused, the clock, the loop, the speed, the volume
    tty.send(b" ");
    remote.await_get("PlaybackStatus", json!("Paused"));
    tty.send(DOWN);
    remote.await_get("Position", json!(0));
    tty.send(RIGHT);
    remote.await_get("Position", json!(5_000_000));
    says(&mut tty, b"L", "loop on");
    remote.await_get("LoopStatus", json!("Track"));
    says(&mut tty, b"]", "speed 110%");
    remote.await_get("Rate", json!(1.1));
    says(&mut tty, b"0", "volume 102");
    remote.await_get("Volume", json!(1.02));
    says(&mut tty, &b"9".repeat(60), "volume 0");
    remote.await_get("Volume", json!(1.0));
    remote.send("Stop", &[]);
    remote.await_get("PlaybackStatus", json!("Stopped"));
    // read-only: nothing can be set
    let set = remote.run(
        "busctl",
        &[
            "set-property",
            &remote.name,
            PATH,
            PLAYER,
            "Volume",
            "d",
            "0.5",
        ],
    );
    assert!(!set.status.success());
    tty.send(b"q");
    assert_eq!(tty.exit(), 0);
    // without sound and length: Volume 1.0, no mpris:length
    let raw = sandbox.path("clip.h264");
    ffmpeg(
        &sandbox,
        &[
            "-f",
            "lavfi",
            "-i",
            "testsrc2=s=160x90:r=25:d=20",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
            raw.to_str().unwrap(),
        ],
    );
    let mut tty = sandbox.launch().arg(&raw).size(SMALL).start();
    let remote = Remote::new(&sandbox, &bus, tty.pid);
    tty.await_status("> 00:01");
    assert_eq!(
        remote.get(PLAYER, "Volume"),
        json!({"type": "d", "data": 1.0})
    );
    let metadata = remote.get(PLAYER, "Metadata");
    let keys: Vec<&String> = metadata["data"].as_object().unwrap().keys().collect();
    assert_eq!(keys, ["mpris:trackid", "xesam:title", "xesam:url"]);
    tty.send(b"q");
    assert_eq!(tty.exit(), 0);
}

#[test]
fn calls_are_answered_at_once_and_drive_the_player() {
    // row M2: mpris_call and remote of bin/ascii-viewer:1539 and :1573 at tag python-final: every call answered at once with an empty reply; PlayPause toggles, Play only when not playing, Pause only when playing, Seek by microseconds, SetPosition to microseconds whatever the track, Previous to 0, Stop to the stopped mark `.`, Quit exit 0; Raise, Next and OpenUri do nothing
    let mut sandbox = Sandbox::new();
    let bus = Bus::new(&mut sandbox);
    let video = clip(&sandbox, "clip.mp4", 30.0, "160x90", 25);
    let mut tty = sandbox.launch().arg(&video).size(SMALL).start();
    let remote = Remote::new(&sandbox, &bus, tty.pid);
    tty.await_playing();
    remote.send("Pause", &[]);
    tty.await_status("| ");
    // a window for what must not happen: Pause or Play when it is so already toggling it
    remote.send("Pause", &[]);
    std::thread::sleep(Duration::from_millis(600));
    assert!(tty.screen().row(24).starts_with("| "));
    remote.send("Play", &[]);
    tty.await_status("> ");
    remote.send("Play", &[]);
    std::thread::sleep(Duration::from_millis(600));
    assert!(tty.screen().row(24).starts_with("> "));
    for mark in ["| ", "> ", "| "] {
        remote.send("PlayPause", &[]);
        tty.await_status(mark);
    }
    let track = "objpath:/org/mpris/MediaPlayer2/Track/1";
    let calls: [(&str, &[&str], &str); 6] = [
        ("Previous", &[], "| 00:00"),
        ("Seek", &["int64:5000000"], "| 00:05"),
        ("Seek", &["int64:-2000000"], "| 00:03"),
        ("SetPosition", &[track, "int64:20000000"], "| 00:20"),
        (
            "SetPosition",
            &["objpath:/not/a/track", "int64:9000000"],
            "| 00:09",
        ),
        ("Previous", &[], "| 00:00"),
    ];
    for (method, args, status) in calls {
        remote.send(method, args);
        tty.await_status(status);
    }
    // a window for what must not happen: any of these moving it
    remote.send("Next", &[]);
    let (reply, _) = remote.call(&format!("{ROOT}.Raise"), &[]);
    assert!(reply.len() == 1 && reply[0].starts_with("method return "));
    remote.send("OpenUri", &["string:file:///other.mp4"]);
    std::thread::sleep(Duration::from_millis(600));
    assert!(tty.screen().row(24).starts_with("| 00:00 / 00:30"));
    remote.send("Stop", &[]);
    tty.await_status(". 00:00");
    let (reply, took) = remote.call(&format!("{ROOT}.Quit"), &[]);
    assert!(
        reply.len() == 1 && reply[0].starts_with("method return ") && took < Duration::from_secs(5),
        "Quit: {reply:?} in {took:?}"
    );
    assert_eq!(tty.exit(), 0);
}

#[test]
fn playerctl_drives_it_as_the_media_keys_do() {
    // rows M1, M1a: the media keys run `playerctl previous`, `position 5-`, `play-pause`, `next`, `position 5+` with no player named (B 4.1-4.4); on the test's bus ours is the one player
    let mut sandbox = Sandbox::new();
    let bus = Bus::new(&mut sandbox);
    let video = clip(&sandbox, "clip.mp4", 30.0, "160x90", 25);
    let mut tty = sandbox.launch().arg(&video).size(SMALL).start();
    let remote = Remote::new(&sandbox, &bus, tty.pid);
    tty.await_playing();
    assert_eq!(
        remote.playerctl(&["-l"]),
        format!("asciier.instance{}", tty.pid)
    );
    assert_eq!(remote.playerctl(&["status"]), "Playing");
    remote.playerctl(&["play-pause"]);
    tty.await_status("| ");
    assert_eq!(remote.playerctl(&["status"]), "Paused");
    for (args, status) in [
        (&["previous"][..], "| 00:00"),
        (&["position", "5+"], "| 00:05"),
        (&["position", "5+"], "| 00:10"),
        (&["position", "5-"], "| 00:05"),
    ] {
        remote.playerctl(args);
        tty.await_status(status);
    }
    assert_eq!(remote.playerctl(&["position"]), "5.000000");
    assert_eq!(remote.playerctl(&["metadata", "xesam:title"]), "clip.mp4");
    // a window for what must not happen: next moving it
    remote.playerctl(&["next"]);
    std::thread::sleep(Duration::from_millis(600));
    assert!(tty.screen().row(24).starts_with("| 00:05"));
    remote.playerctl(&["play-pause"]);
    tty.await_status("> ");
    tty.send(b"q");
    assert_eq!(tty.exit(), 0);
}

#[test]
fn stop_holds_the_frame_until_a_seek_or_space_plays() {
    // rows P8a, P8, M2, H3: halt, seek_to and frame_step of bin/ascii-viewer:1304, :1423 and :1441 at tag python-final (tests/test_viewer.py:187): MPRIS Stop is the stopped state, mark `.`, the last frame held; `.` and `,` do nothing there, nor F1 and a key closing the keys (open_help and close_help of bin/ascii-viewer:2061 and :2096: help_resume is false when not playing; run in this harness the old viewer stayed stopped on its frame); a seek key or an MPRIS Seek plays from its target; `space` plays from 0; under load the screen read after a quiet window was 7 to 14 frames behind the frame mpv held (the review: 3 of 3 runs with four test binaries at once), so the held frame is the one `s` takes, and the screen must come to it (N5: mpv's; the old viewer's `s` took another, P21a)
    let mut sandbox = Sandbox::new();
    let bus = Bus::new(&mut sandbox);
    let video = counter(&sandbox, "clip.mp4", "160x90", 25, 10.0, 10.0, &[]);
    let mut tty = sandbox.launch().arg(&video).size(SMALL).start();
    let remote = Remote::new(&sandbox, &bus, tty.pid);
    tty.await_playing();
    let stop = |tty: &mut Tty| {
        remote.send("Stop", &[]);
        tty.await_status(". ");
        let held = taken(tty, &sandbox);
        tty.until("the frame taken", |screen| frame(screen) == held);
        // the toast gone
        tty.await_status(". ");
        held
    };
    let held = stop(&mut tty);
    // `m` after the steps shows when they are done: its toast is the first
    let at = tty.send(b".,m");
    let sentinel = toast("muted");
    let end = tty.expect(at, &sentinel);
    assert_eq!(find(&tty.out(), b"\x1b[7m", at), Some(end - sentinel.len()));
    tty.await_status(". ");
    assert_eq!(frame(tty.screen()), held, "a step moved it");
    // the keys over the stopped clip, closed: still stopped; a window for what must not happen: playing on
    let at = tty.send(F1);
    tty.expect(at, b"any other key closes");
    tty.send(b"x");
    tty.await_status(". ");
    std::thread::sleep(Duration::from_millis(800));
    let screen = tty.screen();
    assert!(
        screen.row(24).starts_with(". ") && frame(screen) == held,
        "{:?} on frame {}",
        screen.row(24),
        frame(screen)
    );
    // a second on from the held frame, playing
    let jumps = |out: &[u8], held: u32, step: u32| {
        let after = frames(out);
        let first = after.iter().find(|&&number| number > held + 2).unwrap();
        assert!(
            (held + step - 1..held + step + 15).contains(first),
            "from frame {held} on to {first}: {after:?}"
        );
    };
    let from = tty.send(SHIFT_RIGHT);
    tty.await_status("> ");
    tty.until("frames after the target", |screen| {
        frame(screen) > held + 30
    });
    jumps(&tty.out()[from..], held, 25);
    let held = stop(&mut tty);
    let from = tty.len();
    remote.send("Seek", &["int64:2000000"]);
    tty.await_status("> ");
    tty.until("frames after the target", |screen| {
        frame(screen) > held + 55
    });
    jumps(&tty.out()[from..], held, 50);
    stop(&mut tty);
    tty.send(b" ");
    tty.until("playing from the start", |screen| {
        screen.row(24).starts_with("> 00:00") && frame(screen) < 25
    });
    tty.send(b"q");
    assert_eq!(tty.exit(), 0);
}

/// `s` on a stopped counter clip: the frame its picture holds (a bar lit where its middle is brighter than the theme's background).
fn taken(tty: &mut Tty, sandbox: &Sandbox) -> u32 {
    let at = tty.send(b"s");
    let start = tty.expect(at, b"\x1b[7m saved ");
    let end = tty.expect(start, b" \x1b[0m") - 5;
    let name = String::from_utf8(tty.out()[start..end].to_vec()).unwrap();
    let picture = image::open(sandbox.path(name)).unwrap().to_rgb8();
    let (w, h) = picture.dimensions();
    let lit = |bar: u32| {
        let (left, right) = (w * bar / 8 + w / 32, w * (bar + 1) / 8 - w / 32);
        let sum: u64 = (h * 3 / 8..h * 5 / 8)
            .flat_map(|y| (left..right).map(move |x| (x, y)))
            .map(|(x, y)| picture.get_pixel(x, y)[1] as u64)
            .sum();
        sum / ((right - left) as u64 * (h / 4) as u64) > 50
    };
    (0..8).filter(|&bar| lit(bar)).map(|bar| 1 << bar).sum()
}

#[test]
fn a_seek_from_the_stopped_state_counts_from_the_frame_on_screen() {
    // rows P8a, P9, N5: a seek after MPRIS Stop, by key or by MPRIS, plays from its target counted from the held frame, the one `s` takes, and the target is the first frame drawn after it; the debug build draws a 200 x 60 window slower than the clip's 25 frames a second, so mpv has a frame due when it stops, which play asked with the seek showed first and counted the seek from (seen before the fix: 35, 36, 61; under load at 80 x 24 also 29, 30, 55 and 28, 54; in this form, with the play sent beside the seek: 64, 65, 90, 3 of 3); after the stop the window is 80 x 24 and the clip at 25 %: mpv holds a frame not taken within 200 ms as shown and plays on, and under load the 200 x 60 viewer missed the target (mpv's log, three test binaries at once: the seek counted from the held frame, the first frame drawn 1 to 10 past the target, 4 of 4); the clip lasts 20 s and starts over at 80 x 24, which the viewer draws in time: under load the 200 x 60 one drew no frame of the first second, or the clip ran out (seen with three mpris groups and a playback group at once: 4 of 6 copies; the code before the requests went one at a time, 2 of 6)
    let mut sandbox = Sandbox::new();
    let bus = Bus::new(&mut sandbox);
    let video = counter(&sandbox, "clip.mp4", "160x90", 25, 20.0, 20.0, &[]);
    let mut tty = sandbox.view(&video);
    let remote = Remote::new(&sandbox, &bus, tty.pid);
    tty.await_playing();
    let seeks: [(&[u8], &[&str]); 3] = [
        (SHIFT_RIGHT, &[]),
        (b"", &["int64:1000000"]),
        (SHIFT_RIGHT, &[]),
    ];
    for (keys, seek) in seeks {
        remote.send("Stop", &[]);
        tty.await_status(". ");
        tty.resize(SMALL);
        says(&mut tty, b"{{", "speed 25%");
        // the screen can lag mpv under load: the frame `s` takes is the reference, and the screen must come to it
        let held = taken(&mut tty, &sandbox);
        tty.until("the frame taken", |screen| frame(screen) == held);
        let from = tty.send(keys);
        if !seek.is_empty() {
            remote.send("Seek", seek);
        }
        tty.until("frames after the target", |screen| {
            screen.row(24).starts_with("> ") && frame(screen) > held + 27
        });
        let mut after = replay(&tty.out()[from..], SMALL, frame);
        after.dedup();
        let first = after.iter().find(|&&number| number != held);
        assert_eq!(first, Some(&(held + 25)), "from frame {held}: {after:?}");
        // on from the start at full speed, then at full size: the screen's lag under load would otherwise run the clip out
        says(&mut tty, b"\\", "speed 100%");
        tty.send(DOWN);
        tty.until("playing from the start", |screen| {
            screen.row(24).starts_with("> ") && frame(screen) < 25
        });
        tty.resize(WIDE);
    }
    tty.send(b"q");
    assert_eq!(tty.exit(), 0);
}

#[test]
fn a_step_back_right_after_a_seek_from_the_stopped_state_ends_paused() {
    // rows P8a, P15: `l,` in one batch after MPRIS Stop: the seek leaves the stopped state and `,` pauses on the frame before its target, never drawn playing, as the old viewer did (seek_to and frame_step of bin/ascii-viewer:1423 and :1441 at tag python-final; run in this harness: stopped on frame 25, `l,` 149); seen before a step dropped the play: `> 00:01`, `> 00:02`, `> 00:07` after the window (3 of 3), and before the requests went one at a time on the frame before the held one (24 for 25); a wait for `| ` alone passed without the fix (the review: 2 of 9), finding the pause at the clip's end
    steps_from_the_stopped_state(b"l,", 124);
}

#[test]
fn a_step_right_after_a_seek_from_the_stopped_state_shows_the_target() {
    // rows P8a, P15: `l.` in one batch after MPRIS Stop: the seek plays from its target and `.` pauses there: the old viewer started playing at the target and its step showed the target's frame, the first it read (seek_to and frame_step of bin/ascii-viewer:1423 and :1441 at tag python-final; run in this harness: stopped on frame 25, `l.` 150); seen before the fix: played on to the clip's end, the mark `|` and the run going on there
    steps_from_the_stopped_state(b"l.", 125);
}

/// `keys` in one write after MPRIS Stop: they end paused `on` frames after the held one, never drawn playing, and stay there; the held frame is the one `s` takes (N5: mpv's; the old viewer's `s` took another, P21a, so the old values come from a run that read the screen).
fn steps_from_the_stopped_state(keys: &[u8], on: u32) {
    let mut sandbox = Sandbox::new();
    let bus = Bus::new(&mut sandbox);
    let video = counter(&sandbox, "clip.mp4", "160x90", 25, 20.0, 20.0, &[]);
    let mut tty = sandbox.launch().arg(&video).size(SMALL).start();
    let remote = Remote::new(&sandbox, &bus, tty.pid);
    tty.await_playing();
    remote.send("Stop", &[]);
    tty.await_status(". ");
    // the screen can lag mpv under load: the frame `s` takes is the one held
    let held = taken(&mut tty, &sandbox);
    tty.until("the frame taken", |screen| frame(screen) == held);
    tty.await_status(". ");
    let at = tty.send(keys);
    let (target, clock) = (held + on, format!("| 00:{:02}", (held + on) / 25));
    tty.until(&format!("frame {target} and {clock:?}"), |screen| {
        frame(screen) == target % 256 && screen.row(24).starts_with(&clock)
    });
    // a window for what must not happen: playing on
    std::thread::sleep(Duration::from_millis(500));
    let screen = tty.screen();
    let (status, now) = (screen.row(24), frame(screen));
    assert!(
        status.starts_with(&clock) && now == target % 256,
        "{status:?} on frame {now}"
    );
    let marks = replay(&tty.out()[at..], SMALL, |screen| screen.row(24));
    assert!(
        !marks.iter().any(|row| row.starts_with("> ")),
        "drawn playing: {marks:?}"
    );
    tty.send(b"q");
    assert_eq!(tty.exit(), 0);
}

#[test]
fn requests_are_dropped_on_the_page_and_served_under_the_keys() {
    // row M5: remote of bin/ascii-viewer:1573 at tag python-final: on the render page every request is answered and dropped (PlayPause, Seek, Quit change nothing); under F1's keys they are served: PlayPause plays under them, Quit ends it
    let mut sandbox = Sandbox::new();
    let bus = Bus::new(&mut sandbox);
    let video = clip(&sandbox, "clip.mp4", 20.0, "160x90", 25);
    let mut tty = sandbox.launch().arg(&video).size(SMALL).start();
    let remote = Remote::new(&sandbox, &bus, tty.pid);
    tty.await_playing();
    tty.send(b"r");
    tty.until("the page", |screen| screen.row(1).starts_with(" Render"));
    remote.await_get("PlaybackStatus", json!("Paused"));
    let held = remote.get(PLAYER, "Position");
    remote.send("PlayPause", &[]);
    remote.send("Seek", &["int64:5000000"]);
    let (reply, _) = remote.call(&format!("{ROOT}.Quit"), &[]);
    assert!(reply.len() == 1 && reply[0].starts_with("method return "));
    // a window for what must not happen: any of them acted on
    std::thread::sleep(Duration::from_secs(1));
    assert!(tty.running(), "quit from the page");
    assert_eq!(
        remote.get(PLAYER, "PlaybackStatus")["data"],
        json!("Paused")
    );
    assert_eq!(remote.get(PLAYER, "Position"), held);
    assert!(tty.screen().row(1).starts_with(" Render"));
    tty.send(b"q");
    tty.await_status("> ");
    remote.await_get("PlaybackStatus", json!("Playing"));
    let at = tty.send(F1);
    tty.expect(at, b"any other key closes");
    remote.await_get("PlaybackStatus", json!("Paused"));
    remote.send("PlayPause", &[]);
    remote.await_get("PlaybackStatus", json!("Playing"));
    let (reply, _) = remote.call(&format!("{ROOT}.Quit"), &[]);
    assert!(reply.len() == 1 && reply[0].starts_with("method return "));
    assert_eq!(tty.exit(), 0);
}

#[test]
fn quit_under_the_keys_continues_the_scope_first() {
    // rows M5, N14: MPRIS Quit while F1's keys show over audio alone, the scope stopped: served, the scope continued and ended, exit 0 (the old viewer waited on the stopped scope for ever, A6a)
    let mut sandbox = Sandbox::new();
    let bus = Bus::new(&mut sandbox);
    let tone = tone(&sandbox, "tone.m4a", 30.0);
    let mut tty = sandbox.launch().arg(&tone).size(SMALL).start();
    let remote = Remote::new(&sandbox, &bus, tty.pid);
    sandbox.await_noted("scope.out", "ready");
    tty.await_playing();
    let at = tty.send(F1);
    tty.expect(at, b"any other key closes");
    remote.await_get("PlaybackStatus", json!("Paused"));
    let (reply, _) = remote.call(&format!("{ROOT}.Quit"), &[]);
    assert!(reply.len() == 1 && reply[0].starts_with("method return "));
    let status = tty.exit_within(Duration::from_secs(10));
    assert_eq!(
        status.and_then(|status| status.code()),
        Some(0),
        "{status:?}"
    );
    assert!(sandbox.noted("scope.out").ends_with("term\n"));
    assert!(tty.out().ends_with(RESTORE));
}

#[test]
fn no_signal_is_declared_or_sent() {
    // row M4: MPRIS_XML of bin/ascii-viewer:162 at tag python-final declares no signal, and the viewer sends none (no PropertiesChanged, no Seeked) while it plays, pauses, seeks, loops and changes speed and volume; a monitor on the test's bus shows the bus's own signals
    let mut sandbox = Sandbox::new();
    let bus = Bus::new(&mut sandbox);
    let video = clip(&sandbox, "clip.mp4", 20.0, "160x90", 25);
    let watcher = Remote {
        sandbox: &sandbox,
        bus: &bus,
        name: String::new(),
    };
    let log = std::fs::File::create(sandbox.path("signals.log")).unwrap();
    let mut monitor = watcher.client("dbus-monitor");
    monitor
        .arg("type='signal'")
        .stdout(Stdio::from(log))
        .stderr(Stdio::null());
    // SAFETY: prctl alone, between fork and exec: the monitor dies with the test, however that ends.
    unsafe {
        use std::os::unix::process::CommandExt;
        monitor.pre_exec(|| {
            libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL);
            Ok(())
        })
    };
    let mut monitor = monitor.spawn().unwrap();
    let end = Instant::now() + PATIENCE;
    while !std::fs::read_to_string(sandbox.path("signals.log"))
        .unwrap()
        .contains("NameAcquired")
    {
        assert!(Instant::now() < end, "the monitor is not on the bus");
        std::thread::sleep(Duration::from_millis(20));
    }
    let mut tty = sandbox.launch().arg(&video).size(SMALL).start();
    let remote = Remote::new(&sandbox, &bus, tty.pid);
    let unique = remote.ask_bus("GetNameOwner", &["s", &remote.name]);
    let unique = unique.as_str().unwrap().to_string();
    let described = remote.introspect();
    for interface in [ROOT, PLAYER] {
        let start = described
            .find(&format!("<interface name=\"{interface}\">"))
            .unwrap();
        let body = &described[start..start + described[start..].find("</interface>").unwrap()];
        assert!(!body.contains("<signal"), "{interface}: {body}");
    }
    tty.await_playing();
    tty.send(b" ");
    tty.await_status("| ");
    for (keys, text) in [
        (&b"L"[..], "loop on"),
        (b"]", "speed 110%"),
        (b"0", "volume 102"),
        (b"m", "muted"),
    ] {
        says(&mut tty, keys, text);
    }
    tty.send(DOWN);
    tty.await_status("| 00:00");
    remote.send("Seek", &["int64:3000000"]);
    tty.await_status("| 00:03");
    remote.send("PlayPause", &[]);
    tty.await_status("> ");
    tty.send(b"q");
    assert_eq!(tty.exit(), 0);
    remote.await_name(false);
    let _ = monitor.kill();
    let _ = monitor.wait();
    let log = std::fs::read_to_string(sandbox.path("signals.log")).unwrap();
    let sent = |sender: &str| -> Vec<String> {
        log.lines()
            .filter(|line| {
                line.starts_with("signal ") && line.contains(&format!(" sender={sender} "))
            })
            .map(str::to_string)
            .collect()
    };
    // the bus told of the viewer's name coming and going, so the monitor saw its whole run
    assert!(
        log.contains(&remote.name) && !sent("org.freedesktop.DBus").is_empty(),
        "{log}"
    );
    assert_eq!(sent(&unique), Vec::<String>::new());
}

#[test]
fn without_a_bus_it_plays_silently() {
    // row M6: serve_mpris of bin/ascii-viewer:1512 at tag python-final: no session bus to own a name on (nobody listens at the address), no MPRIS, nothing said, it plays as ever
    let sandbox = Sandbox::new();
    let video = clip(&sandbox, "clip.mp4", 20.0, "160x90", 25);
    let mut tty = sandbox.launch().arg(&video).size(SMALL).start();
    tty.await_playing();
    says(&mut tty, b"m", "muted");
    tty.await_status("> ");
    // the picture covers rows 2 to 22: nothing written above or below it
    let screen = tty.screen();
    assert_eq!(
        (screen.row(1), screen.row(23)),
        (String::new(), String::new())
    );
    tty.send(b"q");
    assert_eq!(tty.exit(), 0);
    // before the alternate screen the title alone, after it nothing
    let out = tty.out();
    let entered = find(&out, b"\x1b[?1049h", 0).unwrap();
    let title = out[..entered].strip_prefix(b"\x1b[22;2t\x1b]2;");
    assert!(
        title.is_some_and(|title| title.iter().position(|&b| b == 0x07) == Some(title.len() - 1)),
        "{}",
        shown(&out[..entered])
    );
    assert!(out.ends_with(RESTORE));
}
