# asciier

Project file. Record decisions, facts, results and open questions as they happen. Facts carry a source; inference is marked.

## Rules

- Texts: compressed (/ultracave, /articulate, /tractatus). No hard wraps: one line per sentence, item or paragraph.
- Commits: /caveman-commit, no trailers, UTC dates.
- No personal data in the repository: names, home paths, hosts, hardware models, addresses.
- A behaviour change starts with its test.
- No config file, no hidden options, no extension points.
- `tests/*.py`, `bin/*`, `lib/*` in comments: those files at tag `python-final`, where expected values came from.

## Goal

One program: `asciier FILE` views video, audio and stills as ASCII art in the kitty theme's colours; `asciier convert [options] INPUT` writes PNG or MP4.

## Layout

| path | owns |
|---|---|
| `src/main.rs` | argv dispatch, exit codes |
| `src/look.rs` | the look's seven settings, limits, tone table |
| `src/theme.rs` | kitty theme text, slot mapping, mono's colour, theme collection |
| `src/font.rs` | font file, cell metrics, glyph masks, density ramp (FreeType) |
| `src/cells.rs` | picture to cells (libchafa), cells to terminal text |
| `src/picture.rs` | stills, SVG, cut and scale, PNG |
| `src/paint.rs` | cells to pixels: layout, glyph tiles |
| `src/convert.rs` | `asciier convert`: options, still, frame and video runs |
| `src/term.rs` | terminal: raw mode, restore, geometry, keys, kitty graphics |
| `src/mpv.rs` | libmpv: playback, frames |
| `src/view.rs` | the viewer's loop, modes, layers, status row |
| `src/still.rs` | still view: zoom, pan, turn, folder, trash keys |
| `src/trash.rs` | freedesktop trash |
| `src/page.rs` | render page |
| `src/mpris.rs` | MPRIS service |
| `src/desktop.rs` | scope child, clipboard, Hyprland, kitty remote control |
| `src/keys.txt` | the key tables F1 shows |
| `tests/` | black-box runs in a pty; `bl1nk.conf` theme fixture, `golden/` decoder fixtures |

## Flow

`cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt --check`, then `cargo install --path . --root ~/.local --locked`.

## Decisions

- 2026-11-10: rewrite of the Python tools (tag `python-final`) in Rust, as one program.
- 2026-11-10: playback by libmpv in-process (software render API), driven from one thread.
- 2026-11-10: glyph shapes stay libchafa's; fonts by FreeType, as Pillow drove it: measured identical cell metrics, ramp and glyph tiles.
- 2026-11-10: stills by the `image` crate (png, jpeg, gif, webp, bmp, tiff, ico, tga, qoi, pnm, dds, hdr, exr, avif) and resvg.
- 2026-11-10: MPRIS by libdbus on the loop thread; no D-Bus signals.
- 2026-11-10: mpv runs with `config=no`; of mpv.conf only audio options are read.
- 2026-11-11: converter video in two stages: frames read and their cells found on a thread, painting and writing on the main one; frames in order, 8 in flight.
- 2026-11-11: libchafa's threads for the live picture: one a 10000 cells, at most one a core; the converter keeps libchafa's default (one a core).
- 2026-11-11: libchafa's limits held in `cells::grid`: a picture over 65535 px a side is reduced first, a grid over 8191 cells a side is an error; past them libchafa 1.18 ends the process, the Python tools' too.
- 2026-11-11: a crop window over 179 megapixels is an error: it is allocated whole.
- 2026-11-11: Python tools removed from the tree; tag `python-final` keeps them.
- 2026-11-12: the converter's Ctrl-C (N25): a SIGINT handler sets a flag and wakes a thread of its own, which kills the children before a video's encoding and the decoder's group while frames are read; cleanup and messages as before, then death by SIGINT: a shell shows 130, a script running it stops; a SIGINT the parent left ignored stays ignored.
- 2026-11-12: mpv holds the frame of a one-frame stream without sound for `image-display-duration`: 0, `inf` for a picture, so a picture mpv opens stays until `q` and a one-frame video ends after its frame (N7, P14). A picture by mpv's demuxer at the load: one of FFmpeg's picture demuxers (`*_pipe`, `fits`, `alias_pix`, ...), mpv's `mf` (a picture by its suffix, 1 s long), or FFmpeg's MP4 one without a length (HEIC, AVIF; every MP4 or MOV video has one, fragmented too). The length alone does not tell: a Matroska live stream, NUT, raw H.264 and HEVC have none; nor the track's image flag: mpv sets it for a one-frame Matroska video and for one HEVC or AV1 frame in MP4. Measured: 17 pictures of 11 demuxers and 34 one-frame videos of 13 that reach mpv, mpv 0.41, FFmpeg 9.0.

- 2026-11-12: the converter writes each stderr line in one write, so child messages fall between complete lines (`src/convert.rs`).
- 2026-11-12: MPRIS requests are dropped from `r` onward, including the pending frame grab, because the old page opened immediately (`Viewer::serve_mpris`, M5).
- 2026-11-13: seeks, steps, pauses and grabs reach mpv in order; a seek settles after playback restarts, or after its mark returns with no seek or restart pending; a step settles once mpv confirms pause (`src/mpv.rs`).
- 2026-11-13: a playing `.` pauses and seeks to the frame after the one shown; an ambiguous landing is checked by mpv's step, then restored if the check advanced past a variable-rate frame (`Mpv::go`, `Mpv::settled`).
- 2026-11-13: past the picture's end, `.` only pauses, `,` first seeks past the end, and grabs use the held frame's time; two successive looks with advancing time and no new or dropped frame identify that end (`Mpv::events`).
- 2026-11-13: `s` frames returned with the clip's end are saved before quitting; video look-key toasts start after redraw; poll deadlines round up to milliseconds (`Viewer::mpv_batch`, `Viewer::key`, `poll_wait`).
- 2026-11-13: converter grabs use the first frame without seeking at 0 s; a later input seek yielding no frame is retried after the input when ffprobe does not place the target past the end (`convert::frame_at`, N38).
- 2026-11-13: animations are counted from GIF, APNG and WebP structure; damaged files retain decoder verdicts, with no Pillow emulation. A GIF's first-frame extent is measured before decoding (`src/picture.rs`, N6, N7, N41).
- 2026-11-15: frame-time estimates use mpv target times, capped by mpv's clock, and restart when speed is requested (`Mpv::count`, N26).
- 2026-11-17: video loops by a viewer seek to 0 at the natural end; audio alone retains mpv's gapless loop. This replaces recovery around mpv's own video-loop seek, where queued requests stalled (`Viewer::mpv_batch`, `Mpv::again`, N4).
- 2026-11-17: both step directions wait for the next look at a possible picture end; frame-time bounds survive missing reports and restart at seeks; settled times correct only the rendered frame with the matching target (`src/mpv.rs`).
- 2026-11-17: format walks validate signatures before measuring headers; PNG profiles are capped at 1 MiB, including embedded icon PNGs; WebP metadata must fit the file; AVIF measures its primary item's associated dimensions across every association box, including compatible major brands (`src/picture.rs`).
- 2026-11-17: EXR data windows obey the pixel limit; the built-in still decoder refuses a decoded block over 2,863,311,520 bytes across its channels. Four FLOAT channels within the pixel limit fit; more channels can be refused. Approved limit (`picture::exr_measured`).
- 2026-11-17: every MPRIS call uses the state left by preceding calls; SetPosition and Previous remain absolute through the asynchronous seek queue (`Viewer::serve_mpris`; `tests/mpris.rs` batch tests).

## Open questions

- Video zoom.
- Denoise strength after downscale on heavy grain.

## Log

- 2026-11-10: baseline commit of the Python tools, tag `python-final`. Rust scaffold: module contracts, `look` and `font` complete.
- 2026-11-11: every module in; 235 unit tests.
- 2026-11-11: parity, measured with throwaway scripts against `python-final`, theme `tests/bl1nk.conf`: converter stills identical to the Python tools' for PNG, SVG and alpha sources (34 option runs), JPEG sources 0.04-0.19 % of px apart (decoder); viewer screens of stills, equal cells: photo 54-86 %, flat art 79-89 %, grey ramp 90-99 % (N6 sampling).
- 2026-11-11: speed, same machine: converter, 12 s 1080p30 clip, 3.6 s (Python 5.4 s); viewer playing it, CPU seconds in 6 s with libchafa's threads sized to the window: 80x24 2.5, 200x60 5.8, 400x100 16.1 (default threads 3.2, 7.8, 21.8).

- 2026-11-12: mpv 0.41 adds up the relative seeks it holds queued (queue_seek, player/playloop.c): a batch of seek keys sent before mpv runs the first lands at their sum, clamped once; black-box tests pin the landing, the viewer's one seek a batch is code reading (src/view.rs keys()).
- 2026-11-12: under load (four viewer suites at once, load 40-58) a paused clip's frame read after `|` and 300 ms of quiet was 10-24 frames behind the one drawn next; tests pin a frame by a seek while paused, after which no later one landed in 12 runs at load 36-53.
- 2026-11-12: unit tests pin against python-final: both faces' ramps (glyph order, coverages to the bit); the fc-match faces Regular and ExtraBold (fontconfig takes the face whose weight is nearest 210); the 0.001 floor on a slot's luminance; mixed's 0.045 bound (cells at 0.04406 and 0.04606); half-to-even tones; the work factor's effect on libchafa's glyphs; the viewer's work factor 0.2 (a 320 x 96 noise still in a 40 x 13 window of 10 px cells shows python-final's rows; at 0.5, 33 of its 480 glyphs differ).
- 2026-11-13: SVG parts render on threads named `svg part`; tests/stills.rs waits on that name until no part is in flight (Z8).
- 2026-11-13: folder checks count APNG and WebP frames without decoding pixels: 2000 x 2000 two-frame files took 1 to 6 s a call before, under 0.1 ms after (debug build).
- 2026-11-13: tests: the harness ends a run by SIGTERM, then SIGKILL for its session after 5 s; the convert tests end what a failed run left working in its sandbox's folder, each process held by a pidfd; tests/cells.rs folded into tests/stills.rs; one FNV-1a 64 helper, `picture::fnv`.
- 2026-11-13: review rounds 4 and 5, each two adversarial reviews, a stress run of 10 whole suites, two or three at once (1-min load up to 119), and two fixers: 27 and 16 fix commits; playback failed in 7 and 3 of the 10 whole-suite runs, every other group 10 of 10.
- 2026-11-15: the GIF size guard against 274,893 hand-built and mutated GIF, PNG and WebP files, each process under an 8 GB address-space limit: 0 aborts, 0 escaping panics (319 aborts before); 4,555 PNG, GIF and WebP files of a desktop system: 0 verdicts changed.
- 2026-11-15: tests: the black-box harness records when each read of the output came (`Tty::came`) and waits on replayed updates (`Tty::until_drawn`); playback tests that raced a short clip's end start paused or use longer clips; the convert harness sends no Ctrl-C to a reaped run, and three Ctrl-C tests wait for what both converters do (frames fed to the encoder, the run's end), so the convert group takes 10-11 s on the old tools, not 41 s.
- 2026-11-15: review round 6: stress 3 of 10 whole suites green at 1-min load up to 92 (18 failures in 7 runs, all in playback timing tests and four mpv unit tests, one hang of a new test); fixes: requests held through a loop's restart, the N26 cap, 11 test reworks; whole suite then 518 passed.
- 2026-11-16: review round 7: stress 8 of 10 whole suites green (2 failures, at load 24 and 71); a 14-format audit of what each decoder reads and allocates before the size check, with hostile files under an 8 GB address-space limit; fixes: requests numbered against mpv's replies, the size walks above, 15 test reworks; 5,564 real PNG, GIF, WebP, AVIF and QOI files kept their verdicts.
- 2026-11-17: independent review found two AVIF size-check omissions; regressions cover later association boxes and compatible major brands (`2d6dde8`, `6e8c58d`).
- 2026-11-17: queued MPRIS calls reproduced stale status and summed absolute targets; private-bus batch regressions fail before the fix and pass after (`c6e9022`).
- 2026-11-17: final combined source `c6e9022`: `cargo test --offline --no-fail-fast` passed three times, 544 passed and 3 ignored per run (one accepted clock limit, two helper probes invoked by their parent test); 393.91, 388.61 and 388.89 s. `cargo clippy --all-targets -- -D warnings` and `cargo fmt --check` passed.
- 2026-11-17: installed release checks: 1920x1200 RGB8 PNG decoded by the installed wallpaper/lock image loader; H.264 320x180, 50 frames and AAC with VAAPI. Hidden kitty still/video checks passed at 1280x720, 142x36 cells; focus unchanged, created resources removed. Pixel appearance remains a manual observation.
- 2026-11-17: three paired conversions of a 12 s 1080p30 clip, 160 columns and 1920 px width, theme tests/bl1nk.conf, nice 19: median wall seconds Rust 3.097, Python 3.998; median CPU seconds 24.565, 22.480. All six MP4 outputs byte-identical, SHA-256 463f40fe453cd389777880164d9a9527b12fe669189073c02dc3e86aa51a61b6.
- 2026-11-17: production source with comments, excluding Rust test items: Rust 9372 physical lines, 377469 bytes; python-final 3663 lines, 179037 bytes. Documented source serialization compressed with xz -9: 94028 and 49920 bytes; dependencies excluded. Source size grew; these metrics do not measure Kolmogorov complexity.
