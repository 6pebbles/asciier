//! Trash, undo and delete on a still: `d` to the home trash as the freedesktop spec has it, the next image, the empty folder, `u` and what stops it, the `D` question; which draws start from a cleared screen (inventory section 8, the trash side of V16a).

mod common;

use common::screen::Screen;
use common::*;
use std::fs;
use std::ops::Range;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The sandbox's folder `pics` with a grey ramp of each size, `(name, w, h)`: in image mode kitty is told the size of the image it is sent, so the size tells which is on screen.
fn pics(sandbox: &Sandbox, stills: &[(&str, u32, u32)]) -> PathBuf {
    let folder = sandbox.folder("pics");
    for &(name, w, h) in stills {
        save(&gradient(w, h), &folder.join(name));
    }
    folder
}

/// A sandbox whose runs say kitty: the viewer asks the terminal about its graphics and shows stills as images.
fn kitty_sandbox() -> Sandbox {
    let mut sandbox = Sandbox::new();
    sandbox.set("KITTY_WINDOW_ID", "1");
    sandbox
}

/// The viewer on `file` in a window of `size`, its first draw written whole; asked about kitty graphics, the terminal says yes.
fn open(sandbox: &Sandbox, file: &Path, size: Size) -> Tty {
    let tty = sandbox
        .launch()
        .arg(file)
        .size(size)
        .kitty(KITTY_OK, Duration::ZERO)
        .start();
    tty.expect(0, b"\x1b[?2026l");
    tty
}

/// Where the first draw after byte `from` whose status row writes `status` lies in the output, once it is whole.
fn draw_with(tty: &Tty, from: usize, status: &[u8]) -> Range<usize> {
    let end = tty.expect(from, [status, b"\x1b[?2026l"].concat());
    let out = tty.out();
    let start = out[..end]
        .windows(8)
        .rposition(|w| w == b"\x1b[?2026h")
        .expect("a draw");
    start..end
}

/// Send `keys`; the bytes of the draw with the toast `text` that follows.
fn says(tty: &mut Tty, keys: &[u8], text: &str) -> Vec<u8> {
    let at = tty.send(keys);
    let draw = draw_with(tty, at, &toast(text));
    tty.out()[draw].to_vec()
}

/// The `D` question about `name` as the status row writes it.
fn question(name: &str) -> Vec<u8> {
    toast(&format!("delete {name} permanently? y/n"))
}

/// Whether a draw starts from a cleared screen.
fn cleared(draw: &[u8]) -> bool {
    find(draw, b"\x1b[2J", 0).is_some()
}

/// The size of the last image `out` sends kitty; `None` when it sends none.
fn placed(out: &[u8]) -> Option<(u32, u32)> {
    let (keys, _) = placements(out).pop()?;
    let value = |key: &str| {
        keys.split(',')
            .find_map(|pair| pair.strip_prefix(key))?
            .parse()
            .ok()
    };
    Some((value("s=")?, value("v=")?))
}

/// The texts `out` writes in reverse video (toasts, the `D` question), in order.
fn reversed(out: &[u8]) -> Vec<String> {
    let (mut found, mut at) = (Vec::new(), 0);
    while let Some(start) = find(out, b"\x1b[7m ", at) {
        let Some(end) = find(out, b" \x1b[0m", start + 5) else {
            break;
        };
        found.push(String::from_utf8_lossy(&out[start + 5..end]).into_owned());
        at = end;
    }
    found
}

/// The names in `dir`, sorted; none when it is not there.
fn names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

fn ino(path: &Path) -> u64 {
    fs::symlink_metadata(path).unwrap().ino()
}

fn mode(path: &Path) -> u32 {
    fs::symlink_metadata(path).unwrap().mode() & 0o7777
}

/// The mode a folder gets when made as any folder: 0777 less this process's umask, which the viewer inherits.
fn default_mode() -> u32 {
    let status = fs::read_to_string("/proc/self/status").unwrap();
    let umask = status
        .lines()
        .find_map(|line| line.strip_prefix("Umask:"))
        .unwrap();
    0o777 & !u32::from_str_radix(umask.trim(), 8).unwrap()
}

/// A path as a trash record writes it: its bytes percent-encoded, `/ A-Z a-z 0-9 _ . - ~` kept (row T3).
fn quote(path: &Path) -> String {
    path.as_os_str()
        .as_bytes()
        .iter()
        .map(|&byte| {
            if byte.is_ascii_alphanumeric() || b"/_.-~".contains(&byte) {
                (byte as char).to_string()
            } else {
                format!("%{byte:02X}")
            }
        })
        .collect()
}

/// The local time as a DeletionDate, by date(1) in the run environment, the viewer's time zone.
fn now(sandbox: &Sandbox) -> String {
    let said = sandbox
        .command("date")
        .arg("+%Y-%m-%dT%H:%M:%S")
        .output()
        .unwrap();
    String::from_utf8(said.stdout).unwrap().trim().to_string()
}

/// A folder that lets nothing in or out while this lives: its permissions come back when it goes, before the sandbox is removed.
struct Locked(PathBuf);

impl Locked {
    fn new(dir: &Path) -> Locked {
        // SAFETY: getuid reads the process's user id and cannot fail.
        assert_ne!(unsafe { libc::getuid() }, 0, "permissions do not stop root");
        fs::set_permissions(dir, fs::Permissions::from_mode(0o555)).unwrap();
        Locked(dir.to_path_buf())
    }
}

impl Drop for Locked {
    fn drop(&mut self) {
        let _ = fs::set_permissions(&self.0, fs::Permissions::from_mode(0o755));
    }
}

#[test]
fn d_twice_empties_the_folder_and_u_brings_the_last_back() {
    // rows T1, T3, T5, T6, T7, V16a, K5: tests/test_viewer_tty.py:230 at tag python-final in image mode: after each `d` the next image sent at its size, then the empty folder with nothing sent; `j k l + - Up` do nothing there, `u` puts the last back and sends it
    let sandbox = kitty_sandbox();
    let folder = pics(&sandbox, &[("a.png", 80, 40), ("b.png", 40, 80)]);
    let mut tty = open(&sandbox, &folder.join("a.png"), WIDE);
    assert_eq!(placed(&tty.out()), Some((80, 40)));
    let draw = says(&mut tty, b"d", "trashed a.png");
    assert_eq!(placed(&draw), Some((40, 80)));
    assert!(!folder.join("a.png").exists());
    assert!(sandbox.path("data/Trash/files/a.png").is_file());
    let draw = says(&mut tty, b"d", "trashed b.png");
    assert!(
        find(&draw, b"no images in pics", 0).is_some()
            && find(&draw, b"u undo  q quit", 0).is_some(),
        "{}",
        shown(&draw)
    );
    assert!(draw.starts_with(b"\x1b[?2026h\x1b[0m\x1b_Ga=d,d=A,q=2\x1b\\\x1b[2J"));
    assert_eq!(placed(&draw), None);
    assert!(names(&folder).is_empty());
    let at = tty.send(b"jkl+-\x1b[A");
    tty.send(b"u");
    let draw = draw_with(&tty, at, &toast("restored b.png"));
    let out = tty.out();
    assert_eq!(reversed(&out[at..draw.end]), ["restored b.png"]);
    assert_eq!(placements(&out[at..draw.end]).len(), 1);
    assert_eq!(placed(&out[draw]), Some((40, 80)));
    tty.send(b"q");
    assert_eq!(tty.exit(), 0);
    assert_eq!(names(&folder), ["b.png"]);
    assert_eq!(names(&sandbox.path("data/Trash/files")), ["a.png"]);
    assert_eq!(names(&sandbox.path("data/Trash/info")), ["a.png.trashinfo"]);
    let record = fs::read_to_string(sandbox.path("data/Trash/info/a.png.trashinfo")).unwrap();
    let path = quote(&folder.canonicalize().unwrap().join("a.png"));
    assert!(record.contains(&format!("\nPath={path}\n")), "{record}");
}

#[test]
fn capital_d_asks_and_only_y_deletes() {
    // rows T9, V16a: tests/test_viewer_tty.py:259 at tag python-final: the question in reverse video on the status row as long as it waits; `n` keeps the image, `D y` deletes it for good, no trash made; the question and `kept` draw over the picture, the next image comes from a cleared screen
    let sandbox = Sandbox::new();
    let folder = pics(&sandbox, &[("a.png", 80, 40), ("b.png", 40, 80)]);
    let mut tty = open(&sandbox, &folder.join("a.png"), SMALL);
    let at = tty.send(b"D");
    let draw = draw_with(&tty, at, &question("a.png"));
    assert!(!cleared(&tty.out()[draw]));
    // a window for what must not happen, past a toast's 1.2 s: the question going
    std::thread::sleep(Duration::from_secs(2));
    let row = tty.screen().row(24);
    assert!(row.starts_with(" delete a.png permanently? y/n"), "{row:?}");
    assert!(tty.screen().cell(24, 2).pen.reverse);
    let draw = says(&mut tty, b"n", "kept a.png");
    assert!(!cleared(&draw));
    assert!(folder.join("a.png").exists());
    let draw = says(&mut tty, b"Dy", "deleted a.png");
    assert!(cleared(&draw));
    tty.send(b"q");
    assert_eq!(tty.exit(), 0);
    assert_eq!(names(&folder), ["b.png"]);
    assert!(!sandbox.path("data/Trash").exists());
}

#[test]
fn any_other_answer_keeps_the_image_and_is_used_up() {
    // rows T9, V21, V16a: tests/test_viewer.py:1794 at tag python-final with `n l q d u Esc Y Enter`, and `Q` and F1 (key of bin/ascii-viewer:2227: after `D`, F1 is a key that keeps the image): `kept <name>` and nothing else, no step, quit, trash, undo or help, the image not sent again; the undo stack as it was
    let sandbox = kitty_sandbox();
    let folder = pics(
        &sandbox,
        &[("a.png", 80, 40), ("b.png", 40, 80), ("c.png", 60, 60)],
    );
    let mut tty = open(&sandbox, &folder.join("a.png"), WIDE);
    says(&mut tty, b"d", "trashed a.png");
    for key in [
        &b"n"[..],
        b"l",
        b"q",
        b"Q",
        b"d",
        b"u",
        ESC,
        b"Y",
        ENTER,
        F1,
    ] {
        let at = tty.send(b"D");
        let asked = draw_with(&tty, at, &question("b.png"));
        assert_eq!(
            placed(&tty.out()[asked]),
            None,
            "the question sends no image"
        );
        let at = tty.send(key);
        let draw = draw_with(&tty, at, &toast("kept b.png"));
        let out = tty.out();
        let after = &out[at..draw.end];
        let texts = reversed(after);
        assert!(
            texts
                .iter()
                .all(|text| text == "kept b.png" || text == "delete b.png permanently? y/n"),
            "{}: {texts:?}",
            shown(key)
        );
        assert!(placements(after).is_empty(), "{}", shown(key));
        assert!(find(after, b"any other key closes", 0).is_none());
    }
    assert!(tty.running());
    assert_eq!(names(&folder), ["b.png", "c.png"]);
    let draw = says(&mut tty, b"u", "restored a.png");
    assert_eq!(placed(&draw), Some((80, 40)));
    tty.send(b"q");
    assert_eq!(tty.exit(), 0);
}

/// wl-copy that notes its call and takes its input, then waits for `<itself>.go` (or the sandbox's end) before it exits 0, noting `done`: a copy that ends when the test says.
const GATED_COPY: &str = r#"#!/bin/sh
{ printf '%s\n' "$@"; echo --; } >> "$0.calls"
cat > "$0.stdin"
n=0
while [ ! -e "$0.go" ] && [ -e "$0" ] && [ "$n" -lt 6000 ]; do sleep 0.01; n=$((n + 1)); done
echo done > "$0.done"
exit 0
"#;

#[test]
fn the_question_shows_instead_of_any_toast() {
    // rows T9, S14: status of bin/ascii-viewer:2143 at tag python-final: the question shows instead of a toast, one set with it (`3D`, one batch: its end draws the question again) and one set while it waits (a copy that ends then: `copied WxH` of copy_image, bin/ascii-viewer:1657)
    let sandbox = kitty_sandbox();
    sandbox.program("wl-copy", GATED_COPY);
    let folder = pics(&sandbox, &[("a.png", 80, 40), ("b.png", 40, 80)]);
    let mut tty = open(&sandbox, &folder.join("a.png"), WIDE);
    // one batch: the toast is set when the question comes
    let at = tty.send(b"3D");
    let asked = draw_with(&tty, at, &question("a.png"));
    assert!(find(&tty.out()[at..asked.end], b"level 5%", 0).is_none());
    // the toast's end draws the status row again: the question still
    draw_with(&tty, asked.end, &question("a.png"));
    says(&mut tty, b"n", "kept a.png");
    let at = tty.send(b"y");
    tty.expect(at, toast("copying ..."));
    sandbox.await_noted("wl-copy.calls", "--");
    tty.until("the toast gone", |screen| screen.row(60).is_empty());
    let at = tty.send(b"D");
    draw_with(&tty, at, &question("a.png"));
    let gate = tty.len();
    sandbox.stub_data("wl-copy.go", "");
    sandbox.await_noted("wl-copy.done", "done");
    // the copy's end draws the status row again, nothing else does now: the question on it
    let draw = draw_with(&tty, gate, &question("a.png"));
    assert_eq!(
        reversed(&tty.out()[gate..draw.end]),
        ["delete a.png permanently? y/n"]
    );
    says(&mut tty, b"n", "kept a.png");
    assert!(find(&tty.out(), b"copied", gate).is_none());
    tty.send(b"q");
    assert_eq!(tty.exit(), 0);
    assert_eq!(names(&folder), ["a.png", "b.png"]);
}

#[test]
fn a_long_name_is_cut_in_the_question() {
    // row T9: status of bin/ascii-viewer:2143 at tag python-final (tests/test_viewer.py:1772): the name gets cols - 26 cells, 14 in 40 columns; a longer one is cut to one less and `…`, the question kept whole
    let sandbox = Sandbox::new();
    let folder = pics(
        &sandbox,
        &[("abcdefghij.png", 80, 40), ("abcdefghijk.png", 40, 80)],
    );
    let size = Size {
        rows: 10,
        cols: 40,
        xpx: 360,
        ypx: 200,
    };
    let mut tty = open(&sandbox, &folder.join("abcdefghij.png"), size);
    let at = tty.send(b"D");
    tty.expect(
        at,
        b"\x1b[10;1H\x1b[7m delete abcdefghij.png permanently? y/n \x1b[0m",
    );
    says(&mut tty, b"n", "kept abcdefghij.png");
    says(&mut tty, b"l", "abcdefghijk.png  2/2");
    let at = tty.send(b"D");
    tty.expect(
        at,
        "\x1b[10;1H\x1b[7m delete abcdefghijk.p… permanently? y/n \x1b[0m",
    );
    says(&mut tty, b"n", "kept abcdefghijk.png");
    tty.send(b"q");
    assert_eq!(tty.exit(), 0);
}

#[test]
fn y_answers_the_question_and_copies_only_after_it() {
    // rows T9, S14: tests/test_viewer.py:1805 at tag python-final: `y` after `D` deletes and copies nothing; the next `y` copies the next image (copy_image of bin/ascii-viewer:1657: `copying ...`, then `copied WxH`)
    let sandbox = kitty_sandbox();
    let folder = pics(&sandbox, &[("a.png", 80, 40), ("b.png", 40, 80)]);
    let mut tty = open(&sandbox, &folder.join("a.png"), WIDE);
    let draw = says(&mut tty, b"Dy", "deleted a.png");
    assert_eq!(placed(&draw), Some((40, 80)));
    let at = tty.send(b"y");
    let asked = tty.expect(at, toast("copying ..."));
    tty.expect(asked, toast("copied 40x80"));
    assert!(
        !reversed(&tty.out()[..at])
            .iter()
            .any(|text| text.starts_with("cop"))
    );
    assert_eq!(sandbox.noted("wl-copy.calls"), "--type\nimage/png\n--\n");
    tty.send(b"q");
    assert_eq!(tty.exit(), 0);
    assert_eq!(names(&folder), ["b.png"]);
}

#[test]
fn a_delete_is_no_undo_and_leaves_the_stack() {
    // rows T9, T7, V16a: tests/test_viewer.py:1785 at tag python-final: `D y` is not undone and leaves the trashed file undoable; `nothing to undo` sends no image again
    let sandbox = kitty_sandbox();
    let folder = pics(
        &sandbox,
        &[("a.png", 80, 40), ("b.png", 40, 80), ("c.png", 60, 60)],
    );
    let mut tty = open(&sandbox, &folder.join("a.png"), WIDE);
    let draw = says(&mut tty, b"d", "trashed a.png");
    assert_eq!(placed(&draw), Some((40, 80)));
    let draw = says(&mut tty, b"Dy", "deleted b.png");
    assert_eq!(placed(&draw), Some((60, 60)));
    let draw = says(&mut tty, b"u", "restored a.png");
    assert_eq!(placed(&draw), Some((80, 40)));
    let draw = says(&mut tty, b"u", "nothing to undo");
    assert_eq!(placed(&draw), None);
    tty.send(b"q");
    assert_eq!(tty.exit(), 0);
    assert_eq!(names(&folder), ["a.png", "c.png"]);
    assert!(names(&sandbox.path("data/Trash/files")).is_empty());
    assert!(names(&sandbox.path("data/Trash/info")).is_empty());
}

#[test]
fn the_next_image_is_the_next_by_name_that_opens() {
    // rows T5, T6, T7, V16a: tests/test_viewer.py:1579, :1587, :1594, :1600, :1637 and :1648 at tag python-final: after `d` the next image by name, wrapping round, past a file that is no picture; one too large to open is passed and named (`; <name> too large`); none left that opens: the empty folder; `u` from there puts them back, the last trashed first, each shown
    let sandbox = kitty_sandbox();
    let folder = pics(
        &sandbox,
        &[("a.png", 80, 40), ("c.png", 60, 60), ("e.png", 40, 80)],
    );
    fs::write(folder.join("b.png"), "not a picture").unwrap();
    png_header(&folder.join("d.png"), 20_000, 10_000);
    let mut tty = open(&sandbox, &folder.join("e.png"), WIDE);
    let draw = says(&mut tty, b"d", "trashed e.png");
    assert_eq!(placed(&draw), Some((80, 40)));
    let draw = says(&mut tty, b"d", "trashed a.png");
    assert_eq!(placed(&draw), Some((60, 60)));
    let draw = says(&mut tty, b"d", "trashed c.png; d.png too large");
    assert!(find(&draw, b"no images in pics", 0).is_some());
    for (name, size) in [
        ("c.png", (60, 60)),
        ("a.png", (80, 40)),
        ("e.png", (40, 80)),
    ] {
        let draw = says(&mut tty, b"u", &format!("restored {name}"));
        assert_eq!(placed(&draw), Some(size), "{name}");
    }
    says(&mut tty, b"u", "nothing to undo");
    tty.send(b"q");
    assert_eq!(tty.exit(), 0);
    assert_eq!(
        names(&folder),
        ["a.png", "b.png", "c.png", "d.png", "e.png"]
    );
    assert!(names(&sandbox.path("data/Trash/files")).is_empty());
}

#[test]
fn the_trash_holds_each_file_with_its_record() {
    // row T3: trash of bin/ascii-viewer:348 at tag python-final (tests/test_viewer.py:1348, :1365, :1378): a record of mode 0600, three lines, Path percent-encoded from the bytes, DeletionDate in local time; a name the trash holds (a file without its record, or a record) passed for `<base>.2<ext>`; the file renamed, never copied: its inode stays
    let sandbox = kitty_sandbox();
    let folder = sandbox.folder("my pics");
    let stills = [("a b#é.png", 80, 40), ("x.png", 40, 80), ("y.png", 60, 60)];
    for (name, w, h) in stills {
        save(&gradient(w, h), &folder.join(name));
    }
    let (files, info) = (
        sandbox.path("data/Trash/files"),
        sandbox.path("data/Trash/info"),
    );
    fs::create_dir_all(&files).unwrap();
    fs::create_dir_all(&info).unwrap();
    fs::write(files.join("x.png"), "orphan").unwrap();
    fs::write(info.join("y.png.trashinfo"), "taken").unwrap();
    let inodes = stills.map(|(name, ..)| ino(&folder.join(name)));
    let before = now(&sandbox);
    let mut tty = open(&sandbox, &folder.join("a b#é.png"), WIDE);
    for (name, ..) in stills {
        says(&mut tty, b"d", &format!("trashed {name}"));
    }
    let after = now(&sandbox);
    tty.send(b"q");
    assert_eq!(tty.exit(), 0);
    assert_eq!(names(&files), ["a b#é.png", "x.2.png", "x.png", "y.2.png"]);
    assert_eq!(
        names(&info),
        [
            "a b#é.png.trashinfo",
            "x.2.png.trashinfo",
            "y.2.png.trashinfo",
            "y.png.trashinfo"
        ]
    );
    assert_eq!(fs::read_to_string(files.join("x.png")).unwrap(), "orphan");
    assert_eq!(
        fs::read_to_string(info.join("y.png.trashinfo")).unwrap(),
        "taken"
    );
    let dir = quote(&folder.canonicalize().unwrap());
    assert!(dir.ends_with("/my%20pics"), "{dir}");
    let stored = [
        ("a b#é.png", "a%20b%23%C3%A9.png"),
        ("x.2.png", "x.png"),
        ("y.2.png", "y.png"),
    ];
    for ((stored, listed), inode) in stored.into_iter().zip(inodes) {
        let record = info.join(format!("{stored}.trashinfo"));
        let text = fs::read_to_string(&record).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert!(lines.len() == 3 && text.ends_with('\n'), "{text:?}");
        assert_eq!(lines[0], "[Trash Info]");
        assert_eq!(lines[1], format!("Path={dir}/{listed}"));
        let date = lines[2].strip_prefix("DeletionDate=").unwrap();
        assert!(
            before.as_str() <= date && date <= after.as_str(),
            "{date} not within {before} .. {after}"
        );
        assert_eq!(mode(&record), 0o600, "{stored}");
        assert_eq!(ino(&files.join(stored)), inode, "{stored} is a copy");
    }
}

#[test]
fn the_home_trash_is_the_data_homes_when_absolute_else_the_homes() {
    // rows T1, T2: home_trash and trash_ready of bin/ascii-viewer:289 and :329 at tag python-final: $XDG_DATA_HOME/Trash for an absolute value, else ~/.local/share/Trash (a relative value and none alike); the trash, files/ and info/ made 0700, missing folders above them as any folder
    let mut sandbox = kitty_sandbox();
    let folder = pics(
        &sandbox,
        &[("a.png", 80, 40), ("b.png", 40, 80), ("c.png", 60, 60)],
    );
    let deep = sandbox.path("deep/share");
    let homes = sandbox.path("home/.local/share/Trash");
    let runs = [
        (
            "a.png",
            Some(deep.clone().into_os_string()),
            deep.join("Trash"),
        ),
        ("b.png", Some("relative/share".into()), homes.clone()),
        ("c.png", None, homes),
    ];
    for (name, data, trash) in runs {
        match data {
            Some(data) => sandbox.set("XDG_DATA_HOME", data),
            None => sandbox.unset("XDG_DATA_HOME"),
        }
        let mut tty = open(&sandbox, &folder.join(name), WIDE);
        says(&mut tty, b"d", &format!("trashed {name}"));
        tty.send(b"q");
        assert_eq!(tty.exit(), 0);
        assert!(
            trash.join("files").join(name).is_file(),
            "{name} not in {}",
            trash.display()
        );
        assert!(trash.join(format!("info/{name}.trashinfo")).is_file());
        for dir in [&trash, &trash.join("files"), &trash.join("info")] {
            assert_eq!(mode(dir), 0o700, "{}", dir.display());
        }
    }
    for dir in ["deep", "deep/share", "home/.local", "home/.local/share"] {
        assert_eq!(mode(&sandbox.path(dir)), default_mode(), "{dir}");
    }
    assert!(!sandbox.path("relative").exists());
    assert!(names(&sandbox.path("data")).is_empty());
}

#[test]
fn a_trash_that_fails_says_so_and_changes_nothing() {
    // rows T2, T3, T5, V16a: remove_image of bin/ascii-viewer:1933 at tag python-final (tests/test_viewer.py:1714, :1386): `trash failed` and nothing else, the picture drawn over, not from a cleared screen: a trash that is no folder (trash_ready), a folder that lets its file go nowhere (the record made first goes again), a file gone meanwhile
    let sandbox = Sandbox::new();
    let folder = pics(&sandbox, &[("a.png", 80, 40), ("b.png", 40, 80)]);
    let trash = sandbox.path("data/Trash");
    let mut tty = open(&sandbox, &folder.join("a.png"), SMALL);
    let picture = |tty: &mut Tty| (1..24).map(|row| tty.screen().row(row)).collect::<Vec<_>>();
    let before = picture(&mut tty);
    fs::write(&trash, "no folder").unwrap();
    let draw = says(&mut tty, b"d", "trash failed");
    assert!(!cleared(&draw));
    assert_eq!(picture(&mut tty), before);
    assert_eq!(fs::read_to_string(&trash).unwrap(), "no folder");
    fs::remove_file(&trash).unwrap();
    {
        let _locked = Locked::new(&folder);
        let draw = says(&mut tty, b"d", "trash failed");
        assert!(!cleared(&draw));
    }
    assert!(trash.join("info").is_dir());
    assert!(names(&trash.join("info")).is_empty() && names(&trash.join("files")).is_empty());
    fs::remove_file(folder.join("a.png")).unwrap();
    let draw = says(&mut tty, b"d", "trash failed");
    assert!(!cleared(&draw));
    assert_eq!(picture(&mut tty), before);
    tty.send(b"q");
    assert_eq!(tty.exit(), 0);
    assert_eq!(names(&folder), ["b.png"]);
    assert!(names(&trash.join("files")).is_empty());
}

#[test]
fn u_puts_back_the_last_trashed_first_as_the_same_file() {
    // rows T7, S10, Z6, V16a: undo_trash and untrash of bin/ascii-viewer:1954 and :389 at tag python-final (tests/test_viewer.py:1648, :1397, :1752): the last trashed back first and shown as show() of bin/ascii-viewer:1677 shows an image, unturned at the fit, by a hard link to the stored file (the inode stays), the stored file and its record removed; the stack is this run's: another run has nothing to undo
    let sandbox = kitty_sandbox();
    let folder = pics(
        &sandbox,
        &[("a.png", 80, 40), ("b.png", 40, 80), ("c.png", 60, 60)],
    );
    let (a, b) = (folder.join("a.png"), folder.join("b.png"));
    let inodes = [ino(&a), ino(&b)];
    let mut tty = open(&sandbox, &a, WIDE);
    // turned a quarter right, then a zoom step: 40 x 80 px at 2^0.5
    let at = tty.send(b"k");
    let turned = tty.expect(at, b"a=T,f=24,t=t,s=40,v=80,");
    tty.expect(turned, b"\x1b[?2026l");
    let draw = says(&mut tty, b"+", "zoom 141%");
    assert_eq!(placed(&draw), Some((57, 113)));
    says(&mut tty, b"d", "trashed a.png");
    says(&mut tty, b"d", "trashed b.png");
    let draw = says(&mut tty, b"u", "restored b.png");
    assert_eq!(placed(&draw), Some((40, 80)));
    assert!(b.exists() && !a.exists());
    let draw = says(&mut tty, b"u", "restored a.png");
    assert_eq!(placed(&draw), Some((80, 40)));
    let draw = says(&mut tty, b"u", "nothing to undo");
    assert_eq!(placed(&draw), None);
    assert_eq!([ino(&a), ino(&b)], inodes);
    assert!(names(&sandbox.path("data/Trash/files")).is_empty());
    assert!(names(&sandbox.path("data/Trash/info")).is_empty());
    says(&mut tty, b"d", "trashed a.png");
    tty.send(b"q");
    assert_eq!(tty.exit(), 0);
    let mut tty = open(&sandbox, &b, WIDE);
    says(&mut tty, b"u", "nothing to undo");
    tty.send(b"q");
    assert_eq!(tty.exit(), 0);
    assert_eq!(names(&sandbox.path("data/Trash/files")), ["a.png"]);
}

#[test]
fn a_file_put_back_that_does_not_open_leaves_the_screen_as_it_was() {
    // row T7: undo_trash of bin/ascii-viewer:1954 at tag python-final (its TooLarge case): a stored file changed in the trash into one too large to open, or into no picture, goes back all the same, `restored <name>`; the image on screen stays (sent again: the draw is from a cleared screen)
    let sandbox = kitty_sandbox();
    let folder = pics(
        &sandbox,
        &[("a.png", 80, 40), ("b.png", 40, 80), ("c.png", 60, 60)],
    );
    let files = sandbox.path("data/Trash/files");
    let mut tty = open(&sandbox, &folder.join("a.png"), WIDE);
    says(&mut tty, b"d", "trashed a.png");
    says(&mut tty, b"d", "trashed b.png");
    // rewritten in place: the inode the stack knows stays
    png_header(&files.join("b.png"), 20_000, 10_000);
    fs::write(files.join("a.png"), "not a picture").unwrap();
    for name in ["b.png", "a.png"] {
        let draw = says(&mut tty, b"u", &format!("restored {name}"));
        assert_eq!(placed(&draw), Some((60, 60)), "{name}");
    }
    tty.send(b"q");
    assert_eq!(tty.exit(), 0);
    assert_eq!(fs::read(folder.join("a.png")).unwrap(), b"not a picture");
    assert_eq!(names(&folder), ["a.png", "b.png", "c.png"]);
    assert!(names(&files).is_empty());
}

#[test]
fn u_says_why_a_file_cannot_go_back() {
    // rows T8, V16a: undo_trash of bin/ascii-viewer:1954 at tag python-final (tests/test_viewer.py:1661): `nothing to undo`; `cannot restore: <name> exists`, nothing replaced, the entry kept; `restore failed` for another error (a folder that lets nothing in), the entry kept; none of these draws from a cleared screen, a file put back does
    let sandbox = Sandbox::new();
    let folder = pics(&sandbox, &[("a.png", 80, 40), ("b.png", 40, 80)]);
    let a = folder.join("a.png");
    let bytes = fs::read(&a).unwrap();
    let mut tty = open(&sandbox, &a, SMALL);
    let draw = says(&mut tty, b"u", "nothing to undo");
    assert!(!cleared(&draw));
    let draw = says(&mut tty, b"d", "trashed a.png");
    assert!(cleared(&draw));
    fs::write(&a, "newer").unwrap();
    let draw = says(&mut tty, b"u", "cannot restore: a.png exists");
    assert!(!cleared(&draw));
    assert_eq!(fs::read(&a).unwrap(), b"newer");
    fs::remove_file(&a).unwrap();
    {
        let _locked = Locked::new(&folder);
        let draw = says(&mut tty, b"u", "restore failed");
        assert!(!cleared(&draw));
    }
    assert!(!a.exists());
    let draw = says(&mut tty, b"u", "restored a.png");
    assert!(cleared(&draw));
    assert_eq!(fs::read(&a).unwrap(), bytes);
    tty.send(b"q");
    assert_eq!(tty.exit(), 0);
}

#[test]
fn u_drops_what_the_trash_no_longer_holds() {
    // rows T8, T6: undo_trash and untrash of bin/ascii-viewer:1954 and :389 at tag python-final (tests/test_viewer.py:1671, :1686, :1433): `<name> is no longer in the trash` when its stored file is gone, its record is gone, or the name holds another file (put back elsewhere, the slot taken again); the entry dropped, the ones under it still undoable, the other file left alone; the empty folder's hint follows the stack
    let sandbox = kitty_sandbox();
    let folder = pics(
        &sandbox,
        &[("a.png", 80, 40), ("b.png", 40, 80), ("c.png", 60, 60)],
    );
    let (files, info) = (
        sandbox.path("data/Trash/files"),
        sandbox.path("data/Trash/info"),
    );
    let mut tty = open(&sandbox, &folder.join("a.png"), WIDE);
    says(&mut tty, b"d", "trashed a.png");
    says(&mut tty, b"d", "trashed b.png");
    // emptied from the trash
    fs::remove_file(files.join("b.png")).unwrap();
    fs::remove_file(info.join("b.png.trashinfo")).unwrap();
    let draw = says(&mut tty, b"u", "b.png is no longer in the trash");
    assert_eq!(placed(&draw), None);
    let draw = says(&mut tty, b"u", "restored a.png");
    assert_eq!(placed(&draw), Some((80, 40)));
    // its record gone: the stored file is no longer this run's to take
    says(&mut tty, b"d", "trashed a.png");
    fs::remove_file(info.join("a.png.trashinfo")).unwrap();
    says(&mut tty, b"u", "a.png is no longer in the trash");
    says(&mut tty, b"u", "nothing to undo");
    assert!(files.join("a.png").is_file());
    // put back elsewhere, then the slot taken by another file
    let draw = says(&mut tty, b"d", "trashed c.png");
    assert!(
        find(&draw, b"u undo  q quit", 0).is_some(),
        "{}",
        shown(&draw)
    );
    fs::rename(files.join("c.png"), sandbox.path("elsewhere.png")).unwrap();
    fs::remove_file(info.join("c.png.trashinfo")).unwrap();
    fs::write(files.join("c.png"), "another file").unwrap();
    let other = "[Trash Info]\nPath=/elsewhere/c.png\nDeletionDate=2026-01-01T00:00:00\n";
    fs::write(info.join("c.png.trashinfo"), other).unwrap();
    let draw = says(&mut tty, b"u", "c.png is no longer in the trash");
    assert!(
        find(&draw, b"no images in pics", 0).is_some() && find(&draw, b"u undo", 0).is_none(),
        "{}",
        shown(&draw)
    );
    assert!(find(&draw, b"\x1b[2mq quit\x1b[0m", 0).is_some());
    tty.send(b"q");
    assert_eq!(tty.exit(), 0);
    assert!(names(&folder).is_empty());
    assert_eq!(
        fs::read_to_string(files.join("c.png")).unwrap(),
        "another file"
    );
    assert_eq!(
        fs::read_to_string(info.join("c.png.trashinfo")).unwrap(),
        other
    );
}

/// The 40 keys tests/test_viewer.py:1727 at tag python-final pins as doing nothing on the empty folder.
const IDLE: [&[u8]; 40] = [
    b"h", b"l", b"j", b"k", UP, DOWN, RIGHT, LEFT, b"+", b"=", b"-", b"v", b"y", b"s", b"r", b"d",
    b"D", b"b", b"c", b"g", b"`", b" ", b"p", b"m", b"L", b"[", b"]", b"\\", b"\x7f", b"1", b"2",
    b"3", b"4", b"7", b"8", b"0", b"9", b".", b",", b"x",
];

#[test]
fn the_empty_folder_takes_only_u_q_f_and_f1() {
    // rows T6, V16a: key of bin/ascii-viewer:2238 at tag python-final (tests/test_viewer.py:1720): on the empty folder `u`, `q`, `Q`, `f` and F1 act, the 40 other keys do nothing: no toast, question, page, help or copy; `f` after them shows they were read (it asks hyprctl, then draws), and the `u` sent after that is the first toast; `u` and closing the help draw from a cleared screen
    let sandbox = Sandbox::new();
    let folder = pics(&sandbox, &[("a.png", 80, 40)]);
    let mut tty = open(&sandbox, &folder.join("a.png"), SMALL);
    let empty =
        |screen: &Screen| screen.row(24).is_empty() && screen.row(12).trim() == "no images in pics";
    let draw = says(&mut tty, b"d", "trashed a.png");
    assert!(cleared(&draw));
    tty.until("the empty folder without the toast", empty);
    let at = tty.send(&IDLE.concat());
    tty.send(b"f");
    sandbox.await_noted("hyprctl.calls", "--");
    let sent = tty.send(b"u");
    let draw = draw_with(&tty, sent, &toast("restored a.png"));
    let out = tty.out();
    let idle = &out[at..draw.start];
    assert!(reversed(idle).is_empty(), "{:?}", reversed(idle));
    for what in [&b"any other key closes"[..], b" Render", b"\x1b_Ga=T"] {
        assert!(find(idle, what, 0).is_none(), "{}", shown(what));
    }
    let mut screen = Screen::new(24, 80);
    screen.feed(&out[..draw.start]);
    assert!(empty(&screen), "{}", screen.text());
    assert_eq!(screen.row(13).trim(), "u undo  q quit");
    assert!(cleared(&out[draw]));
    assert_eq!(sandbox.noted("hyprctl.calls"), "clients\n-j\n--\n");
    assert_eq!(sandbox.noted("wl-copy.calls"), "");
    assert_eq!(names(&folder), ["a.png"]);
    assert!(names(&sandbox.path("data/Trash/files")).is_empty());
    says(&mut tty, b"d", "trashed a.png");
    tty.until("the empty folder without the toast", empty);
    let at = tty.send(F1);
    tty.expect(at, b"any other key closes");
    let at = tty.send(b"x");
    let end = tty.expect(at, b"u undo  q quit");
    assert!(cleared(&tty.out()[at..end]));
    tty.send(b"Q");
    assert_eq!(tty.exit(), 0);
}

#[test]
fn the_empty_folder_says_so_centred_on_the_middle_row() {
    // rows T6, S15, V16, K5: notice and screen of bin/ascii-viewer:2169 and :2192 at tag python-final (tests/test_viewer.py:1600, :1618, :1626): kitty's images deleted, a cleared screen, `no images in <folder>` on row max(rows // 2, 1), under it in SGR 2 `u undo  q quit`, or `q quit` with nothing to undo, each cut to the width and centred; the folder by its own name, a link to it resolved
    let sandbox = Sandbox::new();
    let folder = pics(&sandbox, &[("a.png", 80, 40)]);
    let mut tty = open(&sandbox, &folder.join("a.png"), WIDE);
    let draw = says(&mut tty, b"d", "trashed a.png");
    assert_eq!(
        shown(&draw),
        shown(b"\x1b[?2026h\x1b[0m\x1b_Ga=d,d=A,q=2\x1b\\\x1b[2J\x1b[30;92Hno images in pics\x1b[0m\x1b[31;94H\x1b[2mu undo  q quit\x1b[0m\x1b[60;1H\x1b[7m trashed a.png \x1b[0m\x1b[?2026l")
    );
    says(&mut tty, b"u", "restored a.png");
    tty.send(b"q");
    assert_eq!(tty.exit(), 0);
    let narrow = Size {
        rows: 5,
        cols: 12,
        xpx: 108,
        ypx: 100,
    };
    let mut tty = open(&sandbox, &folder.join("a.png"), narrow);
    let draw = says(&mut tty, b"d", "trashed a.");
    let notice = b"\x1b[2J\x1b[2;1Hno images in\x1b[0m\x1b[3;1H\x1b[2mu undo  q qu\x1b[0m\x1b[5;1H";
    assert!(find(&draw, notice, 0).is_some(), "{}", shown(&draw));
    says(&mut tty, b"u", "restored a");
    tty.send(b"q");
    assert_eq!(tty.exit(), 0);
    symlink(&folder, sandbox.path("album")).unwrap();
    let size = Size {
        rows: 5,
        cols: 20,
        xpx: 180,
        ypx: 100,
    };
    let mut tty = open(&sandbox, &sandbox.path("album/a.png"), size);
    let draw = says(&mut tty, b"Dy", "deleted a.png");
    let notice = b"\x1b[2J\x1b[2;2Hno images in pics\x1b[0m\x1b[3;8H\x1b[2mq quit\x1b[0m\x1b[5;1H";
    assert!(find(&draw, notice, 0).is_some(), "{}", shown(&draw));
    tty.send(b"q");
    assert_eq!(tty.exit(), 0);
    assert!(names(&folder).is_empty());
}

#[test]
fn a_link_goes_to_the_trash_itself_and_back() {
    // rows T4, T7, T9, S15: trash and untrash of bin/ascii-viewer:353 and :389 at tag python-final (tests/test_viewer.py:1700, :1464): the link itself moves, its target stays and is the next image; `u` puts the link back; `D y` deletes the link alone
    let sandbox = kitty_sandbox();
    let folder = pics(&sandbox, &[("x.png", 80, 40)]);
    symlink("x.png", folder.join("y.png")).unwrap();
    let bytes = fs::read(folder.join("x.png")).unwrap();
    let stored = sandbox.path("data/Trash/files/y.png");
    let mut tty = open(&sandbox, &folder.join("y.png"), WIDE);
    let draw = says(&mut tty, b"d", "trashed y.png");
    assert_eq!(placed(&draw), Some((80, 40)));
    assert_eq!(fs::read_link(&stored).unwrap(), Path::new("x.png"));
    assert_eq!(names(&folder), ["x.png"]);
    let record = fs::read_to_string(sandbox.path("data/Trash/info/y.png.trashinfo")).unwrap();
    let path = quote(&folder.canonicalize().unwrap().join("y.png"));
    assert!(record.contains(&format!("\nPath={path}\n")), "{record}");
    says(&mut tty, b"u", "restored y.png");
    assert_eq!(
        fs::read_link(folder.join("y.png")).unwrap(),
        Path::new("x.png")
    );
    assert!(names(&sandbox.path("data/Trash/files")).is_empty());
    let draw = says(&mut tty, b"Dy", "deleted y.png");
    assert_eq!(placed(&draw), Some((80, 40)));
    tty.send(b"q");
    assert_eq!(tty.exit(), 0);
    assert_eq!(names(&folder), ["x.png"]);
    assert!(!folder.join("x.png").is_symlink());
    assert_eq!(fs::read(folder.join("x.png")).unwrap(), bytes);
}

#[test]
fn d_in_a_batch_trashes_the_image_its_steps_reached() {
    // rows S8, T5, T9: key of bin/ascii-viewer:2234 at tag python-final: another key settles the steps first, so `ld` from a.png trashes b.png and shows c.png, and `hD` from there asks about a.png
    let sandbox = kitty_sandbox();
    let folder = pics(
        &sandbox,
        &[("a.png", 80, 40), ("b.png", 40, 80), ("c.png", 60, 60)],
    );
    let mut tty = open(&sandbox, &folder.join("a.png"), WIDE);
    let draw = says(&mut tty, b"ld", "trashed b.png");
    assert_eq!(placed(&draw), Some((60, 60)));
    let at = tty.send(b"hD");
    let asked = draw_with(&tty, at, &question("a.png"));
    assert_eq!(placed(&tty.out()[asked]), Some((80, 40)));
    says(&mut tty, b"n", "kept a.png");
    tty.send(b"q");
    assert_eq!(tty.exit(), 0);
    assert_eq!(names(&folder), ["a.png", "c.png"]);
    assert_eq!(names(&sandbox.path("data/Trash/files")), ["b.png"]);
}

#[test]
fn a_delete_that_fails_says_so_and_changes_nothing() {
    // rows T10, T9, V16a: remove_image of bin/ascii-viewer:1933 at tag python-final (tests/test_viewer.py:1820): `delete failed` when the file cannot go (its folder lets nothing out), the image stays, drawn over, not from a cleared screen; asked again, `y` deletes it
    let sandbox = Sandbox::new();
    let folder = pics(&sandbox, &[("a.png", 80, 40), ("b.png", 40, 80)]);
    let mut tty = open(&sandbox, &folder.join("a.png"), SMALL);
    let picture = |tty: &mut Tty| (1..24).map(|row| tty.screen().row(row)).collect::<Vec<_>>();
    let before = picture(&mut tty);
    {
        let _locked = Locked::new(&folder);
        let draw = says(&mut tty, b"Dy", "delete failed");
        assert!(!cleared(&draw));
    }
    assert!(folder.join("a.png").exists());
    assert_eq!(picture(&mut tty), before);
    let draw = says(&mut tty, b"Dy", "deleted a.png");
    assert!(cleared(&draw));
    tty.send(b"q");
    assert_eq!(tty.exit(), 0);
    assert_eq!(names(&folder), ["b.png"]);
}

#[test]
fn a_picture_only_mpv_decodes_is_not_trashed() {
    // rows N7, V26: a PCX picture, which the built-in decoders do not take, opens as a one-frame video, which has no trash: `d`, `D` and `u` do nothing to it (the old viewer opened it as a still, which `d` trashes); `m` after them shows they are done, `muted` the only toast
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
    let at = tty.send(b"dDum");
    let end = tty.expect(at, toast("muted"));
    let texts = reversed(&tty.out()[at..end]);
    assert!(texts.iter().all(|text| text == "muted"), "{texts:?}");
    tty.send(b"q");
    assert_eq!(tty.exit(), 0);
    assert!(pcx.is_file());
    assert!(names(&sandbox.path("data")).is_empty());
}
