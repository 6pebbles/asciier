"""Still images for the ascii tools (ascii-viewer, ascii-convert): opened upright by their EXIF
orientation, an SVG rasterized by resvg, transparency kept or put over a background."""

import io
import subprocess
import warnings
from pathlib import Path
from xml.sax.saxutils import quoteattr

from PIL import Image, ImageOps

SVG_MAX = 8192  # px on an SVG raster's long side, at most
# Pillow's decompression-bomb guard is for untrusted uploads; these tools open the user's own
# files. Its warning (over MAX_IMAGE_PIXELS) would print over the viewer's screen; its error
# (over twice that) becomes TooLarge
warnings.filterwarnings("ignore", category=Image.DecompressionBombWarning)


class TooLarge(Exception):
    """A still image over Pillow's pixel limit: not opened, so a huge file cannot take gigabytes."""

    def __init__(self, path):
        super().__init__(f"{Path(path).name} is too large to open (over {2 * Image.MAX_IMAGE_PIXELS / 1e6:.0f} megapixels)")


def is_svg(path):
    return Path(path).suffix.lower() == ".svg"


def raster_width(want, size):
    """The width to rasterize an SVG of size (w, h) at for want px across: want, as long as its
    long side stays within SVG_MAX."""
    return min(want, max(1, SVG_MAX * size[0] // max(size)))


def resvg(args, document=b""):
    """resvg's PNG for args (the input among them; "-" reads document) as an Image; OSError
    with resvg's message when it fails."""
    r = subprocess.run(["resvg", *args, "-c"], input=document, capture_output=True)
    if r.returncode:
        raise OSError(r.stderr.decode(errors="replace").strip())
    return Image.open(io.BytesIO(r.stdout))


def svg_part(path, logical, rotation, box, size):
    """The box (left, top, right, bottom) of the SVG at path, drawn logical (w, h) px and turned
    rotation quarters right, rasterized alone at size (w, h): only that part is drawn, so the
    cost follows size, not the zoom. RGBA; OSError when resvg fails."""
    path = Path(path).resolve()
    w, h = logical
    shift = ((0, 0), (h, 0), (w, h), (0, w))[rotation]  # the turned picture back over the view
    left, top, right, bottom = box
    document = (
        f'<svg xmlns="http://www.w3.org/2000/svg" width="{size[0]}" height="{size[1]}" '
        f'viewBox="{left} {top} {right - left} {bottom - top}" preserveAspectRatio="none">'
        f'<image href={quoteattr(str(path))} width="{w}" height="{h}" preserveAspectRatio="none" '
        f'transform="translate({shift[0]} {shift[1]}) rotate({90 * rotation})"/></svg>')
    return resvg(["--resources-dir", str(path.parent), "-"], document.encode())


def is_animation(image):
    """Whether an opened image has several frames to play: an animated GIF, WebP, APNG. MPO, a
    camera JPEG with extra pictures in it (previews, stereo pairs), is a still."""
    return image.format != "MPO" and getattr(image, "n_frames", 1) > 1


def identifies(path):
    """Whether path holds a still image by its header alone: an SVG, or a file Pillow identifies
    that is not an animation. open_picture()'s test without the decode, so a file damaged past
    its header, or of a format Pillow identifies but cannot decode, passes here and fails there."""
    if is_svg(path):
        return True
    try:
        with Image.open(path) as im:
            return not is_animation(im)
    except Image.DecompressionBombError:  # an image: open_picture says it is too large
        return True
    except (OSError, ValueError):
        return False


def upright(path, svg_width=None):
    """A still image as stored, in its own mode, upright by its EXIF orientation; None for an
    animation. An SVG is rasterized by resvg (Pillow cannot open one), svg_width px wide or at
    the size it declares. Raises TooLarge over Pillow's pixel limit, OSError or ValueError for
    anything else (video, audio)."""
    try:
        if is_svg(path):
            return resvg([*(["-w", str(svg_width)] if svg_width else []), str(path)])
        with Image.open(path) as im:
            if is_animation(im):
                return None
            ImageOps.exif_transpose(im, in_place=True)  # loads it; without an orientation, no copy
            return im
    except Image.DecompressionBombError:
        raise TooLarge(path) from None


def open_picture(path, svg_width=1600):
    """A still image, upright, as RGB, or RGBA when it has transparency (an SVG svg_width px
    wide); None for anything else (video, audio, an animation, a file that does not open).
    Raises TooLarge over Pillow's pixel limit."""
    try:
        picture = upright(path, svg_width)
    except (OSError, ValueError):
        return None
    if picture is None:
        return None
    if picture.has_transparency_data:  # alpha band, palette or colour-key transparency
        picture = picture if picture.mode == "RGBA" else picture.convert("RGBA")
        if picture.getchannel("A").getextrema()[0] < 255:
            return picture
    return picture if picture.mode == "RGB" else picture.convert("RGB")


def flatten(image, background):
    """image as RGB, its transparency over background: a colour, or an RGB image of its size,
    which is changed."""
    if image.mode != "RGBA":
        return image if image.mode == "RGB" else image.convert("RGB")
    if not isinstance(background, Image.Image):
        background = Image.new("RGB", image.size, background)
    background.paste(image, (0, 0), image)
    return background
