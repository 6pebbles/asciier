import unittest

import numpy as np
from support import THEME

import math

from ascii_theme import LIGHTNESS, WARM, Recolour, brightest_slot, extremes, oklab, rgb, theme, tone_table

PALETTE = [THEME[f"color{i}"] for i in range(16)]


def grey_slots(recolour):
    """The palette slots Recolour counts as grey (bl1nk: 0, 6, 7, 8, 14, 15)."""
    return set(recolour.greys.tolist())


def reference_slots(recolour, colours):
    """Recolour's rule one colour at a time, in plain Python (only Oklab in one go): the nearest
    grey by lightness under the chroma needed, else the nearest hue (lightness weighted by
    LIGHTNESS)."""
    lab, greys, hues = recolour.lab.tolist(), recolour.greys.tolist(), recolour.hues.tolist()
    out = []
    for L, a, b in oklab(colours).tolist():
        hue = math.atan2(b, a)
        need = recolour.warm if WARM[0] <= math.degrees(hue) % 360 <= WARM[1] else recolour.chroma
        if math.hypot(a, b) < need:
            out.append(min(greys, key=lambda i: (lab[i][0] - L) ** 2))
            continue

        def cost(i):
            gap = abs(math.atan2(lab[i][2], lab[i][1]) - hue)
            return min(gap, 2 * math.pi - gap) ** 2 + LIGHTNESS * (lab[i][0] - L) ** 2

        out.append(min(hues, key=cost))
    return out


class ThemeTest(unittest.TestCase):
    def test_parses_kitty_theme(self):
        self.assertEqual(len(THEME), 18)
        self.assertEqual(THEME["color0"], "#1A1C1D")
        self.assertEqual(theme("color3 #abcdef\nfoo #000000\n"), {"color3": "#abcdef"})

    def test_extremes_and_brightest(self):
        self.assertEqual(extremes(THEME), ("#111111", "#F8F8F2"))
        self.assertEqual(brightest_slot(THEME), 15)
        vivid = {**THEME, "color11": "#ffff00", "color15": "#eeeeee"}  # yellow is brighter, but mono stays grey
        self.assertEqual((brightest_slot(vivid), extremes(vivid)[1]), (15, "#eeeeee"))
        no_grey = {k: v for k, v in THEME.items()} | {f"color{i}": "#3355ff" for i in range(16)} | {"foreground": "#ffcc00", "background": "#0000aa"}
        self.assertEqual((brightest_slot(no_grey), extremes(no_grey)[1]), (16, "#ffcc00"))  # nothing as light: the lightest
        dark_grey = {**no_grey, "color0": "#000000"}  # its only grey is black: mono must not take it
        self.assertEqual(brightest_slot(dark_grey), 16)
        self.assertEqual(brightest_slot({**THEME, "foreground": "#FFFFFF"}), 16)
        self.assertEqual(rgb("#0a10Ff"), (10, 16, 255))


class ToneTableTest(unittest.TestCase):
    def test_neutral_is_none(self):
        self.assertIsNone(tone_table(0, 0))

    def test_level_up_raises_black_point(self):
        t = tone_table(40, 0)
        cut = round(255 * 0.4)
        self.assertEqual(len(t), 256)
        self.assertTrue(all(v == 0 for v in t[: cut - 1]))
        self.assertEqual(t[255], 255)
        self.assertEqual(list(t), sorted(t))

    def test_level_down_lowers_white_point(self):
        t = tone_table(-40, 0)
        self.assertEqual(t[0], 0)
        self.assertTrue(all(v == 255 for v in t[round(255 * 0.6) + 1 :]))

    def test_contrast_spreads_from_mid_grey(self):
        up, down = tone_table(0, 50), tone_table(0, -50)
        self.assertLess(up[64], 64)
        self.assertGreater(up[192], 192)
        self.assertGreater(down[64], 64)
        self.assertLess(down[192], 192)


class RecolourTest(unittest.TestCase):
    def slot(self, recolour, colour):
        return int(recolour.slots([colour])[0])

    def test_greys_take_grey_slots(self):
        r = Recolour(PALETTE)
        greys = grey_slots(r)
        for v in (20, 90, 160, 250):
            self.assertIn(self.slot(r, (v, v, v)), greys)

    def test_hue_wins_over_lightness(self):
        r = Recolour(PALETTE)
        self.assertIn(self.slot(r, (0, 200, 0)), (2, 10))  # green
        self.assertIn(self.slot(r, (40, 60, 220)), (4, 12))  # blue
        self.assertIn(self.slot(r, (60, 110, 40)), (2, 10))  # a dull green stays green

    def test_skin_goes_grey_unless_vivid(self):
        skin = (224, 172, 140)
        self.assertIn(self.slot(Recolour(PALETTE), skin), grey_slots(Recolour(PALETTE)))
        self.assertIn(self.slot(Recolour(PALETTE, skin=False), skin), (1, 3, 9, 11))

    def test_zero_saturation_is_all_grey(self):
        r = Recolour(PALETTE, saturation=0)
        for rgb in ((255, 0, 0), (0, 255, 0), (0, 0, 255)):
            self.assertIn(self.slot(r, rgb), grey_slots(r))

    def test_every_key_follows_the_rule_from_its_centre(self):
        key = np.arange(1 << 18)
        centre = (((key[:, None] >> np.array([12, 6, 0])) & 63) * 4 + 2).astype(np.uint8)
        for r in (Recolour(PALETTE), Recolour(PALETTE, 40, skin=False), Recolour(PALETTE, 300)):
            self.assertEqual(r.slots(centre).tolist(), reference_slots(r, centre))  # all 262144 keys, about 1 s each

    def test_slots_do_not_depend_on_order(self):
        rng = np.random.default_rng(3)
        colours = rng.integers(0, 256, (5000, 3), np.uint8)
        twins = (colours & 0b11111100) | rng.integers(0, 4, colours.shape, np.uint8)  # each in its colour's 6-bit group
        both = np.concatenate([colours, twins])
        forward, backward = Recolour(PALETTE), Recolour(PALETTE)
        backward.slots(both[::-1])  # the twins first: a group would keep the slot of whichever came first
        self.assertEqual(forward.slots(both).tolist(), backward.slots(both).tolist())
        centres = (both & 0b11111100) | 2
        self.assertEqual(forward.slots(both).tolist(), Recolour(PALETTE).slots(centres).tolist())


if __name__ == "__main__":
    unittest.main()
