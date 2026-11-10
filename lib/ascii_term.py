"""A picture as cells for the ascii tools (grid(), in ascii-viewer and ascii-convert alike):
libchafa's glyphs (ascii_chafa), flat cells given glyphs by brightness (ascii_density), each
cell's colour one of the theme's 16 ANSI slots (ascii_theme). Term writes the cells as
ascii-viewer's terminal text, so the terminal draws them in the theme's colours. Also
font_path(), kitty_terminal() and load_script()."""

import functools
import os
import subprocess

import numpy as np

import ascii_density as density
from ascii_chafa import ascii_art, cells
from ascii_theme import Recolour, brightest_slot, luminance

FONT = "JetBrainsMono Nerd Font Mono"  # kitty's font: the density ramp is measured in it
CODES = [b"\x1b[%dm" % (30 + i if i < 8 else 82 + i) for i in range(16)] + [b"\x1b[39m"]  # by slot; 16 the foreground


@functools.cache
def font_path(bold=False):
    """The font file, from fc-match once a weight (8 ms a call)."""
    # weight 210 is fontconfig's black: the match is the family's heaviest face at or under it
    weight = ":weight=210" if bold else ""
    return subprocess.run(["fc-match", "-f", "%{file}", FONT + weight], capture_output=True, text=True).stdout


def grid(rgb, width, height, cols, rows, font_ratio, recolour, luma, glyphs, font, mono, work=0.2):
    """A picture (width x height, packed RGB) as an ascii_chafa.Grid of cols x rows cells (rows
    None: as many as cols needs) of font_ratio: chafa's glyphs, each cell's colour a slot
    through recolour, then flat cells (all for "density") given glyphs by brightness
    (ascii_density), measured in font against luma (each slot's linear luminance, FG last)."""
    cw, ch, out = ascii_art(rgb, width, height, cols, rows, font_ratio, mono=mono, work=work)
    g = cells(out, cw, ch, recolour.slots)
    return density.apply(g, rgb, width, height, glyphs, font, recolour.slots, luma, mono=mono)


def load_script(path, name):
    """The Python script at path, which has no .py to import it by, run as a module called name:
    ascii-convert inside ascii-viewer, the tools inside the tests."""
    import importlib.machinery
    import importlib.util

    loader = importlib.machinery.SourceFileLoader(name, str(path))
    module = importlib.util.module_from_spec(importlib.util.spec_from_loader(name, loader))
    loader.exec_module(module)
    return module


def kitty_terminal():
    """Whether this window is kitty's, which draws images over its graphics protocol."""
    return bool(os.environ.get("KITTY_WINDOW_ID")) or os.environ.get("TERM") == "xterm-kitty"


class Term:
    """A theme's colours set up for drawing: recolour maps chafa's truecolour to slots (skin as
    Recolour's), mono draws every glyph in the theme's brightest colour, luma holds each slot's
    linear luminance (the 16 colours, then the foreground) for the density ramp. ascii-convert
    paints with the same palette, recolour and luma."""

    def __init__(self, colours, saturation=100, skin=True):
        self.palette, self.skin = [colours[f"color{i}"] for i in range(16)], skin
        self.recolour = Recolour(self.palette, saturation, skin)
        bright = brightest_slot(colours)
        self.mono = CODES[bright]
        self.luma = [luminance(c) for c in (*self.palette, colours["foreground"])]
        self.luma_mono = self.luma[:16] + [self.luma[bright]]  # mono's cells take the foreground slot

    def saturate(self, percent):
        self.recolour = Recolour(self.palette, percent, self.skin)

    def render(self, frame, cols, rows, font_ratio, mono=False, glyphs="mixed", bold=False):
        """(cells wide, cells high, text) for frame (w, h, packed RGB) fitted into cols x rows
        cells of font_ratio (width / height): grid() (the ramp measured in the bold face when
        bold), written as text."""
        w, h, data = frame
        g = grid(data, w, h, cols, rows, font_ratio, self.recolour, self.luma_mono if mono else self.luma,
                 glyphs, font_path(bold), mono)
        ch, cw = g.chars.shape
        return cw, ch, self.text(g, mono)

    def text(self, grid, mono):
        """Terminal text for a Grid: a colour code where the slot changes along a row, blank
        for slot 0 (ANSI black: chafa's dark-cell dots), the theme's brightest for mono."""
        if mono:
            return b"\n".join(self.mono + row.tobytes() for row in grid.chars)
        lines = []
        for chars, slots in zip(grid.chars, grid.slots):
            chars = np.where(slots == 0, ord(" "), chars).astype(np.uint8)
            cut = np.flatnonzero(np.diff(slots.astype(np.int16))) + 1
            starts, ends = np.r_[0, cut], np.r_[cut, len(chars)]
            lines.append(b"".join(CODES[slots[s]] + chars[s:e].tobytes() for s, e in zip(starts, ends)) + CODES[16])
        return b"\n".join(lines)
