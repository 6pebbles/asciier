"""Theme colours for the ascii tools (ascii-viewer, ascii-convert), the tone controls
(tone_table) and the look settings' LIMITS.

chafa's own 16-colour mode picks each cell's colour against xterm's stock palette, and the
terminal then shows that slot in the theme's colour: a green chafa calls "yellow" (slot 3)
comes out as the theme's orange. So the tools ask chafa for truecolour instead and recolour
here: each cell's RGB goes to one of the theme's own 16 slots (Recolour), which ascii-viewer
writes as that slot's plain ANSI code (ascii_term) and ascii-convert paints in the slot's
colour. The terminal still draws the theme's colours, now chosen by hue.
Hue first, because in ASCII the glyph already carries the brightness: a coloured cell (Oklab
chroma over CHROMA) takes the theme colour nearest in hue, the lighter or darker of a pair by
lightness; a near-grey cell takes the nearest of the theme's greys. Plain nearest-colour would
send a dull green field to the theme's slate grey, the closest in lightness.
"""

import contextlib
import math
import re
from pathlib import Path

import numpy as np

CHROMA = 0.035  # Oklab chroma under which a colour counts as grey (pixels and theme slots alike), at saturation 100
LIGHTNESS = 1.5  # weight of a lightness gap against a hue gap (radians), between coloured slots
# Skin: Oklab hues 20 to 75 degrees with chroma up to about 0.09 cover skin from very light to
# very dark (lips reach 0.12). A warm hue there would take the theme's orange or red slot, far
# more saturated (bl1nk's orange is 0.16, its red 0.21): skin would come out orange or red. In
# that hue band a colour needs WARM_CHROMA to take a hue, under it a grey. Grass and sky have
# skin's chroma but other hues, so they keep theirs; vivid oranges and bricks still pass.
WARM = (20, 75)  # degrees
WARM_CHROMA = 0.10

LIMITS = {"level": (-95, 95), "contrast": (-95, 95), "saturation": (0, 300)}  # the look's settings, percent


def clamp(setting, value):
    """value within the LIMITS of setting."""
    low, high = LIMITS[setting]
    return min(max(value, low), high)


THEME = Path("~/.config/kitty/current-theme.conf").expanduser()
KITTY_THEMES = Path("~/.cache/kitty/kitty-themes.zip").expanduser()
COLOURS = re.compile(r"^(foreground|background|color\d+)\s+(#[0-9A-Fa-f]{6})", re.M)
LINEAR = ((np.arange(256) / 255 + 0.055) / 1.055) ** 2.4  # sRGB byte -> linear light
LINEAR[:11] = np.arange(11) / 255 / 12.92  # sRGB's straight segment near black


def theme(text=None):
    """foreground, background and color0-15 of a kitty theme (the current one by default)."""
    return dict(COLOURS.findall(THEME.read_text() if text is None else text))


@contextlib.contextmanager
def kitty_theme_index():
    """kitty's own cache of its theme collection, open: yields (entries of its themes.json,
    read), read(entry) the entry's .conf text (KeyError for a file the zip lacks). Raises
    OSError, ValueError or KeyError for a cache that is missing or odd."""
    import json
    import zipfile

    with zipfile.ZipFile(KITTY_THEMES) as z:
        index = next((n for n in z.namelist() if n.endswith("/themes.json")), None)
        if index is None:
            raise KeyError("themes.json")
        root = index[: -len("themes.json")]
        yield json.loads(z.read(index)), lambda entry: z.read(root + entry["file"]).decode()


def rgb(colour):
    """(r, g, b) bytes of a "#rrggbb" colour."""
    return tuple(int(colour[i : i + 2], 16) for i in (1, 3, 5))


def luminance(colour):
    """Linear luminance of a "#rrggbb" colour: additive, ordered as perceived lightness."""
    r, g, b = (LINEAR[v] for v in rgb(colour))
    return 0.2126 * r + 0.7152 * g + 0.0722 * b


def oklab(rgb):
    """Oklab (L, a, b) of (n, 3) sRGB bytes, as an (n, 3) float array."""
    lin = LINEAR[np.asarray(rgb, np.uint8).reshape(-1, 3)]
    lms = lin @ np.array([[0.4122214708, 0.2119034982, 0.0883024619],
                          [0.5363325363, 0.6806995451, 0.2817188376],
                          [0.0514459929, 0.1073969566, 0.6299787005]])
    return lms ** (1 / 3) @ np.array([[0.2104542553, 1.9779984951, 0.0259040371],
                                      [0.7936177850, -2.4285922050, 0.7827717662],
                                      [-0.0040720468, 0.4505937099, -0.8086757660]])


class Recolour:
    """Each colour to the palette slot (0-15) of the theme colour it maps to, as above.
    saturation (percent, 100 as is) scales how much chroma a colour needs to take a hue: at 50
    it needs twice CHROMA, at 300 a third of it (near-greys take a hue too), at 0 everything is
    grey. skin keeps the skin band (WARM) grey below WARM_CHROMA, scaled the same way.
    Colours go by 6 bits a channel, each group mapped once from its centre, so the result
    depends on the colour alone, not on what came before it (in this process or another)."""

    def __init__(self, palette, saturation=100, skin=True):
        need = 100 / saturation if saturation > 0 else math.inf
        self.chroma, self.warm = CHROMA * need, (WARM_CHROMA * need if skin else CHROMA * need)
        self.lab = oklab([rgb(h) for h in palette])
        grey = np.hypot(self.lab[:, 1], self.lab[:, 2]) < CHROMA
        every = np.arange(len(palette))
        self.greys = np.flatnonzero(grey) if grey.any() else every
        self.hues = np.flatnonzero(~grey) if (~grey).any() else every
        self.lut = np.full(1 << 18, -1, np.int8)

    def pick(self, colours):
        """The slots of (n, 3) uint8 colours, worked out (slots() keeps them)."""
        L, a, b = oklab(colours).T
        hue = np.arctan2(b, a)
        warm = (WARM[0] <= np.degrees(hue) % 360) & (np.degrees(hue) % 360 <= WARM[1])
        grey = np.hypot(a, b) < np.where(warm, self.warm, self.chroma)
        lab = self.lab
        nearest_grey = self.greys[np.argmin((lab[self.greys, 0][None] - L[:, None]) ** 2, axis=1)]
        gap = np.abs(np.arctan2(lab[self.hues, 2], lab[self.hues, 1])[None] - hue[:, None])
        cost = np.minimum(gap, 2 * math.pi - gap) ** 2 + LIGHTNESS * (lab[self.hues, 0][None] - L[:, None]) ** 2
        return np.where(grey, nearest_grey, self.hues[np.argmin(cost, axis=1)])

    def slots(self, colours):
        """Palette slots (0-15, uint8) for (n, 3) uint8 colours, from a table by 6-bit key."""
        colours = np.asarray(colours, np.uint8).reshape(-1, 3)
        key = (colours[:, 0].astype(np.int32) >> 2) << 12 | (colours[:, 1].astype(np.int32) >> 2) << 6 | colours[:, 2] >> 2
        slots = self.lut[key]
        if (slots < 0).any():
            new = np.unique(key[slots < 0])
            centre = ((new[:, None] >> np.array([12, 6, 0])) & 63) * 4 + 2
            self.lut[new] = self.pick(centre.astype(np.uint8))
            slots = self.lut[key]
        return slots.astype(np.uint8)


def tone_table(level=0, contrast=0):
    """Byte map for the ascii tools' tone controls, both percent from -95 to 95; None when both
    are 0, so the picture passes untouched.
    The level is a levels slider both ways. Up from 0 the black point rises: channels under it
    go to black, so dark areas drop out to blank, and the rest stretches back to full range.
    Down from 0 the white point falls: channels over it go to white and the rest stretches up,
    brightening the picture.
    The contrast then scales every channel's distance from mid grey, as mpv's contrast does:
    up spreads the tones apart, down pulls them together towards grey."""
    if not level and not contrast:
        return None
    if level > 0:
        cut = 255 * level / 100
        tones = [0 if v < cut else (v - cut) * 255 / (255 - cut) for v in range(256)]
    elif level < 0:
        top = 255 * (1 + level / 100)
        tones = [255 if v >= top else v * 255 / top for v in range(256)]
    else:
        tones = list(range(256))
    gain = 1 + contrast / 100
    return bytes(min(255, max(0, round(127.5 + (t - 127.5) * gain))) for t in tones)


MONO_SPAN = 0.05  # Oklab lightness under the lightest colour that mono still takes, for a less colourful one


def brightest(pool):
    """Mono's colour of pool ("#rrggbb" colours): of those within MONO_SPAN Oklab lightness of
    the lightest, the least colourful (ties: the lighter). Mono keeps nearly the full range, and
    takes the near white just under a vivid yellow, never a theme's dark grey."""
    lab = oklab([rgb(c) for c in pool])
    chroma = np.hypot(lab[:, 1], lab[:, 2])
    near = np.flatnonzero(lab[:, 0] >= lab[:, 0].max() - MONO_SPAN)
    return pool[min(near, key=lambda i: (round(chroma[i], 3), -lab[i, 0]))]


def extremes(colours):
    """(darkest, brightest) of a theme's colours: background, foreground and color0-15. Mono
    draws with these, so it keeps the theme's full range: with bl1nk the plain foreground is a
    mid grey, the brightest is color15's near white."""
    pool = [v for k, v in colours.items() if k in ("background", "foreground") or k.startswith("color")]
    return min(pool, key=luminance), brightest(pool)


def brightest_slot(colours):
    """brightest() of color0-15 and the foreground (16), as a slot: the viewer draws mono in it."""
    pool = [colours[f"color{i}"] for i in range(16)] + [colours["foreground"]]
    return pool.index(brightest(pool))
