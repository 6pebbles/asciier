import re
import unittest

import numpy as np
from support import THEME, gradient

from ascii_chafa import Grid
from ascii_term import Term

SGR = re.compile(rb"\x1b\[[0-9;]*m")
TERM = Term(THEME)


class TextTest(unittest.TestCase):
    def test_codes_where_slot_changes(self):
        grid = Grid(np.array([[65, 66, 67, 68]], np.uint8), np.array([[1, 1, 9, 16]], np.uint8))
        self.assertEqual(TERM.text(grid, False), b"\x1b[31mAB\x1b[91mC\x1b[39mD\x1b[39m")

    def test_slot_zero_is_blank(self):
        grid = Grid(np.array([[65, 66]], np.uint8), np.array([[0, 2]], np.uint8))
        self.assertEqual(TERM.text(grid, False), b"\x1b[30m \x1b[32mB\x1b[39m")

    def test_mono_draws_in_brightest(self):
        grid = Grid(np.array([[65], [66]], np.uint8), np.zeros((2, 1), np.uint8))
        self.assertEqual(TERM.text(grid, True), b"\x1b[97mA\n\x1b[97mB")


class RenderTest(unittest.TestCase):
    def test_every_mode_fills_the_grid(self):
        picture = gradient(400, 400)
        frame = (picture.width, picture.height, picture.tobytes())
        for glyphs in ("mixed", "density", "shape"):
            for mono in (False, True):
                cw, ch, out = TERM.render(frame, 60, 50, 9 / 20, mono, glyphs)
                lines = out.split(b"\n")
                self.assertEqual((cw, len(lines)), (60, ch), (glyphs, mono))
                self.assertTrue(all(len(SGR.sub(b"", line)) == cw for line in lines), (glyphs, mono))

    def test_font_ratio_sets_rows(self):
        picture = gradient(400, 400)
        frame = (picture.width, picture.height, picture.tobytes())
        self.assertEqual(TERM.render(frame, 60, 100, 0.5)[1], 30)
        self.assertEqual(TERM.render(frame, 60, 100, 9 / 20)[1], 27)


if __name__ == "__main__":
    unittest.main()
