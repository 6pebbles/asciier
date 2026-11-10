# asciier

Terminal viewer and converter: video, audio and images as ASCII art in the kitty theme's colours.

```
asciier FILE                      view
asciier convert [options] INPUT   write PNG or MP4
```

Keys: F1.

Build: `cargo install --path . --root ~/.local --locked`.

Needs: libmpv, libchafa, FreeType, libdbus, libdav1d; `fc-match`; for video conversion `ffmpeg`, `ffprobe`.

License: GPL-3.0-or-later.
