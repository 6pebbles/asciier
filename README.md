# asciier

View video and images as ASCII art in your kitty theme’s colours, play audio, and export PNG images or MP4 video.

![Asciier playing a ringed planet in GitHub Dark, shrinking a square crop and exporting the frame in Gruvbox Dark](docs/demo.webp)

## Installation

| Linux distribution | Package or native development packages | Verification |
| --- | --- | --- |
| Arch Linux (x86_64) | Run `makepkg -si` from [packaging/aur](packaging/aur). | Package built and tested |
| Debian 13 / Ubuntu 26.04 LTS | Build from source with `libmpv-dev libchafa-dev libfreetype-dev libdbus-1-dev libdav1d-dev`. | Build untested |
| Fedora 44 | Build from source with `mpv-devel chafa-devel freetype-devel dbus-devel libdav1d-devel`. | Build untested |

### Build from source

Install Rust 1.88 or newer with Cargo, a C toolchain and `pkg-config`. The build links to libmpv, Chafa 1.10+, FreeType 2.12.1+, D-Bus 1.6+ and dav1d 1.3+. On Debian 13, use a newer Rust toolchain than the default `rustc` 1.85.

Install from the repository root.

```sh
cargo install --path . --root ~/.local --locked
```

## Runtime dependencies

- A **kitty colour scheme** at `~/.config/kitty/current-theme.conf` defines `foreground`, `background` and `color0` through `color15`.
- **Fontconfig** locates the JetBrainsMono Nerd Font Mono typeface with `fc-match`.
- **FFmpeg** supplies `ffmpeg` and `ffprobe` for video conversion. MP4 export uses H.264 through VAAPI when available, or `libx264`, and AAC for audio.

### Optional integrations

| Component | Feature |
| --- | --- |
| kitty graphics protocol | Original still image (`v`), render-page preview and visual crop editor |
| kitty theme collection (`~/.cache/kitty/kitty-themes.zip`) | Dark themes in the render-page picker |
| `kitten` | Ligature control in kitty, lookup of uncached themes by name and inline preview after `convert --at` |
| `wl-copy` (wl-clipboard) | Copy a still image to the Wayland clipboard (`y`) |
| `hyprctl` (Hyprland) | Toggle fullscreen (`f`) and use the monitor size as an export preset |
| `scope` | Oscilloscope during audio-only playback |

## Usage

```sh
asciier FILE
asciier convert [options] INPUT
```

Press `Space` to pause, `.` to step one frame and `r` to open Render. In **Crop**, use `[` and `]` to choose an aspect ratio, `-` and `+` to resize the window, and the arrow keys to move it. Press `Enter` to return. In **Theme**, move through the list to preview colours, press `Enter` to select one, then `s` to export the PNG.

Press `F1` for viewer controls or run `asciier convert --help` for converter options.

## Tests

Run `cargo test --locked`. The full suite also uses `playerctl` and `busctl`.

## License

[GPL-3.0-or-later](LICENSE).
