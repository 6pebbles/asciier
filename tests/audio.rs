//! Audio alone in the viewer: the oscilloscope that owns the screen, the status row under it, the keys that act and those that do not, the end and the loop, the help over it, a resize (inventory section 3, H3, H5, V10, N14).

mod common;

use common::*;
use std::time::{Duration, Instant};

const SHIFT_UP: &[u8] = b"\x1b[1;2A";
const SHIFT_DOWN: &[u8] = b"\x1b[1;2B";
/// What the keys' screen starts with: a synchronized update, kitty's images deleted, the screen cleared.
const KEYS_DRAWN: &[u8] = b"\x1b[?2026h\x1b[0m\x1b_Ga=d,d=A,q=2\x1b\\\x1b[2J";

/// What the scope stub noted when it started, as `key=value` pairs.
fn scope_notes(sandbox: &Sandbox) -> Vec<(String, String)> {
    sandbox
        .await_noted("scope.out", "ready")
        .lines()
        .filter_map(|line| line.split_once('='))
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect()
}

fn note<'a>(notes: &'a [(String, String)], key: &str) -> &'a str {
    notes
        .iter()
        .find(|(known, _)| known == key)
        .map_or("", |(_, value)| value)
}

/// The scope's pid, once it is ready.
fn scope_pid(sandbox: &Sandbox) -> u32 {
    note(&scope_notes(sandbox), "pid").parse().unwrap()
}

/// The fields of /proc/<pid>/stat after the name: state, parent, group, session, ...; `None` once the process is gone.
fn stat(pid: u32) -> Option<Vec<String>> {
    let text = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    Some(
        text.rsplit_once(')')?
            .1
            .split_whitespace()
            .map(str::to_string)
            .collect(),
    )
}

/// Wait until the process `pid` is stopped (state `T`), or with `stopped` false, runs on.
fn await_stopped(pid: u32, stopped: bool) {
    let end = Instant::now() + PATIENCE;
    loop {
        let state = stat(pid).map(|fields| fields[0].clone());
        if state
            .as_deref()
            .is_some_and(|state| (state == "T") == stopped)
        {
            return;
        }
        assert!(Instant::now() < end, "process {pid}: {state:?}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// How every status write in a window of `rows` starts: the scope's cursor and colour saved, the bottom row erased.
fn under(rows: u16) -> Vec<u8> {
    format!("\x1b7\x1b[{rows};1H\x1b[0m\x1b[2K").into_bytes()
}

/// A whole status write of the row `text` in a window of `rows`: saved, erased, written, restored.
fn status_write(rows: u16, text: &str) -> Vec<u8> {
    [
        under(rows),
        format!("\x1b[{rows};1H\x1b[0m{text}\x1b[0m\x1b8").into_bytes(),
    ]
    .concat()
}

/// The status writes in `out` for a window of `rows`: what each writes between the erased row and `ESC 8`.
fn writes(out: &[u8], rows: u16) -> Vec<Vec<u8>> {
    let start = under(rows);
    let (mut found, mut at) = (Vec::new(), 0);
    while let Some(begin) = find(out, &start, at) {
        let from = begin + start.len();
        let Some(end) = find(out, b"\x1b8", from) else {
            break;
        };
        found.push(out[from..end].to_vec());
        at = end;
    }
    found
}

/// The seconds a status row shows (`> 01:05 / 02:10 ━━` is 65); `None` for a toast or a blank row.
fn clock(row: &str) -> Option<u32> {
    let clock = row.get(2..)?.split(' ').next()?;
    let (minutes, seconds) = clock.split_once(':')?;
    Some(minutes.parse::<u32>().ok()? * 60 + seconds.parse::<u32>().ok()?)
}

/// The seconds a status write shows; `None` for a toast.
fn seconds(write: &[u8]) -> Option<u32> {
    clock(String::from_utf8_lossy(write).split_once("H\x1b[0m")?.1)
}

/// The cells of the bar in a status write.
fn bar(write: &[u8]) -> usize {
    String::from_utf8_lossy(write).matches('━').count()
}

/// A status write's text before its bar: mark, clock and length.
fn row_text(write: &[u8]) -> String {
    let text = String::from_utf8_lossy(write);
    let line = text.split_once("H\x1b[0m").map_or("", |(_, line)| line);
    line.split('\x1b')
        .next()
        .unwrap_or_default()
        .trim_end_matches(['━', ' '])
        .to_string()
}

/// Send `keys`, then wait for the toast `text`.
fn says(tty: &mut Tty, keys: &[u8], text: &str) {
    let at = tty.send(keys);
    tty.expect(at, toast(text));
}

/// The keys under F1, once shown: the times they are drawn within a second once the drawing has rested 400 ms (at most 5 s): playing sound under them draws them with each move of its clock.
fn keys_drawn(tty: &Tty) -> usize {
    let end = Instant::now() + Duration::from_secs(5);
    while Instant::now() < end
        && tty.wait(tty.len(), Duration::from_millis(400), |out| {
            find(out, b"any other key closes", 0).is_some()
        })
    {}
    let from = tty.len();
    // a window for something that must not happen: a slower machine draws fewer, never more
    std::thread::sleep(Duration::from_secs(1));
    count(&tty.out()[from..], b"any other key closes")
}

#[test]
fn the_scope_owns_the_screen_until_the_viewer_ends() {
    // rows A1, N1: run and main of bin/ascii-viewer:2396 and :2465 at tag python-final (tests/test_viewer.py:629): `scope` from PATH with SCOPE_SPARE_ROWS=1 and SCOPE_TARGET the player's stream, stdin /dev/null, stdout and stderr the terminal, a child in the viewer's session, ended by SIGTERM and waited for before the viewer ends; PIPEWIRE_PROPS not handed on (the old viewer set it for its mpv alone); N1: the stream `asciier-<pid>`
    let sandbox = Sandbox::new();
    let tone = tone(&sandbox, "tone.m4a", 10.0);
    let mut tty = sandbox.launch().arg(&tone).size(SMALL).start();
    let notes = scope_notes(&sandbox);
    let scope: u32 = note(&notes, "pid").parse().unwrap();
    let target = format!("asciier-{}", tty.pid);
    for (key, value) in [
        ("rows", "1"),
        ("target", target.as_str()),
        ("props", "none"),
        ("in", "/dev/null"),
        ("out", tty.slave_name.as_str()),
        ("err", tty.slave_name.as_str()),
    ] {
        assert_eq!(note(&notes, key), value, "{key} in {notes:?}");
    }
    let fields = stat(scope).expect("the scope runs");
    let (parent, session): (u32, u32) = (fields[1].parse().unwrap(), fields[3].parse().unwrap());
    // the viewer leads its own session
    assert_eq!((parent, session), (tty.pid, tty.pid));
    tty.await_playing();
    tty.send(b"q");
    assert_eq!(tty.exit(), 0);
    assert!(
        sandbox.noted("scope.out").ends_with("ready\nterm\n"),
        "ended before the viewer: {:?}",
        sandbox.noted("scope.out")
    );
    assert!(stat(scope).is_none(), "the scope waited for");
}

#[test]
fn the_stream_bears_the_name_the_scope_records() {
    // rows A2, N1: STREAM and launch_mpv of bin/ascii-viewer:116 and :1324 at tag python-final (tests/test_viewer.py:647): PIPEWIRE_PROPS `{ node.name = <stream> }` names the player's PipeWire stream, the stream SCOPE_TARGET names; N1: `asciier-<pid>`; libmpv plays in the viewer itself (N19), so its own environment carries it, as a program it starts sees (hyprctl, by `f`); with ao=null no stream is made, so the node itself is not seen here
    let sandbox = Sandbox::new();
    sandbox.program(
        "hyprctl",
        "#!/bin/sh\necho \"props=${PIPEWIRE_PROPS-none}\" >> \"$0.calls\"\n",
    );
    let tone = tone(&sandbox, "tone.m4a", 10.0);
    let mut tty = sandbox.launch().arg(&tone).size(SMALL).start();
    let stream = format!("asciier-{}", tty.pid);
    assert_eq!(note(&scope_notes(&sandbox), "target"), stream);
    tty.await_playing();
    tty.send(b"f");
    assert_eq!(
        sandbox.await_noted("hyprctl.calls", "props="),
        format!("props={{ node.name = {stream} }}\n")
    );
    tty.send(b"q");
    assert_eq!(tty.exit(), 0);
}

/// Once the output has rested, no status write within a second: a window for what must not happen.
fn no_writes_after(tty: &Tty) {
    tty.settle(Duration::from_millis(400));
    let from = tty.len();
    std::thread::sleep(Duration::from_secs(1));
    let after = &tty.out()[from..];
    assert_eq!(count(after, b"\x1b7"), 0, "{}", shown(after));
}

#[test]
fn the_status_row_goes_under_the_scope_between_decsc_and_decrc() {
    // rows A3, P16: draw and run of bin/ascii-viewer:2184 and :2403 at tag python-final: `ESC 7`, the bottom row erased, the status row as for video (its bar round(room x position / length), room 80 - 15 - 1), `ESC 8`, never a synchronized update; while playing it is written as the clock runs (the old viewer after each poll of at most 0.25 s), paused only after a key or the toast's end
    let sandbox = Sandbox::new();
    let tone = tone(&sandbox, "tone.m4a", 10.0);
    let mut tty = sandbox.launch().arg(&tone).size(SMALL).start();
    tty.await_playing();
    tty.await_status("> 00:02");
    let out = tty.out();
    assert!(
        find(&out, b"\x1b[?2026h", 0).is_none(),
        "a synchronized update: {}",
        shown(&out)
    );
    let written = writes(&out, 24);
    assert!(
        written
            .iter()
            .all(|write| write.starts_with(b"\x1b[24;1H\x1b[0m") && write.ends_with(b"\x1b[0m")),
        "{written:?}"
    );
    // a second at most from one write to the next while the clock runs; each bar within its second, the length 10 s or a little over (AAC)
    let rows: Vec<(u32, usize)> = written
        .iter()
        .filter_map(|write| Some((seconds(write)?, bar(write))))
        .collect();
    assert!(
        rows.windows(2)
            .all(|pair| pair[1].0 >= pair[0].0 && pair[1].0 - pair[0].0 <= 1),
        "{rows:?}"
    );
    for &(second, bar) in &rows {
        let (low, high) = (
            (64.0 * second as f64 / 10.1).round() as usize,
            (64.0 * (second + 1) as f64 / 10.0).round() as usize,
        );
        assert!((low..=high).contains(&bar), "{bar} cells at {second} s");
    }
    assert!(rows.iter().any(|&(second, _)| second == 1), "{rows:?}");
    tty.send(b" ");
    tty.await_status("| ");
    let at = tty.send(DOWN);
    tty.expect(at, status_write(24, "| 00:00 / 00:10"));
    // paused: nothing written but for a key and the toast's end
    no_writes_after(&tty);
    let at = tty.send(b"]");
    let toasted = tty.expect(
        at,
        [
            under(24),
            b"\x1b[24;1H\x1b[7m speed 110% \x1b[0m\x1b8".to_vec(),
        ]
        .concat(),
    );
    tty.expect(toasted, status_write(24, "| 00:00 / 00:10"));
    no_writes_after(&tty);
    tty.send(b"q");
    assert_eq!(tty.exit(), 0);
}

#[test]
fn picture_keys_do_nothing() {
    // row A4: AUDIO_KEYS of bin/ascii-viewer:136 at tag python-final (tests/test_viewer.py:689): the look and glyph keys, `s`, `r`, the frame steps, a still's keys, Esc and Enter: no toast, no file, no page, no pause
    let sandbox = Sandbox::new();
    let tone = tone(&sandbox, "tone.m4a", 10.0);
    let mut tty = sandbox.launch().arg(&tone).size(SMALL).start();
    tty.await_playing();
    // `m` after them shows when they are done: its toast is the first
    let at = tty.send(b"3412`78cbgsr.,vdDuy+=-HJK\x1b\rm");
    let sentinel = toast("muted");
    let end = tty.expect(at, &sentinel);
    assert_eq!(
        find(&tty.out(), b"\x1b[7m", at),
        Some(end - sentinel.len()),
        "no toast of their own"
    );
    tty.until("playing on", |screen| screen.row(24).starts_with("> 00:"));
    assert!(
        find(&tty.out(), b"\x1b[?2026h", at).is_none(),
        "something drawn over the scope"
    );
    tty.send(b"q");
    assert_eq!(tty.exit(), 0);
    let mut files: Vec<_> = std::fs::read_dir(&sandbox.dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .filter(|name| name.starts_with("tone"))
        .collect();
    files.sort();
    assert_eq!(files, ["tone.m4a"]);
}

#[test]
fn playback_keys_act() {
    // rows A4, P9, P10, P11, P12, P13: AUDIO_KEYS of bin/ascii-viewer:136 at tag python-final: `space` and `p`, the seeks with `h j k l` (SEEKS), the speeds with `\` (SPEEDS), the volumes (VOLUMES), `m`, `L`, `f`, `Q`, with the steps, limits and toasts of video
    let sandbox = Sandbox::new();
    let clients = format!(
        r#"[{{"pid": {}, "address": "0x5ca1ab1e"}}]"#,
        std::process::id()
    );
    sandbox.stub_data("hyprctl.clients", &clients);
    let tone = tone(&sandbox, "tone.m4a", 70.0);
    let mut tty = sandbox.launch().arg(&tone).size(SMALL).start();
    tty.await_playing();
    tty.send(b" ");
    tty.await_status("| ");
    tty.send(DOWN);
    tty.await_status("| 00:00 / 01:10");
    // from the start forward by the step, the second shown the step's or, mpv a little short of it (the ignored test below), the one before; back to the start, held there
    let seeks: [(&[u8], &[u8], u32); 6] = [
        (b"l", b"h", 5),
        (RIGHT, LEFT, 5),
        (b"k", b"j", 60),
        (UP, DOWN, 60),
        (SHIFT_UP, SHIFT_DOWN, 5),
        (b"\x1b[1;2C\x1b[1;2C", b"\x1b[1;2D\x1b[1;2D", 2),
    ];
    for (forward, back, step) in seeks {
        tty.send(forward);
        tty.until(&format!("{step} s on after {}", shown(forward)), |screen| {
            let row = screen.row(24);
            row.starts_with("| ") && clock(&row).is_some_and(|at| at == step || at + 1 == step)
        });
        tty.send(back);
        tty.await_status("| 00:00 / 01:10");
    }
    let toasts: [(&[u8], &str); 16] = [
        (b"]", "speed 110%"),
        (b"[", "speed 100%"),
        (b"}", "speed 200%"),
        (b"\x7f", "speed 100%"),
        (b"{", "speed 50%"),
        (b"\x08", "speed 100%"),
        (b"}", "speed 200%"),
        (b"\\", "speed 100%"),
        (b"0", "volume 102"),
        (b"*", "volume 104"),
        (b"9", "volume 102"),
        (b"/", "volume 100"),
        (b"m", "muted"),
        (b"m", "unmuted"),
        (b"L", "loop on"),
        (b"L", "loop off"),
    ];
    for (keys, text) in toasts {
        says(&mut tty, keys, text);
    }
    tty.send(b"p");
    tty.await_status("> 00:0");
    tty.send(b"p");
    tty.await_status("| 00:0");
    tty.send(b"f");
    let calls = sandbox.await_noted("hyprctl.calls", "dispatch");
    assert_eq!(
        calls,
        "clients\n-j\n--\ndispatch\nhl.dsp.window.fullscreen({ mode = \"fullscreen\", window = \"address:0x5ca1ab1e\" })\n--\n"
    );
    tty.send(b"Q");
    assert_eq!(tty.exit(), 0);
}

#[test]
fn the_end_quits_and_l_loops() {
    // rows A5, P13: run of bin/ascii-viewer:2410 at tag python-final: the sound's end is the viewer's, exit 0; with `L` mpv loops the file (loop-file=inf, bin/ascii-viewer:2325), `L` again lets it end
    let sandbox = Sandbox::new();
    let short = tone(&sandbox, "short.m4a", 1.0);
    let mut tty = sandbox.launch().arg(&short).size(SMALL).start();
    tty.await_status("> 00:00 / 00:01");
    let start = Instant::now();
    assert_eq!(tty.exit(), 0);
    let took = start.elapsed();
    assert!(took > Duration::from_millis(500), "ended after {took:?}");
    let tone = tone(&sandbox, "tone.m4a", 4.0);
    let mut tty = sandbox.launch().arg(&tone).size(SMALL).start();
    tty.await_playing();
    says(&mut tty, b"L", "loop on");
    tty.await_status("> 00:03 / 00:04");
    tty.await_status("> 00:00 / 00:04");
    says(&mut tty, b"L", "loop off");
    assert_eq!(tty.exit(), 0);
}

#[test]
fn without_a_scope_it_plays_on() {
    // row A1a: run of bin/ascii-viewer:2396 at tag python-final: no `scope` to run: the sound plays, the status row and the keys work, the end quits
    let sandbox = Sandbox::new();
    sandbox.remove_program("scope");
    let tone = tone(&sandbox, "tone.m4a", 3.0);
    let mut tty = sandbox.launch().arg(&tone).size(SMALL).start();
    tty.await_playing();
    says(&mut tty, b"m", "muted");
    let at = tty.send(F1);
    tty.expect(at, b"any other key closes");
    tty.send(b"x");
    tty.await_status("> 00:0");
    assert_eq!(tty.exit(), 0);
}

#[test]
fn a_scope_that_ends_is_not_started_again() {
    // row A1a: the scope is started once (run of bin/ascii-viewer:2396 at tag python-final); one that ended stays ended: F1 stops nothing (no `ESC 7` before the keys), the viewer plays on to the end
    let sandbox = Sandbox::new();
    sandbox.program("scope", "#!/bin/sh\necho start >> \"$0.out\"\nexit 3\n");
    let tone = tone(&sandbox, "tone.m4a", 3.0);
    let mut tty = sandbox.launch().arg(&tone).size(SMALL).start();
    sandbox.await_noted("scope.out", "start");
    tty.await_playing();
    let at = tty.send(F1);
    tty.expect(at, KEYS_DRAWN);
    assert!(find(&tty.out(), &[b"\x1b7", KEYS_DRAWN].concat(), at).is_none());
    tty.send(b"x");
    tty.await_status("> 00:0");
    assert_eq!(tty.exit(), 0);
    assert_eq!(sandbox.noted("scope.out"), "start\n");
}

#[test]
fn help_stops_the_scope_and_pauses_until_it_closes() {
    // rows A6, H3, H5: open_help and help_key of bin/ascii-viewer:2060 and :2080 at tag python-final (tests/test_viewer.py:614): F1 pauses the sound, SIGSTOPs the scope, saves its cursor and colour (`ESC 7`) once it is stopped, then the keys; any other key clears the screen, restores them (`ESC 8`), SIGCONTs the scope and plays on; paused, it stays paused
    let sandbox = Sandbox::new();
    let tone = tone(&sandbox, "tone.m4a", 30.0);
    let mut tty = sandbox.launch().arg(&tone).size(SMALL).start();
    let scope = scope_pid(&sandbox);
    tty.await_playing();
    let at = tty.send(F1);
    tty.expect(at, [b"\x1b7", KEYS_DRAWN].concat());
    assert_eq!(stat(scope).unwrap()[0], "T", "stopped before the keys show");
    let draws = keys_drawn(&tty);
    assert_eq!(
        draws, 0,
        "the keys drawn {draws} times: it plays under them"
    );
    let held = seconds(writes(&tty.out(), 24).last().unwrap()).unwrap();
    let at = tty.send(b"x");
    let closed = tty.expect(at, b"\x1b[2J\x1b8");
    tty.expect(closed, under(24));
    await_stopped(scope, false);
    tty.until("playing on", |screen| {
        let row = screen.row(24);
        row.starts_with("> ") && clock(&row) > Some(held)
    });
    // paused, it stays paused under the keys and after them
    tty.send(b" ");
    tty.await_status("| ");
    let at = tty.send(F1);
    tty.expect(at, [b"\x1b7", KEYS_DRAWN].concat());
    assert_eq!(stat(scope).unwrap()[0], "T");
    let at = tty.send(b"x");
    tty.expect(at, b"\x1b[2J\x1b8");
    await_stopped(scope, false);
    tty.await_status("| ");
    // a window for what must not happen: playing on
    std::thread::sleep(Duration::from_millis(800));
    assert!(tty.screen().row(24).starts_with("| "));
    tty.send(b"q");
    assert_eq!(tty.exit(), 0);
}

#[test]
fn quitting_under_help_does_not_wait_on_the_stopped_scope() {
    // rows N14, A6, V5: SIGTERM or Ctrl-C while the keys show and the scope is stopped: it is continued and ended, exit 0, the terminal given back; the old viewer waited on the stopped scope for ever (A6a)
    let sandbox = Sandbox::new();
    let tone = tone(&sandbox, "tone.m4a", 30.0);
    for (what, key) in [("SIGTERM", None), ("Ctrl-C", Some(b"\x03"))] {
        // the run before's notes
        let _ = std::fs::remove_file(sandbox.path("bin/scope.out"));
        let mut tty = sandbox.launch().arg(&tone).size(SMALL).start();
        let scope = scope_pid(&sandbox);
        tty.await_playing();
        let at = tty.send(F1);
        tty.expect(at, b"any other key closes");
        await_stopped(scope, true);
        match key {
            Some(key) => {
                tty.send(key);
            }
            None => tty.signal(libc::SIGTERM),
        }
        let status = tty.exit_within(Duration::from_secs(10));
        assert_eq!(
            status.and_then(|status| status.code()),
            Some(0),
            "{what}: {status:?}"
        );
        assert!(
            tty.out().ends_with(RESTORE),
            "{what}: {}",
            shown(&tty.out())
        );
        assert!(
            sandbox.noted("scope.out").ends_with("term\n"),
            "{what}: {:?}",
            sandbox.noted("scope.out")
        );
        assert!(stat(scope).is_none(), "{what}: the scope is gone");
    }
}

#[test]
fn a_resize_writes_the_status_row_alone_on_the_new_bottom_row() {
    // row V10: draw of bin/ascii-viewer:2184 at tag python-final on SIGWINCH (tests/test_viewer_tty.py:143): the status row on the new bottom row, its bar to the new width, nothing else (the scope owns the rest)
    let sandbox = Sandbox::new();
    let tone = tone(&sandbox, "tone.m4a", 10.0);
    let mut tty = sandbox.launch().arg(&tone).size(SMALL).start();
    tty.await_status("> 00:03");
    tty.send(b" ");
    tty.await_status("| ");
    tty.settle(Duration::from_millis(400));
    let last = writes(&tty.out(), 24).pop().unwrap();
    let (text, cells) = (row_text(&last), bar(&last));
    assert!(
        text.starts_with("| 00:0") && text.ends_with(" / 00:10") && cells > 0,
        "{text:?}"
    );
    for (rows, cols) in [(30, 100), (10, 60)] {
        let before = tty.len();
        tty.resize(Size {
            rows,
            cols,
            xpx: cols * 9,
            ypx: rows * 20,
        });
        tty.expect(before, under(rows));
        tty.settle(Duration::from_millis(400));
        let after = tty.out()[before..].to_vec();
        let written = writes(&after, rows);
        let write = [under(rows), written[0].clone(), b"\x1b8".to_vec()].concat();
        // nothing but that row, written once or more
        assert_eq!(
            count(&after, &write) * write.len(),
            after.len(),
            "more than the status row: {}",
            shown(&after)
        );
        assert_eq!(row_text(&written[0]), text);
        // the same position in a room of cols - 16 cells instead of 64
        let room = cols as f64 - 16.0;
        let (low, high) = (
            (room * (cells as f64 - 0.5) / 64.0).round() as usize,
            (room * (cells as f64 + 0.5) / 64.0).round() as usize,
        );
        let now = bar(&written[0]);
        assert!(
            (low..=high).contains(&now),
            "{now} cells in {cols} columns for {cells} in 80"
        );
    }
    tty.send(b"q");
    assert_eq!(tty.exit(), 0);
}
