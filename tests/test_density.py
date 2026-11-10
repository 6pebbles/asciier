import unittest

import numpy as np
from support import THEME, gradient
from PIL import Image

import ascii_density as density
from ascii_chafa import ascii_art, cells
from ascii_theme import LINEAR
from ascii_term import Term, font_path

TERM = Term(THEME)


def reference_cell_light(rgb, width, height, cols, rows):
    """cell_light by plain numpy block reductions, float64."""
    fine = np.asarray(Image.frombytes("RGB", (width, height), rgb).resize((cols * 4, rows * 4), Image.BOX))
    lin = LINEAR[fine] @ np.array([0.2126, 0.7152, 0.0722])
    blocks = lin.reshape(rows, 4, cols, 4)
    mean_rgb = fine.reshape(rows, 4, cols, 4, 3).mean(axis=(1, 3))
    return mean_rgb, blocks.mean(axis=(1, 3)), blocks.std(axis=(1, 3))


def grid_for(picture, cols):
    rgb = picture.tobytes()
    cw, ch, out = ascii_art(rgb, picture.width, picture.height, cols, None, 0.5)
    return cells(out, cw, ch, TERM.recolour.slots), rgb


class RampTest(unittest.TestCase):
    def test_sorted_from_space_to_densest(self):
        glyphs, cover = density.ramp(font_path())
        self.assertEqual(len(glyphs), 95)
        self.assertEqual(glyphs[0], ord(" "))
        self.assertEqual(cover[0], 0)
        self.assertEqual(cover[-1], 1)
        self.assertTrue((np.diff(cover) >= 0).all())


class CellLightTest(unittest.TestCase):
    def test_matches_reference(self):
        rng = np.random.default_rng(5)
        for picture in (rng.integers(0, 256, (270, 480, 3), np.uint8), np.asarray(gradient(480, 270))):
            h, w = picture.shape[:2]
            for cols, rows in ((120, 34), (37, 11)):
                got = density.cell_light(picture.tobytes(), w, h, cols, rows)
                want = reference_cell_light(picture.tobytes(), w, h, cols, rows)
                self.assertLessEqual(np.abs(got[0] - want[0]).max(), 1)  # Pillow's fixed-point box filter
                np.testing.assert_allclose(got[1], want[1], atol=1e-4)
                np.testing.assert_allclose(got[2], want[2], atol=2e-3)


class ApplyTest(unittest.TestCase):
    def apply(self, grid, rgb, picture, mode, mono=False):
        return density.apply(grid, rgb, picture.width, picture.height, mode, font_path(),
                             TERM.recolour.slots, TERM.luma_mono if mono else TERM.luma, mono=mono)

    def test_shape_leaves_grid(self):
        picture = gradient()
        grid, rgb = grid_for(picture, 60)
        self.assertIs(self.apply(grid, rgb, picture, "shape"), grid)

    def test_density_follows_brightness(self):
        picture = gradient(640, 160)
        grid, rgb = grid_for(picture, 80)
        out = self.apply(grid, rgb, picture, "density")
        glyphs, cover = density.ramp(font_path())
        coverage = dict(zip(glyphs.tolist(), cover.tolist()))
        # a cell lights about coverage x its colour's luminance: brighter greys take sparser glyphs
        cover_of = np.vectorize(coverage.get)(out.chars)
        light = (cover_of * np.asarray(TERM.luma)[out.slots] * (out.slots != 0)).mean(0)
        want = density.cell_light(rgb, picture.width, picture.height, *out.chars.shape[::-1])[1].mean(0)
        self.assertGreater(np.corrcoef(want, light)[0, 1], 0.98)

    def test_mixed_keeps_edges(self):
        a = np.zeros((160, 320, 3), np.uint8)
        a[:, 162:] = 255  # an edge inside a cell (8 px a cell at 40 columns)
        picture = Image.fromarray(a)
        grid, rgb = grid_for(picture, 40)
        out = self.apply(grid, rgb, picture, "mixed")
        np.testing.assert_array_equal(out.chars[:, 20], grid.chars[:, 20])

    def test_mono_keeps_slots(self):
        picture = gradient()
        grid, rgb = grid_for(picture, 60)
        out = self.apply(grid, rgb, picture, "density", mono=True)
        np.testing.assert_array_equal(out.slots, grid.slots)


if __name__ == "__main__":
    unittest.main()
