"""Shared test setup: lib/ on the path, the bl1nk theme fixture in place of the user's current
kitty theme, bin/ scripts loaded as modules, and media made with ffmpeg."""

import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT / "lib"))

import ascii_theme  # noqa: E402
from ascii_term import load_script  # noqa: E402

ascii_theme.THEME = ROOT / "tests" / "bl1nk.conf"
THEME = ascii_theme.theme()

_scripts = {}


def script(name):
    """bin/name as a module, loaded once."""
    if name not in _scripts:
        _scripts[name] = load_script(ROOT / "bin" / name, name.replace("-", "_"))
    return _scripts[name]


def ffmpeg(out, *args):
    subprocess.run(["ffmpeg", "-v", "error", "-nostdin", "-y", *args, str(out)], check=True)
    return out


def testsrc(out, seconds=2, size="320x240", rate=25, *extra):
    """A test pattern clip with a sine tone."""
    return ffmpeg(out, "-f", "lavfi", "-i", f"testsrc2=d={seconds}:r={rate}:s={size}",
                  "-f", "lavfi", "-i", f"sine=d={seconds}", "-c:v", "libx264", "-g", "5", "-c:a", "aac", *extra)


def gradient(width=320, height=160):
    """A left-to-right grey ramp, as a Pillow image."""
    import numpy as np
    from PIL import Image

    row = np.linspace(0, 255, width).astype(np.uint8)
    return Image.fromarray(np.repeat(np.repeat(row[None, :, None], height, 0), 3, 2))
