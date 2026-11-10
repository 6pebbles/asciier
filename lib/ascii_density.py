"""Tone for the ascii tools: glyphs chosen by how much ink they put in a cell, with ordered
dithering, where chafa's shapes would band.

chafa picks each cell's glyph by shape: the symbol whose outline best matches the cell's
pixels. On an edge that is right; on a flat area or a smooth gradient every cell looks alike,
chafa gives a few glyphs to many shades, and the picture shows bands. Here a cell's glyph comes
from its brightness instead: each printable ASCII glyph is rendered once in the font and its
ink coverage measured, so the ramp runs over all 95, darkest to densest, and a 4x4 Bayer matrix
dithers between neighbouring glyphs, so a gradient steps by single cells, not bands.

Modes: "shape" is chafa alone; "density" gives every cell a glyph by brightness; "mixed" (the
default) keeps chafa's glyph where a cell holds an edge (its brightness varies) and uses the
ramp on flat cells. Brightness is linear light: a glyph of coverage c in a colour of luminance
L lights a cell about c * L, so a cell of luminance Y takes coverage Y / L of the densest
glyph, darker theme colours drawing denser glyphs.
"""

import functools

import numpy as np
from PIL import Image, ImageDraw, ImageFont

from ascii_theme import LINEAR

MODES = ("mixed", "density", "shape")
FLAT = 0.045  # linear-light deviation within a cell under which "mixed" counts it as flat
BAYER = np.array([[0, 8, 2, 10], [12, 4, 14, 6], [3, 11, 1, 9], [15, 7, 13, 5]]) / 16 + 1 / 32
_LUMA = np.outer([0.2126, 0.7152, 0.0722], LINEAR).astype(np.float32)  # each channel's share


@functools.cache
def ramp(font_path):
    """(glyphs, coverage): the printable ASCII glyphs (uint8 codes) sorted by ink coverage in
    the font, and their coverage scaled to 0 (space) .. 1 (the densest)."""
    font = ImageFont.truetype(font_path, 64)
    ascent, descent = font.getmetrics()
    w, h = round(font.getlength("M")), ascent + descent
    codes = np.arange(32, 127, dtype=np.uint8)
    cover = []
    for code in codes:
        cell = Image.new("L", (w, h))
        ImageDraw.Draw(cell).text((0, 0), chr(code), font=font, fill=255)
        cover.append(np.asarray(cell).mean() / 255)
    cover = np.array(cover)
    order = np.argsort(cover, kind="stable")
    return codes[order], cover[order] / cover.max()


def cell_light(rgb, width, height, cols, rows):
    """Per cell of a cols x rows grid over a picture (packed RGB bytes): mean colour (uint8,
    (rows, cols, 3)), mean linear luminance and its deviation (float arrays)."""
    # Pillow's box filter at a whole factor is the 4x4 block mean, in C: numpy's reductions
    # over strided blocks cost several times the rest of the frame
    fine = Image.frombytes("RGB", (width, height), rgb).resize((cols * 4, rows * 4), Image.BOX)
    mean_rgb = np.asarray(fine.resize((cols, rows), Image.BOX))
    r, g, b = (np.asarray(band) for band in fine.split())
    lin = _LUMA[0][r] + _LUMA[1][g] + _LUMA[2][b]
    mean = _block_mean(lin, cols, rows)
    spread = np.sqrt(np.maximum(_block_mean(lin * lin, cols, rows) - mean * mean, 0))
    return mean_rgb, mean, spread


def _block_mean(plane, cols, rows):
    return np.asarray(Image.fromarray(plane).resize((cols, rows), Image.BOX))


def apply(grid, rgb, width, height, mode, font_path, slot_of, slot_luma, mono=False):
    """grid (chars, slots: ascii_chafa.Grid) with its flat cells (every cell for "density")
    given glyphs by brightness, dithered. rgb is the picture chafa drew (width x height, packed
    RGB). slot_of maps (n, 3) uint8 colours to slots; slot_luma holds each slot's linear
    luminance (17: the 16 colours, then the foreground). In mono every cell keeps its slot."""
    if mode == "shape":
        return grid
    rows, cols = grid.chars.shape
    mean_rgb, light, spread = cell_light(rgb, width, height, cols, rows)
    pick = np.ones((rows, cols), bool) if mode == "density" else spread < FLAT
    slots = grid.slots.copy()
    if not mono:
        slots[pick] = slot_of(mean_rgb[pick])
    lum = np.asarray(slot_luma)[slots]
    want = np.clip(light / np.maximum(lum, 1e-3), 0, 1)  # coverage, of the densest glyph
    glyphs, cover = ramp(font_path)
    above = np.clip(np.searchsorted(cover, want), 1, len(cover) - 1)
    lo, hi = cover[above - 1], cover[above]
    frac = (want - lo) / np.maximum(hi - lo, 1e-6)
    threshold = BAYER[np.arange(rows)[:, None] % 4, np.arange(cols)[None, :] % 4]
    chosen = glyphs[np.where(frac > threshold, above, above - 1)]
    chars = grid.chars.copy()
    chars[pick] = chosen[pick]
    return type(grid)(chars, slots)
