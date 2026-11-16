//! Video in the viewer: frame rate and the end, pause, seeks, frame steps, speed, volume, mute, loop, the status row, the look keys, where and how the picture is written, `s` and `r` on a frame, and sources other than plain clips: turned, MPEG-TS, variable rate, damaged, cover art, animations, one-frame media (inventory section 2, V17, V25).

mod common;

use common::screen::Screen;
use common::*;
use std::path::Path;
use std::time::{Duration, Instant};

const SHIFT_RIGHT: &[u8] = b"\x1b[1;2C";
const SHIFT_LEFT: &[u8] = b"\x1b[1;2D";
const SHIFT_UP: &[u8] = b"\x1b[1;2A";
const SHIFT_DOWN: &[u8] = b"\x1b[1;2B";

/// The frame a 16:9 counter clip shows in an 80 x 24 window, where the picture covers rows 2 to 22 and all 80 columns (the old viewer's placement): read on row 12.
fn frame(screen: &Screen) -> u32 {
    counted(screen, 12, 1, 80)
}

/// The seconds a status row shows (`> 01:05 / 02:10 ━━` is 65); `None` for a toast or a blank row.
fn seconds(row: &str) -> Option<u32> {
    let clock = row.get(2..)?.split(' ').next()?;
    let (minutes, seconds) = clock.split_once(':')?;
    Some(minutes.parse::<u32>().ok()? * 60 + seconds.parse::<u32>().ok()?)
}

/// What each draw in `out` showed in an 80 x 24 window: the status row's seconds and the counter's frame.
fn drawn(out: &[u8]) -> Vec<(Option<u32>, u32)> {
    replay(out, SMALL, |screen| {
        (seconds(&screen.row(24)), frame(screen))
    })
}

/// The status row and the counter's frame as the last whole update in the output left them, in an 80 x 24 window: each update erases the status row first and writes it last, so a look at the screen while one is under way finds the row blank (seen at load 82: `""` read after a window of quiet).
fn last_drawn(tty: &Tty) -> (String, u32) {
    let drawn = replay(&tty.out(), SMALL, |screen| (screen.row(24), frame(screen)));
    drawn.last().cloned().unwrap_or_default()
}

/// Each update in an 80 x 24 window that ends past byte `from`: when it came, the counter's frame and the status row.
fn updates(tty: &Tty, from: usize) -> Vec<(Instant, u32, String)> {
    let out = tty.out();
    let mut screen = Screen::new(24, 80);
    let (mut fed, mut seen) = (0, Vec::new());
    while let Some(start) = find(&out, b"\x1b[?2026h", fed) {
        let Some(close) = find(&out, b"\x1b[?2026l", start) else {
            break;
        };
        screen.feed(&out[fed..close + 8]);
        fed = close + 8;
        if fed > from {
            seen.push((tty.came(close).unwrap(), frame(&screen), screen.row(24)));
        }
    }
    seen
}

/// The clip's time a status row shows by its bar, finer than its clock: the bar's cells of its room (`cols` less the text and a space) times `length`; 0 before the bar's first cell.
fn bar_time(row: &str, cols: usize, length: f64) -> f64 {
    let text = row.trim_end_matches(['━', ' ']).chars().count();
    row.matches('━').count() as f64 * length / (cols - text - 1) as f64
}

/// The frames drawn while the status row showed `second`, each once.
fn frames_in(drawn: &[(Option<u32>, u32)], second: u32) -> Vec<u32> {
    let mut frames: Vec<u32> = drawn
        .iter()
        .filter(|(at, _)| *at == Some(second))
        .map(|(_, frame)| *frame)
        .collect();
    frames.dedup();
    frames
}

/// Pause playing media: `space`, the mark `|`, then quiet (frames decoded before the pause still land).
fn pause(tty: &mut Tty) {
    tty.send(b" ");
    tty.await_status("| ");
    tty.settle(Duration::from_millis(300));
}

/// `s`: the frame its picture holds, once saved. mpv takes it once it has done the seeks and steps asked before (N5: the frame mpv holds).
fn taken(tty: &mut Tty, sandbox: &Sandbox) -> u32 {
    let at = tty.send(b"s");
    let start = tty.expect(at, b"\x1b[7m saved ");
    let end = tty.expect(start, b" \x1b[0m") - 5;
    let name = String::from_utf8(tty.out()[start..end].to_vec()).unwrap();
    saved(&sandbox.path(name)).0
}

/// Pause at the start, then go to frame 0: under load a clip of a few seconds ends before a key sent once it plays on (seen: the viewer gone before the key, the terminal echoing it after the restore). `space` goes once the first synchronized update is out, which the viewer writes as it starts to play, before its first frame, and after it took the terminal (keys typed before that are dropped, V3; the old viewer's first update comes after that too); sent once the first frame showed, after a start slow under load, it came too late for a 2.2 s clip (the review: 3 of 6 runs at load 105, the first rows seen 1.2 to 3.5 s after the launch at load 62). Keys sent before the viewer read the ones before can reach it in the same read and add up with them (V14): Left and two Shift+Right landed on 0 (the review's probe of this shape: 7 of 8 copies at load 38-54, and the stress run's 5 of 44). The pause can land on frame 0, where Left changes nothing on screen and a redraw of frame 0 tells nothing of it, so the screen is moved off frame 0 first, by Shift+Right; Left then lands on 0 from there, alone or with it.
fn paused_at_the_start(tty: &mut Tty) {
    tty.expect(0, b"\x1b[?2026l");
    tty.send(b" ");
    tty.await_status("| ");
    // frames decoded before the pause still land
    tty.settle(Duration::from_millis(300));
    let at = tty.send(SHIFT_RIGHT);
    let off = |screen: &Screen| frame(screen) != 0 && screen.row(24).starts_with("| ");
    if !tty.wait(at, PATIENCE, |out| replay(out, SMALL, off).contains(&true)) {
        tty.fail("no draw off frame 0 after Shift+Right");
    }
    let at = tty.send(LEFT);
    let start = |screen: &Screen| frame(screen) == 0 && screen.row(24).starts_with("| 00:00");
    if !tty.wait(at, PATIENCE, |out| {
        replay(out, SMALL, start).contains(&true)
    }) {
        tty.fail("no draw of frame 0 after Left");
    }
}

/// Send `keys`, then wait for the toast `text`.
fn says(tty: &mut Tty, keys: &[u8], text: &str) {
    let at = tty.send(keys);
    tty.expect(at, toast(text));
}

/// Send `keys`, then wait until the counter shows `number` (mod 256) and the status row starts with `status`.
fn lands(tty: &mut Tty, keys: &[u8], number: u32, status: &str) {
    tty.send(keys);
    let what = format!("frame {number} and {status:?} after {}", shown(keys));
    tty.until(&what, |screen| {
        frame(screen) == number % 256 && screen.row(24).starts_with(status)
    });
}

/// The picture rows of the last synchronized update in `out` that writes any: each row's place (row, column) and its bytes up to the closing SGR 0.
fn written_rows(out: &[u8]) -> Vec<(usize, usize, Vec<u8>)> {
    let (mut last, mut at) = (Vec::new(), 0);
    while let Some(start) = find(out, b"\x1b[?2026h", at) {
        let Some(end) = find(out, b"\x1b[?2026l", start) else {
            break;
        };
        let update = &out[start..end];
        let (mut rows, mut from) = (Vec::new(), 0);
        while let Some(csi) = find(update, b"\x1b[", from) {
            from = csi + 2;
            let tail = &update[from..];
            let digits = tail
                .iter()
                .take_while(|b| b.is_ascii_digit() || **b == b';')
                .count();
            let place = String::from_utf8_lossy(&tail[..digits]).into_owned();
            let (Some(&b'H'), Some((row, col))) = (tail.get(digits), place.split_once(';')) else {
                continue;
            };
            // a picture row: bold or a colour code first; the status row, its erasing and the toast start otherwise
            let body = &tail[digits + 1..];
            let picture = [&b"\x1b[1m"[..], b"\x1b[3", b"\x1b[9"]
                .iter()
                .any(|start| body.starts_with(start));
            if let (true, Some(close)) = (picture, find(body, b"\x1b[0m", 0)) {
                rows.push((
                    row.parse().unwrap(),
                    col.parse().unwrap(),
                    body[..close].to_vec(),
                ));
            }
        }
        if !rows.is_empty() {
            last = rows;
        }
        at = end;
    }
    last
}

/// The SGR sequences in a row's bytes, and its text without them.
fn split_codes(row: &[u8]) -> (Vec<String>, String) {
    let (mut codes, mut text, mut at) = (Vec::new(), String::new(), 0);
    while at < row.len() {
        if row[at..].starts_with(b"\x1b[") {
            let end = at + row[at..].iter().position(|&b| b == b'm').unwrap() + 1;
            codes.push(String::from_utf8_lossy(&row[at..end]).into_owned());
            at = end;
        } else {
            text.push(row[at] as char);
            at += 1;
        }
    }
    (codes, text)
}

/// The number a counter frame shows in a picture `s` saved (a bar lit where its middle is brighter than the theme's background), and the picture's size.
fn saved(path: &Path) -> (u32, (u32, u32)) {
    let picture = image::open(path).unwrap().to_rgb8();
    let (w, h) = picture.dimensions();
    let lit = |bar: u32| {
        let (left, right) = (w * bar / 8 + w / 32, w * (bar + 1) / 8 - w / 32);
        let mut sum = 0u64;
        for y in h * 3 / 8..h * 5 / 8 {
            for x in left..right {
                sum += picture.get_pixel(x, y)[1] as u64;
            }
        }
        sum / ((right - left) as u64 * (h / 4) as u64) > 50
    };
    (
        (0..8).filter(|&bar| lit(bar)).map(|bar| 1 << bar).sum(),
        (w, h),
    )
}

#[test]
fn frames_come_at_the_clips_rate_and_the_end_quits() {
    // rows P2, P14, P16, N26: play_rate and run of bin/ascii-viewer:431 and :2386 at tag python-final: 25 frames a second, exit 0 at the end, past the clip's 3 s: the run lasts more than 2.5 s from its launch, as mpv plays no faster than the clock (taken from the frame on screen instead, which lags mpv under load, the end measured 2.46 s at 166 runnable tasks), and at most 10 s into the clip by that frame; run in this harness the old viewer drew all 25 frames of each second; the status row's bar, room x position / length cells, shows the time of the frame on screen (N26)
    let sandbox = Sandbox::new();
    let video = counter(&sandbox, "clip.mp4", "160x90", 25, 3.0, 3.0, &[]);
    let launched = Instant::now();
    let mut tty = sandbox.launch().arg(&video).size(SMALL).start();
    tty.await_playing();
    // the clip's time then, by the frame on screen: a test thread late under load sees a later frame (a start taken here instead gave 1.36 s from 00:01 to the end in a run at load 45)
    let (start, from) = (Instant::now(), last_drawn(&tty).1);
    assert_eq!(tty.exit(), 0);
    let (ran, took) = (
        launched.elapsed().as_secs_f64(),
        from as f64 / 25.0 + start.elapsed().as_secs_f64(),
    );
    assert!(
        ran > 2.5 && took < 10.0,
        "ended {ran:.2} s after the launch, {took:.2} s into the clip by frame {from}"
    );
    // each draw: the clock by the bar (3/64 s a cell) against the frame it shows, outside the clip's last 0.5 s, where the clock is the sound's past the last frame drawn (seen: frame 70 under the whole bar); frame less clock seen from -0.14 to +0.02 s in 28 runs at load 19 to 48, and from -0.08 to +0.02 s in 38 runs at load 30 to 55 in review
    let rows = replay(&tty.out(), SMALL, |screen| (screen.row(24), frame(screen)));
    for (row, number) in &rows {
        let time = *number as f64 / 25.0;
        if seconds(row).is_some() && time < 2.5 {
            let clock = bar_time(row, 80, 3.0);
            assert!(
                (time - clock).abs() < 0.3,
                "frame {number} ({time:.2} s) drawn at {clock:.2} s: {row:?}"
            );
        }
    }
    let drawn = drawn(&tty.out());
    // no more frames than the clip's own: a debug build among the other tests drew 1 to 25 of a second's 25, as loaded as the machine was
    let second = frames_in(&drawn, 1);
    assert!(
        second.windows(2).all(|pair| pair[0] < pair[1]),
        "{second:?}"
    );
    assert!(
        (1..=27).contains(&second.len()),
        "{} frames in a second: {second:?}",
        second.len()
    );
    assert!(
        drawn.last().is_some_and(|(_, number)| *number >= 50),
        "the last frames before the end: {drawn:?}"
    );
}

/// A window of 32 x 9 cells: a debug build draws a frame in it faster than the cap lets it, under load too. The counter covers rows 1 to 8 and all 32 columns.
const QUICK: Size = Size {
    rows: 9,
    cols: 32,
    xpx: 288,
    ypx: 180,
};

/// The frames first drawn while the status row showed `second`, in the order drawn: a frame on screen when the clock turns shows again under the next second, and counts where it showed first.
fn first_drawn_in(drawn: &[(Option<u32>, u32)], second: u32) -> Vec<u32> {
    let mut before = None;
    let mut first = Vec::new();
    for &(at, number) in drawn {
        if before != Some(number) && at == Some(second) {
            first.push(number);
        }
        before = Some(number);
    }
    first
}

#[test]
fn a_faster_clip_is_drawn_at_30_frames_a_second_at_most() {
    // rows P2, P24: MAX_FPS of bin/ascii-viewer:106 at tag python-final: of a 60 fps source at most 30 frames a second show, the others passed over (the old viewer drew every other one); the new one draws a frame at most every 1/30 s. With sound the clock is ao=null's, at the wall clock's pace, and in this window a debug build draws as often as the cap lets it: each of seconds 1 to 3 holds 30 frames first drawn in it at most (seen: 30, and 33 or 34 each with the gap at 30 ms); a 4th second would wrap the counter's 8 bits
    let sandbox = Sandbox::new();
    let video = counter(&sandbox, "fast.mp4", "160x90", 60, 5.0, 5.0, &[]);
    let mut tty = sandbox.launch().arg(&video).size(QUICK).start();
    assert_eq!(tty.exit(), 0);
    let drawn = replay(&tty.out(), QUICK, |screen| {
        (seconds(&screen.row(9)), counted(screen, 4, 1, 32))
    });
    for second in 1..=3 {
        let first = first_drawn_in(&drawn, second);
        assert!(
            (1..=30).contains(&first.len()),
            "{} frames first drawn in second {second}: {first:?}",
            first.len()
        );
        assert!(first.windows(2).all(|pair| pair[0] < pair[1]), "{first:?}");
    }
}

#[test]
fn a_stream_without_times_plays_at_25_frames_a_second() {
    // row P2: play_rate of bin/ascii-viewer:431 at tag python-final: 25 frames a second when the stream gives no rate. A raw HEVC stream without timing information, of frames made at 10 a second, holds no time (ffprobe: no duration, the raw demuxer's default rate 25/1), so mpv makes the frames' times up at 25 a second (`No video PTS! Making something up. Using 25.000000 FPS.`, seen from mpv itself): frame n shows under the clock of n/25 s, give or take the frame or so mpv's report of the clock and the frame drawn are apart (at 10 a second frame 10 would show at 00:01, at 30 frame 30). By the wall clock 75 to 85 frames took 3.0 to 3.5 s in 11 runs at load 10 to 29, and 80 took 4.1 s in one heavier: a loaded machine slows mpv's own pace, so the test reads the times mpv made up. 240 frames play 9.6 s, the counter's 8 bits all the way, and the clock's seconds 1 to 8 hold the draws read: a viewer slow under load draws few (11 in 3 s at 80 x 24 under load 50, 6 in 3 s in this window at load 99, the stress run's). A raw MJPEG stream would do as well, but mpv does not open one (probe score 25, mpv takes 26 and up). The old viewer ends on a raw stream with a traceback: its decoder seeks first, which such a stream does not let it (a_stream_without_a_length_shows_its_clock_alone)
    let sandbox = Sandbox::new();
    let raw = sandbox.path("clip.hevc");
    let picture = format!("nullsrc=s=160x90:r=10:d=24,{COUNTER}");
    ffmpeg(
        &sandbox,
        &[
            "-f",
            "lavfi",
            "-i",
            &picture,
            "-c:v",
            "libx265",
            "-x265-params",
            "vui-timing-info=0:log-level=error",
            "-g",
            "5",
            "-pix_fmt",
            "yuv420p",
            "-f",
            "hevc",
            raw.to_str().unwrap(),
        ],
    );
    let probed = probe(&sandbox, &raw, "stream=avg_frame_rate,duration", None);
    assert_eq!(probed, "avg_frame_rate=25/1\nduration=N/A\n");
    let mut tty = sandbox.launch().arg(&raw).size(QUICK).start();
    assert_eq!(tty.exit(), 0);
    // the draws while the clock showed 00:01 to 00:08: each frame within the second the clock shows, 3 frames either side
    let drawn: Vec<(u32, u32)> = replay(&tty.out(), QUICK, |screen| {
        (seconds(&screen.row(9)), counted(screen, 4, 1, 32))
    })
    .into_iter()
    .filter_map(|(at, number)| at.filter(|at| (1..=8).contains(at)).map(|at| (at, number)))
    .collect();
    let inside = drawn
        .iter()
        .filter(|&&(at, number)| number + 3 >= at * 25 && number < at * 25 + 28)
        .count();
    assert!(
        drawn.len() >= 12 && inside * 10 >= drawn.len() * 9,
        "{inside} of {} draws within their second: {drawn:?}",
        drawn.len()
    );
}

#[test]
fn late_frames_are_passed_over_and_the_picture_keeps_up_with_the_sound() {
    // row P4: run of bin/ascii-viewer:2413 at tag python-final shows a frame when the audio clock reaches its time and drops one more than a period late. The debug build draws a 200 x 60 window far slower than the clip's 25 frames a second (about 4 a second seen, 1 under load), so most frames of this 10 s clip are left out (run in this harness the old viewer drew 203 of the 250, and was at the end with the sound too), and the picture keeps up with the sound: when the sound ends the last frame drawn is near the clip's end, where frames shown in turn at the drawing's pace would be some 40 frames in; src/mpv.rs late_frames_are_dropped_and_the_picture_keeps_the_sounds_clock pins the sync itself. By the wall clock the frames drawn ran at 24.8 to 25.9 a second at load 10 to 16, but with three or four of this file's runs at once at 21 to 33: 2 to 8 draws in 6 s, each timed when the test thread saw it (by inference the spread)
    let sandbox = Sandbox::new();
    let video = counter(&sandbox, "clip.mp4", "160x90", 25, 10.0, 10.0, &[]);
    let mut tty = sandbox.view(&video);
    assert_eq!(tty.exit(), 0);
    // the picture covers rows 5 to 55 of the window
    let mut drawn = replay(&tty.out(), WIDE, |screen| counted(screen, 30, 1, 200));
    drawn.dedup();
    let last = drawn.last().copied().unwrap_or(0);
    assert!(last >= 150, "the last frame drawn: {drawn:?}");
    if cfg!(debug_assertions) {
        assert!(
            drawn.len() < 225,
            "{} of 250 frames drawn: few late",
            drawn.len()
        );
    }
}

#[test]
fn space_and_p_pause_and_play_on() {
    // row P8: bin/ascii-viewer:2270 at tag python-final: `space` and `p` pause and play; after `,` play goes on from the frame on screen, here at 25 %, a frame each 160 ms, so that a viewer slow under load draws the frames after it too (at 100 % one drew frame 30 first)
    let sandbox = Sandbox::new();
    let video = counter(&sandbox, "clip.mp4", "160x90", 25, 10.0, 10.0, &[]);
    let mut tty = sandbox.launch().arg(&video).size(SMALL).start();
    tty.await_playing();
    for key in [&b" "[..], b"p"] {
        tty.send(key);
        tty.await_status("| ");
        // a frame of a paused seek: none decoded before the pause can land after it
        lands(&mut tty, DOWN, 0, "| 00:00");
        lands(&mut tty, SHIFT_RIGHT, 25, "| 00:01");
        // a window for what must not happen: the clock or the picture moving on
        std::thread::sleep(Duration::from_millis(600));
        let (status, number) = last_drawn(&tty);
        assert!(status.starts_with("| 00:01 / 00:10"), "{status:?}");
        assert_eq!(number, 25);
        tty.send(key);
        tty.until("playing on", |screen| {
            screen.row(24).starts_with("> ") && frame(screen) > 25
        });
    }
    tty.send(b" ");
    tty.await_status("| ");
    lands(&mut tty, DOWN, 0, "| 00:00");
    lands(&mut tty, SHIFT_RIGHT, 25, "| 00:01");
    lands(&mut tty, b",", 24, "| 00:00");
    says(&mut tty, b"{{", "speed 25%");
    let (from, sent) = (tty.send(b" "), Instant::now());
    tty.until("playing on", |screen| {
        screen.row(24).starts_with("> ") && frame(screen) > 24
    });
    // a frame each 160 ms from frame 24 on: the first one drawn after it by the time the test saw it, at least as late as the viewer drew it (under load 112 the first came about 1 s on: frame 30, the stress run's)
    let seen = sent.elapsed().as_secs_f64();
    let next = replay(&tty.out()[from..], SMALL, frame)
        .into_iter()
        .find(|number| *number != 24)
        .unwrap();
    let latest = 26 + (seen / 0.16) as u32;
    assert!(
        (25..=latest).contains(&next),
        "on from frame 24 to {next}, seen {seen:.2} s after `space`"
    );
    tty.send(b"q");
    assert_eq!(tty.exit(), 0);
}

#[test]
fn space_right_after_the_start_pauses_too() {
    // row P8: at its first status row the clip plays; `space` then must pause it as later; run in this harness the old viewer showed `| 00:00 / 00:30` 1.5 s after it (6 of 6 runs)
    let sandbox = Sandbox::new();
    let video = clip(&sandbox, "clip.mp4", 30.0, "160x90", 25);
    let mut tty = sandbox.launch().arg(&video).size(SMALL).start();
    tty.until("the first status row", |screen| !screen.row(24).is_empty());
    tty.send(b" ");
    // a window for what must not happen: playing on
    std::thread::sleep(Duration::from_secs(2));
    let (status, _) = last_drawn(&tty);
    assert!(status.starts_with("| 00:00"), "{status:?}");
    tty.send(b"q");
    assert_eq!(tty.exit(), 0);
}

#[test]
fn two_spaces_in_one_batch_leave_it_as_it_was() {
    // row P8: each `space` toggles what the one before asked for, as bin/ascii-viewer:2270 at tag python-final flips its own flag; mpv may report neither of two requests so close
    let sandbox = Sandbox::new();
    let video = counter(&sandbox, "clip.mp4", "160x90", 25, 10.0, 10.0, &[]);
    let mut tty = sandbox.launch().arg(&video).size(SMALL).start();
    tty.await_playing();
    let from = last_drawn(&tty).1;
    tty.send(b"  ");
    tty.until("playing on", |screen| {
        screen.row(24).starts_with("> ") && frame(screen) > from + 25
    });
    pause(&mut tty);
    lands(&mut tty, DOWN, 0, "| 00:00");
    tty.send(b"  ");
    // a window for what must not happen: the clock or the picture moving on
    std::thread::sleep(Duration::from_millis(600));
    let (status, number) = last_drawn(&tty);
    assert!(status.starts_with("| 00:00"), "{status:?}");
    assert_eq!(number, 0);
    tty.send(b"q");
    assert_eq!(tty.exit(), 0);
}

#[test]
fn no_key_stops_the_clip() {
    // row P8a: the stopped state, mark `.`, is halt of bin/ascii-viewer:1304 at tag python-final, which MPRIS Stop alone calls (tests/mpris.rs stop_holds_the_frame_until_a_seek_or_space_plays): every key of the video table but q and Q, each sent once the one before acted, and no status row shows `.`; the paused mark `|` and the playing `>` both show
    let sandbox = Sandbox::new();
    // 10 frames a second: a frame's number is its time in tenths (mod 256)
    let video = counter(&sandbox, "long.mp4", "160x90", 10, 70.0, 70.0, &[]);
    let mut tty = sandbox.launch().arg(&video).size(SMALL).start();
    tty.await_playing();
    let at = tty.send(F1);
    tty.expect(at, b"any other key closes");
    tty.send(b"x");
    tty.await_status("> ");
    for (key, mark) in [(b" ", "| "), (b"p", "> "), (b" ", "| ")] {
        tty.send(key);
        tty.await_status(mark);
    }
    let seeks: [(&[u8], u32); 13] = [
        (DOWN, 0),
        (UP, 600),
        (DOWN, 0),
        (b"k", 600),
        (b"j", 0),
        (RIGHT, 50),
        (LEFT, 0),
        (b"l", 50),
        (b"h", 0),
        (SHIFT_UP, 50),
        (SHIFT_DOWN, 0),
        (SHIFT_RIGHT, 10),
        (SHIFT_LEFT, 0),
    ];
    for (keys, tenths) in seeks {
        let status = format!("| {:02}:{:02} / 01:10", tenths / 600, tenths / 10 % 60);
        lands(&mut tty, keys, tenths, &status);
    }
    tty.send(b"p");
    tty.await_status("> ");
    let toasted: [(&[u8], &str); 33] = [
        (b"]", "speed 110%"),
        (b"[", "speed 100%"),
        (b"}", "speed 200%"),
        (b"{", "speed 100%"),
        (BACKSPACE, "speed 100%"),
        (b"\x08", "speed 100%"),
        (b"\\", "speed 100%"),
        (b"0", "volume 102"),
        (b"*", "volume 104"),
        (b"9", "volume 102"),
        (b"/", "volume 100"),
        (b"m", "muted"),
        (b"m", "unmuted"),
        (b"L", "loop on"),
        (b"L", "loop off"),
        (b"3", "level 5% darker"),
        (b"4", "level 0"),
        (b"1", "contrast -5%"),
        (b"2", "contrast 0"),
        (b"3", "level 5% darker"),
        (b"`", "level 0, contrast 0"),
        (b"7", "saturation 90%"),
        (b"8", "saturation 100%"),
        (b"c", "mono"),
        (b"c", "colour"),
        (b"b", "bold"),
        (b"b", "regular"),
        (b"g", "glyphs: density"),
        (b"g", "glyphs: shape"),
        (b"g", "glyphs: mixed"),
        (b"s", "saved "),
        // the keys with no video action, then one that shows they were read
        (b"HJK+=-vyduD", ""),
        (b"m", "muted"),
    ];
    for (keys, text) in toasted {
        if text.is_empty() {
            tty.send(keys);
        } else {
            let at = tty.send(keys);
            tty.expect(at, format!("\x1b[7m {text}"));
        }
    }
    tty.send(b"f");
    sandbox.await_noted("hyprctl.calls", "clients");
    tty.send(b".");
    tty.await_status("| ");
    tty.send(b",");
    tty.await_status("| ");
    tty.send(b" ");
    tty.await_status("> ");
    tty.send(b"r");
    tty.until("the page", |screen| screen.row(1).starts_with(" Render"));
    tty.send(b"r");
    tty.await_status("> ");
    tty.send(b"q");
    assert_eq!(tty.exit(), 0);
    let marks: Vec<String> = replay(&tty.out(), SMALL, |screen| screen.row(24))
        .into_iter()
        .filter_map(|row| row.get(..2).map(str::to_string))
        .collect();
    assert!(!marks.iter().any(|mark| mark == ". "), "{marks:?}");
    assert!(
        ["> ", "| "]
            .iter()
            .all(|mark| marks.iter().any(|seen| seen == mark))
    );
}

#[test]
fn seeks_move_by_their_steps_and_stop_at_both_ends() {
    // row P9: SEEKS and seek_to of bin/ascii-viewer:126 and :1423 at tag python-final: Right Left l h 5 s, Up Down k j 60 s, Shift+Right Shift+Left 1 s, Shift+Up Shift+Down 5 s, exact, clamped to the clip; paused, the frame there shows and the mark stays `|`
    let sandbox = Sandbox::new();
    // 10 frames a second for 130.5 s: a frame's number is its time in tenths (mod 256), the last frame 1304
    let video = counter(&sandbox, "long.mp4", "160x90", 10, 130.5, 0.0, &[]);
    let mut tty = sandbox.launch().arg(&video).size(SMALL).start();
    tty.await_playing();
    pause(&mut tty);
    let steps: [(&[u8], u32); 17] = [
        // the pause lands where mpv is as it reads the key, under load seconds in (the stress run: 5.4 to 9.4 s, `space` read 8.2 to 14.6 s after the launch at load 127 to 173), so Down first, held at the start
        (DOWN, 0),
        (SHIFT_RIGHT, 10),
        (LEFT, 0), // from 1 s: held at the start
        (SHIFT_RIGHT, 10),
        (RIGHT, 60),
        (b"l", 110),
        (SHIFT_UP, 160),
        (UP, 760),
        (SHIFT_LEFT, 750),
        (SHIFT_DOWN, 700),
        (b"h", 650),
        (LEFT, 600),
        (DOWN, 0),
        (b"k", 600),
        (b"j", 0),
        // three in one write: held at the last frame (that a batch adds up to one seek, row V14, needs a target short of the end)
        (b"kkk", 1304),
        (b"jjj", 0),
    ];
    for (keys, tenths) in steps {
        let status = format!("| {:02}:{:02} / 02:10", tenths / 600, tenths / 10 % 60);
        lands(&mut tty, keys, tenths, &status);
    }
    tty.send(b"q");
    assert_eq!(tty.exit(), 0);
}

#[test]
fn frame_steps_move_one_frame_and_pause() {
    // rows P15, N2: frame_step of bin/ascii-viewer:1441 at tag python-final: `.` a frame on, `,` a frame back, then paused, a playing clip paused first, one or two frames after the one on screen at the key (a few more under the accepted limit); `,` on the first frame and `.` on the last only pause; the clock follows the frame; `,` right after a seek is mpv's back step (N2); seen before `.` on a playing clip went from the frame drawn: 1 of 8 runs at load 20 one or two frames on, the others 5 to 21, mpv stepping from the frame it held, past the one drawn under load
    let sandbox = Sandbox::new();
    // frames 0 to 254, all under the counter's wrap; a clip of 2.2 s ended before `.` landed at load 55 (the review: 2 of 18 copies)
    let video = counter(&sandbox, "clip.mp4", "160x90", 25, 10.2, 10.2, &[]);
    let mut tty = sandbox.launch().arg(&video).size(SMALL).start();
    // `.` once playback runs on steadily: from frame 25, past start-up, where frames left out are the rule under load, and once a playing update shows the frame after the one before it, from frame 75 on in any case; sent at frame 3, a step 3 frames past passed by the limit's sign in 3 of 8 runs at load 22 to 33 (the review's variant)
    let before = std::cell::Cell::new(None);
    tty.until_drawn("a steady playing update", 0, |screen| {
        let on = frame(screen);
        let playing = screen.row(24).starts_with("> ");
        let last = if playing {
            before.replace(Some(on))
        } else {
            None
        };
        playing && on >= 25 && (last == Some(on.wrapping_sub(1)) || on >= 75)
    });
    let sent = Instant::now();
    tty.send(b".");
    tty.await_status("| ");
    // the frame on screen at the key: the last drawn playing before the pause (the test reads the screen late under load; a `| ` can show at the start, as mpv reports the open's pause late)
    let draws = updates(&tty, 0);
    let paused = draws
        .iter()
        .position(|(came, _, row)| *came >= sent && row.starts_with("| "));
    let playing: Vec<_> = draws[..paused.unwrap()]
        .iter()
        .filter(|(_, _, row)| row.starts_with("> "))
        .collect();
    let on = playing.last().unwrap().1;
    // the accepted limit (owner 2026-11-13 15:40, its first): a viewer that takes over 200 ms for a frame, or frames mpv drops late, leave the time of the frame on screen unknown, and `.` can land a few frames past it (the stress run: 4 past at load 49); that shows in the last two frames drawn, over 200 ms apart or frames left out between them, and lets `.` go past by the frames mpv played meanwhile
    let few = match playing[..] {
        [.., (before, earlier, _), (last, _, _)] => {
            let (apart, skipped) = (*last - *before, on.saturating_sub(*earlier));
            let played = (apart.as_secs_f64() * 25.0).ceil() as u32;
            if apart > Duration::from_millis(200) || skipped > 2 {
                skipped.max(played)
            } else {
                0
            }
        }
        _ => 0,
    };
    // the frame mpv holds then, which `s` takes and the screen comes to (N5: the old viewer's `s` drew a frame anew at its time; run in this harness, `.` sent on frame 25 showed 27 and its `s` took 28, 3 of 3)
    let held = taken(&mut tty, &sandbox);
    assert!(
        held > on && held <= on + 2 + few,
        "frame {on} on screen, {held} after `.` (the last two drawn: {:?})",
        playing[playing.len().saturating_sub(2)..]
            .iter()
            .map(|(_, frame, _)| frame)
            .collect::<Vec<_>>()
    );
    tty.until("the frame after it", |screen| frame(screen) == held);
    // to the start from anywhere under a minute: Left, 5 s back, missed it from a pause past 5 s (this test's first form beside a load round: 9 of 11 runs at load 70 to 100)
    lands(&mut tty, DOWN, 0, "| 00:00");
    // on the first frame `,` only pauses; a window for what must not happen: the clip moving on
    tty.send(b",");
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(last_drawn(&tty), ("| 00:00 / 00:10".to_string(), 0));
    lands(&mut tty, SHIFT_RIGHT, 25, "| 00:01");
    for (key, number, status) in [
        (b",", 24, "| 00:00"),
        (b",", 23, "| 00:00"),
        (b".", 24, "| 00:00"),
        (b".", 25, "| 00:01"),
    ] {
        lands(&mut tty, key, number, status);
    }
    lands(&mut tty, b"l", 150, "| 00:06");
    lands(&mut tty, &SHIFT_RIGHT.repeat(4), 250, "| 00:10");
    for number in 251..=254 {
        lands(&mut tty, b".", number, "| 00:10");
    }
    // on the last frame `.` only pauses
    tty.send(b".");
    // a window for what must not happen: the picture moving on, or the run ending
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(frame(tty.screen()), 254);
    assert!(tty.running());
    lands(&mut tty, DOWN, 0, "| 00:00");
    tty.send(b"q");
    assert_eq!(tty.exit(), 0);
}

#[test]
fn a_step_on_the_last_frame_keeps_its_place() {
    // rows P15, N21: `.` on the last frame only pauses (frame_step of bin/ascii-viewer:1441 at tag python-final returns there, its clock on that frame): the status row on frame 54 of this 2.2 s clip with 2.2 s of sound shows 2.16 s (63 of the bar's 64 cells) and stays so, and `,` after it shows frame 53; seen before the fix: 54 again, the clock at the length with the whole bar; with 6 s of sound (N21) the same, 23 cells, seen before a step back went to where the step forward started: 54 again, the bar falling to 21 cells
    for (audio, cells) in [(2.2, 63), (6.0, 23)] {
        let sandbox = Sandbox::new();
        let video = counter(&sandbox, "clip.mp4", "160x90", 25, 2.2, audio, &[]);
        let mut tty = sandbox.launch().arg(&video).size(SMALL).start();
        paused_at_the_start(&mut tty);
        lands(&mut tty, b"\x1b[1;2C\x1b[1;2C", 50, "| 00:02");
        for number in 51..=54 {
            lands(&mut tty, b".", number, "| 00:02");
        }
        // mpv plays the sound out, silent, then seeks past the end; the status row as the last whole draw left it, as a read under way finds it erased (seen at load 19: the row read blank)
        let status = |tty: &Tty| replay(&tty.out(), SMALL, |screen| screen.row(24)).pop();
        tty.until("the clock on the last frame", |screen| {
            screen.row(24).matches('━').count() == cells
        });
        tty.settle(Duration::from_millis(300));
        let on_it = status(&tty).unwrap();
        assert_eq!(on_it.matches('━').count(), cells, "{audio}: {on_it:?}");
        tty.send(b".");
        // a window for what must not happen: the clock moving on
        std::thread::sleep(Duration::from_millis(1000));
        assert_eq!(status(&tty), Some(on_it), "{audio}");
        tty.send(b",");
        // the step back's chain, a pause, a seek past the end and mpv's step back, each with its draw, took over 5 s at load 85 (beside a load round: 1 of 11 runs)
        let what = format!("frame 53 after `,`, {audio} s of sound");
        tty.until(&what, |screen| frame(screen) == 53);
        tty.send(b"q");
        assert_eq!(tty.exit(), 0);
    }
}

#[test]
fn a_step_back_soon_after_a_step_onto_the_last_frame_shows_the_frame_before() {
    // rows P15, N2: `,` 50 to 250 ms after `.` onto the last frame, frame 54 of this 2.2 s clip with 2.5 s of sound, which the step plays out, shows frame 53; seen before a step back went to where the step forward started: 54 each time, and 53 at 400 ms
    let sandbox = Sandbox::new();
    let video = counter(&sandbox, "clip.mp4", "160x90", 25, 2.2, 2.5, &[]);
    let mut tty = sandbox.launch().arg(&video).size(SMALL).start();
    paused_at_the_start(&mut tty);
    lands(&mut tty, b"\x1b[1;2C\x1b[1;2C", 50, "| 00:02");
    for number in 51..=53 {
        lands(&mut tty, b".", number, "| 00:02");
    }
    for wait in [50, 150, 250] {
        let at = tty.send(b".");
        // the keys' timing, not a wait for the viewer
        std::thread::sleep(Duration::from_millis(wait));
        tty.send(b",");
        // the end of what the keys set going: frame 54, mpv playing the sound out, a seek past the end, then the step back, each stage with its draw, 0.5 to 0.9 s apart under load (the stress run: read in the middle of it at 151 to 182 runnable tasks, 54 at 2.16 s); frame 53 had been on screen before the keys
        let back = |out: &[u8]| {
            let frames = replay(out, SMALL, frame);
            let onto = frames.iter().position(|number| *number == 54);
            onto.is_some_and(|onto| frames[onto..].contains(&53))
        };
        if !tty.wait(at, PATIENCE, back) {
            tty.fail(&format!("{wait} ms: no frame 53 after 54"));
        }
        // a window for what must not happen: moving on from it
        tty.settle(Duration::from_millis(600));
        let status = tty.screen().row(24);
        assert_eq!(frame(tty.screen()), 53, "{wait} ms: {status:?}");
    }
    tty.send(b"q");
    assert_eq!(tty.exit(), 0);
}

#[test]
fn a_seek_and_a_step_in_one_write_end_paused_as_the_old_viewer_left_them() {
    // rows P9, P15, N5: seek_to and frame_step of bin/ascii-viewer:1423 and :1441 at tag python-final did each key before the next; run in this harness, read on the screen (the frame `s` takes, after the keys, is mpv's, N5: the old viewer's `s` took 150 for `l.`), paused on frame 25, the old viewer showed `l.` and `.l` 151, `l,` and `,l` 149, `..,` 26, `....` 29, paused; seen before the fix: `l.` and `.l` played on to the clip's end with the mark `|` and the run going on there, `l,` 24, `,l` 125, `..,` 24; `....` and `s`, a step taken for done at its mark before mpv had run it: `s` took 28 (the review's trace under load: 2 of 10 copies)
    let sandbox = Sandbox::new();
    let video = counter(&sandbox, "clip.mp4", "160x90", 25, 20.0, 20.0, &[]);
    let mut tty = sandbox.launch().arg(&video).size(SMALL).start();
    tty.await_playing();
    pause(&mut tty);
    for (keys, number, clock) in [
        (&b"l."[..], 151, "| 00:06"),
        (b".l", 151, "| 00:06"),
        (b"l,", 149, "| 00:05"),
        (b",l", 149, "| 00:05"),
        (b"..,", 26, "| 00:01"),
        (b"....", 29, "| 00:01"),
    ] {
        lands(&mut tty, DOWN, 0, "| 00:00");
        lands(&mut tty, SHIFT_RIGHT, 25, "| 00:01");
        tty.send(keys);
        // `s` waits for the keys before it (`..,` shows 26 on the way too)
        assert_eq!(taken(&mut tty, &sandbox), number, "{}", shown(keys));
        lands(&mut tty, b"", number, clock);
        // a window for what must not happen: playing on
        std::thread::sleep(Duration::from_millis(500));
        let screen = tty.screen();
        let on = (frame(screen), screen.row(24));
        assert!(
            on.0 == number && on.1.starts_with(clock),
            "{}: {on:?}",
            shown(keys)
        );
    }
    tty.send(b"q");
    assert_eq!(tty.exit(), 0);
}

#[test]
fn a_step_back_after_a_step_on_the_last_frame_shows_the_frame_before() {
    // rows P15, N21: `.` on the last frame, frame 54 of this 2.2 s clip reached by Shift+Right, only pauses, and `,` then shows frame 53 and its time, whatever the sound left past the picture (frame_step of bin/ascii-viewer:1441 at tag python-final; N21: the old viewer ended with the picture); seen before the fix: frame 53 never came (the review saw 54 stay for 2.2, 2.5 and 6 s of sound, the clock at 00:01 for 2.5 and 6)
    for audio in [2.2, 2.5, 6.0] {
        let sandbox = Sandbox::new();
        let video = counter(&sandbox, "clip.mp4", "160x90", 25, 2.2, audio, &[]);
        let mut tty = sandbox.launch().arg(&video).size(SMALL).start();
        paused_at_the_start(&mut tty);
        lands(&mut tty, SHIFT_RIGHT, 25, "| 00:01");
        // four steps in one write, each its frame
        lands(&mut tty, b"....", 29, "| 00:01");
        // the clock there is mpv's position, past the picture's end the sound's, behind what the sound output holds: up to 0.2 s short of the frame (1.996 for 2.16 in 2 of 16 runs idle; src/mpv.rs `a_seek_onto_the_last_frame_after_steps_puts_the_clock_on_it`, ignored)
        lands(&mut tty, SHIFT_RIGHT, 54, "| 00:0");
        tty.send(b".");
        // a window for what must not happen: the picture moving on, or the run ending
        std::thread::sleep(Duration::from_millis(500));
        assert!(frame(tty.screen()) == 54 && tty.running(), "{audio}");
        lands(&mut tty, b",", 53, "| 00:02");
        tty.send(b"q");
        assert_eq!(tty.exit(), 0);
    }
}

/// Wait until an update of an 80 x 24 window shows the clip playing at 3 s or later, past the picture's end of a clip of 2.2 s at most: a look at the live screen for `> 00:03` missed a row shown for a second under load, and the clip played to its end (the stress runs: 2 of 14 copies at load 14 to 52, 1 of 10 at 71); a picture behind the sound jumps to the sound's position, a second or so skipped (seen at load 86: 00:02, then 00:04).
fn sound_on(tty: &Tty) {
    tty.until_drawn("the clock at 3 s or later", 0, |screen| {
        let row = screen.row(24);
        row.starts_with("> ") && seconds(&row).is_some_and(|at| at >= 3)
    });
}

#[test]
fn a_step_while_the_sound_plays_on_past_the_picture_only_pauses() {
    // rows P15, N5, N21: past the picture's end the sound plays on over the last frame, frame 54 of this 2.2 s clip with 12 s of sound (under load the screen was seconds behind mpv); `.` there only pauses, the clock where the sound was, `s` takes that frame, and `,` shows frame 53 (frame_step of bin/ascii-viewer:1441 at tag python-final; N21: the old viewer ended with the picture); seen before the fix: the playing step's seek to the frame after the one on screen went past the last frame and put the clock back short of the frame, the bar falling from 27 cells to 21 with 6 s of sound, and `s` named the frame `clip-ascii-2s.png` (the review; with 2.6 s of sound `,` kept frame 54 in 1 of 2 runs); here without the fix the clock read `| 00:01` or `| 00:02` after `.` (3 of 3); the name's time is the position mpv gave with the frame, a look late under load (`a_frame_taken_while_the_sound_plays_on_is_named_by_its_time`; src/mpv.rs `a_step_while_the_sound_plays_on_past_the_picture_only_pauses` pins it to a frame)
    let sandbox = Sandbox::new();
    let video = counter(&sandbox, "clip.mp4", "160x90", 25, 2.2, 12.0, &[]);
    let mut tty = sandbox.launch().arg(&video).size(SMALL).start();
    sound_on(&tty);
    tty.send(b".");
    // the screen can lag mpv under load: it must come to the last frame, paused (seen at load 59 without the wait: frame 52 on screen 0.5 s after the pause)
    tty.until("the last frame, paused", |screen| {
        frame(screen) == 54 && screen.row(24).starts_with("| ")
    });
    // a window for what must not happen: the clip moving on, or the clock going back
    std::thread::sleep(Duration::from_millis(500));
    let (row, on) = last_drawn(&tty);
    assert!(
        on == 54 && seconds(&row).is_some_and(|at| at >= 3) && tty.running(),
        "{row:?} on frame {on}"
    );
    let at = tty.send(b"s");
    let start = tty.expect(at, b"\x1b[7m saved ");
    let end = tty.expect(start, b" \x1b[0m") - 5;
    let name = String::from_utf8(tty.out()[start..end].to_vec()).unwrap();
    let time: f64 = name["clip-ascii-".len()..name.len() - "s.png".len()]
        .parse()
        .unwrap();
    let number = saved(&sandbox.path(&name)).0;
    assert!(
        (1.7..=2.7).contains(&time) && number == 54,
        "{name}: {number}"
    );
    lands(&mut tty, b",", 53, "| 00:02");
    tty.send(b"q");
    assert_eq!(tty.exit(), 0);
}

#[test]
fn steps_typed_apart_each_take_their_frame() {
    // rows P15, N5: `.` `.` `,` typed 30 to 100 ms apart end one frame on (frame_step of bin/ascii-viewer:1441 at tag python-final did each before the next key; run in this harness the old viewer showed frame 26, read on the screen, while its `s`, which N5 makes mpv's frame, took 28); seen in this test before the fix: frame 25 for keys 30 ms apart (the review: 2 of 4 runs), mpv taking the step back while the second step waited for its frame
    let sandbox = Sandbox::new();
    let video = counter(&sandbox, "clip.mp4", "160x90", 25, 20.0, 20.0, &[]);
    let mut tty = sandbox.launch().arg(&video).size(SMALL).start();
    tty.await_playing();
    pause(&mut tty);
    for gap in [30, 30, 60, 100] {
        lands(&mut tty, DOWN, 0, "| 00:00");
        lands(&mut tty, SHIFT_RIGHT, 25, "| 00:01");
        for key in [&b"."[..], b".", b","] {
            tty.send(key);
            // the keys' timing, not a wait for the viewer
            std::thread::sleep(Duration::from_millis(gap));
        }
        // `s` waits for the steps; frame 26 also shows on the way
        assert_eq!(taken(&mut tty, &sandbox), 26, "{gap} ms apart");
        lands(&mut tty, b"", 26, "| 00:01");
        // a window for what must not happen: a step still to come
        std::thread::sleep(Duration::from_millis(500));
        assert_eq!(frame(tty.screen()), 26, "{gap} ms apart");
    }
    tty.send(b"q");
    assert_eq!(tty.exit(), 0);
}

#[test]
fn steps_take_one_source_frame_above_30_frames_a_second() {
    // row N3: `.` and `,` step one frame of a 60 fps source; the old viewer stepped on its 30 fps grid, two of them; paused at the start, as the 3 s clip ended before a pause sent once it played on (the stress run: 1 of 10 at load 85)
    let sandbox = Sandbox::new();
    let video = counter(&sandbox, "fast.mp4", "160x90", 60, 3.0, 0.0, &[]);
    let mut tty = sandbox.launch().arg(&video).size(SMALL).start();
    paused_at_the_start(&mut tty);
    for (key, number) in [(b".", 1), (b".", 2), (b",", 1)] {
        lands(&mut tty, key, number, "| 00:00");
    }
    tty.send(b"q");
    assert_eq!(tty.exit(), 0);
}

#[test]
fn speed_keys_step_it_between_25_and_400_percent() {
    // rows P10, P11, N23: SPEEDS and set_speed of bin/ascii-viewer:131 and :1502 at tag python-final, the toasts as the old viewer gave them in this harness: x1.1 and /1.1 to a whole percent, x2, /2, held at 25 and 400, Backspace (0x7f and 0x08) and \ back to 100; the clip plays at the speed; without a sound track the volume keys say `silent` (N23)
    let sandbox = Sandbox::new();
    let video = counter(&sandbox, "clip.mp4", "160x90", 25, 30.0, 0.0, &[]);
    let mut tty = sandbox.launch().arg(&video).size(SMALL).start();
    tty.await_playing();
    pause(&mut tty);
    let up = [
        110, 121, 133, 146, 161, 177, 195, 215, 237, 261, 287, 316, 348, 383, 400, 400,
    ];
    for percent in up {
        says(&mut tty, b"]", &format!("speed {percent}%"));
    }
    let down = [
        364, 331, 301, 274, 249, 226, 205, 186, 169, 154, 140, 127, 115, 105, 95, 86, 78,
    ];
    for percent in down {
        says(&mut tty, b"[", &format!("speed {percent}%"));
    }
    let keys: [(&[u8], u32); 14] = [
        (b"\\", 100),
        (b"}", 200),
        (b"}", 400),
        (b"}", 400),
        (b"{", 200),
        (b"{", 100),
        (b"{", 50),
        (b"{", 25),
        (b"{", 25),
        (BACKSPACE, 100),
        (b"]", 110),
        (b"\x08", 100),
        (b"[", 91),
        (b"\\", 100),
    ];
    for (key, percent) in keys {
        says(&mut tty, key, &format!("speed {percent}%"));
    }
    for key in [b"0", b"9", b"*", b"/"] {
        says(&mut tty, key, "silent");
    }
    // at 400 % the frames come 4 times as fast as the clock's, from the first update playing to one 6 s of the clip on (the counter's frames, mod 256, timed by when each update came, which a reader late under load reads together: 7.6 times over 4 s of the clip at load 55), at least 1.5 times under load, where mpv's own pace falls behind (the stress run: at 200 % 4 s of the clip in 3.8 s at load 71, timed then by the second the screen showed, read late); at 100 % a speed not set
    says(&mut tty, b"}", "speed 200%");
    says(&mut tty, b"}", "speed 400%");
    let at = tty.send(b" ");
    let end = Instant::now() + PATIENCE;
    let pace = loop {
        let playing: Vec<_> = updates(&tty, at)
            .into_iter()
            .filter(|(_, _, row)| row.starts_with("> "))
            .collect();
        let on: u32 = playing
            .windows(2)
            .map(|w| (w[1].1 + 256 - w[0].1) % 256)
            .sum();
        if on >= 150 {
            let took = playing.last().unwrap().0 - playing[0].0;
            break on as f64 * 0.04 / took.as_secs_f64();
        }
        if Instant::now() > end {
            tty.fail("no 6 s of the clip played at 400 %");
        }
        let len = tty.len();
        tty.wait(len, Duration::from_millis(100), |out| !out.is_empty());
    };
    assert!(pace > 1.5 && pace < 13.0, "{pace} s of the clip a second");
    tty.send(b"q");
    assert_eq!(tty.exit(), 0);
}

#[test]
fn volume_keys_step_by_2_up_to_mpv_confs_volume_max() {
    // rows P5, P11, N18: of mpv.conf only audio options are read: volume-max=104 holds, speed=2 does not (mpv's speed stays 100 %); `0 *` +2, `9 /` -2 from mpv's 100 (set_volume of bin/ascii-viewer:1508 at tag python-final, the toasts as the old viewer gave them in this harness)
    let sandbox = Sandbox::new();
    std::fs::write(
        sandbox.path("home/.config/mpv/mpv.conf"),
        "ao=null\nvolume-max=104\nspeed=2\n",
    )
    .unwrap();
    let video = clip(&sandbox, "clip.mp4", 20.0, "160x90", 25);
    let mut tty = sandbox.launch().arg(&video).size(SMALL).start();
    tty.await_playing();
    pause(&mut tty);
    // the speed mpv reports, which the speed keys step from: 100 % (with speed=2 read, `]` would say 220 %); timed by the clock seen on the live screen, 3 s of the clip came in 2.09 s under load, the first sighting late (the stress run)
    says(&mut tty, b"]", "speed 110%");
    says(&mut tty, b"\\", "speed 100%");
    for (key, volume) in [
        (b"0", 102),
        (b"*", 104),
        (b"0", 104),
        (b"9", 102),
        (b"/", 100),
    ] {
        says(&mut tty, key, &format!("volume {volume}"));
    }
    tty.send(b"q");
    assert_eq!(tty.exit(), 0);
}

#[test]
fn the_volume_toast_rounds_mpvs_volume() {
    // row P11: set_volume of bin/ascii-viewer:1508 at tag python-final toasts the volume mpv gives back, rounded as Python rounds, half to even: mpv.conf's volume=50.6 and `0` make 52.6, `volume 53`, then `9` 50.6 and 48.6; volume=50.5 and `0` make 52.5, `volume 52`, then 54.5, `volume 54`
    let rounds = [
        (
            "50.6",
            [("0", "volume 53"), ("9", "volume 51"), ("9", "volume 49")],
        ),
        (
            "50.5",
            [("0", "volume 52"), ("0", "volume 54"), ("9", "volume 52")],
        ),
    ];
    for (volume, toasts) in rounds {
        let sandbox = Sandbox::new();
        std::fs::write(
            sandbox.path("home/.config/mpv/mpv.conf"),
            format!("ao=null\nvolume={volume}\n"),
        )
        .unwrap();
        let video = clip(&sandbox, "clip.mp4", 20.0, "160x90", 25);
        let mut tty = sandbox.launch().arg(&video).size(SMALL).start();
        tty.await_playing();
        pause(&mut tty);
        for (key, text) in toasts {
            says(&mut tty, key.as_bytes(), text);
        }
        tty.send(b"q");
        assert_eq!(tty.exit(), 0);
    }
}

#[test]
fn mute_and_volume_hold_through_seeks_and_loops() {
    // rows P7, P12: start_audio and launch_mpv of bin/ascii-viewer:1313 at tag python-final hand volume and mute to each mpv started for a seek or a loop; `m` toasts `muted` and `unmuted`; paused at the start, as the 3 s clip ended before a pause sent once it played on (the stress run: 2 of 10 at load 83 to 89), and the loop's last second and its start again taken from every update, as a look at the live screen under load missed either
    let sandbox = Sandbox::new();
    let video = counter(&sandbox, "clip.mp4", "160x90", 25, 3.0, 3.0, &[]);
    let mut tty = sandbox.launch().arg(&video).size(SMALL).start();
    paused_at_the_start(&mut tty);
    says(&mut tty, b"m", "muted");
    says(&mut tty, b"0", "volume 102");
    lands(&mut tty, DOWN, 0, "");
    says(&mut tty, b"L", "loop on");
    let at = tty.send(b" ");
    // round the end and on from the start
    let at = tty.until_drawn("the last second", at, |screen| {
        seconds(&screen.row(24)) == Some(2)
    });
    tty.until_drawn("the start again", at, |screen| {
        seconds(&screen.row(24)) == Some(0)
    });
    pause(&mut tty);
    says(&mut tty, b"m", "unmuted");
    says(&mut tty, b"0", "volume 104");
    tty.send(b"q");
    assert_eq!(tty.exit(), 0);
}

#[test]
fn l_loops_the_clip_until_it_is_off() {
    // rows P13, P14, N23: `L` toasts `loop on`, and while on the status row ends in `L` in the last column, a toast showing instead (status of bin/ascii-viewer:2143 at tag python-final); the clip starts over at its end, also after a seek past it (N23: the old viewer quit there, P14a); `L` again: `loop off`, and the end quits
    let sandbox = Sandbox::new();
    let video = counter(&sandbox, "clip.mp4", "160x90", 25, 3.0, 3.0, &[]);
    let mut tty = sandbox.launch().arg(&video).size(SMALL).start();
    tty.until("the first status row", |screen| !screen.row(24).is_empty());
    let at = tty.send(b"L");
    let toasted = tty.expect(at, toast("loop on"));
    let marked = tty.expect(at, b"\x1b[24;80HL\x1b[0m");
    let out = tty.out();
    let update = |at: usize| {
        let start = out[..at]
            .windows(8)
            .rposition(|w| w == b"\x1b[?2026h")
            .unwrap();
        &out[start..find(&out, b"\x1b[?2026l", at).unwrap()]
    };
    assert!(find(update(toasted), b"\x1b[24;80HL", 0).is_none());
    assert!(find(update(marked), b"\x1b[7m", 0).is_none());
    tty.until("the end", |screen| frame(screen) >= 70);
    tty.until("the start again", |screen| frame(screen) < 10);
    // 5 s on is past the end: it goes on round
    tty.send(b"l");
    tty.until("playing on round", |screen| frame(screen) >= 40);
    says(&mut tty, b"L", "loop off");
    assert_eq!(tty.exit(), 0);
}

#[test]
fn space_on_the_last_frame_plays_round_or_quits() {
    // rows P8, P13, P14: `space` on the last frame, paused there by `.`, played on, and the decoder's end then quit, exit 0, or with `L` started the clip again (toggle and ended of bin/ascii-viewer:2270 and :2135 at tag python-final); mpv pauses at once at its end, and on the last frame, which a seek past the end showed at its own time once the step played the sound out, it told nothing of it: the viewer played on that frame under the mark `>` for good (seen before the fix with the loop off; with mpv's own loop, loop-file, mpv played it round)
    let sandbox = Sandbox::new();
    let video = counter(&sandbox, "clip.mp4", "160x90", 25, 2.2, 2.2, &[]);
    for looping in [true, false] {
        let mut tty = sandbox.launch().arg(&video).size(SMALL).start();
        paused_at_the_start(&mut tty);
        if looping {
            says(&mut tty, b"L", "loop on");
        }
        lands(&mut tty, b"\x1b[1;2C\x1b[1;2C", 50, "| 00:02");
        for number in 51..=54 {
            lands(&mut tty, b".", number, "| 00:02");
        }
        // mpv plays the sound out, then a seek past the end shows the frame at its own time, 2.16 s: 63 of the bar's 64 cells, 60 of 61 while looping (room 4 less)
        let cells = if looping { 60 } else { 63 };
        tty.until("the clock on the last frame", |screen| {
            screen.row(24).matches('━').count() == cells
        });
        let at = tty.send(b" ");
        if looping {
            let again = |screen: &Screen| frame(screen) < 10 && screen.row(24).starts_with("> ");
            if !tty.wait(at, PATIENCE, |out| {
                replay(out, SMALL, again).contains(&true)
            }) {
                tty.fail("not from the start again after `space`");
            }
            says(&mut tty, b"L", "loop off");
        }
        let what = if looping { "looping" } else { "not looping" };
        let status = tty.exit_within(PATIENCE).and_then(|status| status.code());
        assert_eq!(status, Some(0), "{what}");
    }
}

#[test]
fn the_status_row_shows_mark_clock_length_and_bar() {
    // row P16: status of bin/ascii-viewer:2143 at tag python-final, the rows as the old viewer wrote them in this harness: `<mark> MM:SS / MM:SS` and round(room x position / length) of `━`, room = cols - len - 1, 4 less while looping, `L` in the last column; only the text written, after the draw erased the row; cut to the width; minutes in full
    let sandbox = Sandbox::new();
    let video = silent_clip(&sandbox, "clip.mp4", 20.0, "160x90", 25);
    let long = sandbox.path("long.mp4");
    // a frame a minute for 123 minutes
    ffmpeg(
        &sandbox,
        &[
            "-f",
            "lavfi",
            "-i",
            "color=c=gray:s=32x18:r=1/60:d=7330",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
            long.to_str().unwrap(),
        ],
    );
    let narrow = Size {
        rows: 5,
        cols: 12,
        xpx: 108,
        ypx: 100,
    };
    let mut tty = sandbox.launch().arg(&video).size(SMALL).start();
    let mut cut = sandbox.launch().arg(&video).size(narrow).start();
    let mut minutes = sandbox.launch().arg(&long).size(SMALL).start();
    minutes.until("123 minutes", |screen| {
        screen.row(24).starts_with("> 00:0") && screen.row(24).ends_with(" / 123:00")
    });
    minutes.send(b"q");
    assert_eq!(minutes.exit(), 0);
    cut.until("the clock", |screen| screen.row(5).starts_with("> 00:0"));
    pause(&mut cut);
    let row = replay(&cut.out(), narrow, |screen| screen.row(5))
        .pop()
        .unwrap();
    assert!(
        row.starts_with("| 00:") && row.ends_with(" / 00") && row.chars().count() == 12,
        "{row:?}"
    );
    cut.send(b"q");
    assert_eq!(cut.exit(), 0);
    tty.await_playing();
    let (row, _) = last_drawn(&tty);
    assert!(
        row.starts_with("> 00:0") && row.contains(" / 00:20 ━"),
        "{row:?}"
    );
    pause(&mut tty);
    let at = tty.send(DOWN);
    tty.expect(at, b"\x1b[24;1H\x1b[0m| 00:00 / 00:20\x1b[0m");
    // the draw: the bottom row erased, then the picture from row 2, then the status
    tty.expect(at, b"\x1b[24;1H\x1b[2K\x1b[2;1H");
    let at = tty.send(RIGHT);
    tty.expect(
        at,
        format!("\x1b[24;1H\x1b[0m| 00:05 / 00:20 {}\x1b[0m", "━".repeat(16)),
    );
    says(&mut tty, b"L", "loop on");
    tty.expect(
        tty.len(),
        format!(
            "\x1b[24;1H\x1b[0m| 00:05 / 00:20 {}\x1b[24;80HL\x1b[0m",
            "━".repeat(15)
        ),
    );
    tty.send(b"q");
    assert_eq!(tty.exit(), 0);
}

#[test]
fn a_stream_without_a_length_shows_its_clock_alone() {
    // row P16: the total and the bar are left out while the length is unknown (status of bin/ascii-viewer:2161 at tag python-final; the old viewer ends on this file with a traceback: its decoder cannot seek in a raw stream)
    let sandbox = Sandbox::new();
    let raw = sandbox.path("clip.h264");
    ffmpeg(
        &sandbox,
        &[
            "-f",
            "lavfi",
            "-i",
            "testsrc2=s=160x90:r=25:d=4",
            "-c:v",
            "libx264",
            "-g",
            "5",
            "-pix_fmt",
            "yuv420p",
            raw.to_str().unwrap(),
        ],
    );
    let mut tty = sandbox.launch().arg(&raw).size(SMALL).start();
    // an update playing past the first second: a look at the live screen for `> 00:01`, a row shown a second, missed it under load and the 4 s clip ended (beside a load round: 1 of 11 runs at load 70 to 100)
    let at = tty.until_drawn("the clock past 00:00", 0, |screen| {
        let row = screen.row(24);
        row.starts_with("> ") && seconds(&row).is_some_and(|at| at >= 1)
    });
    // that update's row: the mark and the clock alone
    let rows = replay(&tty.out()[..at], SMALL, |screen| screen.row(24));
    let row = rows.last().cloned().unwrap_or_default();
    assert!(
        row.starts_with("> ") && seconds(&row).is_some() && row.chars().count() == 7,
        "{row:?}"
    );
    tty.send(b"q");
    assert_eq!(tty.exit(), 0);
}

#[test]
fn the_clock_never_shows_past_the_length() {
    // rows P16, N35: an MKV whose Duration says 1.5 s over 6 s of frames: the frames play on past the declared length (frame 50 is 2 s in; the viewer quits on the last frame it drew, and under load it drew seldom: over 3 s of frames the last at frames 28 and 40 at load 70 and 89, the stress run, hence 6 s), and mpv raises the length to the highest time it has demuxed, ahead of the frame shown (demux.c: "update duration based on highest TS demuxed"): the row read `> 00:00 / 00:01` at first, and no row shows a clock past its length. The old viewer took the container's 1.5 s and capped the clock there (status of bin/ascii-viewer:2160 at tag python-final), the difference N35 approves. The viewer's own cap of the clock at the length (status in src/view.rs) is not reached: mpv's position never passes its duration, raised ahead of the frames played and the position clamped to it while a seek runs (get_playback_time in player/playloop.c), and with the cap taken out this test passed 3 of 3; that cap stands by code reading
    let sandbox = Sandbox::new();
    let video = counter(&sandbox, "clip.mkv", "160x90", 25, 6.0, 0.0, &[]);
    let mut bytes = std::fs::read(&video).unwrap();
    // the segment's Duration: element 0x4489, a float of 8 bytes, in milliseconds
    let at = find(&bytes, &[0x44, 0x89, 0x88], 0).unwrap();
    let length = f64::from_be_bytes(bytes[at + 3..at + 11].try_into().unwrap());
    assert_eq!(length, 6000.0);
    bytes[at + 3..at + 11].copy_from_slice(&1500.0f64.to_be_bytes());
    std::fs::write(&video, bytes).unwrap();
    let probed = probe(&sandbox, &video, "format=duration", None);
    assert_eq!(probed, "duration=1.500000\n");
    let mut tty = sandbox.launch().arg(&video).size(SMALL).start();
    assert_eq!(tty.exit(), 0);
    let drawn = replay(&tty.out(), SMALL, |screen| (frame(screen), screen.row(24)));
    // the clock and the length a row shows, in seconds
    let clocks = |row: &str| {
        let (now, length) = row.get(2..)?.split_once(" / ")?;
        let length = length.split(' ').next()?;
        Some((
            seconds(&format!("> {now}"))?,
            seconds(&format!("> {length}"))?,
        ))
    };
    assert!(drawn.iter().any(|(number, _)| *number >= 50), "{drawn:?}");
    assert!(
        drawn
            .iter()
            .all(|(_, row)| clocks(row).is_none_or(|(now, length)| now <= length)),
        "{drawn:?}"
    );
    // the length shown at the end: the frames' that mpv demuxed, past the 1.5 s declared
    let length = drawn
        .iter()
        .rev()
        .find_map(|(_, row)| clocks(row))
        .unwrap()
        .1;
    assert!((2..=6).contains(&length), "{drawn:?}");
}

/// A window of 24 x 4 cells: each look key draws the frame again, so a long run of them stays quick in it.
const TINY: Size = Size {
    rows: 4,
    cols: 24,
    xpx: 216,
    ypx: 80,
};

/// A paused clip in a tiny window: `keys` (a key, how many at once, the toast that follows) one batch after another.
fn toasts(keys: &[(&str, usize, &str)]) {
    let sandbox = Sandbox::new();
    let video = silent_clip(&sandbox, "clip.mp4", 30.0, "160x90", 25);
    let mut tty = sandbox.launch().arg(&video).size(TINY).start();
    tty.await_playing();
    pause(&mut tty);
    for (key, times, text) in keys {
        says(&mut tty, key.repeat(*times).as_bytes(), text);
    }
    tty.send(b"q");
    assert_eq!(tty.exit(), 0);
}

#[test]
fn look_keys_toast_and_stop_at_their_limits() {
    // rows P17, P18, P19: the look keys of bin/ascii-viewer:2284 at tag python-final with the limits of lib/ascii_theme.py:33 (level and contrast -95..95), the toasts as the old viewer gave them in this harness; a batch toasts its last key's
    toasts(&[
        ("3", 1, "level 5% darker"),
        ("3", 1, "level 10% darker"),
        ("3", 18, "level 95% darker"),
        ("4", 1, "level 90% darker"),
        ("4", 18, "level 0"),
        ("4", 1, "level 5% brighter"),
        ("4", 20, "level 95% brighter"),
        ("`", 1, "level 0, contrast 0"),
        ("2", 1, "contrast +5%"),
        ("2", 20, "contrast +95%"),
        ("1", 1, "contrast +90%"),
        ("1", 18, "contrast 0"),
        ("1", 1, "contrast -5%"),
        ("1", 20, "contrast -95%"),
        ("`", 1, "level 0, contrast 0"),
        ("c", 1, "mono"),
        ("c", 1, "colour"),
        ("b", 1, "bold"),
        ("b", 1, "regular"),
        ("g", 1, "glyphs: density"),
        ("g", 1, "glyphs: shape"),
        ("g", 1, "glyphs: mixed"),
    ]);
}

#[test]
fn saturation_keys_step_by_10_between_0_and_300() {
    // rows P17, P18, P19: `7` and `8` of bin/ascii-viewer:2295 at tag python-final with the limits of lib/ascii_theme.py:33 (saturation 0..300), the toasts as the old viewer gave them in this harness
    toasts(&[
        ("7", 1, "saturation 90%"),
        ("7", 10, "saturation 0%"),
        ("8", 1, "saturation 10%"),
        ("8", 30, "saturation 300%"),
        ("7", 1, "saturation 290%"),
    ]);
}

#[test]
fn the_picture_is_centred_in_rows_of_colour_codes() {
    // rows P20, G4, G5, N22: screen of bin/ascii-viewer:2220 at tag python-final: each row placed, the picture centred where the old viewer placed it in this harness (16:9, 4:3 and 9:16 at 80 x 24 and 200 x 60); a row's text starts with a colour code and ends in `39` (tests/test_term.py:15), then SGR 0; bold puts SGR 1 after each row's place; mono writes `97`, bl1nk's brightest, once a row (tests/test_term.py:23); N22: an anamorphic clip shows at its display shape, 16:9 here (the old viewer showed it as stored, 8:9)
    let sandbox = Sandbox::new();
    let shapes = [
        ("wide", "160x90", ""),
        ("four", "160x120", ""),
        ("tall", "90x160", ""),
        ("stretched", "160x180", ",setsar=2"),
    ];
    for (name, size, sar) in shapes {
        let source = format!("testsrc2=s={size}:r=25:d=20{sar}");
        let file = sandbox.path(format!("{name}.mp4"));
        ffmpeg(
            &sandbox,
            &[
                "-f",
                "lavfi",
                "-i",
                &source,
                "-c:v",
                "libx264",
                "-g",
                "5",
                "-pix_fmt",
                "yuv420p",
                file.to_str().unwrap(),
            ],
        );
    }
    // (clip, window): first row, rows, first column, columns
    let places = [
        ("wide", SMALL, 2, 21, 1, 80),
        ("four", SMALL, 1, 24, 5, 72),
        ("tall", SMALL, 1, 24, 25, 31),
        ("wide", WIDE, 5, 51, 1, 200),
        ("four", WIDE, 1, 60, 12, 178),
        ("tall", WIDE, 1, 60, 63, 76),
        ("stretched", SMALL, 2, 21, 1, 80),
    ];
    let mut ttys: Vec<Tty> = places
        .iter()
        .map(|(name, size, ..)| {
            let file = sandbox.path(format!("{name}.mp4"));
            sandbox.launch().arg(file).size(*size).start()
        })
        .collect();
    for (at, (tty, (name, _, top, rows, left, cols))) in ttys.iter_mut().zip(places).enumerate() {
        assert!(
            tty.wait(0, PATIENCE, |out| !written_rows(out).is_empty()),
            "{name}: no picture"
        );
        // the first goes on below; the others, done, leave the machine to the other tests
        if at > 0 {
            tty.kill();
        }
        let written = written_rows(&tty.out());
        let got: Vec<(usize, usize)> = written.iter().map(|(r, c, _)| (*r, *c)).collect();
        let expected: Vec<(usize, usize)> = (top..top + rows).map(|row| (row, left)).collect();
        assert_eq!(got, expected, "{name} in {:?}", tty.size);
        for (row, _, bytes) in &written {
            let (codes, text) = split_codes(bytes);
            assert_eq!(text.len(), cols, "{name}, row {row}");
            assert!(
                codes.first().is_some_and(|code| code.len() == 5
                    && (code.starts_with("\x1b[3") || code.starts_with("\x1b[9"))),
                "{name}, row {row}: {}",
                shown(bytes)
            );
            assert!(bytes.ends_with(b"\x1b[39m"), "{name}, row {row}");
        }
    }
    let mut tty = ttys.swap_remove(0);
    drop(ttys);
    tty.await_playing();
    pause(&mut tty);
    let at = tty.send(b"b");
    tty.expect(at, toast("bold"));
    let bold = written_rows(&tty.out()[at..]);
    assert_eq!(bold.len(), 21);
    assert!(
        bold.iter()
            .all(|(_, _, bytes)| bytes.starts_with(b"\x1b[1m\x1b["))
    );
    says(&mut tty, b"b", "regular");
    let at = tty.send(b"c");
    tty.expect(at, toast("mono"));
    let mono = written_rows(&tty.out()[at..]);
    assert_eq!(mono.len(), 21);
    for (row, _, bytes) in &mono {
        let (codes, text) = split_codes(bytes);
        assert_eq!(
            (codes, text.len()),
            (vec!["\x1b[97m".to_string()], 80),
            "row {row}"
        );
    }
    tty.send(b"q");
    assert_eq!(tty.exit(), 0);
}

#[test]
fn a_turned_clip_shows_upright() {
    // rows P1, P22: tests/test_viewer.py:68 at tag python-final: a display matrix shows 320 x 240 as 240 x 320; here 160 x 90 turned a quarter left shows as the old viewer showed it in this harness: rows 1 to 24 from column 25, the quadrants green and blue over yellow and red (ffmpeg's transpose=2 of the stored clip draws the same); the render page gets the frame upright: 1080 x 1920
    let sandbox = Sandbox::new();
    let quadrants = "nullsrc=s=160x90:r=25:d=4,format=gbrp,geq=r='if(lt(X,W/2),255,0)':g='if(lt(Y,H/2),255,0)':b=128";
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
            "-g",
            "5",
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
    let mut tty = sandbox.launch().arg(&turned).size(SMALL).start();
    tty.await_playing();
    pause(&mut tty);
    let places: Vec<(usize, usize)> = written_rows(&tty.out())
        .iter()
        .map(|(row, col, _)| (*row, *col))
        .collect();
    assert_eq!(places, (1..=24).map(|row| (row, 25)).collect::<Vec<_>>());
    let slot = |screen: &Screen, row, col| screen.cell(row, col).pen.fg;
    let screen = tty.screen();
    assert_eq!(
        [
            slot(screen, 6, 32),
            slot(screen, 6, 47),
            slot(screen, 17, 32),
            slot(screen, 17, 47)
        ],
        [Some(2), Some(12), Some(11), Some(1)]
    );
    tty.send(b"r");
    tty.until("the page's size", |screen| {
        (1..=24).any(|row| {
            screen
                .row(row)
                .starts_with("   Size       1080x1920  (1920 long side)")
        })
    });
    tty.send(b"q");
    tty.until("the player", |screen| screen.row(24).starts_with("| "));
    tty.send(b"q");
    assert_eq!(tty.exit(), 0);
}

#[test]
fn s_saves_the_frame_on_screen_beside_the_clip() {
    // rows P21, N5: screenshot of bin/ascii-viewer:1621 at tag python-final: toasts `screenshot ...` then `saved <name>`; `<stem>-ascii-<t>s.png` beside the clip (%g of the time to 0.1 s), drawn as the converter draws (1920 px on the long side), an existing file replaced; N5: the frame on screen when paused, also after `,` (the old viewer named and drew a later frame there, P21a)
    let sandbox = Sandbox::new();
    let video = counter(&sandbox, "clip.mp4", "160x90", 25, 6.0, 6.0, &[]);
    std::fs::write(sandbox.path("clip-ascii-0s.png"), "not a picture").unwrap();
    let mut tty = sandbox.launch().arg(&video).size(SMALL).start();
    tty.await_playing();
    pause(&mut tty);
    lands(&mut tty, LEFT, 0, "| 00:00");
    shot(&mut tty, "saved clip-ascii-0s.png");
    assert_eq!(saved(&sandbox.path("clip-ascii-0s.png")), (0, (1920, 1080)));
    lands(&mut tty, SHIFT_RIGHT, 25, "");
    for (key, number) in [
        (b".", 26),
        (b".", 27),
        (b".", 28),
        (b".", 29),
        (b".", 30),
        (b",", 29),
        (b",", 28),
    ] {
        lands(&mut tty, key, number, "");
    }
    shot(&mut tty, "saved clip-ascii-1.1s.png");
    assert_eq!(
        saved(&sandbox.path("clip-ascii-1.1s.png")),
        (28, (1920, 1080))
    );
    tty.send(b"q");
    assert_eq!(tty.exit(), 0);
}

#[test]
fn a_clip_paused_while_playing_takes_and_seeks_from_the_frame_on_screen() {
    // rows N5, P9: paused by `space` at whatever point of a frame, `s` saves the frame that then shows, named by its time, and a seek counts from that frame, no other frame drawn before the target; the debug build draws a 200 x 60 window slower than the clip's 25 frames a second, and under load slower than mpv waits for a frame to be taken: the screen then stayed on a frame before the one `s` took (seen in a parallel run before `Mpv::events` reported the late frame: 66 on screen, `clip-ascii-2.7s.png` taken); on an idle machine the guard is `a_caller_slower_than_mpv_waits_draws_the_frame_mpv_holds` in src/mpv.rs, which fails without the late frame (3 of 3: 36 drawn, 47 held); a 400 x 120 window failed here without it 4 of 4 idle, but under load ran this clip out and slowed the group's timed tests into failing; the clip lasts 30 s, as under load 10 s ran out; the frame a second on is read from whole draws: under load the screen showed it half written, before the draw's end
    let sandbox = Sandbox::new();
    let video = counter(&sandbox, "clip.mp4", "160x90", 25, 30.0, 30.0, &[]);
    let mut tty = sandbox.view(&video);
    tty.await_playing();
    // the picture covers rows 5 to 55 of the window
    let frame = |screen: &Screen| counted(screen, 30, 1, 200);
    for _ in 0..3 {
        tty.send(b" ");
        tty.await_status("| ");
        // the screen can lag mpv under load: the frame `s` takes is the reference, and the screen must come to it
        let at = tty.send(b"s");
        let start = tty.expect(at, b"\x1b[7m saved ");
        let end = tty.expect(start, b" \x1b[0m") - 5;
        let name = String::from_utf8(tty.out()[start..end].to_vec()).unwrap();
        // the counter shows a frame's number mod 256: frame 272 shows 16
        let held = saved(&sandbox.path(&name)).0;
        let names =
            (0..3).map(|round| format!("clip-ascii-{}s.png", tenths((held + 256 * round) * 4)));
        assert!(
            names.clone().any(|own| own == name),
            "{name} for frame {held}"
        );
        tty.until("the frame taken", |screen| frame(screen) == held);
        let (from, on) = (tty.send(SHIFT_RIGHT), (held + 25) % 256);
        let there = |screen: &Screen| frame(screen) == on && screen.row(60).starts_with("| ");
        if !tty.wait(from, PATIENCE, |out| {
            replay(out, WIDE, there).contains(&true)
        }) {
            tty.fail(&format!("no draw of frame {on} paused"));
        }
        let mut after = replay(&tty.out()[from..], WIDE, frame);
        after.dedup();
        after.retain(|number| *number != held);
        assert_eq!(after, [on], "a second on from frame {held}");
        // on from the start: the screen's lag under load would otherwise run the clip out
        tty.send(DOWN);
        tty.until("the start", |screen| frame(screen) == 0);
        tty.send(b" ");
        tty.until("playing on", |screen| {
            screen.row(60).starts_with("> ") && frame(screen) > 25
        });
    }
    tty.send(b"q");
    assert_eq!(tty.exit(), 0);
}

/// `s`, then its toasts: `screenshot ...` and the one that says how it went.
fn shot(tty: &mut Tty, said: &str) {
    let at = tty.send(b"s");
    let asked = tty.expect(at, toast("screenshot ..."));
    tty.expect(asked, toast(said));
}

#[test]
fn s_says_when_it_fails_and_draws_with_the_look() {
    // row P21: screenshot of bin/ascii-viewer:1621 at tag python-final: `screenshot failed` when the file cannot be written; drawn with the player's look: at 95 % darker every value under 242 goes to 0 (tone_table of lib/ascii_theme.py:135), black cells are blank, and bl1nk's background (#111111) is all that is left
    let sandbox = Sandbox::new();
    let video = counter(&sandbox, "clip.mp4", "160x90", 25, 6.0, 6.0, &[]);
    // where the first would go: a folder, which no picture can replace
    sandbox.folder("clip-ascii-1s.png");
    let mut tty = sandbox.launch().arg(&video).size(SMALL).start();
    tty.await_playing();
    pause(&mut tty);
    lands(&mut tty, LEFT, 0, "| 00:00");
    lands(&mut tty, SHIFT_RIGHT, 25, "| 00:01");
    shot(&mut tty, "screenshot failed");
    // the counter cannot be read on screen at that level: the seek first
    lands(&mut tty, SHIFT_RIGHT, 50, "");
    says(&mut tty, &[b'3'; 19], "level 95% darker");
    shot(&mut tty, "saved clip-ascii-2s.png");
    let dark = image::open(sandbox.path("clip-ascii-2s.png"))
        .unwrap()
        .to_rgb8();
    assert!(dark.pixels().all(|px| px.0 == [0x11; 3]));
    tty.send(b"q");
    assert_eq!(tty.exit(), 0);
}

/// Files of a folder closed after a write, from the watch's start on (inotify). Opens are watched too: inotify merges an event into an identical one still unread before it, and the open between two writes keeps them apart.
struct Writes(std::os::fd::OwnedFd);

impl Writes {
    fn watch(folder: &Path) -> Writes {
        use std::os::fd::{AsRawFd, FromRawFd};
        use std::os::unix::ffi::OsStrExt;
        let path = std::ffi::CString::new(folder.as_os_str().as_bytes()).unwrap();
        // SAFETY: plain calls; the descriptor is owned from here on.
        unsafe {
            let fd = libc::inotify_init1(libc::IN_CLOEXEC | libc::IN_NONBLOCK);
            assert!(fd >= 0, "{}", std::io::Error::last_os_error());
            let fd = std::os::fd::OwnedFd::from_raw_fd(fd);
            let mask = libc::IN_OPEN | libc::IN_CLOSE_WRITE;
            let watch = libc::inotify_add_watch(fd.as_raw_fd(), path.as_ptr(), mask);
            assert!(watch >= 0, "{}", std::io::Error::last_os_error());
            Writes(fd)
        }
    }

    /// The names of the files closed after a write since the last call.
    fn names(&self) -> Vec<String> {
        use std::os::fd::AsRawFd;
        let (mut names, mut buf) = (Vec::new(), [0u8; 4096]);
        loop {
            // SAFETY: a read into a buffer of that size.
            let n = unsafe { libc::read(self.0.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len()) };
            if n <= 0 {
                return names;
            }
            // each event: wd, mask, cookie, len (4 bytes each), then a name of len bytes padded with NULs
            let mut at = 0;
            while at + 16 <= n as usize {
                let field =
                    |from: usize| u32::from_ne_bytes(buf[from..from + 4].try_into().unwrap());
                let (mask, len) = (field(at + 4), field(at + 12) as usize);
                if mask & libc::IN_CLOSE_WRITE != 0 {
                    let name = String::from_utf8_lossy(&buf[at + 16..at + 16 + len]);
                    names.push(name.trim_end_matches('\0').to_string());
                }
                at += 16 + len;
            }
        }
    }
}

/// The screenshots written beside the clip from now until `count` writes were seen (at most `PATIENCE`), by name, in order.
fn shots_written(writes: &Writes, count: usize) -> Vec<String> {
    let (mut written, end) = (Vec::new(), Instant::now() + PATIENCE);
    while written.len() < count && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(20));
        written.extend(
            writes
                .names()
                .into_iter()
                .filter(|name| name.starts_with("clip-ascii-")),
        );
    }
    written
}

#[test]
fn each_s_saves_its_own_screenshot() {
    // row P21: screenshot of bin/ascii-viewer:1621 at tag python-final starts a converter for each press, each writing its file, named by its own frame's time, the same file again within the same tenth: two `s` in one write on a paused frame write it twice; seen before the fix: one write, the second press dropped while the first's frame was on its way; on a playing clip two writes too, each file named by the frame it holds (N5: its time to 0.1 s), and the last toast names the last; a toast cannot be counted: it is drawn again with each frame, and the second replaces the first. Run in this harness the old viewer started two converters at once (as a log in a copy of it showed); of 10 runs it passed 4, each under load, and failed 6 on the paused frame, this watch reading the two writes as one (inotify merges identical events left unread), and in review once on the playing clip, the last toast naming the other file: each converter's thread says its own toast when it ends (bin/ascii-viewer:1647), and the one said last is drawn
    let sandbox = Sandbox::new();
    let video = counter(&sandbox, "clip.mp4", "160x90", 25, 6.0, 6.0, &[]);
    let mut tty = sandbox.launch().arg(&video).size(SMALL).start();
    tty.await_playing();
    pause(&mut tty);
    lands(&mut tty, LEFT, 0, "| 00:00");
    let writes = Writes::watch(&sandbox.path("."));
    let at = tty.send(b"ss");
    tty.expect(at, toast("saved clip-ascii-0s.png"));
    assert_eq!(shots_written(&writes, 2), ["clip-ascii-0s.png"; 2]);
    tty.send(b" ");
    tty.until("playing on", |screen| {
        screen.row(24).starts_with("> ") && frame(screen) > 10
    });
    let at = tty.send(b"ss");
    let written = shots_written(&writes, 2);
    assert_eq!(written.len(), 2, "{written:?}");
    tty.expect(at, toast(&format!("saved {}", written[1])));
    for name in &written {
        let held = saved(&sandbox.path(name)).0;
        assert_eq!(*name, format!("clip-ascii-{}s.png", tenths(held * 4)));
    }
    tty.send(b"q");
    assert_eq!(tty.exit(), 0);
}

#[test]
fn s_draws_with_the_look_of_its_press() {
    // row P21: screenshot of bin/ascii-viewer:1621 at tag python-final takes the converter's options at the press (a clip of 60 s: under load the keys, the save and the test's reading of the picture outlasted one of 10, and the viewer had ended at `4`): `s3` in one write on a playing clip of grey 235 at 90 % darker saves at 90 %, the grey still inked, where 95 %, the level after `3`, leaves nothing but bl1nk's background (#111111, as s_says_when_it_fails_and_draws_with_the_look shows); run in this harness the old viewer saved it at 90 %; seen before the look went with the grab: `grey-ascii-2.1s.png: 0 px inked`, the look read when mpv's frame came
    let sandbox = Sandbox::new();
    let video = sandbox.path("grey.mp4");
    ffmpeg(
        &sandbox,
        &[
            "-f",
            "lavfi",
            "-i",
            "color=c=0xebebeb:s=160x90:r=25:d=60",
            "-f",
            "lavfi",
            "-i",
            "sine=d=60",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
            "-c:a",
            "aac",
            video.to_str().unwrap(),
        ],
    );
    let mut tty = sandbox.launch().arg(&video).size(SMALL).start();
    tty.await_playing();
    says(&mut tty, &[b'3'; 18], "level 90% darker");
    let at = tty.send(b"s3");
    let start = tty.expect(at, b"\x1b[7m saved ");
    let end = tty.expect(start, b" \x1b[0m") - 5;
    let name = String::from_utf8(tty.out()[start..end].to_vec()).unwrap();
    let shot = image::open(sandbox.path(&name)).unwrap().to_rgb8();
    let inked = shot.pixels().filter(|px| px.0 != [0x11; 3]).count();
    assert!(inked > 10_000, "{name}: {inked} px inked");
    // the key after it took: from 95 %, `4` makes 90 %
    says(&mut tty, b"4", "level 90% darker");
    tty.send(b"q");
    assert_eq!(tty.exit(), 0);
}

/// The output name the render page shows on its second row.
fn page_name(tty: &mut Tty) -> String {
    let row = tty.screen().row(2);
    row.split_whitespace()
        .next()
        .unwrap_or_default()
        .to_string()
}

/// `s` on the render page: the frame its export `name` beside the clip holds, and its size, once the file reads whole.
fn exported(tty: &mut Tty, sandbox: &Sandbox, name: &str) -> (u32, (u32, u32)) {
    tty.send(b"s");
    let (file, end) = (sandbox.path(name), Instant::now() + PATIENCE);
    while image::open(&file).is_err() {
        assert!(Instant::now() < end, "no {name}");
        std::thread::sleep(Duration::from_millis(20));
    }
    saved(&file)
}

/// A time of `hundredths` of a second, to a tenth, as `%g` writes it (`1.2`, `3`); 25 fps frames never fall on a half.
fn tenths(hundredths: u32) -> String {
    let tenths = (hundredths + 5) / 10;
    match tenths % 10 {
        0 => format!("{}", tenths / 10),
        part => format!("{}.{part}", tenths / 10),
    }
}

/// The frame on screen when the render page came, the page opened after byte `at`: the last drawn before the page's first update.
fn under_page(out: &[u8], at: usize) -> u32 {
    let page = find(out, b" Render", at).unwrap();
    let page = out[..page]
        .windows(8)
        .rposition(|w| w == b"\x1b[?2026h")
        .unwrap();
    drawn(&out[..page]).last().unwrap().1
}

/// The playing updates before the render page came, the page opened after byte `at`, in an 80 x 24 window: when each came and the counter's frame.
fn playing_under_page(tty: &Tty, at: usize) -> Vec<(Instant, u32)> {
    let out = tty.out();
    let page = find(&out, b" Render", at).unwrap();
    let mut screen = Screen::new(24, 80);
    let (mut fed, mut seen) = (0, Vec::new());
    while let Some(start) = find(&out[..page], b"\x1b[?2026h", fed) {
        let Some(close) = find(&out[..page], b"\x1b[?2026l", start) else {
            break;
        };
        screen.feed(&out[fed..close + 8]);
        fed = close + 8;
        if screen.row(24).starts_with("> ") {
            seen.push((tty.came(close).unwrap(), frame(&screen)));
        }
    }
    seen
}

#[test]
fn r_opens_the_page_on_the_frame_and_closing_it_plays_on() {
    // rows P22, R3, R16, N5, N23: open_page and close_page of bin/ascii-viewer:2048 and :2112 at tag python-final: the page on the frame at full size, named by its time when it opens (N5: the frame mpv holds, within 3 frames of the one drawn while playing, the one on screen when paused, its time to 0.1 s); of R3's defaults the size, 1920 on the long side, and the crop, the whole frame; R16's export, `s`, the full-size PNG beside the clip under that name; playback pauses meanwhile and goes on after; the page's lines as the old viewer drew them in this harness; on the last frame too (N23: the old viewer could end there, P29); frames 0 to 254, all under the counter's wrap: of 8 s, the clip ended before the pause sent after the page closed (the stress run: 2 of 12 runs at load 88 to 114)
    let sandbox = Sandbox::new();
    let video = counter(&sandbox, "clip.mp4", "160x90", 25, 10.2, 10.2, &[]);
    let mut tty = sandbox.launch().arg(&video).size(SMALL).start();
    tty.await_playing();
    let at = tty.send(b"r");
    tty.until("the page", |screen| screen.row(1).starts_with(" Render"));
    let opened = Instant::now();
    let last = under_page(&tty.out(), at);
    // the accepted limit of 2026-11-15 (N5's, the fifth): a viewer drawing more than 3 frames apart from mpv under heavy load leaves mpv's frame, which the page takes when mpv answers, past the one on screen (the stress run: 4 past at load 91; round 6: 24 past at load 63); that shows in the last two frames drawn before the page, more than 3 apart or over 3 frames' time apart, and lets the page's frame go past by the frames mpv played meanwhile, as `frame_steps_move_one_frame_and_pause` lets a playing `.`; within 3 of it otherwise
    let few = match playing_under_page(&tty, at)[..] {
        [.., (before, earlier), (latest, _)] => {
            let (apart, skipped) = (latest - before, last.saturating_sub(earlier));
            let played = (apart.as_secs_f64() * 25.0).ceil() as u32;
            if apart > Duration::from_millis(120) || skipped > 3 {
                skipped.max(played)
            } else {
                0
            }
        }
        _ => 0,
    };
    // the frame comes with the first render (the old page rendered in the background)
    tty.until("the preview's frame", |screen| {
        screen.row(1).contains(" 1920x1080 · 160 columns ")
    });
    let screen = tty.screen();
    assert!(
        (1..=24).any(|row| screen
            .row(row)
            .starts_with("   Size       1920x1080  (1920 long side)")),
        "{}",
        screen.text()
    );
    // the page's export holds the frame it was given: within 3 of the one on screen, more only under the limit's sign
    let name = page_name(&mut tty);
    let (grabbed, size) = exported(&mut tty, &sandbox, &name);
    assert_eq!(size, (1920, 1080));
    assert!(
        grabbed.abs_diff(last) <= 3 + few,
        "{name}: frame {grabbed} for frame {last} on screen, past it by {few} more at most"
    );
    tty.send(&b"j".repeat(8));
    tty.send(ENTER);
    tty.until("the crop editor on the whole frame", |screen| {
        screen.row(1).starts_with(" Crop") && screen.row(1).contains(" source · crop 160x90 ")
    });
    assert!(
        tty.screen()
            .row(7)
            .starts_with("   W          100%   (160 px)")
    );
    tty.send(b"q");
    tty.until("the menu", |screen| screen.row(1).starts_with(" Render"));
    // a window for what must not happen, playing on under the page: open 4 s at least, from when the test saw it
    std::thread::sleep(Duration::from_secs(4).saturating_sub(opened.elapsed()));
    let open = opened.elapsed().as_secs_f64();
    let closed = tty.send(b"q");
    tty.until("the player", |screen| {
        screen.row(24).starts_with("> ") && seconds(&screen.row(24)).is_some()
    });
    // the first clock drawn after the page, by its bar, against the frame given to the page: mpv plays on from that frame until the viewer's pause reaches it, as long as the viewer takes to open the page (seen at load 29 to 40: 0.54 to 1.24 s on, the page drawn 1.6 to 2.6 s after `r` and open 7.3 to 11.3 s), while playing on under the page would move the clock on by all the time the page was open: half of it is the bound (one of a second from the clock drawn before `r` failed 3 of 12 runs at load 43 to 45 in review)
    let after = replay(&tty.out()[closed..], SMALL, |screen| screen.row(24))
        .into_iter()
        .find(|row| seconds(row).is_some())
        .map(|row| bar_time(&row, 80, 10.2))
        .unwrap();
    let from = grabbed as f64 / 25.0;
    assert!(
        after - from < open / 2.0,
        "frame {grabbed} ({from:.2} s) given to the page, the clock at {after:.2} s after it was open {open:.1} s"
    );
    tty.until("playing on", |screen| {
        seconds(&screen.row(24)).is_some_and(|at| at > after as u32)
    });
    // paused: the frame on screen, the last drawn before the page (the test's own look right after the pause can be older: at load 40 by 7 to 13 frames, the viewer drawing frames mpv played before its pause after a quiet 300 ms), named by its time; it stays paused (N5: run in this harness the old viewer's export, drawn anew at the name's time, was not the frame on screen in 4 of 6 runs, 75 for 76 and 78 for 77)
    pause(&mut tty);
    let at = tty.send(b"r");
    tty.until("the page", |screen| screen.row(1).starts_with(" Render"));
    let shown = under_page(&tty.out(), at);
    let name = page_name(&mut tty);
    let (taken, _) = exported(&mut tty, &sandbox, &name);
    assert_eq!(
        taken, shown,
        "{name}: frame {taken} for frame {shown} on screen"
    );
    assert_eq!(name, format!("clip-ascii-{}s.png", tenths(shown * 4)));
    tty.send(b"q");
    tty.until("the player on the frame on screen", |screen| {
        screen.row(24).starts_with("| ") && frame(screen) == shown
    });
    // a window for what must not happen: playing on after the page closed
    std::thread::sleep(Duration::from_millis(600));
    let (status, number) = last_drawn(&tty);
    assert!(
        status.starts_with("| ") && number == shown,
        "{status:?} on {number}"
    );
    // on the last frame
    lands(&mut tty, UP, 254, "| 00:10 / 00:10");
    tty.send(b"r");
    tty.until("the page", |screen| screen.row(1).starts_with(" Render"));
    tty.send(b"q");
    tty.until("the player", |screen| screen.row(24).starts_with("| 00:10"));
    assert!(tty.running());
    tty.send(b"q");
    assert_eq!(tty.exit(), 0);
}

#[test]
fn a_frame_taken_while_playing_is_named_by_its_time() {
    // rows N5, P21, P22: the name's time is that of the frame mpv holds, to 0.1 s, for `s` and `r` alike (screenshot and open_page of bin/ascii-viewer:1621 and :2048 at tag python-final named it by the frame on screen); keys right after `s` keep the viewer busy while mpv answers and plays on (each `c` draws the frame again; 10 leave the colour as it was, and under load 40 outlasted an 8 s clip), keys right after `r` wait for the page; seen before the fix, named by the clock when the answer was read: with `r` and the keys 5.1 to 5.4 s for frames at 1.1 and 1.2 s, and in a full parallel run without them `clip-ascii-1.8s.png` holding frame 41 (1.64 s); with `s` and the keys, the clock put back in place of the frame's time: 1.3 and 1.4 s for frame 25 (1 s), 3 of 3
    let sandbox = Sandbox::new();
    let video = counter(&sandbox, "clip.mp4", "160x90", 25, 10.0, 10.0, &[]);
    let mut tty = sandbox.launch().arg(&video).size(SMALL).start();
    tty.await_playing();
    // looping: under load 50 the keys, the save and the test's reading of the picture outlasted the clip, and `r` came after its end; a longer clip would take the counter past its 8 bits
    says(&mut tty, b"L", "loop on");
    let at = tty.send(&[&b"s"[..], &[b'c'; 10]].concat());
    let start = tty.expect(at, b"\x1b[7m saved ");
    let end = tty.expect(start, b" \x1b[0m") - 5;
    let shot = String::from_utf8(tty.out()[start..end].to_vec()).unwrap();
    let taken = saved(&sandbox.path(&shot)).0;
    // the page's export would take the same name within the same tenth
    std::fs::remove_file(sandbox.path(&shot)).unwrap();
    tty.send(b"r");
    tty.until("the page", |screen| screen.row(1).starts_with(" Render"));
    let name = page_name(&mut tty);
    let (grabbed, _) = exported(&mut tty, &sandbox, &name);
    tty.send(b"q");
    tty.until("the player", |screen| screen.row(24).starts_with("> "));
    tty.send(b"q");
    assert_eq!(tty.exit(), 0);
    assert_eq!(shot, format!("clip-ascii-{}s.png", tenths(taken * 4)));
    assert_eq!(name, format!("clip-ascii-{}s.png", tenths(grabbed * 4)));
}

#[test]
fn a_frame_taken_while_the_sound_plays_on_is_named_by_its_time() {
    // rows N5, N21, P21: past the picture's end the sound plays on over the last frame, frame 49 of this 2 s clip with 12 s of sound; `s` there saves that frame named by the position mpv gave with it, 1.96 s, which comes a look later under load, or an earlier frame's when mpv dropped the last frames untaken (seen with three test binaries at once: 1.9 to 2.1 s, and 2 s unloaded); seen before: `clip-ascii-3s.png`, the sound's position; with 6 s of sound the clip ended before the toast at load 45-55 (the review: 4 of 18 copies)
    let sandbox = Sandbox::new();
    let video = counter(&sandbox, "clip.mp4", "160x90", 25, 2.0, 12.0, &[]);
    let mut tty = sandbox.launch().arg(&video).size(SMALL).start();
    sound_on(&tty);
    let at = tty.send(b"s");
    let start = tty.expect(at, b"\x1b[7m saved ");
    let end = tty.expect(start, b" \x1b[0m") - 5;
    let name = String::from_utf8(tty.out()[start..end].to_vec()).unwrap();
    let time: f64 = name["clip-ascii-".len()..name.len() - "s.png".len()]
        .parse()
        .unwrap();
    let number = saved(&sandbox.path(&name)).0;
    assert!(
        (1.5..=2.5).contains(&time) && number == 49,
        "{name}: {number}"
    );
    tty.send(b"q");
    assert_eq!(tty.exit(), 0);
}

#[test]
fn mpeg_ts_counts_time_from_its_start() {
    // row P23: tests/test_viewer.py:47 at tag python-final: times count from the container's start (here 3 s plus the muxer's delay), a seek lands at or before its target, the frame due or the one before (run in this harness the old viewer landed a second in on its seek to the start)
    let sandbox = Sandbox::new();
    let video = counter(
        &sandbox,
        "late.ts",
        "160x90",
        25,
        4.0,
        4.0,
        &["-output_ts_offset", "3"],
    );
    let mut tty = sandbox.launch().arg(&video).size(SMALL).start();
    tty.await_playing();
    pause(&mut tty);
    let first = drawn(&tty.out())
        .into_iter()
        .find_map(|(clock, _)| clock)
        .unwrap();
    assert_eq!(first, 0);
    lands(&mut tty, LEFT, 0, "| 00:00");
    tty.send(SHIFT_RIGHT);
    tty.until("a second on", |screen| {
        [24, 25].contains(&frame(screen)) && screen.row(24).starts_with("| ")
    });
    tty.send(b"q");
    assert_eq!(tty.exit(), 0);
}

/// A counter clip of variable rate, `seconds` long at 25 frames a second with frames 10 to `after` - 1 left out, with as much sound.
fn gap_clip(sandbox: &Sandbox, seconds: u32, after: u32) -> std::path::PathBuf {
    let gap = sandbox.path("gap.mkv");
    let picture =
        format!("nullsrc=s=160x90:r=25:d={seconds},{COUNTER},select='lt(n,10)+gte(n,{after})'");
    let tone = format!("sine=d={seconds}");
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
            "-c:a",
            "aac",
            "-fps_mode",
            "vfr",
            "-c:v",
            "libx264",
            "-g",
            "5",
            "-pix_fmt",
            "yuv420p",
            gap.to_str().unwrap(),
        ],
    );
    gap
}

#[test]
fn a_gap_in_a_variable_rate_clip_holds_the_frame_before_it() {
    // row P24: Decoder.read of bin/ascii-viewer:479 at tag python-final holds the last frame across a gap; run in this harness the old viewer showed frames up to 9, then 35 to 49 of a 2 s clip with frames 10 to 34 left out. Here frames 10 to 109 of 6 s are left out: frames 1 to 9 are due by 0.36 s, 110 at 4.4 s, so the frame before the gap stays 4 s; the hold is timed by when the updates came, from the first that shows a frame before the gap to the first after it, as the test's own first sight of one before the gap came late in the hold under load (1.06 s at load 73, the stress run; the 1 s gap of the 2 s clip measured 0.40 to 0.66 s at 111 to 215 runnable tasks), so half the gap is the bound
    let sandbox = Sandbox::new();
    let gap = gap_clip(&sandbox, 6, 110);
    let mut tty = sandbox.launch().arg(&gap).size(SMALL).start();
    assert_eq!(tty.exit(), 0);
    let draws = updates(&tty, 0);
    let came = |shows: &dyn Fn(u32) -> bool| {
        let first = draws.iter().find(|(_, number, _)| shows(*number));
        first.map(|(came, ..)| *came)
    };
    let before = came(&|number| (1..10).contains(&number));
    let after = came(&|number| number >= 110);
    let took = before.zip(after).map(|(before, after)| after - before);
    assert!(
        took.is_some_and(|took| took > Duration::from_secs(2)),
        "held for {took:?}"
    );
    let mut numbers: Vec<u32> = drawn(&tty.out()).into_iter().map(|(_, n)| n).collect();
    numbers.dedup();
    let after = numbers.iter().position(|number| *number >= 110).unwrap();
    assert!(
        after > 0
            && numbers[after - 1] <= 9
            && !numbers.iter().any(|number| (10..110).contains(number)),
        "{numbers:?}"
    );
}

#[test]
fn the_clock_stands_with_the_frame_held_through_a_gap() {
    // rows P16, P24, N26: the clock is the time of the frame on screen: frame 9 is due at 0.36 s and held until frame 35 comes at 1.4 s, so at about 1 s the clock shows 00:00 over frame 9 and 00:01 comes with frame 35 (the old viewer's clock ran with the sound, clock and position of bin/ascii-viewer:1394 at tag python-final: 00:01 over frame 9)
    let sandbox = Sandbox::new();
    let gap = gap_clip(&sandbox, 2, 35);
    let mut tty = sandbox.launch().arg(&gap).size(SMALL).start();
    assert_eq!(tty.exit(), 0);
    let drawn = replay(&tty.out(), SMALL, |screen| (frame(screen), screen.row(24)));
    // the time a status row shows by its bar: 64 cells for this 2 s clip
    let time = |row: &str| row.matches('━').count() as f64 * 2.0 / 64.0;
    // mpv reports the time and hands over the frame apart, under load a frame or so either way (seen in parallel runs: frame 9 under 00:01 and a bar of 44 cells, frame 35's time; frame 35 under 00:00); a time inside the gap is the clock running on
    let held: Vec<f64> = drawn
        .iter()
        .filter(|(number, _)| *number == 9)
        .map(|(_, row)| time(row))
        .collect();
    let inside = |time: &f64| (0.6..1.2).contains(time);
    assert!(!held.is_empty() && !held.iter().any(inside), "{drawn:?}");
    let first = drawn.iter().find(|(_, row)| row.starts_with("> 00:01 "));
    assert!(
        first.is_some_and(|(number, row)| *number >= 35 || time(row) >= 1.2),
        "{drawn:?}"
    );
}

#[test]
fn damaged_frames_are_passed_over() {
    // row P25: Decoder.next_frame of bin/ascii-viewer:468 at tag python-final passes over a frame that does not decode; here 300 bytes zeroed at three tenths of a 4 s clip take frames 32 to 41, which leaves 58 frames past them for a viewer slowed by load (a 2 s clip damaged mid-file left 8, of which such a viewer drew none in some runs); run in this harness the old viewer ended at the damage instead: frames 0 to 30, then exit 0
    let sandbox = Sandbox::new();
    let video = counter(&sandbox, "hurt.mp4", "160x90", 25, 4.0, 0.0, &[]);
    let mut bytes = std::fs::read(&video).unwrap();
    let at = bytes.len() * 3 / 10;
    bytes[at..at + 300].fill(0);
    std::fs::write(&video, bytes).unwrap();
    let launched = Instant::now();
    let mut tty = sandbox.launch().arg(&video).size(SMALL).start();
    assert_eq!(tty.exit(), 0);
    let ran = launched.elapsed().as_secs_f64();
    let mut numbers: Vec<u32> = drawn(&tty.out()).into_iter().map(|(_, n)| n).collect();
    numbers.dedup();
    // on past the damage, to the clip's end: the run lasts more than 3.6 s from its launch, as mpv plays no faster than the clock (by the frame on screen at the test's look, which lags mpv under load, an end can measure short)
    assert!(numbers.last() >= Some(&42), "{numbers:?}");
    assert!(ran > 3.6, "ended {ran:.2} s after the launch");
}

#[test]
fn a_file_with_cover_art_is_audio_alone() {
    // rows P26, A3: probe of bin/ascii-viewer:422 at tag python-final: the attached picture is no video; the scope draws, the status row is written between ESC 7 and ESC 8, no picture (with `-frames:v 1` as tests/test_viewer.py:84 made it mpv cannot load such a file, nor could the old viewer's mpv)
    let sandbox = Sandbox::new();
    let cover = sandbox.path("cover.m4a");
    ffmpeg(
        &sandbox,
        &[
            "-f",
            "lavfi",
            "-i",
            "sine=d=3",
            "-f",
            "lavfi",
            "-i",
            "color=s=32x32:d=1",
            "-map",
            "0",
            "-map",
            "1",
            "-c:a",
            "aac",
            "-c:v",
            "png",
            "-disposition:v",
            "attached_pic",
            cover.to_str().unwrap(),
        ],
    );
    let mut tty = sandbox.launch().arg(&cover).size(SMALL).start();
    sandbox.await_noted("scope.out", "ready");
    tty.expect(0, b"\x1b7\x1b[24;1H\x1b[0m\x1b[2K\x1b[24;1H\x1b[0m> 00:");
    tty.send(b"q");
    assert_eq!(tty.exit(), 0);
    assert!(
        find(&tty.out(), b"\x1b[?2026h", 0).is_none(),
        "a picture was drawn"
    );
}

#[test]
fn an_animation_plays_as_video() {
    // row V17: bin/ascii-viewer:1223 at tag python-final: an animated GIF is no still: it plays with a status row and ends (run in this harness the old viewer showed `> 00:01 / 00:03`)
    let sandbox = Sandbox::new();
    let gif = sandbox.path("anim.gif");
    ffmpeg(
        &sandbox,
        &[
            "-f",
            "lavfi",
            "-i",
            "testsrc2=s=80x45:r=10:d=3",
            gif.to_str().unwrap(),
        ],
    );
    let mut tty = sandbox.launch().arg(&gif).size(SMALL).start();
    tty.await_status("> 00:01 / 00:03 ━");
    assert!(tty.screen().inked() > 1000);
    assert_eq!(tty.exit(), 0);
}

/// `picture` as an APNG of one frame: acTL and fcTL before the PNG's image data; or, `behind` a default image, acTL alone there, the image data that default image, outside the animation, and the frame after it in an fdAT of the same data (ffmpeg writes one frame as a plain PNG).
fn one_frame_apng(picture: &image::RgbImage, behind: bool) -> Vec<u8> {
    let mut png = Vec::new();
    let encoder = image::codecs::png::PngEncoder::new(&mut png);
    picture.write_with_encoder(encoder).unwrap();
    let chunk = |kind: &[u8], data: &[u8]| {
        let mut crc = flate2::Crc::new();
        crc.update(kind);
        crc.update(data);
        let length = (data.len() as u32).to_be_bytes();
        [&length[..], kind, data, &crc.sum().to_be_bytes()].concat()
    };
    let (w, h) = picture.dimensions();
    // sequence 0, the whole picture, a delay of 1/10 s
    let control = [
        &[0; 4][..],
        &w.to_be_bytes(),
        &h.to_be_bytes(),
        &[0; 8],
        &[0, 1, 0, 10, 0, 0],
    ]
    .concat();
    let data = find(&png, b"IDAT", 0).unwrap() - 4; // where its length starts
    let length = u32::from_be_bytes(png[data..data + 4].try_into().unwrap()) as usize;
    let (pixels, end) = (&png[data + 8..data + 8 + length], data + 12 + length);
    let animation = chunk(b"acTL", &[0, 0, 0, 1, 0, 0, 0, 0]);
    if !behind {
        return [
            &png[..data],
            &animation,
            &chunk(b"fcTL", &control),
            &png[data..],
        ]
        .concat();
    }
    let frame = chunk(b"fdAT", &[&1u32.to_be_bytes()[..], pixels].concat()); // sequence 1
    let rest = [&chunk(b"fcTL", &control)[..], &frame, &png[end..]].concat();
    [&png[..data], &animation, &png[data..end], &rest].concat()
}

/// `picture` as an animated WebP of one frame: the encoder's lossless frame in an ANMF chunk (ffmpeg writes one frame as a plain WebP).
fn one_frame_webp(picture: &image::RgbImage) -> Vec<u8> {
    let mut frame = Vec::new();
    let encoder = image::codecs::webp::WebPEncoder::new_lossless(&mut frame);
    picture.write_with_encoder(encoder).unwrap();
    let chunk = |kind: &[u8], data: &[u8]| {
        let length = (data.len() as u32).to_le_bytes();
        [kind, &length[..], data, &[0][..data.len() % 2]].concat()
    };
    let (w, h) = picture.dimensions();
    let size = [(w - 1).to_le_bytes(), (h - 1).to_le_bytes()].map(|side| side[..3].to_vec());
    let size = size.concat();
    // offset, size, a duration of 100 ms, no flags; the frame's chunk follows the encoder's RIFF header
    let head = [&[0; 6][..], &size, &[100, 0, 0, 0], &frame[12..]].concat();
    let body = [
        &b"WEBP"[..],
        &chunk(b"VP8X", &[&[0x02, 0, 0, 0][..], &size].concat()), // the animation flag
        &chunk(b"ANIM", &[0; 6]),
        &chunk(b"ANMF", &head),
    ]
    .concat();
    [&b"RIFF"[..], &(body.len() as u32).to_le_bytes(), &body].concat()
}

#[test]
fn a_one_frame_animation_is_a_still() {
    // rows V17, S11: is_animation of lib/ascii_image.py:61 at tag python-final counts frames (Pillow's n_frames): an APNG or an animated WebP of one frame is a still, which `+` zooms, shown without a status row (it went to mpv while their headers decided)
    let sandbox = Sandbox::new();
    let picture = gradient(160, 90);
    let files = [
        ("one.png", one_frame_apng(&picture, false)),
        ("one.webp", one_frame_webp(&picture)),
    ]
    .map(|(name, bytes)| {
        let file = sandbox.path(name);
        std::fs::write(&file, bytes).unwrap();
        file
    });
    let mut ttys = files
        .each_ref()
        .map(|file| sandbox.launch().arg(file).size(SMALL).start());
    for (tty, file) in ttys.iter_mut().zip(&files) {
        let name = file.file_name().unwrap().to_string_lossy();
        tty.until(&format!("{name}'s picture"), |screen| screen.inked() > 600);
        let at = tty.send(b"+");
        tty.expect(at, b"\x1b[7m zoom ");
        tty.send(b"q");
        assert_eq!(tty.exit(), 0, "{name}");
        let status = replay(&tty.out(), SMALL, |screen| screen.row(24).starts_with("> "));
        assert!(!status.contains(&true), "{name}: a status row");
    }
}

#[test]
fn a_one_frame_apng_behind_a_default_image_plays_as_video() {
    // rows V17, S1: is_animation of lib/ascii_image.py:61 at tag python-final: Pillow's n_frames counts an APNG's default image outside the animation (no fcTL before its image data), so one frame behind it makes two, an animation, which plays through mpv under a status row and ends (it opened as a still while frames were counted by decoding, which passes over that image); run in this harness the old viewer did the same
    let sandbox = Sandbox::new();
    let apng = sandbox.path("default.png");
    std::fs::write(&apng, one_frame_apng(&gradient(160, 90), true)).unwrap();
    let mut tty = sandbox.launch().arg(&apng).size(SMALL).start();
    assert_eq!(tty.exit(), 0);
    let rows = replay(&tty.out(), SMALL, |screen| screen.row(24));
    assert!(
        rows.iter().any(|row| row.starts_with("> 00:00")),
        "no status row: {rows:?}"
    );
}

#[test]
fn a_picture_only_mpv_decodes_shows_as_one_frame() {
    // rows V17, N7: a PCX picture, which the built-in decoders do not take, opens as a one-frame video, its picture under a status row without a length, held until `q` (the old viewer opened it as a still, without a status row)
    let sandbox = Sandbox::new();
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
    let mut tty = sandbox.launch().arg(&pcx).size(SMALL).start();
    tty.until("the picture", |screen| {
        screen.inked() > 1000 && screen.row(24) == "> 00:00"
    });
    // a window for what must not happen: an end
    std::thread::sleep(Duration::from_secs(1));
    assert!(tty.running());
    tty.send(b"q");
    assert_eq!(tty.exit(), 0);
}

#[test]
fn a_heic_picture_shows_until_q() {
    // rows V17, N7: a HEIC picture (tests/golden/picture-testsrc2.heic, heif-enc's of ffmpeg's testsrc2), which the built-in decoders do not take and mpv reads through FFmpeg's MP4 demuxer, opens as a one-frame video held until `q` (it ended right after its frame while the rule went by demuxer names; run in this harness the old viewer drew it under `> 00:00` and ended within the second)
    let sandbox = Sandbox::new();
    let heic = sandbox.path("pic.heic");
    std::fs::copy(
        Path::new(CRATE).join("tests/golden/picture-testsrc2.heic"),
        &heic,
    )
    .unwrap();
    let mut tty = sandbox.launch().arg(&heic).size(SMALL).start();
    tty.until("the picture", |screen| {
        screen.inked() > 1000 && screen.row(24) == "> 00:00"
    });
    // a window for what must not happen: an end
    std::thread::sleep(Duration::from_secs(1));
    assert!(tty.running());
    tty.send(b"q");
    assert_eq!(tty.exit(), 0);
}

#[test]
fn a_step_on_a_picture_only_pauses() {
    // rows P15, N7: a picture mpv holds until `q` (N7) has one frame, its first and its last: `.` and `,` only pause there, as on a clip's last frame, and the keys after them act: `s` saves the picture, `space` plays on; seen before the fix: after `.`, `s` saved nothing and the status row stayed `| 00:00`, mpv's step playing on to a frame that never comes (the review's probe on this file, 2 of 2); the old viewer showed this file as a video and ended within the second (N7)
    let sandbox = Sandbox::new();
    let heic = sandbox.path("pic.heic");
    std::fs::copy(
        Path::new(CRATE).join("tests/golden/picture-testsrc2.heic"),
        &heic,
    )
    .unwrap();
    let mut tty = sandbox.launch().arg(&heic).size(SMALL).start();
    tty.until("the picture", |screen| {
        screen.inked() > 1000 && screen.row(24) == "> 00:00"
    });
    // each key from playing, then from paused; `s` waits for the requests before it (N5)
    for key in [&b"."[..], b","] {
        tty.send(key);
        tty.await_status("| 00:00");
        shot(&mut tty, "saved pic-ascii-0s.png");
        tty.send(key);
        shot(&mut tty, "saved pic-ascii-0s.png");
        tty.send(b" ");
        tty.await_status("> 00:00");
    }
    assert!(image::open(sandbox.path("pic-ascii-0s.png")).is_ok());
    tty.send(b"q");
    assert_eq!(tty.exit(), 0);
}

#[test]
fn a_step_takes_an_animated_gif_one_frame_on() {
    // rows P15, V17: an animated GIF plays as video (V17), where `.` from a pause shows the next frame, as frame_step of bin/ascii-viewer:1441 at tag python-final does on any video; seen before the fix: frame 0 after each `.`, under `| 00:00 / 00:08`, the GIF taken for a picture by mpv's demuxer alone, where a step only pauses (the review's probe)
    let sandbox = Sandbox::new();
    let gif = sandbox.path("anim.gif");
    let picture = format!("nullsrc=s=160x90:r=10:d=8,{COUNTER}");
    let gray = ["-pix_fmt", "gray", gif.to_str().unwrap()];
    ffmpeg(
        &sandbox,
        &[&["-f", "lavfi", "-i", &picture][..], &gray].concat(),
    );
    let mut tty = sandbox.launch().arg(&gif).size(SMALL).start();
    paused_at_the_start(&mut tty);
    for number in 1..=3 {
        lands(&mut tty, b".", number, "| 00:00 / 00:08");
    }
    tty.send(b"q");
    assert_eq!(tty.exit(), 0);
}

#[test]
fn a_picture_mpv_knows_by_its_suffix_shows_until_q() {
    // rows V17, N7: an X-Face picture, which neither the built-in decoders nor FFmpeg's probes take, opens through mpv's picture demuxer by its suffix (mf), 1 s long at mf-fps 1, as a one-frame video held until `q` (it ended after its frame while the rule went by the length; run in this harness the old viewer drew nothing and ended)
    let sandbox = Sandbox::new();
    let face = sandbox.path("pic.xface");
    ffmpeg(
        &sandbox,
        &[
            "-f",
            "lavfi",
            "-i",
            "testsrc2=s=48x48:d=1",
            "-frames:v",
            "1",
            "-c:v",
            "xface",
            "-pix_fmt",
            "monob",
            "-f",
            "image2",
            face.to_str().unwrap(),
        ],
    );
    let mut tty = sandbox.launch().arg(&face).size(SMALL).start();
    tty.until("the picture", |screen| {
        screen.inked() > 300 && screen.row(24).starts_with("> 00:00")
    });
    // a window for what must not happen: an end
    std::thread::sleep(Duration::from_secs(1));
    assert!(tty.running());
    tty.send(b"q");
    assert_eq!(tty.exit(), 0);
}

#[test]
fn a_one_frame_video_draws_and_ends() {
    // rows V17, P14, N7: an MP4, and a Matroska file written as a live stream, which has no length (held until `q` while the rule went by the length); run in this harness the old viewer drew each and ended, exit 0 (the MP4 in 0.45 s)
    let sandbox = Sandbox::new();
    let videos = [("one.mp4", None), ("live.mkv", Some("-live"))].map(|(name, live)| {
        let file = sandbox.path(name);
        let mut args = vec!["-f", "lavfi", "-i", "testsrc2=s=160x90:r=25:d=1"];
        args.extend(["-frames:v", "1", "-c:v", "libx264", "-pix_fmt", "yuv420p"]);
        args.extend(live.map(|live| [live, "1"]).iter().flatten());
        args.push(file.to_str().unwrap());
        ffmpeg(&sandbox, &args);
        file
    });
    let mut ttys = videos
        .each_ref()
        .map(|video| sandbox.launch().arg(video).size(SMALL).start());
    for (tty, video) in ttys.iter_mut().zip(&videos) {
        let name = video.file_name().unwrap().to_string_lossy();
        let status = tty.exit_within(Duration::from_secs(10));
        let drew = replay(&tty.out(), SMALL, Screen::inked)
            .into_iter()
            .max()
            .unwrap_or(0);
        assert!(drew > 1000, "{name}: no picture: {drew} cells");
        assert_eq!(status.and_then(|status| status.code()), Some(0), "{name}");
    }
}

#[test]
fn the_end_quits_on_the_last_frame_drawn() {
    // rows P14, P16: ended() of bin/ascii-viewer:2135 at tag python-final quits at the end, nothing drawn after the last frame; mpv's stop on the last frame (keep-open) is no pause to show: the last status row of a clip with sound and of one without is `>`
    let sandbox = Sandbox::new();
    let sound = counter(&sandbox, "sound.mp4", "160x90", 25, 1.5, 1.5, &[]);
    let silent = counter(&sandbox, "silent.mp4", "160x90", 25, 1.5, 0.0, &[]);
    let mut ttys = [&sound, &silent].map(|clip| sandbox.launch().arg(clip).size(SMALL).start());
    for tty in &mut ttys {
        assert_eq!(tty.exit(), 0);
        let marks: Vec<String> = replay(&tty.out(), SMALL, |screen| screen.row(24))
            .into_iter()
            .filter(|row| row.starts_with("> ") || row.starts_with("| "))
            .collect();
        let last = &marks[marks.len().saturating_sub(3)..];
        assert!(
            last.last().is_some_and(|row| row.starts_with("> ")),
            "{last:?}"
        );
    }
}

#[test]
fn sound_that_outlasts_the_video_plays_on_over_its_last_frame() {
    // rows N21, P14: a 1 s picture with 3 s of sound plays to the sound's end on its last frame (the old viewer ended with the last frame, after 1.3 s here)
    let sandbox = Sandbox::new();
    let video = counter(&sandbox, "clip.mp4", "160x90", 25, 1.0, 3.0, &[]);
    let launched = Instant::now();
    let mut tty = sandbox.launch().arg(&video).size(SMALL).start();
    assert_eq!(tty.exit(), 0);
    // the run lasts more than 2.5 s from its launch, as mpv plays no faster than the clock (by the bar at the test's look, which lags mpv under load, an end can measure short)
    let ran = launched.elapsed().as_secs_f64();
    assert!(ran > 2.5, "ended {ran:.2} s after the launch");
    // a frame of the picture held while the clock runs on
    let drawn = drawn(&tty.out());
    assert!(
        drawn
            .iter()
            .any(|(at, number)| *at == Some(2) && *number > 0),
        "{:?}",
        &drawn[drawn.len().saturating_sub(10)..]
    );
}

#[test]
fn video_plays_on_without_sound_and_after_its_sound_ends() {
    // rows P6, P14: bin/ascii-viewer:1339 at tag python-final: without a sound track, or with sound shorter than the picture, the picture plays at its pace to its end; clips of 5 s, as under load the viewer drew nothing in a 3 s clip's last second (the stress run, load 75) and start-up alone neared a bound from the launch (the accepted limit, owner 2026-11-13 15:40, its third): over 4.5 s from the launch, past 1 s of sound and start-up
    let sandbox = Sandbox::new();
    let silent = counter(&sandbox, "silent.mp4", "160x90", 25, 5.0, 0.0, &[]);
    let short = counter(&sandbox, "short.mp4", "160x90", 25, 5.0, 1.0, &[]);
    let mut launched = Vec::new();
    let mut ttys = [&silent, &short].map(|video| {
        launched.push(Instant::now());
        sandbox.launch().arg(video).size(SMALL).start()
    });
    // each clip's time when it is first seen playing, the clock past 00:00, by the frame of its last such draw then: a test thread late under load sees a later frame; read from each run's output, as a run that ended leaves the screen blank (seen at load 83: the second run gone before the test looked at it)
    let playing = |screen: &Screen| {
        let row = screen.row(24);
        let past = row.starts_with("> ") && row.contains(" / ") && !row.starts_with("> 00:00 ");
        past.then(|| frame(screen))
    };
    let last_playing = |out: &[u8]| replay(out, SMALL, playing).into_iter().flatten().last();
    let mut seen = Vec::new();
    for tty in &ttys {
        if !tty.wait(0, PATIENCE, |out| last_playing(out).is_some()) {
            tty.fail("never seen playing");
        }
        seen.push((Instant::now(), last_playing(&tty.out()).unwrap()));
    }
    // and how far into the clip each ended: more than 4.5 s from the launch, as mpv plays no faster than the clock, and at most 12 s into the clip by that frame, which under load lags mpv (an end taken from it alone measured 2.18 to 2.42 s of a 3 s clip at load 50 to 77)
    let (mut took, end) = ([None; 2], Instant::now() + PATIENCE);
    while took.contains(&None) && Instant::now() < end {
        for (i, tty) in ttys.iter_mut().enumerate() {
            if took[i].is_none() && !tty.running() {
                let (at, from) = seen[i];
                let ran = launched[i].elapsed().as_secs_f64();
                took[i] = Some((ran, from as f64 / 25.0 + at.elapsed().as_secs_f64()));
            }
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    for (tty, took) in ttys.iter_mut().zip(took) {
        assert_eq!(tty.exit(), 0);
        let (ran, took) = took.unwrap();
        assert!(
            ran > 4.5 && took < 12.0,
            "ended {ran:.2} s after the launch, {took:.2} s into the clip"
        );
        let drawn = drawn(&tty.out());
        // the last 2 s play on, to the last frames (a debug build among the other tests drops late ones)
        let late: Vec<u32> = [3, 4]
            .iter()
            .flat_map(|second| frames_in(&drawn, *second))
            .collect();
        assert!(!late.is_empty(), "{drawn:?}");
        assert!(
            drawn.last().is_some_and(|(_, number)| *number >= 75),
            "{drawn:?}"
        );
    }
}

#[test]
fn every_run_starts_at_the_defaults() {
    // row V25: Player.__init__ of bin/ascii-viewer:1245 at tag python-final: level 0, contrast 0, saturation 100, glyphs mixed, bold and mono off, speed 100 %, unmuted, loop off, mpv's own volume (100); nothing is kept from a run before
    let sandbox = Sandbox::new();
    let video = clip(&sandbox, "clip.mp4", 20.0, "160x90", 25);
    for _ in 0..2 {
        let mut tty = sandbox.launch().arg(&video).size(SMALL).start();
        tty.await_playing();
        pause(&mut tty);
        for (key, text) in [
            ("3", "level 5% darker"),
            ("1", "contrast -5%"),
            ("7", "saturation 90%"),
            ("g", "glyphs: density"),
            ("b", "bold"),
            ("c", "mono"),
            ("]", "speed 110%"),
            ("m", "muted"),
            ("L", "loop on"),
            ("0", "volume 102"),
        ] {
            says(&mut tty, key.as_bytes(), text);
        }
        tty.send(b"q");
        assert_eq!(tty.exit(), 0);
    }
}
