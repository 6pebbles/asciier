# asciier

Project file. Record decisions, facts, results and open questions as they happen. Facts carry a source; inference is marked.

## Rules

- Texts: compressed (/ultracave, /articulate, /tractatus). No hard wraps: one line per sentence, item or paragraph.
- Commits: /caveman-commit, no trailers, UTC dates.
- No personal data in the repository: names, home paths, hosts, hardware models, addresses.
- A behaviour change starts with its test.
- No config file, no hidden options, no extension points.

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
| `tests/` | black-box runs in a pty |

## Flow

`cargo test`, `cargo clippy -- -D warnings`, `cargo fmt --check`, then `cargo install --path . --root ~/.local --locked`.

## Decisions

- 2026-11-10: rewrite of the Python tools (tag `python-final`) in Rust, as one program.
- 2026-11-10: playback by libmpv in-process (software render API), driven from one thread.
- 2026-11-10: glyph shapes stay libchafa's; fonts by FreeType, as Pillow drove it: measured identical cell metrics, ramp and glyph tiles.
- 2026-11-10: stills by the `image` crate (png, jpeg, gif, webp, bmp, tiff, ico, tga, qoi, pnm, dds, hdr, exr, avif) and resvg.
- 2026-11-10: MPRIS by libdbus on the loop thread; no D-Bus signals.
- 2026-11-10: mpv runs with `config=no`; of mpv.conf only audio options are read.

## Open questions

- Video zoom.
- Denoise strength after downscale on heavy grain.

## Log

- 2026-11-10: baseline commit of the Python tools, tag `python-final`. Rust scaffold: module contracts, `look` and `font` complete.
