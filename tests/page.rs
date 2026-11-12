//! The render page (`r`) through a terminal: its menu and keys, the size and theme lists, the prompt, the crop editor, the preview and the crop editor's picture as kitty placements, export, help over it, failures, short windows and terminals without kitty graphics, and back to the player, on stills and video (inventory section 10; the page's side of H1, H5, S12, P22, V21).

mod common;

use common::screen::Screen;
use common::*;
use image::{Rgb, RgbImage, Rgba, RgbaImage};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// 100 x 30 cells of 9 x 20 px, the window of the old suite's page tests (tests/test_viewer.py:279 at tag python-final): the menu's 27 rows, a blank one and the prompt's; right of the menu a frame of 51 x 28 cells inside, a room of 459 x 560 px.
const PAGE: Size = Size {
    rows: 30,
    cols: 100,
    xpx: 900,
    ypx: 600,
};
/// The menu column's cells (RenderPage.WIDTH, bin/ascii-viewer:609 at tag python-final).
const WIDTH: usize = 46;

/// The menu on an 800 x 400 still `pic.png`, the player's look untouched, as the screen shows it: RenderPage.draw of bin/ascii-viewer:1096 at tag python-final, as the old viewer drew it in this harness.
const MENU: [&str; 27] = [
    " Render",
    " pic-ascii.png",
    "",
    " > Level      0",
    "   Contrast   0",
    "   Saturation 100%",
    "   Skin       grey (calm)",
    "   Glyphs     mixed (shapes, dithered flats)",
    "   Density    20.0 px",
    "   Columns    160",
    "   Fit        contain (margins)",
    "   Crop       free, editor  (enter)",
    "   Crop W     100%",
    "   Crop H     100%",
    "   Crop X     0%",
    "   Crop Y     0%",
    "   Size       1920x960  (1920 long side)",
    "   Theme      current",
    "   Bold       off",
    "   Mono       off",
    "",
    " ───────────────────────────────────────────",
    "   Export",
    "   Back",
    "",
    " j k move   h l nudge   enter set   ` default",
    " s export   / theme search   q back",
];

/// The crop editor on the whole of an 800 x 400 still: RenderPage.draw_crop of bin/ascii-viewer:1169 at tag python-final, as the old viewer drew it in this harness.
const CROP: [&str; 15] = [
    " Crop",
    " the source; the bright frame is the crop",
    "",
    "   Ratio      free",
    "   X          0%   (0 px)",
    "   Y          0%   (0 px)",
    "   W          100%   (800 px)",
    "   H          100%   (400 px)",
    "",
    " h j k l    move (H J K L finer)",
    " - =        smaller, larger",
    " [ ]        ratio",
    " < > { }    width, height (free)",
    " c centre   0 whole picture",
    " enter q    back to the menu",
];

/// The page's keys under F1: the docstring's part from `Render page (r):` (bin/ascii-viewer:47 at tag python-final), as the old viewer wrote it in this harness, 100 columns wide.
const PAGE_KEYS: [&str; 19] = [
    "Render page (r): a value typed or a theme search (/) takes the printable keys; Enter keeps a",
    "value, Esc drops it, and Enter, Esc, Tab, Up or Down end a search (the last three move on):",
    "key               menu                    list (size, theme)      crop editor",
    "F1                these keys              these keys              these keys",
    "j k Down Up       move down, up           move down, up           window down, up 5 %",
    "h l Left Right    value down, up          back, pick              window left, right 5 %",
    "J K H L           -                       -                       the same by 1 %",
    "Tab               -                       move down               -",
    "Enter             open or set it          pick                    back to the menu",
    "q Esc             back to the player      back to the menu        back to the menu",
    "Q r               back to the player      -                       -",
    "`                 back to the default     -                       -",
    "s                 export the PNG          -                       -",
    "/                 search the themes       search (themes)         -",
    "- + =             -                       -                       window smaller, larger",
    "[ ]               -                       -                       ratio: previous, next",
    "< > { }           -                       -                       width, height (free ratio)",
    "c 0               -                       -                       centre it; the whole picture",
    "Backspace         typing: delete          search: delete          -",
];

/// The menu column as the screen shows it: each row's first 46 cells, trailing blanks dropped.
fn column(screen: &Screen) -> Vec<String> {
    (1..=screen.rows)
        .map(|row| {
            let cells: String = screen.row(row).chars().take(WIDTH).collect();
            cells.trim_end().to_string()
        })
        .collect()
}

/// What the screen shows on `row` right of the menu column: the frame.
fn framed(screen: &Screen, row: usize) -> String {
    screen.row(row).chars().skip(WIDTH).collect()
}

/// The frame's top edge for `title`, `wide` cells inside: the title centred in `─` as Python's str.center puts it (RenderPage.frame of bin/ascii-viewer:1208 at tag python-final).
fn top(title: &str, wide: usize) -> String {
    let pad = wide - title.chars().count();
    let left = pad / 2 + (pad & wide & 1);
    format!("┌{}{title}{}┐", "─".repeat(left), "─".repeat(pad - left))
}

/// A menu row as (marked by the cursor, label, value): ` > Level      5% darker` gives (true, `Level`, `5% darker`); `None` for a row of another kind.
fn entry(line: &str) -> Option<(bool, String, String)> {
    let mut cells: Vec<char> = line.chars().collect();
    cells.resize(cells.len().max(14), ' ');
    if cells[0] != ' ' || cells[2] != ' ' || !matches!(cells[1], ' ' | '>') {
        return None;
    }
    let label: String = cells[3..13].iter().collect();
    let value: String = cells[14..].iter().collect();
    Some((cells[1] == '>', label.trim_end().to_string(), value))
}

/// The value the menu shows on the row labelled `label`.
fn value(screen: &Screen, label: &str) -> Option<String> {
    column(screen)
        .iter()
        .filter_map(|line| entry(line))
        .find(|(_, name, _)| name == label)
        .map(|(.., value)| value)
}

/// The label of the row the cursor marks.
fn cursor(screen: &Screen) -> Option<String> {
    column(screen)
        .iter()
        .filter_map(|line| entry(line))
        .find(|(marked, ..)| *marked)
        .map(|(_, label, _)| label)
}

/// Wait until the update the output ends in, if one is open, has ended: the screen then shows whole draws.
fn settled(tty: &Tty) {
    let out = tty.out();
    if let Some(open) = out.windows(8).rposition(|w| w == b"\x1b[?2026h")
        && find(&out, b"\x1b[?2026l", open).is_none()
    {
        tty.expect(open, b"\x1b[?2026l");
    }
}

/// Send `keys`, then wait until the row labelled `label` shows `want` and its draw has ended; no keys: only the wait.
fn shows(tty: &mut Tty, keys: &[u8], label: &str, want: &str) {
    tty.send(keys);
    let what = format!("{label} {want:?} after {}", shown(keys));
    tty.until(&what, |screen| {
        value(screen, label).as_deref() == Some(want)
    });
    settled(tty);
}

/// Send `keys`, then wait until the cursor marks the row `label` and its draw has ended.
fn onto(tty: &mut Tty, keys: &[u8], label: &str) {
    tty.send(keys);
    let what = format!("the cursor on {label} after {}", shown(keys));
    tty.until(&what, |screen| cursor(screen).as_deref() == Some(label));
    settled(tty);
}

/// Send `keys`, then wait until the menu column's row `row` reads `text` and its draw has ended.
fn reads(tty: &mut Tty, keys: &[u8], row: usize, text: &str) {
    tty.send(keys);
    let what = format!("row {row} {text:?} after {}", shown(keys));
    tty.until(&what, |screen| column(screen)[row - 1] == text);
    settled(tty);
}

/// A kitty placement in the output: the cell it is put at (`row;col`), its keys, the file it names, where it starts.
struct Placed {
    cell: String,
    keys: String,
    file: PathBuf,
    start: usize,
}

/// Wait for the first kitty placement written from byte `from` on, and for the end of its update.
fn placed(tty: &Tty, from: usize) -> Placed {
    const HEAD: &[u8] = b"\x1b_Ga=T,";
    let start = tty.expect(from, HEAD) - HEAD.len();
    let end = tty.expect(start, b"\x1b[?2026l");
    let out = tty.out();
    let csi = out[..start]
        .windows(2)
        .rposition(|pair| pair == b"\x1b[")
        .expect("a cursor move before it");
    let cell = String::from_utf8_lossy(&out[csi + 2..start]);
    let (keys, file) = placements(&out[start..end]).remove(0);
    Placed {
        cell: cell.trim_end_matches('H').to_string(),
        keys,
        file,
        start,
    }
}

/// Wait for a kitty placement with `keys` written from byte `from` on, passing over others.
fn placed_with(tty: &Tty, mut from: usize, keys: &str) -> Placed {
    loop {
        let found = placed(tty, from);
        if found.keys == keys {
            return found;
        }
        from = found.start + 1;
    }
}

/// How many kitty placements were written from byte `from` on to `to`.
fn placed_between(tty: &Tty, from: usize, to: usize) -> usize {
    count(&tty.out()[from..to], b"\x1b_Ga=T,")
}

/// Wait for `text` written from byte `from` on, then for the end of the update that holds it; the output's length there.
fn drawn(tty: &Tty, from: usize, text: &[u8]) -> usize {
    let after = tty.expect(from, text);
    tty.expect(after, b"\x1b[?2026l")
}

/// The RGB pixel at (x, y) of a placement's file `w` px wide.
fn pixel(pixels: &[u8], w: usize, x: usize, y: usize) -> [u8; 3] {
    let at = (y * w + x) * 3;
    [pixels[at], pixels[at + 1], pixels[at + 2]]
}

/// The viewer on `picture` in kitty (`KITTY_WINDOW_ID` set, the file probe answered at once) in a window of `size`, the still on screen as itself.
fn in_kitty(sandbox: &mut Sandbox, picture: &Path, size: Size) -> Tty {
    sandbox.set("KITTY_WINDOW_ID", "1");
    let tty = sandbox
        .launch()
        .arg(picture)
        .size(size)
        .kitty(KITTY_OK, Duration::ZERO)
        .start();
    tty.expect(0, b"\x1b_Ga=T,");
    tty
}

/// `r`: the page, then its preview; the preview's placement.
fn open(tty: &mut Tty) -> Placed {
    let at = tty.send(b"r");
    tty.until("the page", |screen| screen.row(1).starts_with(" Render"));
    placed(tty, at)
}

/// `keys` on the page of a still in kitty: back to the still, its image sent again.
fn back(tty: &mut Tty, keys: &[u8]) {
    let at = tty.send(keys);
    placed(tty, at);
    tty.until("the still", |screen| !screen.row(1).starts_with(" Render"));
}

/// `q` from the player: exit 0, and no /dev/shm file that a kitty command of the run named is left.
fn quits_clean(tty: &mut Tty) {
    tty.send(b"q");
    assert_eq!(tty.exit(), 0);
    let left: Vec<PathBuf> = transfer_files(&tty.out())
        .into_iter()
        .filter(|file| file.exists())
        .collect();
    assert!(left.is_empty(), "left in /dev/shm: {left:?}");
}

/// The picture a page exported at `path`.
fn exported(path: &Path) -> RgbImage {
    image::open(path)
        .unwrap_or_else(|e| panic!("{path:?}: {e}"))
        .to_rgb8()
}

#[test]
fn the_menu_and_its_preview_in_a_frame() {
    // rows R1, R2, R3, R15, R18, K5, V10, V15, N29: RenderPage.draw and frame of bin/ascii-viewer:1096 and :1208 at tag python-final, as the old viewer drew this still in this harness: every row of the column written and padded to 46 cells, the cursor's row in reverse video, the name, the rule and the hints in SGR 90; the frame in SGR 90 from column 47 to 99 titled with the output's size and columns; the preview centred inside it, from a cleared screen with kitty's images deleted, menu and preview in one synchronized update, sent again only for a new window; a new window size draws the page at once, without a frame where it leaves 10 cells or less (N29: the old viewer sent the preview as f=32, RGBX)
    let mut sandbox = Sandbox::new();
    let picture = save(&gradient(800, 400), &sandbox.path("pic.png"));
    let mut tty = in_kitty(&mut sandbox, &picture, PAGE);
    let preview = open(&mut tty);
    assert_eq!(
        (preview.cell.as_str(), preview.keys.as_str()),
        ("10;48", "a=T,f=24,t=t,s=459,v=230,c=51,r=12,C=1,z=-1,q=2")
    );
    assert_eq!(
        std::fs::metadata(&preview.file).unwrap().len(),
        459 * 230 * 3
    );
    let out = tty.out();
    let update = out[..preview.start]
        .windows(8)
        .rposition(|w| w == b"\x1b[?2026h")
        .unwrap();
    assert!(
        out[update..].starts_with(b"\x1b[?2026h\x1b_Ga=d,d=A,q=2\x1b\\\x1b[2J\x1b[0m"),
        "{}",
        shown(&out[update..update + 60])
    );
    assert!(
        find(&out[..preview.start], b"\x1b[1;1H Render", update).is_some(),
        "the menu outside the preview's update"
    );
    let screen = tty.screen();
    assert_eq!(column(screen)[..27], MENU);
    assert!(column(screen)[27..].iter().all(String::is_empty));
    let (marked, next) = (screen.cell(4, 2).pen, screen.cell(5, 2).pen);
    assert!(marked.reverse && screen.cell(4, 45).pen.reverse && !next.reverse);
    assert!(!screen.cell(4, 46).pen.reverse, "45 cells reversed");
    for (row, col) in [
        (2, 2),
        (22, 2),
        (26, 2),
        (27, 2),
        (1, 47),
        (15, 99),
        (30, 60),
    ] {
        assert_eq!(
            screen.cell(row, col).pen.fg,
            Some(8),
            "SGR 90 at {row}, {col}"
        );
    }
    assert_eq!(framed(screen, 1), top(" 1920x960 · 160 columns ", 51));
    for row in 2..30 {
        assert_eq!(framed(screen, row), format!("│{}│", " ".repeat(51)));
    }
    assert_eq!(framed(screen, 30), format!("└{}┘", "─".repeat(51)));
    // the cursor moved: every row written again, the picture not
    let at = tty.send(b"j");
    let end = drawn(&tty, at, b"\x1b[5;1H\x1b[7m > Contrast   0");
    let out = tty.out();
    for row in 1..=30 {
        let place = format!("\x1b[{row};1H");
        assert!(
            find(&out[..end], place.as_bytes(), at).is_some(),
            "row {row}"
        );
    }
    assert_eq!(placed_between(&tty, at, end), 0, "the picture sent again");
    // 57 columns leave the frame 10 cells: no frame, no picture
    let at = tty.len();
    let narrow = Size {
        rows: 30,
        cols: 57,
        xpx: 513,
        ypx: 600,
    };
    tty.resize(narrow);
    let end = drawn(&tty, at, b"\x1b[1;1H Render");
    tty.until("the page without a frame", |screen| {
        screen.row(1) == " Render" && screen.row(30).is_empty()
    });
    assert_eq!(placed_between(&tty, at, end), 0);
    assert_eq!(cursor(tty.screen()).as_deref(), Some("Contrast"));
    // 100 columns again: the frame and the picture (the old viewer sent first the preview it had drawn for the narrow window, s=72, then this one)
    let at = tty.len();
    tty.resize(PAGE);
    let again = placed_with(&tty, at, "a=T,f=24,t=t,s=459,v=230,c=51,r=12,C=1,z=-1,q=2");
    assert_eq!(again.cell, "10;48");
    // back: kitty's images deleted, then the still as itself
    let at = tty.send(b"q");
    let still = placed(&tty, at);
    assert!(find(&tty.out()[..still.start], KITTY_CLEAR, at).is_some());
    assert_eq!(
        still.keys,
        "a=T,f=24,t=t,s=800,v=400,c=89,r=20,C=1,z=-1,q=2"
    );
    quits_clean(&mut tty);
}

#[test]
fn the_page_opens_on_the_players_look_and_its_own_defaults() {
    // rows R3, R18: LOOK and RenderPage.__init__ of bin/ascii-viewer:628 and :630 at tag python-final: the player's level, contrast, saturation, glyphs, bold and mono, the rest at the page's defaults; each page anew, and what one page sets is not the player's
    let mut sandbox = Sandbox::new();
    let picture = save(&gradient(800, 400), &sandbox.path("pic.png"));
    let mut tty = in_kitty(&mut sandbox, &picture, PAGE);
    let at = tty.send(b"318gbc");
    tty.expect(at, toast("mono"));
    open(&mut tty);
    settled(&tty);
    let opened = [
        ("Level", "5% darker"),
        ("Contrast", "-5%"),
        ("Saturation", "110%"),
        ("Skin", "grey (calm)"),
        ("Glyphs", "density (dithered)"),
        ("Density", "20.0 px"),
        ("Columns", "160"),
        ("Fit", "contain (margins)"),
        ("Crop", "free, editor  (enter)"),
        ("Crop W", "100%"),
        ("Size", "1920x960  (1920 long side)"),
        ("Theme", "current"),
        ("Bold", "on"),
        ("Mono", "on"),
    ];
    for (label, want) in opened {
        assert_eq!(value(tty.screen(), label).as_deref(), Some(want), "{label}");
    }
    onto(&mut tty, b"jjjjjjj", "Fit");
    shows(&mut tty, b"l", "Fit", "cover (edges cut)");
    onto(&mut tty, b"kkkkkkk", "Level");
    shows(&mut tty, b"l", "Level", "10% darker");
    // the player's keys are none of the page's: nothing on the page, nothing reaches the player (j after them shows when they are done)
    let before = column(tty.screen());
    onto(&mut tty, b"3cdm j", "Contrast");
    let unmarked = |rows: &[String]| -> Vec<String> {
        rows.iter()
            .map(|row| row.replacen(" > ", "   ", 1))
            .collect()
    };
    assert_eq!(unmarked(&column(tty.screen())), unmarked(&before));
    back(&mut tty, b"q");
    assert!(
        picture.exists()
            && std::fs::read_dir(sandbox.path("data"))
                .unwrap()
                .next()
                .is_none()
    );
    // the player's level was 5 % darker
    let at = tty.send(b"3");
    tty.expect(at, toast("level 10% darker"));
    open(&mut tty);
    settled(&tty);
    for (label, want) in [
        ("Level", "10% darker"),
        ("Fit", "contain (margins)"),
        ("Bold", "on"),
        ("Mono", "on"),
    ] {
        assert_eq!(value(tty.screen(), label).as_deref(), Some(want), "{label}");
    }
    back(&mut tty, b"q");
    quits_clean(&mut tty);
}

#[test]
fn j_k_move_round_the_rows_and_h_l_step_their_values() {
    // rows R4, R6, R11, R14, R18: RenderPage.key, nudge, columns, set_columns and value_text of bin/ascii-viewer:971, :902, :687, :692 and :881 at tag python-final: j k Down Up move round the 19 rows; h l Left Right step level and contrast by 5, saturation by 10, the crop's parts by 5, density by a px (the columns follow), columns by 10 (the density follows), toggle skin, fit, bold and mono, cycle glyphs and the sizes (the screen unknown: its preset is 1920's size); ` puts a row back; nothing on Crop, Export, Back; held keys draw the last step's settings (the old page's worker drew the latest settings only)
    let mut sandbox = Sandbox::new();
    let picture = save(&gradient(800, 400), &sandbox.path("pic.png"));
    let mut tty = in_kitty(&mut sandbox, &picture, PAGE);
    open(&mut tty);
    // (keys, row, value): no value is a move onto the row, no keys a look at it
    let steps: [(&[u8], &str, Option<&str>); 42] = [
        (b"k", "Back", None),
        (DOWN, "Level", None),
        (UP, "Back", None),
        (b"j", "Level", None),
        (b"l", "Level", Some("5% darker")),
        (RIGHT, "Level", Some("10% darker")),
        (b"hhh", "Level", Some("5% brighter")),
        (LEFT, "Level", Some("10% brighter")),
        (b"j", "Contrast", None),
        (b"l", "Contrast", Some("+5%")),
        (b"hh", "Contrast", Some("-5%")),
        (b"j", "Saturation", None),
        (b"h", "Saturation", Some("90%")),
        (b"lll", "Saturation", Some("120%")),
        (b"j", "Skin", None),
        (b"l", "Skin", Some("theme colour")),
        (b"h", "Skin", Some("grey (calm)")),
        (b"j", "Glyphs", None),
        (b"h", "Glyphs", Some("shape (chafa)")),
        (b"l", "Glyphs", Some("mixed (shapes, dithered flats)")),
        (b"l", "Glyphs", Some("density (dithered)")),
        (b"j", "Density", None),
        (b"l", "Density", Some("21.0 px")),
        (b"", "Columns", Some("152")),
        (b"j", "Columns", None),
        (b"l", "Columns", Some("162")),
        (b"", "Density", Some("19.8 px")),
        (b"h", "Columns", Some("152")),
        (b"", "Density", Some("21.1 px")),
        (b"`", "Density", Some("20.0 px")),
        (b"j", "Fit", None),
        (b"l", "Fit", Some("cover (edges cut)")),
        (b"h", "Fit", Some("contain (margins)")),
        (b"jj", "Crop W", None),
        (b"h", "Crop W", Some("95%")),
        (b"j", "Crop H", None),
        (b"l", "Crop H", Some("105%")),
        (b"j", "Crop X", None),
        (b"h", "Crop X", Some("-5%")),
        (b"j", "Crop Y", None),
        (b"l", "Crop Y", Some("5%")),
        (b"`", "Crop Y", Some("0%")),
    ];
    for (keys, label, want) in steps {
        match want {
            Some(want) => shows(&mut tty, keys, label, want),
            None => onto(&mut tty, keys, label),
        }
    }
    let crops = [("Crop X", "0%"), ("Crop H", "100%"), ("Crop W", "100%")];
    for (label, want) in crops {
        onto(&mut tty, b"k", label);
        shows(&mut tty, b"`", label, want);
    }
    // Crop has nothing to step: every row as it was (j after h l shows when they are done)
    onto(&mut tty, b"k", "Crop");
    let unmarked = |rows: Vec<String>| -> Vec<String> {
        rows.iter()
            .map(|row| row.replacen(" > ", "   ", 1))
            .collect()
    };
    let before = unmarked(column(tty.screen()));
    onto(&mut tty, b"hlj", "Crop W");
    assert_eq!(unmarked(column(tty.screen())), before);
    onto(&mut tty, b"jjjj", "Size");
    for (keys, want) in [
        (&b"l"[..], "1080x540  (1080 long side)"),
        (b"l", "3840x1920  (3840 long side)"),
        (b"l", "1920x960  (screen)"),
        (RIGHT, "1920x960  (1920 long side)"),
        (LEFT, "1920x960  (screen)"),
        (b"`", "1920x960  (1920 long side)"),
    ] {
        shows(&mut tty, keys, "Size", want);
    }
    // without kitty's cache the theme list is `current` alone, and so is the cycle
    onto(&mut tty, b"j", "Theme");
    shows(&mut tty, b"l", "Theme", "current");
    reads(&mut tty, ENTER, 6, " 1 themes");
    assert!(column(tty.screen())[2].starts_with(" > current"));
    reads(&mut tty, ESC, 1, " Render");
    onto(&mut tty, b"j", "Bold");
    shows(&mut tty, b"l", "Bold", "on");
    onto(&mut tty, b"j", "Mono");
    shows(&mut tty, b"h", "Mono", "on");
    // nothing on Export and Back: every row as it was, no file, still the page
    onto(&mut tty, b"j", "Export");
    let before = unmarked(column(tty.screen()));
    onto(&mut tty, b"hl`j", "Back");
    onto(&mut tty, b"hl`j", "Level");
    assert_eq!(unmarked(column(tty.screen())), before);
    assert!(!sandbox.path("pic-ascii.png").exists());
    // 40 held keys: a drawing or two, the last at the last step's settings (a slower machine may read them in two batches)
    let at = tty.send(&b"l".repeat(40));
    placed(&tty, at);
    shows(&mut tty, b"", "Level", "95% darker");
    let drawings = placed_between(&tty, at, tty.len());
    assert!((1..=2).contains(&drawings), "{drawings} drawings");
    back(&mut tty, b"q");
    quits_clean(&mut tty);
}

#[test]
fn enter_toggles_or_prompts_and_a_typed_value_is_held_to_its_range() {
    // rows R5, R8, R11, R18, H2, N12: RenderPage.key, prompt_key, commit and draw of bin/ascii-viewer:1000, :1015, :928 and :1148 at tag python-final: Enter toggles, or prompts ` <Label> (<hint>): <typed>_` under the menu (row 29 here); printable keys type, Backspace deletes, Enter applies, whole numbers held to the row's range, or says `not a value: <text>` until the next key; Esc drops it; a typed column count sets the density that gives it; F1 over the prompt leaves it as it was; N12: density `nan` is no value (the old viewer ended with a traceback there, R8a)
    let mut sandbox = Sandbox::new();
    let picture = save(&gradient(800, 400), &sandbox.path("pic.png"));
    let mut tty = in_kitty(&mut sandbox, &picture, PAGE);
    open(&mut tty);
    let level = |typed: &str| format!(" Level (-95..95, up darker): {typed}_");
    reads(&mut tty, ENTER, 29, &level(""));
    reads(&mut tty, b"1", 29, &level("1"));
    let at = tty.send(F1);
    tty.expect(at, b"any other key closes");
    reads(&mut tty, b"x", 29, &level("1"));
    reads(&mut tty, b"2", 29, &level("12"));
    reads(&mut tty, BACKSPACE, 29, &level("1"));
    reads(&mut tty, b"\x085", 29, &level("5"));
    shows(&mut tty, ENTER, "Level", "5% darker");
    assert_eq!(column(tty.screen())[28], "");
    reads(&mut tty, ENTER, 29, &level(""));
    shows(&mut tty, b"200\r", "Level", "95% darker");
    reads(&mut tty, ENTER, 29, &level(""));
    shows(&mut tty, b" -7 \r", "Level", "7% brighter");
    reads(&mut tty, ENTER, 29, &level(""));
    reads(&mut tty, b"abc\r", 29, " not a value: abc");
    assert_eq!(value(tty.screen(), "Level").as_deref(), Some("7% brighter"));
    // the next key clears the message
    onto(&mut tty, b"j", "Contrast");
    assert_eq!(column(tty.screen())[28], "");
    reads(&mut tty, b"\r40", 29, " Contrast (-95..95): 40_");
    reads(&mut tty, ESC, 29, "");
    assert_eq!(value(tty.screen(), "Contrast").as_deref(), Some("0"));
    onto(&mut tty, b"j", "Saturation");
    reads(&mut tty, ENTER, 29, " Saturation (percent, 0..300): _");
    shows(&mut tty, b"301\r", "Saturation", "300%");
    onto(&mut tty, b"j", "Skin");
    shows(&mut tty, ENTER, "Skin", "theme colour");
    onto(&mut tty, b"j", "Glyphs");
    shows(&mut tty, ENTER, "Glyphs", "density (dithered)");
    onto(&mut tty, b"j", "Density");
    reads(&mut tty, ENTER, 29, " Density (px 4..120): _");
    reads(&mut tty, b"nan\r", 29, " not a value: nan");
    assert_eq!(value(tty.screen(), "Density").as_deref(), Some("20.0 px"));
    reads(&mut tty, ENTER, 29, " Density (px 4..120): _");
    shows(&mut tty, b"12.5\r", "Density", "12.5 px");
    assert_eq!(value(tty.screen(), "Columns").as_deref(), Some("256"));
    onto(&mut tty, b"j", "Columns");
    reads(&mut tty, ENTER, 29, " Columns (8..2000): _");
    shows(&mut tty, b"100\r", "Columns", "100");
    assert_eq!(value(tty.screen(), "Density").as_deref(), Some("32.0 px"));
    tty.until("the frame's title", |screen| {
        framed(screen, 1) == top(" 1920x960 · 100 columns ", 51)
    });
    onto(&mut tty, b"j", "Fit");
    shows(&mut tty, ENTER, "Fit", "cover (edges cut)");
    onto(&mut tty, b"jjjjjjjj", "Bold");
    shows(&mut tty, b"\n", "Bold", "on");
    onto(&mut tty, b"j", "Mono");
    shows(&mut tty, ENTER, "Mono", "on");
    back(&mut tty, b"q");
    quits_clean(&mut tty);
}

#[test]
fn the_size_list_and_the_screen_s_size() {
    // rows R6, R8, R9, R10, R18: RenderPage.target, open_list, list_key, commit and draw of bin/ascii-viewer:670, :1038, :1044, :928 and :1116 at tag python-final with the hyprctl stub: the list of the presets and `custom WxH`; `screen` is the focused monitor's size from `hyprctl monitors -j`, asked once a page, else 1920's size under its own label; a preset's long side takes the cropped picture's shape (R10: 1080 long side on 16:9 is 1080x608, with crop W 50 960x1080); a custom size typed, made even, refused outside 16..16384; ` puts the preset back; Esc, q, h and Left leave the list, Enter and l pick
    let mut sandbox = Sandbox::new();
    let picture = save(&gradient(320, 180), &sandbox.path("pic.png"));
    let mut tty = in_kitty(&mut sandbox, &picture, PAGE);
    open(&mut tty);
    onto(&mut tty, b"kkkkkk", "Size");
    reads(&mut tty, ENTER, 1, " Size");
    let list = [
        " Size",
        "",
        " > 1920 long side",
        "   1080 long side",
        "   3840 long side",
        "   screen",
        "   custom WxH",
        "",
        " j k move   enter pick   esc back",
    ];
    let screen = tty.screen();
    assert_eq!(column(screen)[..9], list);
    assert!(column(screen)[9..].iter().all(String::is_empty));
    assert!(screen.cell(3, 2).pen.reverse && !screen.cell(4, 2).pen.reverse);
    assert_eq!(screen.cell(9, 2).pen.fg, Some(8));
    reads(&mut tty, b"j", 4, " > 1080 long side");
    reads(&mut tty, b"\t", 5, " > 3840 long side");
    reads(&mut tty, DOWN, 6, " > screen");
    shows(&mut tty, ENTER, "Size", "1920x1080  (screen)");
    shows(&mut tty, b"l", "Size", "1920x1080  (1920 long side)");
    shows(&mut tty, b"h", "Size", "1920x1080  (screen)");
    assert_eq!(sandbox.noted("hyprctl.calls"), "monitors\n-j\n--\n");
    back(&mut tty, b"q");
    sandbox.stub_data(
        "hyprctl.monitors",
        r#"[{"width": 1280, "height": 1024, "focused": false}, {"width": 2560, "height": 1440, "focused": true}]"#,
    );
    open(&mut tty);
    onto(&mut tty, b"kkkkkk", "Size");
    shows(&mut tty, b"h", "Size", "2560x1440  (screen)");
    assert_eq!(value(tty.screen(), "Columns").as_deref(), Some("213"));
    shows(&mut tty, b"l", "Size", "1920x1080  (1920 long side)");
    shows(&mut tty, b"l", "Size", "1080x608  (1080 long side)");
    onto(&mut tty, b"kkkk", "Crop W");
    reads(&mut tty, ENTER, 29, " Crop W (% of picture width): _");
    shows(&mut tty, b"50\r", "Size", "960x1080  (1080 long side)");
    shows(&mut tty, b"`", "Size", "1080x608  (1080 long side)");
    assert_eq!(value(tty.screen(), "Crop W").as_deref(), Some("100%"));
    // the list opens on the preset in force
    onto(&mut tty, b"jjjj", "Size");
    reads(&mut tty, ENTER, 4, " > 1080 long side");
    reads(&mut tty, b"k", 3, " > 1920 long side");
    reads(&mut tty, UP, 7, " > custom WxH");
    reads(&mut tty, ENTER, 29, " Size (WxH, each 16..16384): _");
    shows(&mut tty, b"641 X 479\r", "Size", "642x480");
    assert_eq!(value(tty.screen(), "Columns").as_deref(), Some("54"));
    reads(&mut tty, ENTER, 4, " > 1080 long side");
    reads(&mut tty, b"jjj", 7, " > custom WxH");
    reads(&mut tty, ENTER, 29, " Size (WxH, each 16..16384): _");
    reads(&mut tty, b"15x100\r", 29, " not a value: 15x100");
    assert_eq!(value(tty.screen(), "Size").as_deref(), Some("642x480"));
    shows(&mut tty, b"`", "Size", "1920x1080  (1920 long side)");
    for key in [ESC, b"q", b"h", LEFT] {
        reads(&mut tty, ENTER, 1, " Size");
        reads(&mut tty, b"j", 4, " > 1080 long side");
        reads(&mut tty, key, 1, " Render");
        assert_eq!(
            value(tty.screen(), "Size").as_deref(),
            Some("1920x1080  (1920 long side)"),
            "{}",
            shown(key)
        );
    }
    reads(&mut tty, ENTER, 1, " Size");
    reads(&mut tty, b"jj", 5, " > 3840 long side");
    shows(&mut tty, b"l", "Size", "3840x2160  (3840 long side)");
    assert_eq!(
        sandbox.noted("hyprctl.calls"),
        "monitors\n-j\n--\nmonitors\n-j\n--\n"
    );
    back(&mut tty, b"q");
    quits_clean(&mut tty);
}

#[test]
fn the_theme_list_follows_its_cursor_and_picks_a_theme() {
    // rows R12, R15, R16: kitty_themes, RenderPage.list_key and draw of bin/ascii-viewer:553, :1044 and :1116 at tag python-final with tests/golden/theme-collection.zip as kitty's cache: `current`, then its dark themes in its order (Night, Stored, Twin; entries without a name, a file or a member passed over), the name padded to 24 and in reverse video under the cursor, then a strip of its colours; the preview follows the cursor; `/` searches, case-insensitive, and the filter stays; Enter picks; the export is drawn in the theme picked (Night's background #101014, bl1nk's #111111)
    let mut sandbox = Sandbox::new();
    sandbox.folder("home/.cache/kitty");
    std::fs::copy(
        Path::new(CRATE).join("tests/golden/theme-collection.zip"),
        sandbox.path("home/.cache/kitty/kitty-themes.zip"),
    )
    .unwrap();
    let picture = save(&gradient(800, 400), &sandbox.path("pic.png"));
    let mut tty = in_kitty(&mut sandbox, &picture, PAGE);
    open(&mut tty);
    onto(&mut tty, b"kkkkk", "Theme");
    reads(&mut tty, ENTER, 1, " Theme  (dark, kitty's)");
    let strip = |name: &str, blocks: usize| format!("{name:<27} {}", "█".repeat(blocks));
    let list = [
        " Theme  (dark, kitty's)".to_string(),
        String::new(),
        strip(" > current", 16),
        strip("   Night", 16),
        strip("   Stored", 16),
        strip("   Twin", 1),
        String::new(),
        " / search   j k move   enter pick   esc back".to_string(),
        " 4 themes".to_string(),
    ];
    let screen = tty.screen();
    assert_eq!(column(screen)[..9], list);
    assert!(screen.cell(3, 1).pen.reverse && screen.cell(3, 27).pen.reverse);
    assert!(!screen.cell(3, 28).pen.reverse && !screen.cell(3, 29).pen.reverse);
    let ink = |rgb: u32| Some(0x100_0000 | rgb);
    let strips = [
        (3, 29, 0x1a1c1d),
        (3, 44, 0xf8f8f2),
        (4, 29, 0x18181c),
        (4, 44, 0xf0f0f4),
        (5, 29, 0x0000ff),
        (6, 29, 0x00aa00),
    ];
    for (row, col, rgb) in strips {
        assert_eq!(screen.cell(row, col).pen.fg, ink(rgb), "{row}, {col}");
    }
    assert_eq!(screen.cell(8, 2).pen.fg, Some(8));
    // the preview follows the theme under the cursor
    let at = tty.send(b"j");
    placed(&tty, at);
    assert!(column(tty.screen())[3].starts_with(" > Night"));
    let at = tty.send(b"k");
    placed(&tty, at);
    assert!(column(tty.screen())[2].starts_with(" > current"));
    reads(&mut tty, b"/", 8, " /_");
    reads(&mut tty, b"n", 7, " /n_");
    assert_eq!(column(tty.screen())[7], " 3 themes");
    let at = tty.send(b"I");
    placed(&tty, at);
    let screen = tty.screen();
    assert!(
        column(screen)[2].starts_with(" > Night"),
        "{:?}",
        column(screen)
    );
    assert_eq!(column(screen)[3..6], ["", " /nI_", " 1 themes"]);
    // Enter ends the search and keeps the filter; Enter again picks
    reads(
        &mut tty,
        ENTER,
        5,
        " / search   j k move   enter pick   esc back",
    );
    assert_eq!(column(tty.screen())[5], " 1 themes");
    shows(&mut tty, ENTER, "Theme", "Night");
    // `/` from the menu: the list in search, on the theme chosen; Esc ends the search, Esc again leaves the list
    reads(&mut tty, b"/", 8, " /_");
    assert!(column(tty.screen())[3].starts_with(" > Night"));
    reads(
        &mut tty,
        ESC,
        8,
        " / search   j k move   enter pick   esc back",
    );
    reads(&mut tty, ESC, 1, " Render");
    assert_eq!(value(tty.screen(), "Theme").as_deref(), Some("Night"));
    reads(&mut tty, b"s", 29, " exported pic-ascii.png");
    let file = exported(&sandbox.path("pic-ascii.png"));
    assert_eq!(file.dimensions(), (1920, 960));
    assert_eq!(file.get_pixel(0, 0).0, [0x10, 0x10, 0x14]);
    back(&mut tty, b"q");
    quits_clean(&mut tty);
}

#[test]
fn the_crop_editor_moves_its_window_over_the_source() {
    // rows R13, R15, R6, H5: RenderPage.draw_crop, crop_key and crop_canvas of bin/ascii-viewer:1169, :812 and :846 at tag python-final, as the old viewer drew this still in this harness: its rows, the frame titled with the window in px, the source on a canvas of #181818 with the window's 2 px edge (248, 248, 242), sent as f=24; h j k l move the window 5 % of the picture, H J K L 1 %, - shrinks it to 0.95 about its centre, 0 is the whole picture at the ratio original; a key that leaves the window as it is sends nothing; the editor's picture is sent again after help, the preview after the editor; ` on Crop puts the whole picture back, ratio free; the menu shows the parts as the shortest decimal (the session's change list: the old menu wrote the editor's floats as `1.0%`)
    let mut sandbox = Sandbox::new();
    let picture = save(&gradient(800, 400), &sandbox.path("pic.png"));
    let mut tty = in_kitty(&mut sandbox, &picture, PAGE);
    open(&mut tty);
    onto(&mut tty, b"jjjjjjjj", "Crop");
    let at = tty.send(ENTER);
    let canvas = placed(&tty, at);
    assert_eq!(
        (canvas.cell.as_str(), canvas.keys.as_str()),
        ("10;48", "a=T,f=24,t=t,s=459,v=230,c=51,r=12,C=1,z=-1,q=2")
    );
    let pixels = std::fs::read(&canvas.file).unwrap();
    assert_eq!(pixels.len(), 459 * 230 * 3);
    // the view: the picture with a quarter of it free on every side, 1200 x 600 at 0.3825, the window from (76, 38)
    assert_eq!(pixel(&pixels, 459, 0, 0), [24, 24, 24]);
    assert_eq!(pixel(&pixels, 459, 76, 38), [248, 248, 242]);
    assert_eq!(pixel(&pixels, 459, 77, 100), [248, 248, 242]);
    assert_eq!(pixel(&pixels, 459, 458, 229), [24, 24, 24]);
    let screen = tty.screen();
    assert_eq!(column(screen)[..15], CROP);
    assert!(column(screen)[15..].iter().all(String::is_empty));
    assert_eq!(framed(screen, 1), top(" source · crop 800x400 ", 51));
    let at = tty.send(b"l");
    let moved = placed(&tty, at);
    assert_eq!(column(tty.screen())[4], "   X          5%   (40 px)");
    let pixels = std::fs::read(&moved.file).unwrap();
    assert_eq!(pixel(&pixels, 459, 92, 38), [248, 248, 242]);
    assert_ne!(pixel(&pixels, 459, 76, 100), [248, 248, 242]);
    // x does nothing, and ] on a window of the picture's shape changes the ratio alone: nothing is sent
    let at = tty.send(b"x");
    tty.send(b"]");
    let end = drawn(&tty, at, b"Ratio      original");
    assert_eq!(placed_between(&tty, at, end), 0);
    let at = tty.send(b"-");
    placed(&tty, at);
    let parts = [
        "   X          7.5%   (60 px)",
        "   Y          2.5%   (10 px)",
        "   W          95%   (760 px)",
        "   H          95%   (380 px)",
    ];
    assert_eq!(column(tty.screen())[4..8], parts);
    let at = tty.send(b"0");
    placed(&tty, at);
    let whole: Vec<String> = CROP[3..8]
        .iter()
        .map(|line| line.replace("free", "original"))
        .collect();
    assert_eq!(column(tty.screen())[3..8], whole);
    let at = tty.send(b"J");
    placed(&tty, at);
    assert_eq!(column(tty.screen())[5], "   Y          1%   (4 px)");
    // help over the editor, then its picture again
    let at = tty.send(F1);
    tty.expect(at, b"any other key closes");
    let at = tty.send(b"x");
    placed(&tty, at);
    assert!(tty.screen().row(1).starts_with(" Crop"));
    // back to the menu: the preview of the crop
    let at = tty.send(b"q");
    placed(&tty, at);
    let screen = tty.screen();
    assert_eq!(
        value(screen, "Crop").as_deref(),
        Some("original, editor  (enter)")
    );
    for (label, want) in [("Crop X", "0%"), ("Crop Y", "1%"), ("Crop W", "100%")] {
        assert_eq!(value(screen, label).as_deref(), Some(want), "{label}");
    }
    assert_eq!(framed(screen, 1), top(" 1920x960 · 160 columns ", 51));
    shows(&mut tty, b"`", "Crop", "free, editor  (enter)");
    assert_eq!(value(tty.screen(), "Crop Y").as_deref(), Some("0%"));
    back(&mut tty, b"q");
    quits_clean(&mut tty);
}

#[test]
fn f1_on_the_page_shows_the_page_s_keys() {
    // rows H1, H2, H4, H5, V21: help_screen and help_key of bin/ascii-viewer:2100 and :2080 at tag python-final, as the old viewer wrote them in this harness: the page's part of the keys over a cleared screen, kitty's images deleted, each line at its row after a space, the hint on the bottom row in SGR 2; none of the player's rows; any key closes them, q too, without leaving the page; the preview is sent again
    let mut sandbox = Sandbox::new();
    let picture = save(&gradient(800, 400), &sandbox.path("pic.png"));
    let mut tty = in_kitty(&mut sandbox, &picture, PAGE);
    open(&mut tty);
    let at = tty.send(F1);
    let end = tty.expect(
        at,
        b"\x1b[30;1H\x1b[2m any other key closes\x1b[0m\x1b[?2026l",
    );
    let out = tty.out();
    let help = &out[at..end];
    assert!(
        help.starts_with(b"\x1b[?2026h\x1b[0m\x1b_Ga=d,d=A,q=2\x1b\\\x1b[2J"),
        "{}",
        shown(&help[..help.len().min(60)])
    );
    for (i, line) in PAGE_KEYS.iter().enumerate() {
        let written = format!("\x1b[{};1H {line}", i + 1);
        assert!(
            find(help, written.as_bytes(), 0).is_some(),
            "{written:?} not in {}",
            shown(help)
        );
    }
    assert!(find(help, b"trash it", 0).is_none(), "the player's keys");
    let at = tty.send(b"q");
    placed(&tty, at);
    let screen = tty.screen();
    assert!(screen.row(1).starts_with(" Render"));
    assert_eq!(cursor(screen).as_deref(), Some("Level"));
    assert!(tty.running());
    back(&mut tty, b"q");
    quits_clean(&mut tty);
}

/// A left-to-right grey ramp from 0 to 200: at 95 % darker every value of it goes to black.
fn dim_ramp(w: u32, h: u32) -> RgbImage {
    RgbImage::from_fn(w, h, |x, _| Rgb([(x * 200 / (w - 1)) as u8; 3]))
}

#[test]
fn export_writes_the_png_beside_the_source() {
    // rows R16, N15, N12: RenderPage.export of bin/ascii-viewer:951 at tag python-final: s, or Enter on Export, writes the full-size PNG beside the source as `<stem>-ascii.png` over what was there, and says `exported <name>`; N15: drawn from the settings as they are at once: 19 l on Level and s in one batch draw at 95 % darker, where every value under 242 goes to 0 (tone_table of lib/ascii_theme.py:135), so a ramp up to 200 is bl1nk's background (#111111) alone; N12: a file that cannot be written is `not exported: <reason>` (the old viewer ended with a traceback, R16a), and the page goes on
    let mut sandbox = Sandbox::new();
    let folder = sandbox.folder("pics");
    let picture = save(&dim_ramp(800, 400), &folder.join("dim.png"));
    let out = folder.join("dim-ascii.png");
    std::fs::write(&out, "not a picture").unwrap();
    let mut tty = in_kitty(&mut sandbox, &picture, PAGE);
    open(&mut tty);
    assert_eq!(column(tty.screen())[1], " dim-ascii.png");
    reads(&mut tty, b"s", 29, " exported dim-ascii.png");
    let file = exported(&out);
    assert_eq!(file.dimensions(), (1920, 960));
    assert!(file.pixels().any(|px| px.0 != [0x11; 3]), "a picture");
    std::fs::remove_file(&out).unwrap();
    onto(&mut tty, b"kk", "Export");
    assert_eq!(column(tty.screen())[28], "");
    reads(&mut tty, ENTER, 29, " exported dim-ascii.png");
    assert_eq!(exported(&out).dimensions(), (1920, 960));
    onto(&mut tty, b"jj", "Level");
    let batch = [b"l".repeat(19), b"s".to_vec()].concat();
    reads(&mut tty, &batch, 29, " exported dim-ascii.png");
    assert_eq!(value(tty.screen(), "Level").as_deref(), Some("95% darker"));
    let file = exported(&out);
    assert!(file.pixels().all(|px| px.0 == [0x11; 3]));
    // a folder where the file would go
    std::fs::remove_file(&out).unwrap();
    std::fs::create_dir(&out).unwrap();
    tty.send(b"s");
    tty.until("not exported", |screen| {
        column(screen)[28].starts_with(" not exported: ")
    });
    onto(&mut tty, b"j", "Contrast");
    assert_eq!(column(tty.screen())[28], "");
    back(&mut tty, b"q");
    quits_clean(&mut tty);
}

#[test]
fn a_failing_render_is_said_and_the_page_goes_on() {
    // row R17: RenderPage.work and draw of bin/ascii-viewer:714 and :1157 at tag python-final: a render that fails says `render failed: <error>` under the menu, cut to 45 cells, until the next key, and the page goes on (here the theme file gone, read for a new saturation: the old page drew a step of level or contrast with the theme it held; the wording of the library's error is A); export says why it cannot; with the theme back the next change draws; the session's change list: a failed render clears the preview
    let mut sandbox = Sandbox::new();
    let picture = save(&gradient(800, 400), &sandbox.path("pic.png"));
    let theme = sandbox.path("home/.config/kitty/current-theme.conf");
    let mut tty = in_kitty(&mut sandbox, &picture, PAGE);
    open(&mut tty);
    std::fs::remove_file(&theme).unwrap();
    onto(&mut tty, b"jj", "Saturation");
    tty.send(b"l");
    tty.until("render failed", |screen| {
        column(screen)[28].starts_with(" render failed: ")
    });
    settled(&tty);
    let screen = tty.screen();
    assert_eq!(column(screen)[28].chars().count(), 45);
    assert_eq!(value(screen, "Saturation").as_deref(), Some("110%"));
    assert!(framed(screen, 1).is_empty(), "the preview's frame");
    onto(&mut tty, b"j", "Skin");
    assert_eq!(column(tty.screen())[28], "");
    tty.send(b"s");
    tty.until("not exported", |screen| {
        column(screen)[28].starts_with(" not exported: ")
    });
    assert!(!sandbox.path("pic-ascii.png").exists());
    std::fs::copy(Path::new(CRATE).join("tests/bl1nk.conf"), &theme).unwrap();
    let at = tty.send(b"l");
    placed(&tty, at);
    let screen = tty.screen();
    assert_eq!(value(screen, "Skin").as_deref(), Some("theme colour"));
    assert_eq!(framed(screen, 1), top(" 1920x960 · 160 columns ", 51));
    assert_eq!(column(screen)[28], "");
    back(&mut tty, b"q");
    quits_clean(&mut tty);
}

#[test]
fn a_short_window_shows_the_prompt_and_messages_on_its_last_row() {
    // rows N16, R2a, R1: the menu's 27 rows in a window of 20, the first 20 written (RenderPage.menu of bin/ascii-viewer:1201 at tag python-final), the preview in a frame of 31 x 18 cells; N16: the prompt and a message on the last row (the old page wrote them on row 29, past the window's end)
    let mut sandbox = Sandbox::new();
    let picture = save(&gradient(800, 400), &sandbox.path("pic.png"));
    let short = Size {
        rows: 20,
        cols: 80,
        xpx: 720,
        ypx: 400,
    };
    let mut tty = in_kitty(&mut sandbox, &picture, short);
    let preview = open(&mut tty);
    assert_eq!(
        (preview.cell.as_str(), preview.keys.as_str()),
        ("7;48", "a=T,f=24,t=t,s=279,v=140,c=31,r=7,C=1,z=-1,q=2")
    );
    assert_eq!(column(tty.screen()), MENU[..20]);
    reads(&mut tty, ENTER, 20, " Level (-95..95, up darker): _");
    assert_eq!(column(tty.screen())[..19], MENU[..19]);
    reads(&mut tty, b"x\r", 20, " not a value: x");
    assert_eq!(column(tty.screen())[..19], MENU[..19]);
    back(&mut tty, b"q");
    quits_clean(&mut tty);
}

#[test]
fn r_on_a_still_takes_it_as_turned_over_grey() {
    // row S12: open_page of bin/ascii-viewer:2048 at tag python-final hands the page flatten(self.turned(), GREY), GREY (128, 128, 128) at bin/ascii-viewer:156: the still as turned (k turns it right, IMAGE_TURNS of bin/ascii-viewer:138), its transparency over #808080: a picture with a transparent right half exports as the one whose right half is #808080, and turned right its white left half is on top
    let sandbox = Sandbox::new();
    let folder = sandbox.folder("pics");
    let half = |right: Rgba<u8>| {
        RgbaImage::from_fn(
            160,
            80,
            move |x, _| {
                if x < 80 { Rgba([255; 4]) } else { right }
            },
        )
    };
    half(Rgba([255, 0, 0, 0]))
        .save(folder.join("a.png"))
        .unwrap();
    half(Rgba([128, 128, 128, 255]))
        .save(folder.join("b.png"))
        .unwrap();
    for name in ["a", "b"] {
        let still = folder.join(format!("{name}.png"));
        let mut tty = sandbox.launch().arg(still).size(PAGE).start();
        tty.until("the picture", |screen| screen.inked() > 100);
        tty.send(b"kr");
        tty.until("the page's frame", |screen| {
            framed(screen, 1) == top(" 960x1920 · 80 columns ", 51)
        });
        settled(&tty);
        let screen = tty.screen();
        assert_eq!(column(screen)[1], format!(" {name}-ascii.png"));
        assert_eq!(
            value(screen, "Size").as_deref(),
            Some("960x1920  (1920 long side)")
        );
        reads(&mut tty, b"s", 29, &format!(" exported {name}-ascii.png"));
        tty.send(b"q");
        tty.until("the still", |screen| {
            !screen.row(1).starts_with(" Render") && screen.inked() > 100
        });
        quits_clean(&mut tty);
    }
    let (a, b) = (
        exported(&folder.join("a-ascii.png")),
        exported(&folder.join("b-ascii.png")),
    );
    assert_eq!(a.dimensions(), (960, 1920));
    assert!(a == b, "the transparent half drawn as #808080");
    let light = |rows: std::ops::Range<u32>| {
        let sum: u64 = rows
            .flat_map(|y| (0..960).map(move |x| (x, y)))
            .map(|(x, y)| a.get_pixel(x, y)[1] as u64)
            .sum();
        sum / (960 * 960)
    };
    let (high, low) = (light(0..960), light(960..1920));
    assert!(high > low, "white on top: {high} over {low}");
}

#[test]
fn r_on_a_zoomed_still_takes_all_of_it() {
    // row S12: open_page of bin/ascii-viewer:2048 at tag python-final takes the picture as turned, whatever the zoom: zoomed in, the page's output keeps the still's shape and its crop editor the still's 800 x 400 px
    let mut sandbox = Sandbox::new();
    let picture = save(&gradient(800, 400), &sandbox.path("pic.png"));
    let mut tty = in_kitty(&mut sandbox, &picture, PAGE);
    let at = tty.send(b"++");
    tty.expect(at, b"\x1b[7m zoom ");
    open(&mut tty);
    settled(&tty);
    assert_eq!(
        value(tty.screen(), "Size").as_deref(),
        Some("1920x960  (1920 long side)")
    );
    onto(&mut tty, b"jjjjjjjj", "Crop");
    reads(&mut tty, ENTER, 1, " Crop");
    assert_eq!(column(tty.screen())[6..8], CROP[6..8]);
    reads(&mut tty, b"q", 1, " Render");
    back(&mut tty, b"q");
    quits_clean(&mut tty);
}

#[test]
fn back_from_the_page_by_q_capital_q_r_esc_and_back() {
    // rows R5, R7, V21, K5: RenderPage.key of bin/ascii-viewer:980 and :1000 at tag python-final: q, Q, r and Esc close the page from the menu, Enter on Back too; close_page of bin/ascii-viewer:2112 deletes kitty's images and draws the player again from a cleared screen; q there does not quit
    let mut sandbox = Sandbox::new();
    let picture = save(&gradient(800, 400), &sandbox.path("pic.png"));
    let mut tty = in_kitty(&mut sandbox, &picture, PAGE);
    for key in [&b"q"[..], b"Q", b"r", ESC, ENTER] {
        open(&mut tty);
        if key == ENTER {
            onto(&mut tty, b"k", "Back");
        }
        let at = tty.send(key);
        let still = placed(&tty, at);
        let out = tty.out();
        let clear = find(&out, KITTY_CLEAR, at).expect("kitty's images deleted");
        assert!(clear < still.start);
        assert!(
            find(&out[..still.start], b"\x1b[2J", at).is_some(),
            "cleared"
        );
        tty.until("the still", |screen| !screen.row(1).starts_with(" Render"));
        assert!(tty.running(), "{} quit", shown(key));
    }
    quits_clean(&mut tty);
}

#[test]
fn ctrl_c_and_sigterm_on_the_page_end_it_cleanly() {
    // rows V5, N11: main of bin/ascii-viewer:2440 at tag python-final: SIGTERM, and Ctrl-C from anywhere (N11), the page too, end it with exit 0 and the terminal given back; the preview's /dev/shm file goes with it
    let mut sandbox = Sandbox::new();
    let picture = save(&gradient(800, 400), &sandbox.path("pic.png"));
    for end in ["Ctrl-C", "SIGTERM"] {
        let mut tty = in_kitty(&mut sandbox, &picture, PAGE);
        let preview = open(&mut tty);
        if end == "Ctrl-C" {
            tty.send(b"\x03");
        } else {
            tty.signal(libc::SIGTERM);
        }
        assert_eq!(tty.exit(), 0, "{end}");
        let out = tty.out();
        assert!(
            out.ends_with(RESTORE),
            "{end}: {}",
            shown(&out[out.len().saturating_sub(80)..])
        );
        assert!(!preview.file.exists(), "{end}: the preview's file is left");
    }
}

/// The frame a 16:9 counter clip shows in an 80 x 24 window, where it covers rows 2 to 22 and all 80 columns: read on row 12.
fn frame(screen: &Screen) -> u32 {
    counted(screen, 12, 1, 80)
}

/// The seconds a status row shows (`> 01:05 / 02:10 ━━` is 65); `None` for another row.
fn seconds(row: &str) -> Option<u32> {
    let clock = row.get(2..)?.split(' ').next()?;
    let (minutes, seconds) = clock.split_once(':')?;
    Some(minutes.parse::<u32>().ok()? * 60 + seconds.parse::<u32>().ok()?)
}

/// The frames of the draws from byte `from` on that have a status row: the player's.
fn played(tty: &Tty, from: usize) -> Vec<u32> {
    replay(&tty.out()[from..], SMALL, |screen| {
        seconds(&screen.row(24)).map(|_| frame(screen))
    })
    .into_iter()
    .flatten()
    .collect()
}

/// The frames the player's draws in `out` show, counted from the clip's start: the counter's 8 bits made whole by the clock beside them (25 frames a second).
fn whole_frames(out: &[u8]) -> Vec<u32> {
    let whole = |count: u32, clock: u32| {
        let near = (clock * 25 + 12) as i64;
        (0..4)
            .map(|wraps| count + 256 * wraps)
            .min_by_key(|n| (*n as i64 - near).abs())
            .unwrap()
    };
    replay(out, SMALL, |screen| {
        seconds(&screen.row(24)).map(|clock| whole(frame(screen), clock))
    })
    .into_iter()
    .flatten()
    .collect()
}

#[test]
fn r_on_a_video_pauses_it_under_the_page_until_it_closes() {
    // rows P22, R7, V21, H3: open_page and close_page of bin/ascii-viewer:2048 and :2112 at tag python-final: r on a playing video opens the page on its frame at full size (1920 x 1080 for 160 x 90), named by the frame's time, playback paused under it and under help over it; q and r there go back to the player, cleared, and it plays on from where it paused; q there does not quit
    let mut sandbox = Sandbox::new();
    sandbox.set("KITTY_WINDOW_ID", "1");
    let video = counter(&sandbox, "clip.mp4", "160x90", 25, 20.0, 20.0, &[]);
    let mut tty = sandbox.launch().arg(&video).size(SMALL).start();
    tty.await_playing();
    let on_screen = *whole_frames(&tty.out()).last().expect("a frame before r");
    let asked = std::time::Instant::now();
    let preview = open(&mut tty);
    let opened = asked.elapsed().as_secs_f64();
    assert_eq!(
        (preview.cell.as_str(), preview.keys.as_str()),
        ("9;48", "a=T,f=24,t=t,s=279,v=157,c=31,r=8,C=1,z=-1,q=2")
    );
    let screen = tty.screen();
    assert_eq!(framed(screen, 1), top(" 1920x1080 · 160 columns ", 31));
    let name = column(screen)[1].trim().to_string();
    let time: f64 = name
        .strip_prefix("clip-ascii-")
        .and_then(|rest| rest.strip_suffix("s.png"))
        .and_then(|time| time.parse().ok())
        .unwrap_or_else(|| panic!("{name:?}"));
    let at = tty.send(F1);
    tty.expect(at, b"any other key closes");
    let at = tty.send(b"x");
    placed(&tty, at);
    // a window for what must not happen: playing on under the page, 50 frames
    std::thread::sleep(Duration::from_secs(2));
    let at = tty.send(b"q");
    tty.until("the player", |screen| seconds(&screen.row(24)).is_some());
    assert!(find(&tty.out(), b"\x1b[2J", at).is_some(), "cleared");
    let first = whole_frames(&tty.out()[at..])[0];
    // paused on the page's frame or later, and a second after the page showed at the latest: the pause is asked as the page opens, on mpv's answer to r, and mpv takes it on a thread of its own (half a second late seen at load 70, the viewer drawing the first preview meanwhile)
    let (paused, latest) = (
        (time * 25.0).round() as u32,
        on_screen + ((opened + 1.0) * 25.0).ceil() as u32 + 3,
    );
    assert!(
        first + 2 >= paused && first <= latest,
        "the frame on screen at r {on_screen}, the page's {paused}, after it {first}, the page {opened:.2} s after r"
    );
    tty.until("playing on", |screen| {
        screen.row(24).starts_with("> ") && frame(screen) != first % 256
    });
    open(&mut tty);
    let at = tty.send(b"r");
    tty.until("the player again", |screen| {
        seconds(&screen.row(24)).is_some()
    });
    let resumed = played(&tty, at)[0];
    tty.until("playing on", |screen| {
        screen.row(24).starts_with("> ") && frame(screen) != resumed
    });
    assert!(tty.running());
    tty.send(b"q");
    assert_eq!(tty.exit(), 0);
}

#[test]
fn keys_right_after_r_on_a_video_act_on_the_page() {
    // rows P22, R4: open_page of bin/ascii-viewer:2048 at tag python-final opens the page before the batch's next key: r then j moves the page's cursor; run in this harness the old viewer opened the page on Contrast; seen before the fix: the page on Level, the clip sought 60 s back under it (the page opens on mpv's answer, and keys before it went to the player)
    let sandbox = Sandbox::new();
    let video = counter(&sandbox, "clip.mp4", "160x90", 25, 20.0, 20.0, &[]);
    let mut tty = sandbox.launch().arg(&video).size(SMALL).start();
    tty.await_playing();
    tty.send(b"rj");
    tty.until("the page", |screen| screen.row(1).starts_with(" Render"));
    settled(&tty);
    assert_eq!(cursor(tty.screen()).as_deref(), Some("Contrast"));
    tty.send(b"q");
    tty.until("the player", |screen| seconds(&screen.row(24)).is_some());
    tty.send(b"q");
    assert_eq!(tty.exit(), 0);
}

#[test]
fn r_right_after_s_on_a_video_opens_the_page() {
    // rows P22, P21: screenshot of bin/ascii-viewer:1621 at tag python-final runs in the background, each press its own run, and r opens the page meanwhile; run in this harness the old viewer opened it and saved the screenshot; seen before the fix: the screenshot saved, no page (r was dropped while the screenshot's frame was on its way)
    let sandbox = Sandbox::new();
    let video = counter(&sandbox, "clip.mp4", "160x90", 25, 20.0, 20.0, &[]);
    let mut tty = sandbox.launch().arg(&video).size(SMALL).start();
    tty.await_playing();
    tty.send(b"sr");
    tty.until("the page", |screen| screen.row(1).starts_with(" Render"));
    let end = std::time::Instant::now() + PATIENCE;
    while !std::fs::read_dir(sandbox.path(".")).unwrap().any(|entry| {
        entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with("clip-ascii-")
    }) {
        assert!(std::time::Instant::now() < end, "no screenshot");
        std::thread::sleep(Duration::from_millis(20));
    }
    tty.send(b"q");
    tty.until("the player", |screen| seconds(&screen.row(24)).is_some());
    tty.send(b"q");
    assert_eq!(tty.exit(), 0);
}

#[test]
fn f1_right_after_r_on_a_video_leaves_the_resume_to_the_page() {
    // rows P22, H3, R7: open_page and open_help of bin/ascii-viewer:2048 and :2061 at tag python-final: r pauses and opens the page before the batch's next key, F1 then shows the page's keys and leaves playback to the page (help_resume: not playing), which plays on when it closes; run in this harness the old viewer played on after both closed; seen before the fixes: paused after both, the keys having paused playback before mpv's frame came and the page finding nothing to resume; either fix holds it: it passes without enter_page's `resume |=` (6 of 6 under load 19-22), keys typed after r waiting for the page, so F1 comes once the page has paused, and fails without both (3 of 3)
    let sandbox = Sandbox::new();
    let video = counter(&sandbox, "clip.mp4", "160x90", 25, 20.0, 20.0, &[]);
    let mut tty = sandbox.launch().arg(&video).size(SMALL).start();
    tty.await_playing();
    tty.send(&[b"r", F1].concat());
    tty.until("the page's keys", |screen| {
        screen.row(1).starts_with(" Render page (r):")
    });
    settled(&tty);
    tty.send(b"x");
    tty.until("the page", |screen| {
        let top = screen.row(1);
        top.starts_with(" Render") && !top.starts_with(" Render page")
    });
    let at = tty.send(b"q");
    tty.until("the player", |screen| seconds(&screen.row(24)).is_some());
    let first = played(&tty, at)[0];
    tty.until("playing on", |screen| {
        screen.row(24).starts_with("> ") && frame(screen) != first
    });
    tty.send(b"q");
    assert_eq!(tty.exit(), 0);
}

#[test]
fn without_kitty_graphics_the_menu_works_all_the_same() {
    // row R19 (U, kept: the menu and export work, no preview): no kitty variables, TERM xterm-256color, no probe; the menu, its keys and the export as in kitty (the old page wrote the preview's placement whatever the terminal, bin/ascii-viewer:1161 at tag python-final); nothing of the run left in /dev/shm
    let sandbox = Sandbox::new();
    let picture = save(&gradient(800, 400), &sandbox.path("pic.png"));
    let mut tty = sandbox.launch().arg(&picture).size(PAGE).start();
    tty.until("the picture", |screen| screen.inked() > 1000);
    tty.send(b"r");
    tty.until("the menu", |screen| column(screen)[..27] == MENU);
    onto(&mut tty, b"j", "Contrast");
    shows(&mut tty, b"l", "Contrast", "+5%");
    reads(&mut tty, b"s", 29, " exported pic-ascii.png");
    assert_eq!(
        exported(&sandbox.path("pic-ascii.png")).dimensions(),
        (1920, 960)
    );
    tty.send(b"q");
    tty.until("the still", |screen| {
        !screen.row(1).starts_with(" Render") && screen.inked() > 1000
    });
    quits_clean(&mut tty);
}

#[test]
fn the_preview_is_the_file_s_own_drawing_never_enlarged() {
    // rows R14, R1, R16: RenderPage.build and export of bin/ascii-viewer:745 and :951 at tag python-final: the preview is the file's drawing at the size shown, never enlarged: an output of 200 x 120 px, under the room, is sent at that size, centred, and its pixels are the export's
    let mut sandbox = Sandbox::new();
    let picture = save(&gradient(800, 400), &sandbox.path("pic.png"));
    let mut tty = in_kitty(&mut sandbox, &picture, PAGE);
    open(&mut tty);
    onto(&mut tty, b"kkkkkk", "Size");
    reads(&mut tty, ENTER, 1, " Size");
    reads(&mut tty, b"k", 7, " > custom WxH");
    reads(&mut tty, ENTER, 29, " Size (WxH, each 16..16384): _");
    let at = tty.send(b"200x120\r");
    let preview = placed(&tty, at);
    assert_eq!(
        (preview.cell.as_str(), preview.keys.as_str()),
        ("13;62", "a=T,f=24,t=t,s=200,v=120,c=22,r=6,C=1,z=-1,q=2")
    );
    let screen = tty.screen();
    assert_eq!(value(screen, "Size").as_deref(), Some("200x120"));
    assert_eq!(framed(screen, 1), top(" 200x120 · 17 columns ", 51));
    reads(&mut tty, b"s", 29, " exported pic-ascii.png");
    let file = exported(&sandbox.path("pic-ascii.png"));
    assert_eq!(file.dimensions(), (200, 120));
    assert!(
        std::fs::read(&preview.file).unwrap() == file.into_raw(),
        "the preview's pixels are the file's"
    );
    back(&mut tty, b"q");
    quits_clean(&mut tty);
}
