import contextlib
import io
import json
import os
import re
import shutil
import signal
import subprocess
import sys
import tempfile
import threading
import time
import unittest
from pathlib import Path
from unittest import mock

import support
from support import ROOT, gradient, script, testsrc
from PIL import Image

C = script("ascii-convert")
BL1NK = ROOT / "tests" / "bl1nk.conf"

# ffmpeg as ascii-convert runs it, first on PATH: the video decode's PPM stream is kept in
# $DUMP/decode.ppm and the encoder's input in $DUMP/encode.raw. With $FAIL_AFTER the encoder
# reads that many bytes and quits with an error; with $HOLD it takes $HOLD frames of $FRAME
# bytes, then reads no more until $DUMP/go exists
FFMPEG = """#!/bin/sh
case " $* " in
*" rawvideo "*)
    if [ -n "$FAIL_AFTER" ]; then head -c "$FAIL_AFTER" > /dev/null; exit 1; fi
    if [ -n "$HOLD" ]; then
        { dd bs="$FRAME" count="$HOLD" iflag=fullblock status=none
          while [ ! -e "$DUMP/go" ]; do sleep 0.05; done
          cat; } | tee "$DUMP/encode.raw" | "$REAL" "$@"
    else
        tee "$DUMP/encode.raw" | "$REAL" "$@"
    fi ;;
*" -an "*) "$REAL" "$@" | tee "$DUMP/decode.ppm" ;;
*) exec "$REAL" "$@" ;;
esac
"""


def args(*flags):
    return C.parser.parse_args([*flags, "in.png"])


def streams(path):
    out = subprocess.run(["ffprobe", "-v", "error", "-show_entries", "stream=codec_type,width,height", "-of", "json",
                          str(path)], capture_output=True, text=True, check=True).stdout
    return json.loads(out)["streams"]


class GeometryTest(unittest.TestCase):
    def test_crop_rect(self):
        self.assertEqual(C.crop_rect((200, 100), C.FULL), (0, 0, 200, 100))
        self.assertEqual(C.crop_rect((200, 100), (-10, 50, 50, 100)), (-20, 50, 80, 150))
        self.assertEqual(C.crop_rect((200, 100), (0, 0, 0.1, 0.1)), (0, 0, 1, 1))

    def test_crop_filter(self):
        self.assertEqual(C.crop_filter(args(), (200, 100)), "")
        self.assertEqual(C.crop_filter(args("--crop=-10,0,120,100"), (200, 100)),
                         "pad=240:100:20:0:black,crop=240:100:0:0,")

    def test_crop_past_borders_is_black(self):
        out = C.crop(Image.new("RGB", (100, 100), "white"), args("--crop", "50,0,100,100"))
        self.assertEqual(out.size, (100, 100))
        self.assertEqual(out.getpixel((99, 50)), (0, 0, 0))

    def test_web_size(self):
        self.assertEqual(C.web_size((1920, 1080)), (1920, 1080))
        self.assertEqual(C.web_size((1080, 1920)), (1080, 1920))
        self.assertEqual(C.web_size((500, 500)), (1920, 1920))
        self.assertEqual(C.web_size((1000, 333)), (1920, 640))


class OptionsTest(unittest.TestCase):
    def test_long_names(self):
        a = args("--level", "-20", "--contrast", "10", "--saturation", "50", "--skin", "theme", "--glyphs", "shape",
                 "--mono", "--bold", "--crop", "10,10,50,50", "--columns", "90", "--fit", "cover", "--at", "1.5")
        self.assertEqual((a.level, a.contrast, a.saturation, a.skin, a.glyphs), (-20, 10, 50, "theme", "shape"))
        self.assertEqual((a.mono, a.bold, a.crop, a.columns, a.fit, a.at), (True, True, (10, 10, 50, 50), 90, "cover", 1.5))

    def test_defaults(self):
        a = args()
        self.assertEqual((a.level, a.contrast, a.saturation, a.skin, a.glyphs, a.crop), (0, 0, 100, "grey", "mixed", C.FULL))

    def test_rejects_bad_values(self):
        for flags in (["--level", "96"], ["--saturation", "-1"], ["--crop", "1,2,3"], ["--crop", "0,0,0,10"],
                      ["--columns", "x"], ["--at", "-1"], ["-W", "4"]):
            with self.subTest(flags=flags), contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit):
                args(*flags)


class StyleTest(unittest.TestCase):
    picture = gradient(400, 300)

    def painted(self, a):
        style = C.Style(a, self.picture.size)
        grid = style.art(self.picture)
        style.layout(len(grid))
        return style, style.paint(grid)

    def test_default_is_160_columns(self):
        style, image = self.painted(args())
        self.assertEqual(style.cols, 160)
        self.assertEqual(image.size, style.size())
        self.assertEqual(image.width, round(160 * style.cell_w))

    def test_columns_in_exact_size(self):
        for fit in ("contain", "cover"):
            style, image = self.painted(args("-c", "100", "-W", "1920", "-H", "1080", "--fit", fit))
            self.assertEqual(image.size, (1920, 1080), fit)
            self.assertEqual(style.cols, 100)

    def test_width_alone_sets_columns(self):
        style, image = self.painted(args("-W", "1000"))
        self.assertEqual(image.width, 1000)
        self.assertGreaterEqual(style.cols * style.cell_w, 1000)

    def test_even_size_for_video(self):
        style, _ = self.painted(args("-W", "1001", "-H", "501"))
        self.assertEqual(style.size(even=True), (1002, 502))


class EndToEndTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.tmp = tempfile.TemporaryDirectory()
        cls.dir = Path(cls.tmp.name)
        cls.clip = testsrc(cls.dir / "clip.mp4", 1, "160x90")

    @classmethod
    def tearDownClass(cls):
        cls.tmp.cleanup()

    def test_image(self):
        src = self.dir / "pic.png"
        gradient(400, 300).save(src)
        with mock.patch.object(sys, "argv", ["ascii-convert", "-c", "80", str(src)]), \
                contextlib.redirect_stdout(io.StringIO()) as out:
            C.main()
        self.assertEqual(out.getvalue().strip(), str(self.dir / "pic-ascii.png"))
        self.assertTrue((self.dir / "pic-ascii.png").exists())

    def test_output_path(self):
        src, out = self.dir / "pic2.png", self.dir / "named.png"
        gradient(200, 100).save(src)
        with mock.patch.object(sys, "argv", ["ascii-convert", "-o", str(out), str(src)]), \
                contextlib.redirect_stdout(io.StringIO()):
            C.main()
        self.assertTrue(out.exists())
        self.assertFalse((self.dir / "pic2-ascii.png").exists())

    def test_too_large_exits_with_the_reason(self):
        src = self.dir / "huge.png"
        Image.new("RGB", (50, 50)).save(src)
        with mock.patch.object(sys, "argv", ["ascii-convert", str(src)]), mock.patch.object(Image, "MAX_IMAGE_PIXELS", 1000), \
                self.assertRaises(SystemExit) as caught:
            C.main()
        self.assertIn("huge.png is too large to open", str(caught.exception.code))
        self.assertFalse((self.dir / "huge-ascii.mp4").exists())  # not taken for a video either

    def test_out_name(self):
        self.assertEqual(C.out_name("/d/a.b.png"), "/d/a.b-ascii.png")
        self.assertEqual(C.out_name("/d/v.mkv", video=True), "/d/v-ascii.mp4")
        self.assertEqual(C.out_name("/d/v.mkv", 1.5), "/d/v-ascii-1.5s.png")

    def test_svg_is_a_still(self):
        src, out = self.dir / "icon.svg", self.dir / "icon-out.png"
        src.write_text('<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24"><circle cx="12" cy="12" r="10"/></svg>')
        with mock.patch.object(sys, "argv", ["ascii-convert", "-o", str(out), str(src)]), \
                contextlib.redirect_stdout(io.StringIO()):
            C.main()
        with Image.open(out) as im:
            self.assertEqual((im.format, im.width), ("PNG", round(160 * C.Style(args(), (24, 24)).cell_w)))

    def converted(self, src, env=None):
        r = subprocess.run([str(ROOT / "bin" / "ascii-convert"), "-c", "40", str(src)], capture_output=True, text=True,
                           env=env, stdin=subprocess.DEVNULL)
        self.assertEqual(r.returncode, 0, r.stderr)
        return Path(r.stdout.strip())

    def duration(self, path, stream):
        return float(subprocess.run(["ffprobe", "-v", "error", "-select_streams", stream, "-show_entries", "stream=duration",
                                     "-of", "csv=p=0", str(path)], capture_output=True, text=True, check=True).stdout)

    def test_variable_frame_rate_keeps_its_speed(self):
        src = self.dir / "vfr.mp4"
        # 1 s at 30 fps, then 1 s with two frames of three dropped: r_frame_rate 30, avg_frame_rate 20
        support.ffmpeg(src, "-f", "lavfi", "-i", "testsrc2=d=2:r=30:s=160x90",
                       "-vf", "select='lt(t,1)+not(mod(n,3))'", "-fps_mode", "vfr", "-c:v", "libx264")
        out = self.converted(src)
        self.assertAlmostEqual(self.duration(out, "v:0"), self.duration(src, "v:0"), delta=0.2)  # was 2.7 s for 1.8

    def test_short_audio_is_padded_to_the_video(self):
        src = self.dir / "short.mp4"
        support.ffmpeg(src, "-f", "lavfi", "-i", "testsrc2=d=3:r=25:s=160x90", "-f", "lavfi", "-i", "sine=d=1",
                       "-c:v", "libx264", "-c:a", "aac")
        out = self.converted(src)
        self.assertAlmostEqual(self.duration(out, "v:0"), 3, delta=0.1)
        self.assertAlmostEqual(self.duration(out, "a:0"), 3, delta=0.15)

    def test_progress_counts_the_video_not_longer_audio(self):
        src = self.dir / "long-audio.mp4"
        support.ffmpeg(src, "-f", "lavfi", "-i", "testsrc2=d=3:r=25:s=160x90", "-f", "lavfi", "-i", "sine=d=4",
                       "-c:v", "libx264", "-c:a", "aac")
        r = subprocess.run([str(ROOT / "bin" / "ascii-convert"), "-c", "40", str(src)], capture_output=True, text=True,
                           stdin=subprocess.DEVNULL)
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(r.stderr.split()[-2:], ["75/75", "frames"])  # was 75/100: the file's 4 s
        self.assertAlmostEqual(self.duration(Path(r.stdout.strip()), "v:0"), 3, delta=0.1)

    def test_no_audio(self):
        src = self.dir / "mute.mp4"
        support.ffmpeg(src, "-f", "lavfi", "-i", "testsrc2=d=1:r=25:s=160x90", "-c:v", "libx264")
        self.assertEqual([s["codec_type"] for s in streams(self.converted(src))], ["video"])

    def test_vaapi_that_fails_falls_back_to_libx264(self):
        out = self.converted(self.clip, {**os.environ, "LIBVA_DRIVER_NAME": "bogus"})
        self.assertIn("video", [s["codec_type"] for s in streams(out)])

    def test_video(self):
        r = subprocess.run([str(ROOT / "bin" / "ascii-convert"), str(self.clip)], capture_output=True, text=True)
        self.assertEqual(r.returncode, 0, r.stderr)
        out = Path(r.stdout.strip())
        self.assertEqual(out, self.dir / "clip-ascii.mp4")
        kinds = {s["codec_type"]: s for s in streams(out)}
        self.assertEqual((kinds["video"]["width"], kinds["video"]["height"]), (1920, 1080))
        self.assertIn("audio", kinds)

    def test_preview_frame(self):
        r = subprocess.run([str(ROOT / "bin" / "ascii-convert"), "--at", "0.5", str(self.clip)],
                           capture_output=True, text=True, stdin=subprocess.DEVNULL)
        self.assertEqual(r.returncode, 0, r.stderr)
        with Image.open(r.stdout.strip()) as im:
            self.assertEqual(im.size, (1920, 1080))


class VideoPipelineTest(unittest.TestCase):
    """The video path's worker processes and threads, seen through the ffmpeg wrapper above."""

    FRAME = 1920 * 1080 * 4  # an encoder input frame: 160x90 sources come out 1920x1080, rgb0

    @classmethod
    def setUpClass(cls):
        cls.tmp = tempfile.TemporaryDirectory()
        cls.dir = Path(cls.tmp.name)
        wrapper = cls.dir / "bin" / "ffmpeg"
        wrapper.parent.mkdir()
        wrapper.write_text(FFMPEG)
        wrapper.chmod(0o755)
        cls.env = {**os.environ, "PATH": f"{wrapper.parent}:{os.environ['PATH']}", "REAL": shutil.which("ffmpeg"),
                   "DUMP": str(cls.dir)}

    @classmethod
    def tearDownClass(cls):
        cls.tmp.cleanup()

    def start(self, src, **env):
        """bin/ascii-convert on src, in a process group of its own: a test that fails part-way
        kills it and its workers rather than leave them running."""
        p = subprocess.Popen([str(ROOT / "bin" / "ascii-convert"), "-t", str(BL1NK), "-o", str(src.with_name(src.stem + "-out.mp4")),
                              str(src)], env={**self.env, **env}, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                             stderr=subprocess.PIPE, start_new_session=True)
        self.addCleanup(self.stop, p)
        return p

    @staticmethod
    def stop(p):
        if p.poll() is None:  # still running, so the group is still its own; TERM, not KILL: the
            os.killpg(p.pid, signal.SIGTERM)  # resource tracker ignores it and removes the semaphores
        p.communicate()

    def frames_in(self, path):
        return int(subprocess.run(["ffprobe", "-v", "error", "-count_frames", "-select_streams", "v:0", "-show_entries",
                                   "stream=nb_read_frames", "-of", "csv=p=0", str(path)],
                                  capture_output=True, text=True, check=True).stdout)

    def test_every_frame_is_the_serial_rendering(self):
        src = testsrc(self.dir / "clip.mp4", 1, "160x90")  # 25 frames
        p = self.start(src)
        out, err = p.communicate(timeout=120)
        self.assertEqual(p.returncode, 0, err)
        # the parent's own Style, as convert_video makes it, drawing each decoded frame in turn
        a = C.parser.parse_args(["-t", str(BL1NK), str(src)])
        first = C.frame_at(str(src), 0)
        a.width, a.height = C.web_size(first.size)
        style = C.Style(a, first.size)
        style.layout(len(style.art(first)))
        self.assertEqual(style.size(even=True), (1920, 1080))
        decoded = []
        with open(self.dir / "decode.ppm", "rb") as f:
            while (frame := C.ppm_frame(f)) is not None:
                decoded.append(frame)
        fed = (self.dir / "encode.raw").read_bytes()
        self.assertEqual(len(decoded), 25)
        self.assertEqual(len(fed), 25 * self.FRAME)
        for i, frame in enumerate(decoded):
            serial = style.paint_array(style.art(Image.frombytes("RGB", *frame)), even=True).tobytes()
            self.assertTrue(fed[i * self.FRAME : (i + 1) * self.FRAME] == serial, f"frame {i} differs")
        self.assertEqual(self.frames_in(Path(out.decode().strip())), 25)

    def test_ctrl_c_writes_the_frames_in_flight(self):
        src = testsrc(self.dir / "long.mp4", 10, "160x90")  # 250 frames
        (self.dir / "go").unlink(missing_ok=True)
        p = self.start(src, HOLD="10", FRAME=str(self.FRAME))
        release = threading.Timer(30, (self.dir / "go").touch)  # the encoder is never left holding
        release.start()
        self.addCleanup(release.cancel)
        # the encoder holds after 10 frames; the frames read from the decoder meanwhile are in flight
        err = b""
        while b"\r10/" not in err:
            chunk = os.read(p.stderr.fileno(), 4096)
            self.assertTrue(chunk, err)  # ended before
            err += chunk
        decoded, size, deadline = self.dir / "decode.ppm", -1, time.monotonic() + 10
        while size != decoded.stat().st_size and time.monotonic() < deadline:  # until the reading stops
            size = decoded.stat().st_size
            time.sleep(0.3)
        os.killpg(p.pid, signal.SIGINT)  # Ctrl-C as a terminal sends it, to the process group
        (self.dir / "go").touch()
        out, rest = p.communicate(timeout=60)
        err = (err + rest).decode()
        self.assertEqual(p.returncode, 130, err)
        done = int(re.search(r"stopped at frame (\d+): ", err)[1])
        # every frame read from the decoder is written but the one whose hand-over Ctrl-C cut; the
        # decode dump also holds up to two frames still in the 64 KB pipe (and one of slack here)
        read = size // (len(b"P6\n160 90\n255\n") + 160 * 90 * 3)
        self.assertTrue(read - 4 <= done < 250 and done > 10, (read, err))
        self.assertEqual(len((self.dir / "encode.raw").read_bytes()), done * self.FRAME)  # whole frames only
        self.assertEqual(self.frames_in(Path(out.decode().strip())), done)

    def test_encoder_that_quits_is_an_error_not_a_hang(self):
        src = testsrc(self.dir / "quit.mp4", 2, "160x90")
        p = self.start(src, FAIL_AFTER=str(self.FRAME * 3))
        err = p.communicate(timeout=60)[1].decode()
        self.assertEqual(p.returncode, 1, err)
        self.assertIn("ffmpeg could not encode the video", err)
        self.assertNotIn("Traceback", err)

    @unittest.skipUnless(os.path.exists(C.VAAPI), "no VAAPI render node")
    def test_vaapi_probe_is_at_the_output_size(self):
        # the probe used to encode 64x64, under the 128 px one iGPU's encoder takes: every video went to libx264
        def encodes(w, h):
            return subprocess.run(["ffmpeg", "-v", "error", "-nostdin", "-vaapi_device", C.VAAPI, "-f", "lavfi", "-i",
                                   f"color=s={w}x{h}:d=0.04", "-vf", "format=nv12,hwupload", "-c:v", "h264_vaapi", "-f", "null",
                                   "-"], capture_output=True).returncode == 0

        for size in ((64, 64), (1920, 1080)):
            self.assertEqual(C.vaapi(*size), encodes(*size), size)


if __name__ == "__main__":
    unittest.main()
