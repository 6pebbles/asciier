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
    // rows P2, P4, P14: play_rate and run of bin/ascii-viewer:431 and :2386 at tag python-final: 25 frames a second, each drawn when the clock reaches its time, exit 0 at the end; run in this harness the old viewer drew all 25 frames of each second
    let sandbox = Sandbox::new();
    let video = counter(&sandbox, "clip.mp4", "160x90", 25, 3.0, 3.0, &[]);
    let mut tty = sandbox.launch().arg(&video).size(SMALL).start();
    tty.await_playing();
    let playing = Instant::now();
    assert_eq!(tty.exit(), 0);
    let took = playing.elapsed();
    assert!(
        took > Duration::from_millis(1500) && took < Duration::from_secs(10),
        "{took:?} from 00:01 to the end"
    );
    let drawn = drawn(&tty.out());
    for &(at, number) in &drawn {
        // a draw can carry the clock of the frame before
        let time = number as f64 / 25.0;
        if let Some(at) = at {
            assert!(
                time > at as f64 - 0.1 && time < at as f64 + 1.1,
                "frame {number} drawn at {at} s"
            );
        }
    }
    // no more frames than the clip's own; a frame drawn late is dropped (row P4): a debug build among the other tests drew 1 to 25 of a second's 25, as loaded as the machine was
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

#[test]
fn a_faster_clip_is_drawn_at_30_frames_a_second_at_most() {
    // rows P2, P24: MAX_FPS of bin/ascii-viewer:106 at tag python-final: of a 60 fps source at most 30 frames a second show, the others passed over (the old viewer drew every other one); the new one skips a frame due within 30 ms of the last drawn
    let sandbox = Sandbox::new();
    let video = counter(&sandbox, "fast.mp4", "160x90", 60, 3.0, 0.0, &[]);
    let mut tty = sandbox.launch().arg(&video).size(SMALL).start();
    assert_eq!(tty.exit(), 0);
    let second = frames_in(&drawn(&tty.out()), 1);
    assert!(
        (1..=34).contains(&second.len()),
        "{} frames in a second: {second:?}",
        second.len()
    );
    assert!(
        second.windows(2).all(|pair| pair[0] < pair[1]),
        "{second:?}"
    );
}

#[test]
fn space_and_p_pause_and_play_on() {
    // row P8: bin/ascii-viewer:2270 at tag python-final: `space` and `p` pause and play; after `,` play goes on from the frame on screen
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
        let status = tty.screen().row(24);
        assert!(status.starts_with("| 00:01 / 00:10"), "{status:?}");
        assert_eq!(frame(tty.screen()), 25);
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
    let from = tty.send(b" ");
    tty.until("playing on", |screen| {
        screen.row(24).starts_with("> ") && frame(screen) > 24
    });
    let next = replay(&tty.out()[from..], SMALL, frame)
        .into_iter()
        .find(|number| *number != 24)
        .unwrap();
    assert!((25..=28).contains(&next), "on from frame 24 to {next}");
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
    let status = tty.screen().row(24);
    assert!(status.starts_with("| 00:00"), "{status:?}");
    tty.send(b"q");
    assert_eq!(tty.exit(), 0);
}

#[test]
fn two_spaces_in_one_batch_leave_it_as_it_was() {
    // rows P8, V14: each `space` toggles what the one before asked for, as bin/ascii-viewer:2270 at tag python-final flips its own flag; mpv may report neither of two requests so close
    let sandbox = Sandbox::new();
    let video = counter(&sandbox, "clip.mp4", "160x90", 25, 10.0, 10.0, &[]);
    let mut tty = sandbox.launch().arg(&video).size(SMALL).start();
    tty.await_playing();
    let from = frame(tty.screen());
    tty.send(b"  ");
    tty.until("playing on", |screen| {
        screen.row(24).starts_with("> ") && frame(screen) > from + 25
    });
    pause(&mut tty);
    lands(&mut tty, DOWN, 0, "| 00:00");
    tty.send(b"  ");
    // a window for what must not happen: the clock or the picture moving on
    std::thread::sleep(Duration::from_millis(600));
    let status = tty.screen().row(24);
    assert!(status.starts_with("| 00:00"), "{status:?}");
    assert_eq!(frame(tty.screen()), 0);
    tty.send(b"q");
    assert_eq!(tty.exit(), 0);
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
    let steps: [(&[u8], u32); 15] = [
        (LEFT, 0), // from about 1 s: held at the start
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
        // a batch adds up (row V14): 180 s on, held at the last frame
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
    // rows P15, N2: frame_step of bin/ascii-viewer:1441 at tag python-final: `.` a frame on, `,` a frame back, then paused, a playing clip paused first; `,` on the first frame and `.` on the last only pause; the clock follows the frame; `,` right after a seek is mpv's back step (N2)
    let sandbox = Sandbox::new();
    // frames 0 to 54
    let video = counter(&sandbox, "clip.mp4", "160x90", 25, 2.2, 2.2, &[]);
    let mut tty = sandbox.launch().arg(&video).size(SMALL).start();
    tty.await_playing();
    tty.send(b".");
    tty.await_status("| ");
    tty.settle(Duration::from_millis(300));
    lands(&mut tty, LEFT, 0, "| 00:00");
    // on the first frame `,` only pauses; a window for what must not happen: the clip moving on
    tty.send(b",");
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(
        (frame(tty.screen()), tty.screen().row(24)),
        (0, "| 00:00 / 00:02".to_string())
    );
    lands(&mut tty, SHIFT_RIGHT, 25, "| 00:01");
    for (key, number, status) in [
        (b",", 24, "| 00:00"),
        (b",", 23, "| 00:00"),
        (b".", 24, "| 00:00"),
        (b".", 25, "| 00:01"),
    ] {
        lands(&mut tty, key, number, status);
    }
    lands(&mut tty, SHIFT_RIGHT, 50, "| 00:02");
    for number in 51..=54 {
        lands(&mut tty, b".", number, "| 00:02");
    }
    // on the last frame `.` only pauses
    tty.send(b".");
    // a window for what must not happen: the picture moving on, or the run ending
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(frame(tty.screen()), 54);
    assert!(tty.running());
    lands(&mut tty, LEFT, 0, "| 00:00");
    tty.send(b"q");
    assert_eq!(tty.exit(), 0);
}

#[test]
#[ignore = "defect: after `.` on the last frame of a clip whose sound ends with it, `,` shows the last frame again"]
fn a_step_on_the_last_frame_keeps_its_place() {
    // row P15: `.` on the last frame only pauses (frame_step of bin/ascii-viewer:1441 at tag python-final returns there, its clock on that frame): the status row stays as it was on frame 54 of this 2.2 s clip with 2.2 s of sound, and `,` after it shows frame 53; seen: 54 again, once with `| 00:02 / 00:02` and the whole bar, the clock at the length; a silent clip, or one whose sound outlasts the picture, steps back as it should
    let sandbox = Sandbox::new();
    let video = counter(&sandbox, "clip.mp4", "160x90", 25, 2.2, 2.2, &[]);
    let mut tty = sandbox.launch().arg(&video).size(SMALL).start();
    tty.await_playing();
    pause(&mut tty);
    lands(&mut tty, LEFT, 0, "| 00:00");
    lands(&mut tty, b"\x1b[1;2C\x1b[1;2C", 50, "| 00:02");
    for number in 51..=54 {
        lands(&mut tty, b".", number, "| 00:02");
    }
    let on_it = tty.screen().row(24);
    tty.send(b".");
    // a window for what must not happen: the clock moving on
    std::thread::sleep(Duration::from_millis(1000));
    assert_eq!(tty.screen().row(24), on_it);
    tty.send(b",");
    let end = Instant::now() + Duration::from_secs(5);
    while frame(tty.screen()) != 53 && Instant::now() < end {
        let at = tty.len();
        tty.wait(at, Duration::from_millis(100), |out| !out.is_empty());
    }
    assert_eq!(frame(tty.screen()), 53, "`,` after it");
    tty.send(b"q");
    assert_eq!(tty.exit(), 0);
}

#[test]
fn steps_take_one_source_frame_above_30_frames_a_second() {
    // row N3: `.` and `,` step one frame of a 60 fps source; the old viewer stepped on its 30 fps grid, two of them
    let sandbox = Sandbox::new();
    let video = counter(&sandbox, "fast.mp4", "160x90", 60, 3.0, 0.0, &[]);
    let mut tty = sandbox.launch().arg(&video).size(SMALL).start();
    tty.await_playing();
    pause(&mut tty);
    lands(&mut tty, LEFT, 0, "| 00:00");
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
    let video = silent_clip(&sandbox, "clip.mp4", 30.0, "160x90", 25);
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
    // at 200 % four seconds of the clip take about two
    says(&mut tty, b"}", "speed 200%");
    tty.send(b" ");
    tty.until("the clock", |screen| {
        screen.row(24).starts_with("> ") && seconds(&screen.row(24)).is_some()
    });
    let from = seconds(&tty.screen().row(24)).unwrap();
    tty.until("the next second", |screen| {
        seconds(&screen.row(24)).is_some_and(|at| at > from)
    });
    let (start, first) = (Instant::now(), seconds(&tty.screen().row(24)).unwrap());
    tty.until("four seconds on", |screen| {
        seconds(&screen.row(24)).is_some_and(|at| at >= first + 4)
    });
    let took = start.elapsed();
    assert!(
        took > Duration::from_millis(1300) && took < Duration::from_millis(3000),
        "4 s of the clip in {took:?}"
    );
    tty.send(b"q");
    assert_eq!(tty.exit(), 0);
}

#[test]
fn volume_keys_step_by_2_up_to_mpv_confs_volume_max() {
    // rows P5, P11, N18: of mpv.conf only audio options are read: volume-max=104 holds, speed=2 does not (it plays at 100 %); `0 *` +2, `9 /` -2 from mpv's 100 (set_volume of bin/ascii-viewer:1508 at tag python-final, the toasts as the old viewer gave them in this harness)
    let sandbox = Sandbox::new();
    std::fs::write(
        sandbox.path("home/.config/mpv/mpv.conf"),
        "ao=null\nvolume-max=104\nspeed=2\n",
    )
    .unwrap();
    let video = clip(&sandbox, "clip.mp4", 20.0, "160x90", 25);
    let mut tty = sandbox.launch().arg(&video).size(SMALL).start();
    tty.await_playing();
    tty.until("00:02", |screen| screen.row(24).starts_with("> 00:02"));
    let start = Instant::now();
    tty.until("00:05", |screen| screen.row(24).starts_with("> 00:05"));
    let took = start.elapsed();
    // at 200 % it would take 1.5 s
    assert!(
        took > Duration::from_millis(2200),
        "3 s of the clip in {took:?}"
    );
    pause(&mut tty);
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
fn mute_and_volume_hold_through_seeks_and_loops() {
    // rows P7, P12: start_audio and launch_mpv of bin/ascii-viewer:1313 at tag python-final hand volume and mute to each mpv started for a seek or a loop; `m` toasts `muted` and `unmuted`
    let sandbox = Sandbox::new();
    let video = counter(&sandbox, "clip.mp4", "160x90", 25, 3.0, 3.0, &[]);
    let mut tty = sandbox.launch().arg(&video).size(SMALL).start();
    tty.await_playing();
    pause(&mut tty);
    says(&mut tty, b"m", "muted");
    says(&mut tty, b"0", "volume 102");
    lands(&mut tty, DOWN, 0, "");
    says(&mut tty, b"L", "loop on");
    tty.send(b" ");
    // round the end and on from the start
    tty.until("the last second", |screen| {
        seconds(&screen.row(24)) == Some(2)
    });
    tty.until("the start again", |screen| {
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
    let row = cut.screen().row(5);
    assert!(
        row.starts_with("| 00:") && row.ends_with(" / 00") && row.chars().count() == 12,
        "{row:?}"
    );
    cut.send(b"q");
    assert_eq!(cut.exit(), 0);
    tty.await_playing();
    let row = tty.screen().row(24);
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
    tty.await_status("> 00:01");
    assert_eq!(tty.screen().row(24), "> 00:01");
    tty.send(b"q");
    assert_eq!(tty.exit(), 0);
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

#[test]
fn r_opens_the_page_on_the_frame_and_closing_it_plays_on() {
    // rows P22, N5, N23: open_page and close_page of bin/ascii-viewer:2048 and :2112 at tag python-final: the page on the frame at full size, named by its time (N5: the frame mpv holds, within 3 frames of the one drawn while playing, its time to 0.1 s); playback pauses meanwhile and goes on after; the page's lines as the old viewer drew them in this harness; on the last frame too (N23: the old viewer could end there, P29)
    let sandbox = Sandbox::new();
    let video = counter(&sandbox, "clip.mp4", "160x90", 25, 8.0, 8.0, &[]);
    let mut tty = sandbox.launch().arg(&video).size(SMALL).start();
    tty.await_playing();
    let at = tty.send(b"r");
    tty.until("the page", |screen| screen.row(1).starts_with(" Render"));
    let out = tty.out();
    let page = find(&out, b" Render", at).unwrap();
    let page = out[..page]
        .windows(8)
        .rposition(|w| w == b"\x1b[?2026h")
        .unwrap();
    let (clock, last) = drawn(&out[..page])
        .into_iter()
        .rfind(|(clock, _)| clock.is_some())
        .unwrap();
    let clock = clock.unwrap();
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
    // the page's export holds the frame it was given: within 3 of the one on screen
    let name = page_name(&mut tty);
    let (grabbed, size) = exported(&mut tty, &sandbox, &name);
    assert_eq!(size, (1920, 1080));
    assert!(
        grabbed.abs_diff(last) <= 3,
        "{name}: frame {grabbed} for frame {last} on screen"
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
    // a window for what must not happen: playing on under the page
    std::thread::sleep(Duration::from_millis(1500));
    tty.send(b"q");
    tty.until("the menu", |screen| screen.row(1).starts_with(" Render"));
    tty.send(b"q");
    tty.until("the player", |screen| {
        screen.row(24).starts_with("> ") && seconds(&screen.row(24)).is_some()
    });
    let after = seconds(&tty.screen().row(24)).unwrap();
    assert!(after <= clock + 1, "{clock} s at `r`, {after} s after");
    tty.until("playing on", |screen| {
        seconds(&screen.row(24)).is_some_and(|at| at > after)
    });
    // paused: the frame on screen, named by its time; it stays paused
    pause(&mut tty);
    let held = frame(tty.screen());
    tty.send(b"r");
    tty.until("the page", |screen| screen.row(1).starts_with(" Render"));
    let name = page_name(&mut tty);
    assert_eq!(name, format!("clip-ascii-{}s.png", tenths(held * 4)));
    tty.send(b"q");
    tty.until("the player", |screen| screen.row(24).starts_with("| "));
    // a window for what must not happen: playing on after the page closed
    std::thread::sleep(Duration::from_millis(600));
    assert!(tty.screen().row(24).starts_with("| "));
    // on the last frame
    lands(&mut tty, UP, 199, "| 00:07 / 00:08");
    tty.send(b"r");
    tty.until("the page", |screen| screen.row(1).starts_with(" Render"));
    tty.send(b"q");
    tty.until("the player", |screen| screen.row(24).starts_with("| 00:07"));
    assert!(tty.running());
    tty.send(b"q");
    assert_eq!(tty.exit(), 0);
}

#[test]
#[ignore = "defect: a frame taken while playing is named by the clock when mpv's answer is read, frames after it"]
fn a_frame_taken_while_playing_is_named_by_its_time() {
    // rows N5, P22: the name's time is that of the frame mpv holds, to 0.1 s (open_page of bin/ascii-viewer:2048 at tag python-final named it by the frame on screen); keys right after `r` keep the viewer busy while mpv answers and plays on (each `c` draws the frame again; 40 leave the colour as it was); seen in a full parallel run without them: `clip-ascii-1.8s.png` holding frame 41 (1.64 s)
    let sandbox = Sandbox::new();
    let video = counter(&sandbox, "clip.mp4", "160x90", 25, 8.0, 8.0, &[]);
    let mut tty = sandbox.launch().arg(&video).size(SMALL).start();
    tty.await_playing();
    tty.send(&[&b"r"[..], &[b'c'; 40]].concat());
    tty.until("the page", |screen| screen.row(1).starts_with(" Render"));
    let name = page_name(&mut tty);
    let (grabbed, _) = exported(&mut tty, &sandbox, &name);
    tty.send(b"q");
    tty.until("the player", |screen| screen.row(24).starts_with("> "));
    tty.send(b"q");
    assert_eq!(tty.exit(), 0);
    assert_eq!(name, format!("clip-ascii-{}s.png", tenths(grabbed * 4)));
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

/// A 2 s counter clip of variable rate, frames 10 to 34 of 50 left out, with 2 s of sound.
fn gap_clip(sandbox: &Sandbox) -> std::path::PathBuf {
    let gap = sandbox.path("gap.mkv");
    let picture = format!("nullsrc=s=160x90:r=25:d=2,{COUNTER},select='lt(n,10)+gte(n,35)'");
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
            "sine=d=2",
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
    // row P24: Decoder.read of bin/ascii-viewer:479 at tag python-final holds the last frame across a gap; run in this harness the old viewer showed frames up to 9, then 35 to 49
    let sandbox = Sandbox::new();
    let gap = gap_clip(&sandbox);
    let mut tty = sandbox.launch().arg(&gap).size(SMALL).start();
    // frames 1 to 9 are due by 0.36 s, 35 at 1.4 s: the frame before the gap stays about a second
    tty.until("the frames before the gap", |screen| {
        (1..10).contains(&frame(screen))
    });
    let held = Instant::now();
    tty.until("the frames after it", |screen| frame(screen) >= 35);
    let took = held.elapsed();
    assert!(took > Duration::from_millis(700), "held for {took:?}");
    assert_eq!(tty.exit(), 0);
    let mut numbers: Vec<u32> = drawn(&tty.out()).into_iter().map(|(_, n)| n).collect();
    numbers.dedup();
    let after = numbers.iter().position(|number| *number >= 35).unwrap();
    assert!(
        after > 0
            && numbers[after - 1] <= 9
            && !numbers.iter().any(|number| (10..35).contains(number)),
        "{numbers:?}"
    );
}

#[test]
#[ignore = "defect: the status clock stops while a variable-rate clip holds a frame"]
fn the_clock_runs_on_through_a_gap_in_the_picture() {
    // rows P16, P24: the status row's clock runs with the sound (clock and position of bin/ascii-viewer:1394 at tag python-final), also while a frame is held; frame 9 is due at 0.36 s and frame 35 at 1.4 s, so 9 shows at 1 s
    let sandbox = Sandbox::new();
    let gap = gap_clip(&sandbox);
    let mut tty = sandbox.launch().arg(&gap).size(SMALL).start();
    assert_eq!(tty.exit(), 0);
    let drawn = drawn(&tty.out());
    assert!(drawn.contains(&(Some(1), 9)), "{drawn:?}");
}

#[test]
fn damaged_frames_are_passed_over() {
    // row P25: Decoder.next_frame of bin/ascii-viewer:468 at tag python-final passes over a frame that does not decode; here 300 bytes zeroed mid-file take frames 32 to 41 (run in this harness the old viewer ended there instead: frames 0 to 30, then exit after 1.5 s)
    let sandbox = Sandbox::new();
    let video = counter(&sandbox, "hurt.mp4", "160x90", 25, 2.0, 0.0, &[]);
    let mut bytes = std::fs::read(&video).unwrap();
    let middle = bytes.len() / 2;
    bytes[middle..middle + 300].fill(0);
    std::fs::write(&video, bytes).unwrap();
    let mut tty = sandbox.launch().arg(&video).size(SMALL).start();
    tty.until("the first status row", |screen| !screen.row(24).is_empty());
    let start = Instant::now();
    assert_eq!(tty.exit(), 0);
    let took = start.elapsed();
    let mut numbers: Vec<u32> = drawn(&tty.out()).into_iter().map(|(_, n)| n).collect();
    numbers.dedup();
    // on past the damage, to the clip's end
    assert!(numbers.last() >= Some(&42), "{numbers:?}");
    assert!(took > Duration::from_millis(1600), "ended after {took:?}");
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
#[ignore = "defect: a one-frame video draws its frame but never ends: mpv holds it as an image"]
fn a_one_frame_video_draws_and_ends() {
    // rows V17, P14: run in this harness the old viewer drew it and ended, exit 0 in 0.45 s
    let sandbox = Sandbox::new();
    let one = sandbox.path("one.mp4");
    ffmpeg(
        &sandbox,
        &[
            "-f",
            "lavfi",
            "-i",
            "testsrc2=s=160x90:r=25:d=1",
            "-frames:v",
            "1",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
            one.to_str().unwrap(),
        ],
    );
    let mut tty = sandbox.launch().arg(&one).size(SMALL).start();
    let status = tty.exit_within(Duration::from_secs(10));
    let drew = replay(&tty.out(), SMALL, Screen::inked)
        .into_iter()
        .max()
        .unwrap_or(0);
    assert!(drew > 1000, "no picture: {drew} cells");
    assert_eq!(status.and_then(|status| status.code()), Some(0));
}

#[test]
fn sound_that_outlasts_the_video_plays_on_over_its_last_frame() {
    // rows N21, P14: a 1 s picture with 3 s of sound plays to the sound's end on its last frame (the old viewer ended with the last frame, after 1.3 s here)
    let sandbox = Sandbox::new();
    let video = counter(&sandbox, "clip.mp4", "160x90", 25, 1.0, 3.0, &[]);
    let mut tty = sandbox.launch().arg(&video).size(SMALL).start();
    tty.until("the first status row", |screen| !screen.row(24).is_empty());
    let start = Instant::now();
    assert_eq!(tty.exit(), 0);
    let took = start.elapsed();
    assert!(took > Duration::from_millis(2500), "ended after {took:?}");
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
    // rows P6, P14: bin/ascii-viewer:1339 at tag python-final: without a sound track, or with sound shorter than the picture, the picture plays at its pace to its end
    let sandbox = Sandbox::new();
    let silent = counter(&sandbox, "silent.mp4", "160x90", 25, 3.0, 0.0, &[]);
    let short = counter(&sandbox, "short.mp4", "160x90", 25, 3.0, 1.0, &[]);
    let mut ttys = [&silent, &short].map(|video| sandbox.launch().arg(video).size(SMALL).start());
    for tty in &mut ttys {
        tty.await_playing();
    }
    let start = Instant::now();
    for tty in &mut ttys {
        assert_eq!(tty.exit(), 0);
        let drawn = drawn(&tty.out());
        // the last second plays on, to the last frames (a debug build among the other tests drops late ones)
        let second = frames_in(&drawn, 2);
        assert!(!second.is_empty(), "{second:?}");
        assert!(drawn.last().is_some_and(|(_, number)| *number >= 50));
    }
    let took = start.elapsed();
    assert!(
        took > Duration::from_millis(1500) && took < Duration::from_secs(8),
        "{took:?} from 00:01 to the ends"
    );
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
