//! The viewer as a process: command line, start-up errors, the terminal it takes and gives back, signals, input batches, toasts, quitting, Hyprland and kitty around it, the help screen (inventory sections 1 and 9).

mod common;

use common::*;
use std::ffi::OsStr;
use std::io::Read;
use std::os::fd::{AsRawFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const KEYS: &str = include_str!("../src/keys.txt");

/// A still of 800 x 400 px, drawn once in a 200 x 60 window, the draw read to its end.
fn shown_still(sandbox: &Sandbox) -> Tty {
    let picture = save(&gradient(800, 400), &sandbox.path("pic.png"));
    let mut tty = sandbox.view(&picture);
    tty.until("the picture", |screen| screen.inked() > 1000);
    drawn_out(&tty);
    tty
}

/// Wait for the end of the last synchronized update begun: a draw seen on screen is read to its end, whatever pauses its writing has under load.
fn drawn_out(tty: &Tty) {
    let out = tty.out();
    if let Some(start) = out.windows(8).rposition(|w| w == b"\x1b[?2026h") {
        tty.expect(start, b"\x1b[?2026l");
    }
}

/// The synchronized update that holds the byte at `at`, from its `?2026h` to the end of its `?2026l`, waited for; the test fails when that byte was written outside every update.
fn update_at(tty: &Tty, at: usize) -> Vec<u8> {
    let end = tty.expect(at, b"\x1b[?2026l");
    let out = tty.out();
    let start = out[..at]
        .windows(8)
        .rposition(|w| w == b"\x1b[?2026h")
        .expect("an update begun");
    assert!(
        find(&out[start..at], b"\x1b[?2026l", 0).is_none(),
        "byte {at} is outside the update begun before it: {}",
        shown(&out[start..end.min(start + 3000)])
    );
    out[start..end].to_vec()
}

/// The places (row, column) where an update writes picture rows, in order: a place, then bold or a colour code; the status row, its erasing, a toast and the keys start otherwise.
fn picture_rows(update: &[u8]) -> Vec<(usize, usize)> {
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
        let body = &tail[digits + 1..];
        if [&b"\x1b[1m"[..], b"\x1b[3", b"\x1b[9"]
            .iter()
            .any(|start| body.starts_with(start))
        {
            rows.push((row.parse().unwrap(), col.parse().unwrap()));
        }
    }
    rows
}

#[test]
fn usage_errors_exit_1_on_stderr() {
    // rows V1, N1: bin/ascii-viewer:2441 at tag python-final asks for one argument and both stdin and stdout terminals, else prints `usage: ascii-viewer FILE (needs a terminal)` (N1: `asciier`)
    let sandbox = Sandbox::new();
    let picture = save(&gradient(80, 40), &sandbox.path("pic.png"));
    for args in [vec![], vec![picture.as_os_str(), picture.as_os_str()]] {
        let ran = sandbox.run(&args);
        assert_eq!(ran.code, Some(1), "{args:?}: {ran:?}");
        assert!(
            ran.stderr
                .starts_with("usage: asciier FILE (needs a terminal)\n"),
            "{ran:?}"
        );
        assert_eq!(ran.stdout, "");
    }
    // one file but no terminal: stdin /dev/null, stdout a pipe
    let ran = sandbox.run(&[picture.as_os_str()]);
    assert_eq!(ran.code, Some(1), "{ran:?}");
    assert_eq!(ran.stderr, "usage: asciier FILE (needs a terminal)\n");
    // one side a terminal: stdin the terminal and stdout a pipe, then stdin /dev/null and stdout the terminal
    let (master, slave, _) = pty(SMALL);
    for stdin_is_the_terminal in [true, false] {
        let mut command = sandbox.command(BIN);
        let terminal = Stdio::from(slave.try_clone().unwrap());
        if stdin_is_the_terminal {
            command.stdin(terminal).stdout(Stdio::piped());
        } else {
            command.stdout(terminal);
        }
        let (code, stdout, said) = ended(command.arg(&picture));
        assert_eq!(code, Some(1), "{said}");
        assert_eq!(said, "usage: asciier FILE (needs a terminal)\n");
        assert!(stdout.is_empty());
    }
    // nothing reached the terminal: what the runs wrote comes out of it before this
    put_slave(&slave, b"end");
    assert_eq!(read_until(&master, b"end"), b"end");
}

/// A run of `command` to its end, its stdout (when piped) and stderr read meanwhile: the exit code, stdout, stderr. A run that has not ended within `PATIENCE` is killed with its process group, and the test fails with the stderr it wrote.
fn ended(command: &mut Command) -> (Option<i32>, Vec<u8>, String) {
    let mut child = command
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .unwrap();
    fn drain(pipe: Option<impl Read + Send + 'static>) -> std::thread::JoinHandle<Vec<u8>> {
        std::thread::spawn(move || {
            let mut got = Vec::new();
            if let Some(mut pipe) = pipe {
                let _ = pipe.read_to_end(&mut got);
            }
            got
        })
    }
    let (stdout, stderr) = (drain(child.stdout.take()), drain(child.stderr.take()));
    let deadline = Instant::now() + PATIENCE;
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break Some(status);
        }
        if Instant::now() > deadline {
            // SAFETY: a signal to the group the child leads; the child is not reaped yet, so the id is still its own
            unsafe { libc::kill(-(child.id() as libc::pid_t), libc::SIGKILL) };
            child.wait().unwrap();
            break None;
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let (stdout, stderr) = (
        stdout.join().unwrap(),
        String::from_utf8_lossy(&stderr.join().unwrap()).into_owned(),
    );
    let Some(status) = status else {
        panic!("still running after {PATIENCE:?}, killed; its stderr: {stderr:?}");
    };
    (status.code(), stdout, stderr)
}

/// Write `bytes` to a terminal's slave side, as a program on it would.
fn put_slave(slave: &OwnedFd, bytes: &[u8]) {
    // SAFETY: a write of that slice to a descriptor this test owns.
    let n = unsafe { libc::write(slave.as_raw_fd(), bytes.as_ptr().cast(), bytes.len()) };
    assert_eq!(n, bytes.len() as isize);
}

/// What a terminal's master side gives until `end` has come, at most `PATIENCE` long.
fn read_until(master: &OwnedFd, end: &[u8]) -> Vec<u8> {
    let (mut got, deadline) = (Vec::new(), Instant::now() + PATIENCE);
    while find(&got, end, 0).is_none() {
        assert!(Instant::now() < deadline, "no {end:?} in {}", shown(&got));
        let mut ready = libc::pollfd {
            fd: master.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        let mut buf = [0u8; 4096];
        // SAFETY: a poll of one descriptor, then a read into a buffer of that size.
        if unsafe { libc::poll(&mut ready, 1, 100) } > 0 {
            let n = unsafe { libc::read(master.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len()) };
            got.extend_from_slice(&buf[..n.max(0) as usize]);
        }
    }
    got
}

#[test]
fn help_and_version_are_words() {
    // rows V1, N1: `--help` (also `-h`, N1 as amended) and `--version` are words of the new program, where the old one opened `--help` as a file (bin/ascii-viewer:2441 at tag python-final)
    let sandbox = Sandbox::new();
    for help in ["--help", "-h"] {
        let ran = sandbox.run(&[OsStr::new(help)]);
        assert_eq!(ran.code, Some(0), "{help}: {ran:?}");
        assert!(
            ran.stdout.starts_with(
                "usage: asciier FILE (needs a terminal)\n       asciier convert [options] INPUT"
            ),
            "{help}: {ran:?}"
        );
    }
    let ran = sandbox.run(&[OsStr::new("--version")]);
    assert_eq!(ran.code, Some(0), "{ran:?}");
    assert_eq!(
        ran.stdout,
        format!("asciier {}\n", env!("CARGO_PKG_VERSION"))
    );
}

/// A run that fails at start: its exit code and output; nothing of the alternate screen may be written.
fn failed_start(sandbox: &Sandbox, file: &std::path::Path) -> String {
    let mut tty = sandbox.view(file);
    assert_eq!(tty.exit(), 1);
    let out = tty.out();
    assert!(
        find(&out, b"\x1b[?1049h", 0).is_none(),
        "the alternate screen was entered: {}",
        shown(&out)
    );
    String::from_utf8_lossy(&out).replace("\r\n", "\n")
}

#[test]
fn start_up_errors_print_on_the_normal_screen() {
    // rows V2, V19, V20, N12: the message on the normal screen, exit 1; the wording of the library error is A
    let sandbox = Sandbox::new();
    let said = failed_start(&sandbox, &sandbox.path("missing.mp4"));
    assert!(said.starts_with("asciier: "), "{said:?}");
    // a container with a subtitle stream alone: bin/ascii-viewer:418 says `no video or audio stream in <path>`
    std::fs::write(
        sandbox.path("words.srt"),
        "1\n00:00:00,000 --> 00:00:01,000\nwords\n",
    )
    .unwrap();
    let words = sandbox.path("words.mkv");
    ffmpeg(
        &sandbox,
        &[
            "-i",
            sandbox.path("words.srt").to_str().unwrap(),
            words.to_str().unwrap(),
        ],
    );
    let said = failed_start(&sandbox, &words);
    assert_eq!(
        said,
        format!("asciier: no video or audio stream in {}\n", words.display())
    );
    // over 2 x Pillow's limit by its header: ascii_image.py:19 at tag python-final
    let huge = png_header(&sandbox.path("huge.png"), 20_000, 10_000);
    let said = failed_start(&sandbox, &huge);
    assert_eq!(
        said,
        "asciier: huge.png is too large to open (over 179 megapixels)\n"
    );
}

#[test]
fn a_missing_theme_is_a_message() {
    // rows V2, N12: the old viewer ends here with a traceback (V24)
    let sandbox = Sandbox::new();
    let picture = save(&gradient(80, 40), &sandbox.path("pic.png"));
    std::fs::remove_file(sandbox.path("home/.config/kitty/current-theme.conf")).unwrap();
    let said = failed_start(&sandbox, &picture);
    assert!(said.starts_with("asciier: "), "{said:?}");
    assert!(!said.contains("Traceback"), "{said:?}");
    assert!(said.contains("current-theme.conf"), "{said:?}");
}

#[test]
fn the_terminal_is_taken_and_given_back() {
    // rows V2, V3, V15: bin/ascii-viewer:2445 and 2470 at tag python-final; N1: the title is `asciier` (whether Hyprland's close bind matches it, V2a, is the deployment's)
    let sandbox = Sandbox::new();
    let mut tty = shown_still(&sandbox);
    let out = tty.out();
    assert!(
        out.starts_with(b"\x1b[22;2t\x1b]2;asciier\x07\x1b[?1049h\x1b[?25l"),
        "{}",
        shown(&out[..out.len().min(80)])
    );
    // the draw is one synchronized update
    let draw = find(&out, b"\x1b[?2026h", 0).unwrap();
    assert!(find(&out, b"\x1b[?2026l", draw).is_some());
    assert_eq!(tty.screen().title, "asciier");
    // cbreak while it runs
    let (canonical, echo, _) = tty.modes();
    assert!(!canonical && !echo);
    tty.send(b"q");
    assert_eq!(tty.exit(), 0);
    assert!(tty.out().ends_with(RESTORE), "{}", shown(&tty.out()));
    let (canonical, echo, signals) = tty.modes();
    assert!(canonical && echo && signals, "the terminal's mode is back");
}

#[test]
fn ctrl_z_and_ctrl_backslash_are_ignored() {
    // N11: the old viewer kept ISIG on (V3a): Ctrl-\ killed it, the terminal left raw
    let sandbox = Sandbox::new();
    let mut tty = shown_still(&sandbox);
    let (_, _, signals) = tty.modes();
    assert!(!signals, "ISIG off");
    tty.send(b"\x1a\x1c");
    std::thread::sleep(Duration::from_millis(300));
    assert!(tty.running());
    tty.send(b"q");
    assert_eq!(tty.exit(), 0);
}

#[test]
fn keys_typed_before_the_start_are_dropped() {
    // row V3: tty.setcbreak's TCSAFLUSH at bin/ascii-viewer:2450 at tag python-final
    let sandbox = Sandbox::new();
    let picture = save(&gradient(800, 400), &sandbox.path("pic.png"));
    let mut tty = sandbox.launch().arg(&picture).typed(b"q").start();
    tty.until("the picture", |screen| screen.inked() > 1000);
    std::thread::sleep(Duration::from_millis(300));
    assert!(tty.running(), "the q typed before was taken");
    tty.send(b"q");
    assert_eq!(tty.exit(), 0);
}

#[test]
fn signals_and_ctrl_c_end_it_cleanly() {
    // row V5: SIGHUP, SIGTERM, Ctrl-C exit 0 with the terminal restored; a second SIGHUP during the cleanup changes nothing
    let sandbox = Sandbox::new();
    let video = clip(&sandbox, "clip.mp4", 20.0, "160x90", 25);
    for (what, signals, keys) in [
        ("SIGTERM", vec![libc::SIGTERM], &b""[..]),
        ("SIGHUP twice", vec![libc::SIGHUP, libc::SIGHUP], b""),
        ("SIGINT", vec![libc::SIGINT], b""),
        ("Ctrl-C", vec![], b"\x03"),
    ] {
        let mut tty = sandbox.launch().arg(&video).size(SMALL).start();
        tty.await_playing();
        for signal in signals {
            tty.signal(signal);
        }
        if !keys.is_empty() {
            tty.send(keys);
        }
        assert_eq!(tty.exit(), 0, "{what}");
        assert!(
            tty.out().ends_with(RESTORE),
            "{what}: {}",
            shown(&tty.out())
        );
    }
}

#[test]
fn the_terminal_gone_ends_it() {
    // rows V5, V13: the window closed under it (its master side gone): exit 0
    let sandbox = Sandbox::new();
    let mut tty = shown_still(&sandbox);
    let status = tty.hang_up().expect("it ended");
    assert_eq!(status.code(), Some(0), "{status:?}");
}

#[test]
fn a_batch_of_keys_draws_once_and_quits_at_once() {
    // row V14: tests/test_viewer_tty.py:85 and :93 at tag python-final pin under 1.0 s from the keys to the exit
    let sandbox = Sandbox::new();
    let batches: [Vec<u8>; 2] = [
        [b"3".repeat(300), b"q".to_vec()].concat(),
        [
            b"+++".to_vec(),
            b"l\x1b[B".repeat(150),
            b"+-".repeat(50),
            b"q".to_vec(),
        ]
        .concat(),
    ];
    for batch in batches {
        let mut tty = shown_still(&sandbox);
        let sent = Instant::now();
        tty.send(&batch);
        let status = tty.exit_within(Duration::from_secs(10));
        let took = sent.elapsed();
        assert_eq!(status.and_then(|s| s.code()), Some(0));
        assert!(took < Duration::from_secs(1), "{took:?}");
    }
}

#[test]
fn a_batch_of_look_keys_is_one_draw() {
    // row V14: the keys of one read act, then the screen is drawn once (flush after the batch, bin/ascii-viewer:2351-2377 at tag python-final): `3333` in one write is one draw, its toast the last key's
    let sandbox = Sandbox::new();
    let mut tty = shown_still(&sandbox);
    let at = tty.send(b"3333");
    let sentinel = toast("level 20% darker");
    let end = tty.expect(at, &sentinel);
    let update = update_at(&tty, end - sentinel.len());
    let out = tty.out();
    assert_eq!(
        find(&out, b"\x1b[7m", at),
        Some(end - sentinel.len()),
        "a toast before the batch's: {}",
        shown(&out[at..end])
    );
    assert_eq!(
        count(&out[at..end], b"\x1b[?2026h"),
        1,
        "more than one draw"
    );
    assert!(out[at..].starts_with(&update));
    tty.send(b"q");
    assert_eq!(tty.exit(), 0);
}

#[test]
fn seek_keys_in_one_batch_land_at_their_sum() {
    // row V14: where its summed seek lands: consecutive seek keys of a batch add up, held to the clip once (bin/ascii-viewer:2357-2374 at tag python-final): from 0 on a 130.5 s clip `kkkj` lands at 02:00, where a clamp a key would give 01:10; mpv adds up the relative seeks it holds queued too (queue_seek, player/playloop.c of mpv 0.41), so a viewer seeking once a key lands there as well while mpv runs none of them before the last comes: this pins the landing, not which side adds it up
    let sandbox = Sandbox::new();
    // 10 frames a second: a frame's number is its time in tenths (mod 256)
    let video = counter(&sandbox, "long.mp4", "160x90", 10, 130.5, 0.0, &[]);
    let mut tty = sandbox.launch().arg(&video).size(SMALL).start();
    tty.await_playing();
    tty.send(b" ");
    tty.await_status("| ");
    // the 16:9 counter covers all 80 columns of rows 2 to 22
    for (keys, frame, status) in [
        (&b"j"[..], 0, "| 00:00 / 02:10"),
        (b"kkkj", 1200 % 256, "| 02:00 / 02:10"),
    ] {
        tty.send(keys);
        tty.until(&format!("frame {frame} and {status:?}"), |screen| {
            counted(screen, 12, 1, 80) == frame && screen.row(24).starts_with(status)
        });
    }
    tty.send(b"q");
    assert_eq!(tty.exit(), 0);
}

#[test]
fn a_toast_shows_on_the_bottom_row_for_a_while() {
    // row V16: reverse video on the bottom row, cut to cols - 2, then the row drawn without it (bin/ascii-viewer:2128-2133 at tag python-final); its 1.2 s (TOAST, bin/ascii-viewer:118) is pinned by src/view.rs a_toast_lasts_1_2_s_from_say, as on screen the bounds stay loose: under load the toast shows late, and as briefly
    let sandbox = Sandbox::new();
    let mut tty = shown_still(&sandbox);
    let at = tty.send(b"3");
    let sent = Instant::now();
    tty.expect(at, b"\x1b[60;1H\x1b[7m level 5% darker \x1b[0m");
    let seen = Instant::now();
    tty.until("the bottom row without the toast", |screen| {
        screen.row(60).is_empty()
    });
    // its time runs from the key: the row blank at least 1.1 s after it; seen, gone within 2.5 s: 1.2 s and what the end's draw is late by
    let (lasted, visible) = (sent.elapsed(), seen.elapsed());
    assert!(lasted >= Duration::from_millis(1100), "{lasted:?}");
    assert!(visible < Duration::from_millis(2500), "{visible:?}");
    tty.send(b"q");
    assert_eq!(tty.exit(), 0);
    // a narrow window: 12 columns, the text cut to 10
    let picture = sandbox.path("pic.png");
    let narrow = Size {
        rows: 5,
        cols: 12,
        xpx: 108,
        ypx: 100,
    };
    let mut tty = sandbox.launch().arg(&picture).size(narrow).start();
    tty.until("the picture", |screen| screen.inked() > 0);
    let at = tty.send(b"3");
    tty.expect(at, b"\x1b[5;1H\x1b[7m level 5% d \x1b[0m");
    tty.send(b"q");
    assert_eq!(tty.exit(), 0);
}

#[test]
fn columns_and_lines_leave_the_size_to_the_terminal() {
    // rows V9, N8: the old viewer took COLUMNS and LINES over the terminal's size (shutil.get_terminal_size, bin/ascii-viewer:1714 at tag python-final) and would draw this in 40 x 10; N8: the terminal's 200 x 60 holds
    let mut sandbox = Sandbox::new();
    sandbox.set("COLUMNS", "40");
    sandbox.set("LINES", "10");
    // more than 40 x 10 cells inked
    let mut tty = shown_still(&sandbox);
    let at = tty.send(b"3");
    tty.expect(at, b"\x1b[60;1H\x1b[7m level 5% darker \x1b[0m");
    tty.send(b"q");
    assert_eq!(tty.exit(), 0);
}

#[test]
fn q_and_capital_q_quit() {
    // row V21
    let sandbox = Sandbox::new();
    let video = clip(&sandbox, "clip.mp4", 20.0, "160x90", 25);
    let mut tty = sandbox.launch().arg(&video).size(SMALL).start();
    tty.await_playing();
    tty.send(b"q");
    assert_eq!(tty.exit(), 0);
    let mut tty = shown_still(&sandbox);
    tty.send(b"Q");
    assert_eq!(tty.exit(), 0);
}

#[test]
fn f_fullscreens_the_window_of_its_terminal() {
    // row V22: the dispatch of bin/ascii-viewer:1618 at tag python-final; this test process is the viewer's parent, so its pid is the nearest window owner
    let sandbox = Sandbox::new();
    let clients = format!(
        r#"[{{"pid": 1, "address": "0x1"}}, {{"pid": {}, "address": "0x5ca1ab1e"}}]"#,
        std::process::id()
    );
    sandbox.stub_data("hyprctl.clients", &clients);
    let mut tty = shown_still(&sandbox);
    tty.send(b"f");
    let calls = sandbox.await_noted("hyprctl.calls", "dispatch");
    assert_eq!(
        calls,
        "clients\n-j\n--\ndispatch\nhl.dsp.window.fullscreen({ mode = \"fullscreen\", window = \"address:0x5ca1ab1e\" })\n--\n"
    );
    // no window of an ancestor (pid 1 is one: the walk ends there): silent, nothing dispatched; `3` after it shows when `f` is done
    sandbox.stub_data(
        "hyprctl.clients",
        r#"[{"pid": 4194305, "address": "0xfar"}]"#,
    );
    std::fs::remove_file(sandbox.path("bin/hyprctl.calls")).unwrap();
    let at = tty.send(b"f3");
    let sentinel = toast("level 5% darker");
    let end = tty.expect(at, &sentinel);
    assert_eq!(sandbox.noted("hyprctl.calls"), "clients\n-j\n--\n");
    assert_eq!(
        find(&tty.out(), b"\x1b[7m", at),
        Some(end - sentinel.len()),
        "no toast of its own"
    );
    tty.send(b"q");
    assert_eq!(tty.exit(), 0);
}

#[test]
fn kitty_ligatures_are_off_while_it_runs() {
    // row V4: `kitten @ --to $KITTY_LISTEN_ON disable-ligatures --match id:$KITTY_WINDOW_ID always`, `never` at the end (bin/ascii-viewer:2436 at tag python-final)
    let mut sandbox = Sandbox::new();
    let socket = format!("unix:{}", sandbox.path("kitty").display());
    sandbox.set("KITTY_LISTEN_ON", &socket);
    sandbox.set("KITTY_WINDOW_ID", "7");
    let video = clip(&sandbox, "clip.mp4", 20.0, "160x90", 25);
    let mut tty = sandbox.launch().arg(&video).size(SMALL).start();
    tty.await_playing();
    let call =
        |mode: &str| format!("@\n--to\n{socket}\ndisable-ligatures\n--match\nid:7\n{mode}\n--\n");
    assert_eq!(sandbox.noted("kitten.calls"), call("always"));
    tty.send(b"q");
    assert_eq!(tty.exit(), 0);
    assert_eq!(
        sandbox.noted("kitten.calls"),
        call("always") + &call("never")
    );
}

#[test]
fn keys_without_a_use_on_a_video_do_nothing() {
    // rows V26, V15: `d D u y v + = - H J K`, Esc, Enter (tests/test_viewer.py:1827 at tag python-final); the toast goes out in a frame's synchronized update, with the picture and the erased bottom row (bin/ascii-viewer:2188 and :2209)
    let sandbox = Sandbox::new();
    let video = clip(&sandbox, "clip.mp4", 20.0, "160x90", 25);
    let mut tty = sandbox.launch().arg(&video).size(SMALL).start();
    tty.await_playing();
    // `m` after them shows when they are done: its toast is the first
    let at = tty.send(b"dDuyv+=-HJK\x1b\rm");
    let sentinel = toast("muted");
    let end = tty.expect(at, &sentinel);
    assert_eq!(
        find(&tty.out(), b"\x1b[7m", at),
        Some(end - sentinel.len()),
        "no toast of their own"
    );
    let update = update_at(&tty, end - sentinel.len());
    assert!(
        update.starts_with(b"\x1b[?2026h\x1b[0m"),
        "{}",
        shown(&update)
    );
    let (erased, toasted) = (
        find(&update, b"\x1b[24;1H\x1b[2K", 0),
        find(&update, b"\x1b[24;1H\x1b[7m muted ", 0),
    );
    assert!(erased.is_some_and(|erased| toasted.is_some_and(|toasted| erased < toasted)));
    let rows = picture_rows(&update);
    assert!(
        rows.iter().any(|&(row, _)| row < 24),
        "no picture row: {}",
        shown(&update)
    );
    tty.until("still playing", |screen| {
        screen.row(24).starts_with("> 00:")
    });
    let mut files: Vec<_> = std::fs::read_dir(&sandbox.dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .filter(|name| name.starts_with("clip"))
        .collect();
    files.sort();
    assert_eq!(files, ["clip.mp4"]);
    assert!(
        std::fs::read_dir(sandbox.path("data"))
            .unwrap()
            .next()
            .is_none(),
        "no trash"
    );
    tty.send(b"q");
    assert_eq!(tty.exit(), 0);
}

#[test]
fn a_resize_redraws_at_the_new_size() {
    // row V10: tests/test_viewer_tty.py:143 at tag python-final
    let mut sandbox = Sandbox::new();
    sandbox.set("KITTY_WINDOW_ID", "1");
    let picture = save(&gradient(800, 400), &sandbox.path("pic.png"));
    let mut tty = sandbox
        .launch()
        .arg(&picture)
        .kitty(KITTY_OK, Duration::ZERO)
        .start();
    tty.expect(0, b"a=T,f=24,t=t,s=800,v=400");
    let before = tty.len();
    // 50 x 20 cells, still 9 x 20 px
    tty.resize(Size {
        rows: 20,
        cols: 50,
        xpx: 450,
        ypx: 400,
    });
    let placed = tty.expect(before, b"a=T,f=24,t=t,s=450,v=225");
    assert!(find(&tty.out(), b"\x1b[2J", before).is_some_and(|clear| clear < placed));
    let at = tty.send(b"3");
    tty.expect(at, b"\x1b[20;1H\x1b[7m level 5% darker");
    tty.send(b"q");
    assert_eq!(tty.exit(), 0);
}

#[test]
fn a_resize_draws_the_paused_frame_and_the_keys_again_from_a_cleared_screen() {
    // row V10: SIGWINCH draws at once from a cleared screen (bin/ascii-viewer:2343-2346 at tag python-final, draw(clear=True)): a video's frame at the new size with its status on the new bottom row; the keys with their hint
    let sandbox = Sandbox::new();
    let video = counter(&sandbox, "clip.mp4", "160x90", 25, 10.0, 10.0, &[]);
    let mut tty = sandbox.launch().arg(&video).size(SMALL).start();
    tty.await_playing();
    tty.send(b" ");
    tty.await_status("| ");
    // a frame no later one replaces: a seek's while paused, to 0, then 5 s on (frames decoded before the pause can land after a quiet while under load); the 16:9 counter covers all 80 columns of rows 2 to 22
    for (keys, frame, status) in [(&b"j"[..], 0, "| 00:00"), (b"l", 125, "| 00:05")] {
        tty.send(keys);
        tty.until(&format!("frame {frame} and {status:?}"), |screen| {
            counted(screen, 12, 1, 80) == frame && screen.row(24).starts_with(status)
        });
    }
    let before = tty.len();
    let taller = Size {
        rows: 30,
        cols: 100,
        xpx: 900,
        ypx: 600,
    };
    tty.resize(taller);
    let cleared = tty.expect(before, b"\x1b[2J");
    let update = update_at(&tty, cleared);
    assert!(
        update.starts_with(b"\x1b[?2026h\x1b[0m\x1b[2J\x1b[30;1H\x1b[2K"),
        "{}",
        shown(&update[..update.len().min(60)])
    );
    let rows = picture_rows(&update);
    assert!(
        !rows.is_empty() && rows.iter().all(|&(row, _)| (1..=29).contains(&row)),
        "{rows:?}"
    );
    assert!(find(&update, b"\x1b[30;1H\x1b[0m| 00:05", 0).is_some());
    // the same frame, across all 100 columns
    assert_eq!(counted(tty.screen(), 15, 1, 100), 125);
    assert!(tty.screen().row(30).starts_with("| 00:05"));
    // the keys, then a lower window: drawn again from a cleared screen, the hint on its bottom row
    let at = tty.send(F1);
    tty.expect(
        at,
        b"\x1b[30;1H\x1b[2m j k scroll   any other key closes\x1b[0m\x1b[?2026l",
    );
    let before = tty.len();
    tty.resize(Size {
        rows: 20,
        cols: 100,
        xpx: 900,
        ypx: 400,
    });
    let hint: &[u8] = b"\x1b[20;1H\x1b[2m j k scroll   any other key closes\x1b[0m\x1b[?2026l";
    let end = tty.expect(before, hint);
    let update = update_at(&tty, end - hint.len());
    assert!(
        update.starts_with(b"\x1b[?2026h\x1b[0m\x1b_Ga=d,d=A,q=2\x1b\\\x1b[2J\x1b[1;1H Keys"),
        "{}",
        shown(&update[..update.len().min(60)])
    );
    assert_eq!(tty.screen().row(20), " j k scroll   any other key closes");
    tty.send(b"q");
    tty.send(b"q");
    assert_eq!(tty.exit(), 0);
}

#[test]
fn a_resize_draws_an_ascii_still_again_at_the_new_size() {
    // row V10: tests/test_viewer_tty.py:143 at tag python-final in ASCII: CSI 2J, then the picture's rows at the new size (draw(clear=True), bin/ascii-viewer:2346)
    let sandbox = Sandbox::new();
    let mut tty = shown_still(&sandbox);
    let before = tty.len();
    tty.resize(Size {
        rows: 30,
        cols: 100,
        xpx: 900,
        ypx: 600,
    });
    let cleared = tty.expect(before, b"\x1b[2J");
    let update = update_at(&tty, cleared);
    assert!(
        update.starts_with(b"\x1b[?2026h\x1b[0m\x1b_Ga=d,d=A,q=2\x1b\\\x1b[2J"),
        "{}",
        shown(&update[..update.len().min(60)])
    );
    // 800 x 400 px in 100 x 30 cells of 9 x 20 px: all 100 columns of rows 4 to 26
    let rows: Vec<(usize, usize)> = (4..=26).map(|row| (row, 1)).collect();
    assert_eq!(picture_rows(&update), rows);
    tty.send(b"q");
    assert_eq!(tty.exit(), 0);
}

#[test]
fn a_late_kitty_reply_is_no_keys() {
    // rows V11, V12: tests/test_viewer_tty.py:168 at tag python-final: the answer after the probe gave up holds d, s, r, f and l
    let mut sandbox = Sandbox::new();
    sandbox.set("KITTY_WINDOW_ID", "1");
    let folder = sandbox.folder("pics");
    for i in 0..3 {
        save(&gradient(80, 40), &folder.join(format!("{i}.png")));
    }
    let ebadf: &[u8] = b"\x1b_Gi=31;EBADF:Failed to open file for graphics transmission: /dev/shm/tty-graphics-protocol-x with error: [2] No such file or directory\x1b\\\x1b[?62;22c";
    let mut tty = sandbox
        .launch()
        .arg(folder.join("0.png"))
        .kitty(ebadf, Duration::from_millis(1600))
        .start();
    tty.expect(0, b"\x1b_Gi=31,");
    std::thread::sleep(Duration::from_millis(2500));
    tty.send(b"q");
    assert_eq!(tty.exit(), 0);
    let mut names: Vec<_> = std::fs::read_dir(&folder)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    assert_eq!(names, ["0.png", "1.png", "2.png"]);
    assert!(
        std::fs::read_dir(sandbox.path("data"))
            .unwrap()
            .next()
            .is_none()
    );
    assert_eq!(sandbox.noted("hyprctl.calls"), "");
    assert!(find(&tty.out(), b"\x1b[7m", 0).is_none(), "no toast");
}

/// The player's part of the keys, as the help shows them in a window `cols` wide: table rows whole, prose wrapped.
fn player_keys() -> Vec<&'static str> {
    let page = KEYS.find("\nRender page (r):").unwrap();
    KEYS[..page].trim_end().lines().collect()
}

#[test]
fn f1_shows_the_keys_and_any_key_closes_them() {
    // rows V23, H1, H2, H4, K5, V21: F1 as kitty, xterm and rxvt send it (bin/ascii-viewer:146 at tag python-final); kitty's images deleted, then each line of src/keys.txt at its row after a space, the hint in SGR 2 on the bottom row (bin/ascii-viewer:2100); q and Q close the keys and do not quit
    let sandbox = Sandbox::new();
    let mut tty = shown_still(&sandbox);
    let lines = player_keys();
    assert!(lines.len() < 59, "the player's keys fit 60 rows");
    for (f1, close) in [
        (&b"\x1bOP"[..], &b"q"[..]),
        (b"\x1b[11~", b"q"),
        (b"\x1b[P", b"Q"),
    ] {
        let at = tty.send(f1);
        let end = tty.expect(
            at,
            b"\x1b[60;1H\x1b[2m any other key closes\x1b[0m\x1b[?2026l",
        );
        let out = tty.out();
        let screen = &out[at..end];
        assert!(
            screen.starts_with(b"\x1b[?2026h\x1b[0m\x1b_Ga=d,d=A,q=2\x1b\\\x1b[2J"),
            "{}",
            shown(&screen[..screen.len().min(60)])
        );
        for (i, line) in lines.iter().enumerate() {
            let written = format!("\x1b[{};1H {line}", i + 1);
            assert!(
                find(screen, written.as_bytes(), 0).is_some(),
                "{written:?} not in {}",
                shown(screen)
            );
        }
        // q or Q closes the keys and does not quit; the picture comes back from a cleared screen
        let at = tty.send(close);
        tty.expect(at, b"\x1b[2J");
        tty.until("the picture again", |screen| {
            screen.inked() > 1000 && !screen.row(1).starts_with(" Keys")
        });
        drawn_out(&tty);
        assert!(tty.running());
    }
    // a key that closes the keys has no other effect: 3 there is no level, so 4 after it brightens from 0
    let at = tty.send(F1);
    tty.expect(at, b"any other key closes");
    let at = tty.send(b"3");
    tty.expect(at, b"\x1b[2J");
    let at = tty.send(b"4");
    tty.expect(at, toast("level 5% brighter"));
    tty.send(b"q");
    assert_eq!(tty.exit(), 0);
}

#[test]
fn the_keys_scroll_when_they_do_not_fit() {
    // rows H1, H2: in 20 rows the 34 lines scroll a line by j, Down, k and Up, clamped, and the hint says so; where they fit, j and Up close them as any other key does (bin/ascii-viewer:2080-2098 at tag python-final)
    let sandbox = Sandbox::new();
    let picture = save(&gradient(800, 400), &sandbox.path("pic.png"));
    let size = Size {
        rows: 20,
        cols: 100,
        xpx: 900,
        ypx: 400,
    };
    let mut tty = sandbox.launch().arg(&picture).size(size).start();
    tty.until("the picture", |screen| screen.inked() > 100);
    let lines = player_keys();
    let cut = |line: &str| line.chars().take(99).collect::<String>();
    tty.send(F1);
    tty.until("the keys", |screen| {
        screen.row(20) == " j k scroll   any other key closes"
    });
    assert_eq!(
        tty.screen().row(1),
        format!(" {}", cut(lines[0])).trim_end()
    );
    tty.send(b"j");
    tty.until("one line down", |screen| {
        screen.row(1) == format!(" {}", cut(lines[1])).trim_end()
    });
    tty.send(DOWN);
    tty.until("two lines down", |screen| {
        screen.row(1) == format!(" {}", cut(lines[2])).trim_end()
    });
    tty.send(UP);
    tty.until("one line up", |screen| {
        screen.row(1) == format!(" {}", cut(lines[1])).trim_end()
    });
    // past the end: the last line on row 19
    tty.send(&b"j".repeat(40));
    tty.until("the last line", |screen| {
        screen.row(19) == format!(" {}", cut(lines[lines.len() - 1])).trim_end()
    });
    tty.send(&b"k".repeat(40));
    tty.until("the first line", |screen| {
        screen.row(1) == format!(" {}", cut(lines[0])).trim_end()
    });
    tty.send(b"x");
    tty.until("the picture", |screen| {
        !screen.row(20).contains("any other key") && screen.inked() > 100
    });
    tty.send(b"q");
    assert_eq!(tty.exit(), 0);
    // 200 x 60: they fit, and a scroll key closes them; the picture comes back from a cleared screen
    let mut tty = shown_still(&sandbox);
    for close in [&b"j"[..], UP] {
        let at = tty.send(F1);
        tty.expect(
            at,
            b"\x1b[60;1H\x1b[2m any other key closes\x1b[0m\x1b[?2026l",
        );
        let at = tty.send(close);
        tty.expect(at, b"\x1b[2J");
        tty.until("the picture again", |screen| {
            screen.inked() > 1000 && !screen.row(1).starts_with(" Keys")
        });
        drawn_out(&tty);
    }
    tty.send(b"q");
    assert_eq!(tty.exit(), 0);
}

#[test]
fn the_keys_are_cut_to_the_window() {
    // row H1: a line goes out after a space, cut to cols - 1, and so does the hint (bin/ascii-viewer:2107 and :2109 at tag python-final, `line[: cols - 1]`, `hint[: cols - 1]`); N27 wraps the prose to the window, which leaves the cut to the rows of the table and the hint
    let sandbox = Sandbox::new();
    let picture = save(&gradient(800, 400), &sandbox.path("pic.png"));
    let row = KEYS
        .lines()
        .find(|line| line.starts_with("h l Left Right"))
        .unwrap();
    assert_eq!(row.chars().count(), 95);
    let cut = |text: &str, cols: u16| text.chars().take(cols as usize - 1).collect::<String>();
    // in 30 columns 29 of the hint's 33 characters
    let hint = cut("j k scroll   any other key closes", 30);
    assert_eq!(hint, "j k scroll   any other key cl");
    for (size, hint) in [
        // the keys fit 40 rows: the short hint, whole
        (
            Size {
                rows: 40,
                cols: 60,
                xpx: 540,
                ypx: 800,
            },
            "any other key closes",
        ),
        (
            Size {
                rows: 20,
                cols: 30,
                xpx: 270,
                ypx: 400,
            },
            hint.as_str(),
        ),
    ] {
        let mut tty = sandbox.launch().arg(&picture).size(size).start();
        tty.until("the picture", |screen| screen.inked() > 0);
        drawn_out(&tty);
        let at = tty.send(F1);
        let written = format!("\x1b[{};1H\x1b[2m {hint}\x1b[0m\x1b[?2026l", size.rows);
        let end = tty.expect(at, written.as_bytes());
        let keys = &tty.out()[at..end];
        // the row's first cols - 1 characters, the next row's place right after them
        let line = format!(" {}\x1b[", cut(row, size.cols));
        assert!(
            find(keys, line.as_bytes(), 0).is_some(),
            "{line:?} not in {}",
            shown(keys)
        );
        tty.send(b"qq");
        assert_eq!(tty.exit(), 0);
    }
}

/// The keys under F1 at a moment: the times they are drawn within a second once the drawing has rested 400 ms (frames decoded before the pause still land), or after 5 s without rest; playing media under them draws them with each frame's clock.
fn drawn_keys(tty: &mut Tty) -> usize {
    let at = tty.send(F1);
    tty.expect(at, b"any other key closes");
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
fn help_pauses_playing_media_until_it_closes() {
    // row H3: playing media pauses under the keys and plays on after; paused media stays paused
    let sandbox = Sandbox::new();
    let video = clip(&sandbox, "clip.mp4", 30.0, "160x90", 25);
    let mut tty = sandbox.launch().arg(&video).size(SMALL).start();
    tty.await_playing();
    let draws = drawn_keys(&mut tty);
    assert_eq!(
        draws, 0,
        "the keys drawn {draws} times: it plays under them"
    );
    tty.send(b"x");
    tty.until("playing again", |screen| {
        !screen.row(1).starts_with(" Keys") && screen.row(24).starts_with("> 00:")
    });
    // paused, it stays paused
    tty.send(b" ");
    tty.await_status("| 00:");
    let draws = drawn_keys(&mut tty);
    assert_eq!(
        draws, 0,
        "the keys drawn {draws} times: it plays under them"
    );
    tty.send(b"x");
    tty.until("the frame again", |screen| {
        !screen.row(1).starts_with(" Keys") && screen.row(24).starts_with("| 00:")
    });
    std::thread::sleep(Duration::from_millis(800));
    assert!(
        tty.screen().row(24).starts_with("| 00:"),
        "{}",
        tty.screen().row(24)
    );
    tty.send(b"q");
    assert_eq!(tty.exit(), 0);
}

#[test]
fn help_right_after_the_start_pauses_too() {
    // row H3: at its first draw the video plays (the status row says `>`); the keys asked for then must pause it as later
    let sandbox = Sandbox::new();
    let video = clip(&sandbox, "clip.mp4", 30.0, "160x90", 25);
    let mut tty = sandbox.launch().arg(&video).size(SMALL).start();
    tty.until("the first status row", |screen| !screen.row(24).is_empty());
    let draws = drawn_keys(&mut tty);
    assert_eq!(
        draws, 0,
        "the keys drawn {draws} times: it plays under them"
    );
    tty.send(b"q");
    tty.send(b"q");
    assert_eq!(tty.exit(), 0);
}

#[test]
fn closing_the_keys_sends_the_image_again() {
    // rows H5, K5: in image mode the keys' draw deletes kitty's images first, and closing them places the image again after a delete (bin/ascii-viewer:2089-2098 and :2105 at tag python-final)
    let mut sandbox = Sandbox::new();
    sandbox.set("KITTY_WINDOW_ID", "1");
    let picture = save(&gradient(800, 400), &sandbox.path("pic.png"));
    let mut tty = sandbox
        .launch()
        .arg(&picture)
        .kitty(KITTY_OK, Duration::ZERO)
        .start();
    let placement: &[u8] = b"a=T,f=24,t=t,s=800,v=400";
    tty.expect(0, placement);
    let at = tty.send(F1);
    let end = tty.expect(at, b"any other key closes");
    let keys = &tty.out()[at..end];
    assert!(
        keys.starts_with(b"\x1b[?2026h\x1b[0m\x1b_Ga=d,d=A,q=2\x1b\\\x1b[2J"),
        "{}",
        shown(&keys[..keys.len().min(60)])
    );
    let at = tty.send(b"x");
    let placed = tty.expect(at, placement) - placement.len();
    assert!(
        find(&tty.out(), KITTY_CLEAR, at).is_some_and(|delete| delete < placed),
        "no delete before the placement"
    );
    tty.send(b"q");
    assert_eq!(tty.exit(), 0);
}
