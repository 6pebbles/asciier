import tempfile
import unittest
import warnings
from pathlib import Path
from unittest import mock
from unittest import mock

import support  # noqa: F401  (lib/ on the path)
import ascii_image as I
from PIL import Image

SQUARE = '<svg xmlns="http://www.w3.org/2000/svg" width="100" height="50"><rect x="25" y="10" width="50" height="30"/></svg>'


class OpenPictureTest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.dir = Path(self.tmp.name)

    def tearDown(self):
        self.tmp.cleanup()

    def test_alpha_kept(self):
        src = self.dir / "clear.png"
        Image.new("RGBA", (40, 30), (0, 0, 0, 0)).save(src)
        picture = I.open_picture(src)
        self.assertEqual((picture.mode, picture.size), ("RGBA", (40, 30)))
        self.assertEqual(picture.getpixel((1, 1)), (0, 0, 0, 0))

    def test_palette_transparency_kept(self):
        src = self.dir / "pal.png"
        im = Image.new("P", (10, 10), 0)
        im.putpalette([0, 0, 0, 255, 255, 255])
        im.info["transparency"] = 0
        im.save(src, transparency=0)
        self.assertEqual(I.open_picture(src).mode, "RGBA")

    def test_opaque_is_rgb(self):
        rgb, rgba, grey = self.dir / "a.png", self.dir / "b.png", self.dir / "c.png"
        Image.new("RGB", (10, 10), (9, 8, 7)).save(rgb)
        Image.new("RGBA", (10, 10), (9, 8, 7, 255)).save(rgba)  # an alpha band with nothing transparent in it
        Image.new("L", (10, 10), 5).save(grey)
        for src in (rgb, rgba, grey):
            self.assertEqual(I.open_picture(src).mode, "RGB", src.name)

    def test_svg(self):
        clear, solid = self.dir / "clear.svg", self.dir / "solid.svg"
        clear.write_text(SQUARE)
        solid.write_text(SQUARE.replace("<rect", '<rect width="100" height="50" fill="white"/><rect'))
        picture = I.open_picture(clear)
        self.assertEqual((picture.mode, picture.width), ("RGBA", 1600))
        self.assertEqual(picture.getpixel((5, 5))[3], 0)
        self.assertEqual(picture.getpixel((800, 400)), (0, 0, 0, 255))
        self.assertEqual(I.open_picture(solid).mode, "RGB")
        self.assertEqual(I.open_picture(clear, 300).width, 300)
        self.assertEqual((I.raster_width(500, (100, 50)), I.raster_width(10**6, (100, 50)), I.raster_width(10**6, (50, 100))),
                         (500, I.SVG_MAX, I.SVG_MAX // 2))  # the long side at most SVG_MAX

    def test_svg_part(self):
        src = self.dir / "a b#1 &'\".svg"  # every name goes into the wrapper's href
        src.write_text(SQUARE)  # the rect: x 25..75, y 10..40 of 100 x 50
        alpha = lambda image: image.getchannel("A").getextrema()
        rect = I.svg_part(src, (100, 50), 0, (25, 10, 75, 40), (500, 300))
        self.assertEqual((rect.size, alpha(rect)), ((500, 300), (255, 255)))
        self.assertEqual(alpha(I.svg_part(src, (100, 50), 0, (0, 0, 20, 50), (40, 100))), (0, 0))
        # turned a quarter right: (x, y) -> (50 - y, x), the rect at x 10..40, y 25..75 of 50 x 100
        self.assertEqual(alpha(I.svg_part(src, (100, 50), 1, (10, 25, 40, 75), (30, 50))), (255, 255))
        self.assertEqual(alpha(I.svg_part(src, (100, 50), 1, (0, 0, 50, 20), (50, 20))), (0, 0))
        self.assertEqual(alpha(I.svg_part(src, (100, 50), 2, (25, 10, 75, 40), (50, 30))), (255, 255))
        self.assertEqual(alpha(I.svg_part(src, (100, 50), 3, (10, 25, 40, 75), (30, 50))), (255, 255))
        half = I.svg_part(src, (100, 50), 0, (0, 0, 50, 50), (50, 50))  # the rect's left half at x 25..50
        self.assertEqual((alpha(half.crop((0, 0, 24, 50))), alpha(half.crop((26, 11, 50, 39)))), ((0, 0), (255, 255)))

    def test_flatten_over_grey(self):
        im = Image.new("RGBA", (4, 4), (0, 0, 0, 0))
        im.putpixel((0, 0), (0, 0, 0, 255))
        flat = I.flatten(im, (128, 128, 128))
        self.assertEqual((flat.mode, flat.getpixel((0, 0)), flat.getpixel((3, 3))), ("RGB", (0, 0, 0), (128, 128, 128)))
        self.assertEqual(I.flatten(Image.new("L", (2, 2), 7), "#000000").mode, "RGB")

    def test_exif_orientation_applied_without_a_copy_otherwise(self):
        cam, plain = self.dir / "cam.jpg", self.dir / "plain.jpg"
        exif = Image.Exif()
        exif[0x0112] = 6  # stored turned left: shown a quarter right
        Image.new("RGB", (400, 200)).save(cam, exif=exif)
        Image.new("RGB", (400, 200), (10, 20, 30)).save(plain)
        self.assertEqual(I.open_picture(cam).size, (200, 400))
        with mock.patch.object(Image.Image, "copy", autospec=True, side_effect=Image.Image.copy) as copy, \
                mock.patch.object(Image.Image, "transpose", autospec=True, side_effect=Image.Image.transpose) as turn:
            self.assertEqual(I.open_picture(plain).size, (400, 200))
        self.assertEqual((copy.call_count, turn.call_count), (0, 0))  # the image as loaded, no copy of it

    def test_animation_and_non_images_give_none(self):
        gif, text = self.dir / "a.gif", self.dir / "x.png"
        frames = [Image.new("RGB", (8, 8), (80 * i, 0, 0)) for i in range(3)]
        frames[0].save(gif, save_all=True, append_images=frames[1:])
        text.write_bytes(b"not a picture")
        self.assertIsNone(I.open_picture(gif))
        self.assertIsNone(I.open_picture(text))

    def test_flatten_over_an_image(self):
        im = Image.new("RGBA", (2, 1), (0, 0, 0, 0))
        im.putpixel((0, 0), (255, 0, 0, 255))
        flat = I.flatten(im, Image.new("RGB", (2, 1), (1, 2, 3)))
        self.assertEqual((flat.getpixel((0, 0)), flat.getpixel((1, 0))), ((255, 0, 0), (1, 2, 3)))



class TooLargeTest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.big = Path(self.tmp.name, "big.png")
        Image.new("RGB", (50, 50)).save(self.big)  # 2500 px: over twice a limit of 1000

    def test_over_the_limit_is_too_large_not_unopenable(self):
        with mock.patch.object(Image, "MAX_IMAGE_PIXELS", 1000):
            with self.assertRaises(I.TooLarge) as caught:
                I.open_picture(self.big)
            self.assertTrue(I.identifies(self.big))  # an image all the same: stepping lands on it and says so
        self.assertIn("big.png is too large to open", str(caught.exception))

    def test_under_twice_the_limit_opens_without_a_warning(self):
        with mock.patch.object(Image, "MAX_IMAGE_PIXELS", 2000), warnings.catch_warnings():
            warnings.simplefilter("error")  # a warning would print over the viewer's screen
            warnings.filterwarnings("ignore", category=Image.DecompressionBombWarning)  # as ascii_image sets it
            self.assertEqual(I.open_picture(self.big).size, (50, 50))
        self.assertTrue(any(f[0] == "ignore" and f[2] is Image.DecompressionBombWarning for f in warnings.filters))


if __name__ == "__main__":
    unittest.main()
