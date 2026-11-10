"""libchafa in-process for the ascii tools (ascii-viewer, ascii-convert): ASCII glyphs for an
RGB picture, without starting a chafa process or encoding the picture to hand it over, which
cost more than the drawing itself (about 15 ms to start, 25 ms of PNG for a photo).

Through ctypes; the enum values are libchafa 1.x's, from /usr/include/chafa. The escape codes
come from a fixed terminal description (xterm with truecolour), not the environment, so every
caller gets the same 38;2;r;g;b codes, which cells() reads.
"""

import ctypes

import numpy as np

PIXEL_RGB8 = 8
CANVAS_TRUECOLOR, CANVAS_FGBG = 0, 5  # FGBG: two colours, glyph shapes only, no colour codes
SPACE, ASCII = 1 << 0, 1 << 14
CELL_PX = 8  # px a cell side that chafa samples: its symbols are 8 x 8

vp, ci, cf = ctypes.c_void_p, ctypes.c_int, ctypes.c_float
chafa = ctypes.CDLL("libchafa.so.0")
glib = ctypes.CDLL("libglib-2.0.so.0")
for fn, res, args in (
    (chafa.chafa_symbol_map_new, vp, []),
    (chafa.chafa_symbol_map_add_by_tags, None, [vp, ci]),
    (chafa.chafa_canvas_config_new, vp, []),
    (chafa.chafa_canvas_config_set_geometry, None, [vp, ci, ci]),
    (chafa.chafa_canvas_config_set_canvas_mode, None, [vp, ci]),
    (chafa.chafa_canvas_config_set_symbol_map, None, [vp, vp]),
    (chafa.chafa_canvas_config_set_fg_only_enabled, None, [vp, ci]),
    (chafa.chafa_canvas_config_set_preprocessing_enabled, None, [vp, ci]),
    (chafa.chafa_canvas_config_set_work_factor, None, [vp, cf]),
    (chafa.chafa_canvas_config_unref, None, [vp]),
    (chafa.chafa_canvas_new, vp, [vp]),
    (chafa.chafa_canvas_draw_all_pixels, None, [vp, ci, ctypes.c_char_p, ci, ci, ci]),
    (chafa.chafa_canvas_print, vp, [vp, vp]),
    (chafa.chafa_canvas_unref, None, [vp]),
    (chafa.chafa_term_db_get_default, vp, []),
    (chafa.chafa_term_db_detect, vp, [vp, vp]),
    (chafa.chafa_calc_canvas_geometry, None, [ci, ci, ctypes.POINTER(ci), ctypes.POINTER(ci), cf, ci, ci]),
    (glib.g_string_free, vp, [vp, ci]),
):
    fn.restype, fn.argtypes = res, args


class GString(ctypes.Structure):
    _fields_ = [("str", vp), ("len", ctypes.c_size_t), ("allocated_len", ctypes.c_size_t)]


SYMBOLS = chafa.chafa_symbol_map_new()
chafa.chafa_symbol_map_add_by_tags(SYMBOLS, ASCII | SPACE)
_ENV = (ctypes.c_char_p * 3)(b"TERM=xterm-256color", b"COLORTERM=truecolor", None)
TERM_INFO = chafa.chafa_term_db_detect(chafa.chafa_term_db_get_default(), ctypes.cast(_ENV, vp))


def ascii_art(rgb, width, height, cols, rows, font_ratio, mono=False, work=0.2):
    """(cells wide, cells high, output bytes) for a picture of packed RGB rgb, fitted into
    cols x rows cells (rows None: as many as cols needs) of font_ratio (width / height), glyphs
    foreground only. Preprocessing is off: it auto-levels every frame, which would undo the
    level (ascii_theme.tone_table). Colour output is truecolour escapes; mono has none."""
    cw, ch = ci(cols), ci(rows or 100000)
    chafa.chafa_calc_canvas_geometry(width, height, ctypes.byref(cw), ctypes.byref(ch), font_ratio, 1, 0)
    config = chafa.chafa_canvas_config_new()
    chafa.chafa_canvas_config_set_geometry(config, cw, ch)
    chafa.chafa_canvas_config_set_canvas_mode(config, CANVAS_FGBG if mono else CANVAS_TRUECOLOR)
    chafa.chafa_canvas_config_set_symbol_map(config, SYMBOLS)
    chafa.chafa_canvas_config_set_fg_only_enabled(config, 1)
    chafa.chafa_canvas_config_set_preprocessing_enabled(config, 0)
    chafa.chafa_canvas_config_set_work_factor(config, work)
    canvas = chafa.chafa_canvas_new(config)
    chafa.chafa_canvas_draw_all_pixels(canvas, PIXEL_RGB8, rgb, width, height, width * 3)
    gs = chafa.chafa_canvas_print(canvas, TERM_INFO)
    out = ctypes.string_at(GString.from_address(gs).str, GString.from_address(gs).len)
    glib.g_string_free(gs, 1)
    chafa.chafa_canvas_unref(canvas)
    chafa.chafa_canvas_config_unref(config)
    return cw.value, ch.value, out


FG = 16  # a cell's colour slot for the theme's foreground: chafa's default colour, and all of mono


class Grid:
    """chafa's picture as cells: chars holds each cell's glyph (an ASCII code), slots its
    colour (palette 0-15, or FG), both (rows, cols) uint8 arrays; len() is the row count."""

    def __init__(self, chars, slots):
        self.chars, self.slots = chars, slots

    def __len__(self):
        return len(self.chars)


def cells(out, cols, rows, slot_of):
    """chafa's truecolour output as a Grid, read with numpy. An escape runs from ESC to the
    first 'm'; the bytes outside escapes, newlines aside, are the glyphs, row by row. A
    38;2;R;G;B escape's colour becomes a slot through slot_of, 39m (chafa's reset at each row
    end), 0m and m the foreground; each cell takes the last escape before it."""
    b = np.frombuffer(out, np.uint8)
    pad = np.concatenate([b, np.zeros(16, np.uint8)])  # an escape's fixed-offset reads stay in range
    esc = np.flatnonzero(b == 0x1B)
    ends = np.flatnonzero(b == ord("m"))
    term = ends[np.searchsorted(ends, esc)]
    depth = np.zeros(len(b) + 1, np.int8)
    depth[esc] = 1
    depth[term + 1] -= 1  # -=: an escape can start right where the last one ended
    glyph = (np.cumsum(depth[:-1], dtype=np.int8) == 0) & (b != ord("\n"))
    chars = b[glyph].reshape(rows, cols)
    at = np.cumsum(glyph)[esc]  # the cell each escape takes effect at
    true = (pad[esc + 2] == ord("3")) & (pad[esc + 3] == ord("8")) & (pad[esc + 5] == ord("2"))
    length = term - esc
    slot = np.full(len(esc), 255, np.uint8)  # 255: an escape that leaves the colour alone
    slot[~true & (((length == 4) & (pad[esc + 2] == ord("3")) & (pad[esc + 3] == ord("9")))
                  | ((length == 3) & (pad[esc + 2] == ord("0"))) | (length == 2))] = FG
    if true.any():
        start = esc[true] + 7  # past ESC [ 3 8 ; 2 ;
        win = pad[start[:, None] + np.arange(12)].astype(np.int16) - ord("0")  # R;G;B: digits, ';' 11
        n, col = np.arange(len(start)), np.arange(12)
        semi = win == ord(";") - ord("0")
        s1 = semi.argmax(1)
        s2 = np.where(semi & (col > s1[:, None]), col, 99).min(1)
        rgb = np.empty((len(start), 3), np.uint8)
        for i, (a, e) in enumerate(((0 * s1, s1), (s1 + 1, s2), (s2 + 1, term[true] - start))):
            d0, d1, d2 = (win[n, np.minimum(a + j, 11)] for j in range(3))
            rgb[:, i] = np.select([e - a == 1, e - a == 2], [d0, d0 * 10 + d1], d0 * 100 + d1 * 10 + d2)
        slot[true] = slot_of(rgb)
    keep = slot != 255
    at, slot = at[keep], slot[keep]
    if not len(at):  # mono: no colour at all
        return Grid(chars, np.full((rows, cols), FG, np.uint8))
    last = np.searchsorted(at, np.arange(rows * cols), side="right") - 1
    return Grid(chars, np.where(last >= 0, slot[np.maximum(last, 0)], FG).astype(np.uint8).reshape(rows, cols))
