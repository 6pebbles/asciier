import re
import unittest

import numpy as np
import support  # noqa: F401
from PIL import Image

from ascii_chafa import FG, ascii_art, cells

ESC = re.compile(rb"\x1b\[([0-9;]*)m")


def reference_cells(out, slot_of):
    """cells() the slow way: a regex walk over the escapes, cell by cell."""
    chars, slots, slot = [], [], FG
    for line in out.split(b"\n"):
        row_c, row_s, at = [], [], 0
        for m in ESC.finditer(line):
            for ch in line[at : m.start()]:
                row_c.append(ch)
                row_s.append(slot)
            p = m[1].split(b";")
            if p[:2] == [b"38", b"2"]:
                slot = int(slot_of(np.array([[int(p[2]), int(p[3]), int(p[4])]], np.uint8))[0])
            elif m[1] in (b"39", b"0", b""):
                slot = FG
            at = m.end()
        for ch in line[at:]:
            row_c.append(ch)
            row_s.append(slot)
        chars.append(row_c)
        slots.append(row_s)
    return np.array(chars, np.uint8), np.array(slots, np.uint8)


def colourful(w=240, h=160):
    y, x = np.mgrid[0:h, 0:w]
    return np.stack([x * 255 // w, y * 255 // h, (x + y) * 255 // (w + h)], -1).astype(np.uint8)


def slot_by_red(rgb):
    return (np.asarray(rgb)[:, 0] // 16).astype(np.uint8)


class AsciiArtTest(unittest.TestCase):
    def test_geometry_follows_font_ratio(self):
        rgb = bytes(100 * 100 * 3)
        self.assertEqual(ascii_art(rgb, 100, 100, 40, None, 0.5)[:2], (40, 20))
        self.assertEqual(ascii_art(rgb, 100, 100, 40, None, 9 / 20)[:2], (40, 18))

    def test_fits_rows(self):
        cw, ch, _ = ascii_art(bytes(100 * 400 * 3), 100, 400, 80, 30, 0.5)
        self.assertEqual(ch, 30)
        self.assertLessEqual(cw, 80)

    def test_only_ascii_glyphs(self):
        _, _, out = ascii_art(colourful().tobytes(), 240, 160, 60, None, 0.5)
        text = ESC.sub(b"", out).replace(b"\n", b"")
        self.assertTrue(all(32 <= c < 127 for c in text))


class CellsTest(unittest.TestCase):
    def test_matches_reference_parser(self):
        cw, ch, out = ascii_art(colourful().tobytes(), 240, 160, 60, None, 0.5)
        grid = cells(out, cw, ch, slot_by_red)
        chars, slots = reference_cells(out, slot_by_red)
        np.testing.assert_array_equal(grid.chars, chars)
        np.testing.assert_array_equal(grid.slots, slots)

    def test_mono_is_all_foreground(self):
        picture = np.asarray(Image.fromarray(colourful()).convert("L").convert("RGB"))
        cw, ch, out = ascii_art(picture.tobytes(), 240, 160, 50, None, 0.5, mono=True)
        grid = cells(out, cw, ch, slot_by_red)
        self.assertEqual(grid.slots.shape, (ch, cw))
        self.assertTrue((grid.slots == FG).all())


if __name__ == "__main__":
    unittest.main()
