import contextlib
import io
import os
import re
import struct
import subprocess
import tempfile
import threading
import time
import unittest
from pathlib import Path
from unittest import mock

from support import testsrc, script
from PIL import Image

V = script("ascii-viewer")


class DecoderTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.tmp = tempfile.TemporaryDirectory()
        d = Path(cls.tmp.name)
        cls.plain = testsrc(d / "plain.mp4")
        # MPEG-TS starts its clock late (here 3 s plus the muxer's delay); mpv counts from there
        cls.late = testsrc(d / "late.ts", 2, "320x240", 25, "-output_ts_offset", "3")
        cls.big = testsrc(d / "big.mp4", 1, "1920x1080")

    @classmethod
    def tearDownClass(cls):
        cls.tmp.cleanup()

    def frames(self, path, fps, at=0.0, n=None, decoder=None):
        container, video, _, _ = V.probe(str(path))
        decoder = (decoder or V.Decoder)(container, video, fps)
        run, out = decoder.seek(at), []
        while (n is None or len(out) < n) and (frame := run.read()) is not None:
            out.append(frame)
        decoder.close()
        return out

    def times(self, path, at=0.0, n=30):
        """The media time of each frame read, counted as mpv counts: from the container's start."""
        return self.frames(path, 25, at, n, Timed)

    def test_frames_due_on_time(self):
        for path in (self.plain, self.late):
            first = self.times(path, n=1)[0]
            self.assertLess(first, 0.25, f"{path.name}: times count from the container's start")
            for i, t in enumerate(self.times(path)):
                due = i / 25
                # before the first frame's time, the first frame holds; then each comes when due
                self.assertAlmostEqual(t, max(due, first), delta=0.021, msg=f"{path.name} read {i}")

    def test_seek_lands_on_time(self):
        for path in (self.plain, self.late):
            self.assertAlmostEqual(self.times(path, at=1.0, n=1)[0], 1.0, delta=0.021, msg=path.name)

    def test_lower_rate_drops_frames(self):
        self.assertIn(len(self.frames(self.plain, 10)), (20, 21))

    def test_frames_fit_and_are_even(self):
        w, h, data = self.frames(self.big, 25, n=1)[0]
        self.assertEqual((w, h), (V.MAX_W, V.MAX_H))
        self.assertEqual(len(data), w * h * 3)

    def test_rotated_video_comes_upright(self):
        turned = subprocess.run(["ffmpeg", "-v", "error", "-y", "-display_rotation", "90", "-i", str(self.plain), "-c", "copy",
                                 str(Path(self.tmp.name, "turned.mp4"))], check=True)
        plain = self.frames(self.plain, 25, n=1)[0]
        w, h, data = self.frames(Path(self.tmp.name, "turned.mp4"), 25, n=1)[0]
        self.assertEqual((w, h), (plain[1], plain[0]))  # 320 x 240 stored, 240 x 320 shown
        picture = Image.frombytes("RGB", (w, h), data).rotate(-90, expand=True)  # turned back: the stored frame
        diff = sum(abs(a - b) for a, b in zip(picture.tobytes()[::97], plain[2][::97])) / len(plain[2][::97])
        self.assertLess(diff, 4)

    def test_probe(self):
        container, video, audio, length = V.probe(str(self.plain))
        self.assertEqual((V.play_rate(video), audio, round(length)), (25, True, 2))
        container.close()
        cover = Path(self.tmp.name, "cover.m4a")
        subprocess.run(["ffmpeg", "-v", "error", "-y", "-f", "lavfi", "-i", "sine=d=1", "-f", "lavfi", "-i", "color=s=32x32:d=1",
                        "-map", "0", "-map", "1", "-frames:v", "1", "-c:a", "aac", "-c:v", "png", "-disposition:v", "attached_pic",
                        str(cover)], check=True)
        container, video, audio, _ = V.probe(str(cover))
        self.assertEqual((video, audio), (None, True))  # the cover is no video to play
        container.close()
        with self.assertRaises(SystemExit):
            V.probe(str(Path(self.tmp.name, "missing.mp4")))


class Timed(V.Decoder):
    def picture(self, frame):
        return frame.time - self.start


class PlaybackTest(unittest.TestCase):
    """start, seek_to, frame_step and halt on a real clip (2 s at 25 fps), without mpv."""

    @classmethod
    def setUpClass(cls):
        cls.tmp = tempfile.TemporaryDirectory()
        cls.clip = testsrc(Path(cls.tmp.name, "clip.mp4"))

    @classmethod
    def tearDownClass(cls):
        cls.tmp.cleanup()

    def decoder(self):
        """A Decoder of the clip, and its frame rate."""
        container, video, _, _ = V.probe(str(self.clip))
        fps = V.play_rate(video)
        decoder = V.Decoder(container, video, fps)
        self.addCleanup(decoder.close)
        return decoder, fps

    def player(self):
        decoder, fps = self.decoder()
        return bare_player(path=str(self.clip), fps=fps, decoder=decoder)

    def frame_at(self, seconds):
        """The clip's frame at seconds, from a decoder of its own."""
        return self.decoder()[0].seek(seconds).read()

    def play(self, player, frames):
        """What run() does with each frame it reads, for that many."""
        for _ in range(frames):
            player.advance()

    def test_each_start_or_seek_is_a_new_run_and_halt_ends_it(self):
        player = self.player()
        player.start()
        first = player.proc
        self.assertEqual((player.state, first is None), (V.PLAYING, False))
        player.seek_to(1.0)
        self.assertIsNot(player.proc, first)
        self.assertEqual((player.state, player.base, player.n), (V.PLAYING, 1.0, 0))
        player.halt()
        self.assertEqual((player.proc, player.state), (None, V.STOPPED))

    def test_a_seek_mpv_takes_in_place_restarts_only_the_decoder(self):
        player = self.player()
        player.start()
        first = player.proc
        with mock.patch.object(player, "mpv", return_value={"error": "success"}) as mpv, mock.patch.object(player, "start") as start:
            player.seek_to(1.0)
        start.assert_not_called()
        mpv.assert_called_once_with("seek", 1.0, "absolute", "exact")
        self.assertIsNot(player.proc, first)
        self.assertEqual((player.state, player.base, player.n), (V.PLAYING, 1.0, 0))

    def test_a_paused_seek_shows_the_frame_there(self):
        player = self.player()
        player.start()
        self.play(player, 3)
        player.pause()
        player.seek_to(1.0)
        self.assertEqual((player.state, player.n, player.shown_time()), (V.PAUSED, 1, 1.0))
        self.assertEqual(player.frame, self.frame_at(1.0))

    def test_frame_steps(self):
        player = self.player()
        player.start()
        self.play(player, 10)  # frames 0 to 9 shown
        shown = dict(player.history)
        player.frame_step(-1)
        self.assertEqual((player.state, player.held, player.frame), (V.PAUSED, 8, shown[8]))
        player.frame_step(1)
        self.assertEqual((player.held, player.frame), (None, shown[9]))
        player.frame_step(1)  # past the history's end: read on
        self.assertEqual((player.n, player.frame, player.pos), (11, self.frame_at(0.4), 0.4))
        player.history.clear()  # as when it is far behind
        player.frame_step(-1)  # one run of frames up to 9
        self.assertEqual((player.held, player.frame, sorted(player.history)), (None, shown[9], list(range(10))))
        self.assertEqual(player.state, V.PAUSED)

    def test_advance_shows_counts_and_keeps_each_frame(self):
        player = self.player()
        player.start()
        times = [player.advance() for _ in range(3)]
        self.assertEqual((times, player.n, sorted(player.history)), ([0.0, 0.04, 0.08], 3, [0, 1, 2]))
        self.assertIs(player.frame, player.history[2])
        self.play(player, 47)  # the clip's 50 frames
        self.assertEqual((player.advance(), player.n), (None, 50))

    def test_frame_step_does_nothing_once_stopped(self):
        player = self.player()
        player.start()
        self.play(player, 3)
        player.halt()
        before = player.frame
        for frames in (1, -1):
            player.frame_step(frames)
        self.assertEqual((player.state, player.frame, player.held), (V.STOPPED, before, None))


class RenderPageTest(unittest.TestCase):
    def page(self, size):
        page = bare_page()
        page.values["size"] = V.RenderPage.SIZES.index(size)
        return page

    def test_screen_size_asked_once(self):
        page, calls = self.page("screen"), []
        with mock.patch.object(V, "screen_size", lambda: calls.append(1) or (2560, 1440)):
            for _ in range(5):
                self.assertEqual(page.target(), (2560, 1440))
                page.columns()
        self.assertEqual(len(calls), 1)

    def test_unknown_screen_falls_back(self):
        page = self.page("screen")
        with mock.patch.object(V, "screen_size", lambda: ()):
            self.assertEqual(page.target(), (1920, 1080))

    def test_presets_follow_crop_shape(self):
        page = self.page("1080 long side")
        self.assertEqual(page.target(), (1080, 608))
        page.values["cropw"] = 50
        self.assertEqual(page.target(), (960, 1080))


class RenderPageKeysTest(unittest.TestCase):
    def test_backtick_resets_every_row(self):
        page = bare_page()
        page.values = {"level": 10, "contrast": -10, "density": 31.0, "size": 2, "custom": (100, 100), "theme": 3,
                       "bold": True, "mono": True, "saturation": 50, "skin": False, "fit": "cover", "glyphs": "shape",
                       "cropw": 50, "croph": 40, "cropx": 10, "cropy": 20}
        page.ratio = 3
        for n, item in enumerate(V.RenderPage.MENU):
            page.at = n
            page.key(b"`")
        self.assertEqual(page.values, {**V.RenderPage.DEFAULTS, "density": 20.0})
        self.assertEqual(page.ratio, 0)  # free

    def test_a_failing_render_does_not_hang_export(self):
        page = bare_page(version=1, wanted=(1, None, None), player=mock.Mock(remote_w=os.pipe()[1]),
                         build=mock.Mock(side_effect=RuntimeError("boom")))
        worker = threading.Thread(target=page.work, daemon=True)
        worker.start()
        start = time.monotonic()
        page.export()
        self.assertLess(time.monotonic() - start, 2)
        self.assertIn("boom", page.message)
        page.wanted = (2, None, None)  # the worker is still there for the next one
        with page.cv:
            page.cv.notify_all()
        deadline = time.monotonic() + 2
        while page.build.call_count < 2 and time.monotonic() < deadline:
            time.sleep(0.01)
        self.assertEqual(page.build.call_count, 2)
        page.stop()
        worker.join(2)

    def test_s_after_a_failed_render_says_why_nothing_was_exported(self):
        with tempfile.TemporaryDirectory() as tmp:
            page = bare_page(out=Path(tmp, "pic-ascii.png"), version=1, player=mock.Mock(remote_w=os.pipe()[1]),
                             build=mock.Mock(side_effect=RuntimeError("boom")))
            worker = threading.Thread(target=page.work, daemon=True)
            with page.cv:
                page.wanted = (1, None, None)
            worker.start()
            with page.cv:
                page.cv.wait_for(lambda: page.failure, timeout=2)
            self.assertEqual(page.message, "render failed: boom")
            page.key(b"s")  # every key clears the last message first
            self.assertEqual((page.message, page.out.exists()), ("not exported: render failed: boom", False))
            page.stop()
            worker.join(2)

    def test_s_while_the_render_is_not_done_says_so(self):
        page = bare_page(busy=True, version=1)
        with mock.patch.object(V.RenderPage, "EXPORT_WAIT", 0.05):
            page.key(b"s")
        self.assertEqual(page.message, "not exported: still rendering")


class RenderPageDrawTest(unittest.TestCase):
    """What the render page writes in a 100 x 30 window: every row of the menu column in full, and
    beside it the preview's frame (columns 47 to 99), the picture inside sent only when it is new."""

    def page(self):
        return bare_page(player=mock.Mock(kitty_cell=lambda: (9, 20), kitty_pixels=lambda *a: b"<picture>"),
                         render=mock.Mock(), value_text=lambda item: item,
                         done={"version": 1, "size": (1920, 1080), "cols": 160, "preview": (2, 2, b"\0" * 16)})

    def menu(self, out):
        """{row: its text as it shows} for the rows written from column 1."""
        rows = re.findall(rb"\x1b\[(\d+);1H(.*?)(?=\x1b\[\d+;\d+H|\x1b\[\?2026l)", out, re.S)
        return {int(row): V.SGR_TEXT.sub("", text.decode()) for row, text in rows}

    def edges(self, title):
        """The frame's top, a side row and its bottom, as written."""
        return ("\x1b[1;47H\x1b[90m┌" + title.center(51, "─") + "┐\x1b[0m", "\x1b[15;47H\x1b[90m│\x1b[15;99H│\x1b[0m",
                "\x1b[30;47H\x1b[90m└" + "─" * 51 + "┘\x1b[0m")

    def test_menu_and_preview(self):
        page = self.page()
        first, second = page.draw(100, 30), page.draw(100, 30)
        for out in (first, second):
            rows = self.menu(out)
            self.assertEqual(sorted(rows), list(range(1, 31)))
            self.assertEqual({len(text) for text in rows.values()}, {V.RenderPage.WIDTH})
            self.assertEqual(rows[4], " > Level      level".ljust(V.RenderPage.WIDTH))
            for edge in self.edges(" 1920x1080 · 160 columns "):
                self.assertIn(edge, out.decode())
        self.assertIn(b"<picture>", first)
        self.assertNotIn(b"<picture>", second)  # the same again: the text and the frame only
        self.assertIn(b"<picture>", page.draw(80, 30))  # a new window size: sent again

    def test_lists_prompts_and_messages_fill_the_column_too(self):
        page = self.page()
        page.themes = [("current", {}), ("Dark One", {f"color{i}": "#102030" for i in range(16)})]
        page.list = {"kind": "theme", "query": "", "searching": True, "at_theme": 1, "at": 1}
        page.prompt, page.message = ("level", "1"), "not a value: x"
        rows = self.menu(page.draw(100, 30))
        self.assertEqual((sorted(rows), {len(text) for text in rows.values()}), (list(range(1, 31)), {V.RenderPage.WIDTH}))

    def test_crop_editor(self):
        page = self.page()
        page.cropping, page.values = True, {"cropx": 10, "cropy": 0, "cropw": 50, "croph": 100}
        page.crop_canvas = lambda box: (2, 2, b"\0" * 12)
        out = page.draw(100, 30)
        rows = self.menu(out)
        self.assertEqual((sorted(rows), {len(text) for text in rows.values()}), (list(range(1, 31)), {V.RenderPage.WIDTH}))
        for edge in self.edges(" source · crop 800x900 "):
            self.assertIn(edge, out.decode())
        self.assertIn(b"<picture>", out)
        again = page.draw(100, 30)
        self.assertEqual(self.menu(again), rows)
        self.assertNotIn(b"<picture>", again)  # the same crop: the text only
        self.assertNotIn("┌".encode(), again)


class StatusRedrawTest(unittest.TestCase):
    def screen(self, player, clear=False):
        out = io.BytesIO()
        with mock.patch.object(V.sys, "stdout", mock.Mock(buffer=out)), \
                mock.patch.object(V.shutil, "get_terminal_size", lambda *a: os.terminal_size((100, 30))):
            player.draw(clear=clear)
        return out.getvalue()

    def test_image_mode_sends_the_image_once(self):
        player = still(Image.new("RGB", (900, 600)), pixels=True)
        self.addCleanup(player.drop_shm)  # the pixels' file, which no kitty reads here
        self.assertIn(b"a=T", self.screen(player))
        player.say("level 5% darker")  # a key that changes nothing in image mode: a toast only
        again = self.screen(player)
        self.assertNotIn(b"a=T", again)
        self.assertNotIn(b"\x1b[2J", again)
        self.assertIn(b"level 5% darker", again)
        self.assertIn(b"a=T", self.screen(player, clear=True))  # a cleared screen gets it again
        player.zoom_n = 2
        self.assertIn(b"a=T", self.screen(player))  # and so does another view

    def test_ascii_renders_once_until_the_look_changes(self):
        player = still(Image.new("RGB", (900, 600), (200, 30, 30)))
        with mock.patch.object(player.term, "render", wraps=player.term.render) as render:
            first = self.screen(player)
            player.say("toast")
            self.screen(player)
            self.assertEqual(render.call_count, 1)
            player.mono = True
            self.screen(player)
            self.assertEqual(render.call_count, 2)
        self.assertIn(b"\x1b[3", first)


class ToastExpiryTest(unittest.TestCase):
    """run(), held still or not playing, waits for a key or the toast's end; at its end the status row
    is drawn without it."""

    def run_until(self, player, polls):
        """The timeouts run() gives poll() until the polls-th, and how often it draws."""
        waits = []

        def poll(timeout):
            waits.append(timeout)
            if len(waits) == polls:
                raise SystemExit

        with mock.patch.object(player, "poll", poll), mock.patch.object(player, "draw") as draw, \
                mock.patch.object(player, "serve_mpris"), mock.patch.object(player, "start"):
            with self.assertRaises(SystemExit):
                player.run()
        return waits, draw.call_count

    def test_a_still(self):
        player = still(Image.new("RGB", (90, 60)))
        player.say("hello")
        waits, draws = self.run_until(player, 2)
        self.assertEqual((len(waits), draws, player.toast), (2, 1, "hello"))  # not over yet: no draw but the first
        self.assertAlmostEqual(waits[0], V.TOAST, delta=0.2)
        player.toast_until = time.monotonic() - 1  # over
        self.assertEqual(self.run_until(player, 2), ([0, None], 2))  # the picture, then the status row without it
        self.assertEqual(player.toast, "")

    def test_a_paused_video(self):
        player = bare_player(state=V.PAUSED)
        player.say("paused")
        player.toast_until = time.monotonic() - 1
        self.assertEqual(self.run_until(player, 2), ([0, None], 1))
        self.assertEqual(player.toast, "")


class GeometryTest(unittest.TestCase):
    """The window (its cells, and their px) is measured once and read alike by a draw and the keys
    around it, until poll() sees a resize."""

    def setUp(self):
        self.window, self.calls = ((200, 60), (1800, 1200)), []

        def terminal_size(*args):
            self.calls.append("size")
            return os.terminal_size(self.window[0])

        def ioctl(fd, request, arg):
            self.calls.append("ioctl")
            (cols, rows), (w, h) = self.window
            return struct.pack("HHHH", rows, cols, w, h)

        for patch in (mock.patch.object(V.shutil, "get_terminal_size", terminal_size), mock.patch.object(V.fcntl, "ioctl", ioctl),
                      mock.patch.object(V.sys, "stdout", mock.Mock(buffer=io.BytesIO()))):
            patch.start()
            self.addCleanup(patch.stop)

    def player(self):
        player = bare_player(still=True, fps=V.Fraction(1), pixels=True)
        player.show("pic.png", Image.new("RGB", (4000, 3000)))
        self.addCleanup(player.drop_shm)
        return player

    def test_a_draw_and_the_keys_after_it_measure_once(self):
        player = self.player()
        player.zoom_n = 3
        player.draw()
        for k in (b"+", b"l", b"-", b"v", b"+"):  # zoom, pan, in image mode, then ASCII
            player.key(k)
            player.flush()
        self.assertEqual(self.calls, ["size", "ioctl"])

    def test_a_resize_is_measured_when_poll_sees_it(self):
        player = self.player()
        player.draw()
        self.window = ((100, 30), (1000, 660))  # smaller, and cells of 10 x 22 px
        self.assertEqual((player.viewport(), player.kitty_cell()), ((1800, 1200), (9, 20)))  # until poll() hears of it
        player.winch, wake = os.pipe()
        player.remote_r = -1
        for fd in (player.winch, wake):
            self.addCleanup(os.close, fd)
        os.write(wake, bytes([V.signal.SIGWINCH]))  # what set_wakeup_fd writes
        with mock.patch.object(V.select, "select", return_value=([player.winch], [], [])):
            player.poll(0)
        self.assertEqual((player.viewport(), player.kitty_cell()), ((1000, 660), (10, 22)))
        self.assertEqual(self.calls, ["size", "ioctl"] * 2)


class HeldStepTest(unittest.TestCase):
    def test_a_held_key_opens_one_image_and_lists_once(self):
        with tempfile.TemporaryDirectory() as tmp:
            paths = [Path(tmp, f"{i}.png") for i in range(6)]
            for p in paths:
                Image.new("RGB", (40, 30)).save(p)
            player = still(V.open_picture(paths[0]), paths[0])
            player.flush = lambda: player.settle_step() if player.pending else None
            with mock.patch.object(V, "open_picture", wraps=V.open_picture) as opened, \
                    mock.patch.object(V, "sibling_images", wraps=V.sibling_images) as listed:
                for _ in range(4):
                    player.key(b"l")
                player.flush()
                player.key(b"h")
                player.key(b"k")  # acts on the image reached: settles it first
            self.assertEqual((player.path, player.rotation), (str(paths[3]), 1))
            self.assertEqual((opened.call_count, listed.call_count), (2, 1))
            self.assertEqual(player.toast, "3.png  4/6")

    def test_steps_pass_over_files_that_do_not_open_their_way(self):
        with tempfile.TemporaryDirectory() as tmp:
            paths = [Path(tmp, f"{i}.png") for i in range(4)]
            for p in paths:
                Image.new("RGB", (40, 30)).save(p)
            for bad in (b"not a picture", paths[3].read_bytes()[:60]):  # no header, or one cut short
                paths[3].write_bytes(bad)
                player = still(V.open_picture(paths[1]), paths[1])
                player.key(b"l")
                player.key(b"l")
                player.settle_step()
                self.assertEqual(player.path, str(paths[0]))  # 3 does not open: on, wrapping round

    def test_a_batch_lands_where_presses_one_by_one_do(self):
        with tempfile.TemporaryDirectory() as tmp:
            paths = [Path(tmp, f"{i}.png") for i in range(5)]
            for p in paths:
                Image.new("RGB", (40, 30)).save(p)
            paths[1].write_bytes(b"not a picture")  # passed over: no step of its own
            for keys, want in ((b"ll", 3), (b"lll", 4), (b"llll", 0), (b"llh", 2), (b"h", 4), (b"hhh", 2), (b"lh", 0)):
                for batch in (True, False):
                    with self.subTest(keys=keys, batch=batch):
                        player = still(V.open_picture(paths[0]), paths[0])
                        for k in keys:
                            player.key(bytes([k]))
                            if not batch:
                                player.settle_step()
                        if batch:
                            player.settle_step()
                        self.assertEqual(player.path, str(paths[want]))


class TooLargeTest(unittest.TestCase):
    def test_stepping_passes_over_one_too_large_and_says_so(self):
        with tempfile.TemporaryDirectory() as tmp:
            paths = [Path(tmp, f"{i}.png") for i in range(3)]
            Image.new("RGB", (20, 20)).save(paths[0])
            Image.new("RGB", (50, 50)).save(paths[1])  # 2500 px: too large under a limit of 1000
            Image.new("RGB", (20, 20)).save(paths[2])
            with mock.patch.object(Image, "MAX_IMAGE_PIXELS", 1000):
                player = still(V.open_picture(paths[0]), paths[0])
                player.key(b"l")
                player.settle_step()
            self.assertEqual(player.path, str(paths[2]))
            self.assertEqual(player.toast, "2.png  3/3; 1.png too large")

    def test_opening_one_too_large_exits_with_the_reason(self):
        with tempfile.TemporaryDirectory() as tmp:
            big = Path(tmp, "big.png")
            Image.new("RGB", (50, 50)).save(big)
            with mock.patch.object(Image, "MAX_IMAGE_PIXELS", 1000), self.assertRaises(SystemExit) as caught:
                V.Player(str(big))
            self.assertIn("big.png is too large to open", str(caught.exception.code))


class HelpTest(unittest.TestCase):
    def screen(self, player, size=(100, 30)):
        out = io.BytesIO()
        with mock.patch.object(V.sys, "stdout", mock.Mock(buffer=out)), \
                mock.patch.object(V.shutil, "get_terminal_size", lambda *a: os.terminal_size(size)):
            player.geometry = None
            player.flush()
        return out.getvalue()

    def keys(self, player, data):
        """data as poll() reads it: one read, then whatever flush() draws."""
        player.winch = player.remote_r = -1
        ready = iter([([0], [], [])])
        with mock.patch.object(V.select, "select", lambda *a: next(ready, ([], [], []))), \
                mock.patch.object(V.os, "read", return_value=data), mock.patch.object(player, "flush"):
            player.poll(0)

    def test_the_tables_come_from_the_docstring(self):
        player_keys, page_keys = V.HELP
        self.assertTrue(player_keys[0].startswith("Keys,") and page_keys[0].startswith("Render page (r)"))
        self.assertTrue(any(line.startswith("F1 ") for line in player_keys))
        self.assertTrue(any(line.startswith("F1 ") for line in page_keys))
        self.assertTrue(any(line.startswith("D ") for line in player_keys))
        self.assertFalse(any(line.startswith("Render page") for line in player_keys))

    def test_f1_shows_the_keys_and_any_other_key_closes_without_acting(self):
        player = still(Image.new("RGB", (900, 600)), pixels=True)
        for f1 in (b"\x1bOP", b"\x1b[11~"):
            self.assertEqual(V.KEY.findall(f1 + b"q"), [f1, b"q"])
            self.keys(player, f1)
            self.assertEqual(player.help, 0)
            out = self.screen(player)
            self.assertIn(V.KITTY_CLEAR, out)
            self.assertIn(b"zoomed: pan, a sixteenth", out)
            self.assertNotIn(b"a=T", out)
            self.keys(player, b"q")  # closes: does not quit
            self.assertIsNone(player.help)
            self.assertIn(b"a=T", self.screen(player))  # the image back
            player.drop_shm()

    def test_a_short_window_scrolls(self):
        player = still(Image.new("RGB", (90, 60)))
        self.keys(player, b"\x1bOP")
        first = self.screen(player, (100, 12))
        self.assertIn(b"j k scroll", first)
        self.keys(player, b"jjj")
        self.assertEqual(player.help, 3)
        self.assertNotEqual(self.screen(player, (100, 12)), first)
        self.keys(player, b"kkkkk")
        self.assertEqual(player.help, 0)
        self.keys(player, b"x")
        self.assertIsNone(player.help)

    def test_video_pauses_while_it_shows(self):
        player = bare_player(state=V.PLAYING)
        with mock.patch.object(player, "pause", lambda: setattr(player, "state", V.PAUSED)), \
                mock.patch.object(player, "resume", lambda: setattr(player, "state", V.PLAYING)):
            player.key(b"\x1bOP")
            self.assertEqual(player.state, V.PAUSED)
            self.keys(player, b" ")  # closes help; does not toggle play
            self.assertEqual((player.help, player.state), (None, V.PLAYING))

    def test_the_render_page_shows_its_own_keys(self):
        player = bare_player()
        player.page = mock.Mock(shown=(1, 100, 30), crop_view="x")
        player.key(b"\x1bOP")
        player.page.key.assert_not_called()
        out = self.screen(player)
        self.assertIn(b"Render page (r)", out)
        self.assertNotIn(b"trash it", out)
        self.keys(player, b"\x1b")
        player.page.key.assert_not_called()
        self.assertEqual((player.page.shown, player.page.crop_view), (None, None))  # the preview goes up again

    def test_f1_answers_the_delete_question_as_no(self):
        player = still(Image.new("RGB", (90, 60)))
        player.key(b"D")
        player.key(b"\x1bOP")
        self.assertEqual((player.confirming, player.help), (False, None))
        self.assertTrue(player.toast.startswith("kept"))

    def test_audio_alone_stops_the_scope_and_keeps_its_colour(self):
        scope = subprocess.Popen(["sleep", "30"])
        self.addCleanup(scope.kill)
        player = bare_player(has_video=False, fps=None, scope=scope)
        out = io.BytesIO()
        with mock.patch.object(V.sys, "stdout", mock.Mock(buffer=out)):
            player.key(b"\x1bOP")
            self.assertEqual(Path(f"/proc/{scope.pid}/stat").read_text().rsplit(")", 1)[1].split()[0], "T")
            self.assertTrue(out.getvalue().startswith(b"\x1b7"))  # the scope's cursor and colour, saved
            player.help_key(b"q")
        self.assertTrue(out.getvalue().endswith(b"\x1b[2J\x1b8"))  # restored for it
        time.sleep(0.05)
        self.assertNotEqual(Path(f"/proc/{scope.pid}/stat").read_text().rsplit(")", 1)[1].split()[0], "T")


class ScopeStreamTest(unittest.TestCase):
    """The scope records this viewer's mpv stream alone, not the default sink's monitor (all system audio)."""

    def test_mpv_stream_is_named_and_the_scope_targets_it(self):
        started = []

        def popen(command, **kw):
            started.append((command, kw.get("env") or {}))
            raise OSError

        player = bare_player(has_video=False, fps=None, serve_mpris=lambda: None)
        with mock.patch.object(V.subprocess, "Popen", popen):
            with self.assertRaises(OSError):
                player.run()  # the scope, then mpv
            with self.assertRaises(OSError):
                player.launch_mpv(0.0)
        (scope, scope_env), (mpv, mpv_env) = started
        self.assertEqual((scope[-1], scope_env.get("SCOPE_TARGET")), (V.SCOPE, V.STREAM))
        self.assertEqual((mpv[len(V.CHILD)], mpv_env.get("PIPEWIRE_PROPS")), ("mpv", f"{{ node.name = {V.STREAM} }}"))
        self.assertIn(str(os.getpid()), V.STREAM)  # one per viewer


class KeySplitTest(unittest.TestCase):
    def test_an_escape_sequence_split_across_reads_is_one_key(self):
        player = still(Image.new("RGB", (90, 60)))
        player.winch = player.remote_r = -1
        keys = []
        with mock.patch.object(player, "key", keys.append), mock.patch.object(player, "flush"), \
                mock.patch.object(V.select, "select", return_value=([0], [], [])), \
                mock.patch.object(V.os, "read", side_effect=[b"\x1b[A\x1b[", b"B"]):
            player.poll(0)
        self.assertEqual(keys, [b"\x1b[A", b"\x1b[B"])

    def test_terminal_replies_are_no_keys(self):
        # kitty's answers to the startup probe, should they come after kitty_reads_files stopped waiting: whole, even
        # cut by a read's end, and dropped; the keys around them still count
        late = (b"\x1b_Gi=31;EBADF:Failed to open file for graphics transmission: /dev/shm/x with error: [2] No such file"
                b" or directory\x1b\\")
        for reads in ([b"l" + late + b"\x1b[?62;22c" + b"k"], [b"l\x1b_Gi=31;O", b"K\x1b\\\x1b[?62;", b"22ck"]):
            with self.subTest(reads=reads):
                player = still(Image.new("RGB", (90, 60)))
                player.winch = player.remote_r = -1
                keys = []
                with mock.patch.object(player, "key", keys.append), mock.patch.object(player, "flush"), \
                        mock.patch.object(V.select, "select", return_value=([0], [], [])), \
                        mock.patch.object(V.os, "read", side_effect=reads):
                    player.poll(0)
                self.assertEqual(keys, [b"l", b"k"])

    def test_a_lone_escape_is_a_key(self):
        player = still(Image.new("RGB", (90, 60)))
        player.winch = player.remote_r = -1
        keys, ready = [], iter([([0], [], []), ([], [], [])])
        with mock.patch.object(player, "key", keys.append), mock.patch.object(player, "flush"), \
                mock.patch.object(V.select, "select", lambda *a: next(ready)), \
                mock.patch.object(V.os, "read", return_value=b"\x1b"):
            player.poll(0)
        self.assertEqual(keys, [b"\x1b"])


class AudioKeysTest(unittest.TestCase):
    def test_picture_keys_do_nothing_for_audio(self):
        player = bare_player(has_video=False)
        for k in (b"3", b"4", b"1", b"2", b"7", b"8", b"`", b"c", b"b", b"g", b"s", b"r", b"v", b"+", b"=", b"-"):
            player.key(k)
        self.assertEqual((player.level, player.contrast, player.saturation, player.mono, player.bold, player.glyphs,
                          player.toast), (0, 0, 100, False, False, "mixed", ""))

    def test_playback_keys_pass(self):
        self.assertTrue({b" ", b"p", b"q", b"m", b"L", b"9", b"0", b"[", b"]", b"\x1b[C", b"h"} <= V.AUDIO_KEYS)

    def test_r_opens_the_render_page_for_video_and_stills(self):
        for player in (bare_player(), still(Image.new("RGB", (90, 60)))):
            with mock.patch.object(player, "open_page") as page:
                player.key(b"r")
            page.assert_called_once()


class ConvertArgsTest(unittest.TestCase):
    def test_a_screenshot_and_the_render_page_take_the_look_alike(self):
        look = {"level": 10, "contrast": -5, "saturation": 150, "glyphs": "shape", "bold": True, "mono": True}
        player = bare_player(**look)
        a = script("ascii-convert").parser.parse_args([*player.convert_args(), "pic.png"])
        self.assertEqual({k: getattr(a, k) for k in look}, look)
        with tempfile.TemporaryDirectory() as tmp:
            src = Path(tmp, "pic.png")
            Image.new("RGB", (90, 60)).save(src)
            shot = still(V.open_picture(src), src)
            vars(shot).update(look, remote_w=os.pipe()[1])
            commands, ran = [], threading.Event()

            def run(command, **kw):
                commands.append(command)
                ran.set()
                return subprocess.CompletedProcess(command, 0)

            with mock.patch.object(V.subprocess, "run", run):
                shot.screenshot()
                ran.wait(5)
            self.assertEqual(commands[0][len(V.CHILD) + 1 : -3], player.convert_args())  # then --output, its file, the source
        with mock.patch.object(V, "kitty_themes", list), mock.patch.object(V.threading, "Thread"):
            page = V.RenderPage(player, Image.new("RGB", (90, 60)), Path("pic-ascii.png"))
        self.assertEqual({k: page.values[k] for k in look}, look)


class GlyphsKeyTest(unittest.TestCase):
    def test_g_cycles_the_modes(self):
        player, seen = bare_player(), []
        for _ in range(4):
            player.key(b"g")
            seen.append((player.glyphs, player.toast))
        self.assertEqual(seen, [("density", "glyphs: density"), ("shape", "glyphs: shape"), ("mixed", "glyphs: mixed"),
                                ("density", "glyphs: density")])

    def test_the_render_page_steps_them_both_ways(self):
        page = bare_page()
        self.assertTrue(page.nudge("glyphs", -1))
        self.assertEqual(page.values["glyphs"], "shape")
        page.nudge("glyphs", 1)
        page.nudge("glyphs", 1)
        self.assertEqual(page.values["glyphs"], "density")


class OriginalPngTest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.dir = Path(self.tmp.name)

    def tearDown(self):
        self.tmp.cleanup()

    def png(self, path, rotation=0):
        data, size = V.original_png(path, rotation)
        image = Image.open(io.BytesIO(data))
        self.assertEqual((image.format, image.size), ("PNG", size))
        return image

    def test_native_size_alpha_and_turn(self):
        src = self.dir / "big.png"
        picture = Image.new("RGBA", (3000, 1000), (255, 0, 0, 128))
        picture.putpixel((0, 0), (0, 0, 255, 255))
        picture.save(src)
        self.assertEqual(self.png(src).tobytes(), picture.tobytes())
        turned = self.png(src, 1)
        self.assertEqual((turned.mode, turned.size), ("RGBA", (1000, 3000)))
        self.assertEqual(turned.getpixel((999, 0)), (0, 0, 255, 255))  # top left -> top right

    def test_exif_upright(self):
        src = self.dir / "cam.jpg"
        exif = Image.Exif()
        exif[0x0112] = 6  # stored turned left: shown a quarter right
        Image.new("RGB", (400, 200)).save(src, exif=exif)
        self.assertEqual(self.png(src).size, (200, 400))

    def test_cmyk_becomes_rgb(self):
        src = self.dir / "print.jpg"
        Image.new("CMYK", (50, 40)).save(src)
        self.assertEqual(self.png(src).mode, "RGB")

    def test_svg_at_declared_size(self):
        src = self.dir / "icon.svg"
        src.write_text('<svg xmlns="http://www.w3.org/2000/svg" width="64" height="32"><rect width="64" height="32"/></svg>')
        self.assertEqual(self.png(src).size, (64, 32))


class LevelKeysTest(unittest.TestCase):
    def test_only_3_and_4(self):
        self.assertEqual(set(V.LEVELS), {b"3", b"4"})
        self.assertEqual((V.LEVELS[b"3"], V.LEVELS[b"4"]), (5, -5))

    def test_j_and_k_leave_the_level_of_a_video(self):
        player = bare_player()
        with mock.patch.object(player, "seek"):  # they seek a minute
            for k in (b"j", b"k"):
                player.key(k)
        self.assertEqual(player.level, 0)
        player.key(b"3")
        player.key(b"3")
        player.key(b"4")
        self.assertEqual(player.level, 5)


SQUARE = '<svg xmlns="http://www.w3.org/2000/svg" width="100" height="50"><rect x="25" y="10" width="50" height="30"/></svg>'


class CheckerboardTest(unittest.TestCase):
    def test_checkerboard_squares_in_screen_px(self):
        data = V.checkerboard((24, 16))
        px = lambda x, y: data[(y * 24 + x) * 3]
        dark, light = V.CHECKER
        self.assertEqual((px(0, 0), px(7, 7), px(8, 0), px(0, 8), px(8, 8), px(23, 15)), (dark, dark, light, light, dark, light))


def bare_player(**overrides):
    """A Player without a file, a terminal or mpv: Player.__init__'s plain state for a video, stopped,
    nothing shown yet; then overrides (attributes, methods too)."""
    player = object.__new__(V.Player)
    player.show("clip.mp4", None)
    player.pixels, player.kitty, player.placed, player.rendered, player.still = False, (None, b""), None, None, False
    player.jobs = None
    player.decoder, player.fps, player.has_audio, player.length, player.has_video = None, V.Fraction(25), False, V.math.inf, True
    player.scope, player.loop, player.state, player.proc, player.frame, player.n = None, False, V.STOPPED, None, None, 0
    player.history, player.held, player.base = V.OrderedDict(), None, 0.0
    player.level, player.contrast, player.lut, player.bold, player.mono, player.glyphs = 0, 0, None, False, False, "mixed"
    player.saturation, player.term, player.page = 100, V.Term(V.theme()), None
    player.speed, player.muted, player.volume, player.toast, player.toast_until = 100, False, None, "", 0.0
    player.trashed, player.listing, player.stale, player.audio, player.ipc = [], None, 0, None, None
    player.sock = str(Path(tempfile.gettempdir(), "ascii-viewer-tests-none", "mpv.sock"))  # never there
    player.pos, player.mark, player.running, player.geometry, player.requests = 0.0, 0.0, False, None, V.deque()
    player.help = None
    vars(player).update(overrides)
    if "period" not in overrides:
        player.period = 1 / float(player.fps or 1)
    return player


def bare_page(**overrides):
    """A RenderPage without a player, a worker thread or ascii-convert's fonts: RenderPage.__init__'s plain state
    for a 1600 x 900 picture, the values at their defaults, on the menu's first row, render() doing nothing; then
    overrides (attributes, methods too)."""
    page = object.__new__(V.RenderPage)
    page.player, page.picture, page.out, page.convert = None, Image.new("RGB", (1600, 900)), Path("pic-ascii.png"), script("ascii-convert")
    page.themes, page.values = [("current", V.theme())], page.defaults()
    page.cropped, page.screen, page.cropping, page.ratio, page.crop_view = {}, None, False, 0, None
    page.items, page.at, page.prompt, page.list, page.message = V.RenderPage.MENU + V.RenderPage.ACTIONS, 0, None, None, ""
    page.shrunk, page.style, page.style_key, page.twin, page.twin_key = {}, None, None, None, None
    page.cv, page.wanted, page.done, page.busy, page.running, page.failure = threading.Condition(), None, None, False, True, None
    page.box, page.shown, page.version, page.Image, page.cell_ratio = None, None, 0, Image, 0.6
    page.render = lambda: None
    vars(page).update(overrides)
    return page


def still(picture, path="pic.png", room=(900, 600), pixels=False):
    """A Player showing picture in a window of room px, without a terminal."""
    player = bare_player(still=True, fps=V.Fraction(1), pixels=pixels, viewport=lambda: room, kitty_cell=lambda: (9, 20))
    player.show(str(path), picture)
    return player


def placed(player, cols=100, rows=30):
    """The arguments kitty_pixels gets for the picture on screen: (w, h, rgb, fmt, col, row, cols, rows)."""
    sent = []
    player.kitty_pixels = lambda *args: sent.append(args)
    player.kitty_image(cols, rows, again=True)
    return sent[0]


class StillViewTest(unittest.TestCase):
    def test_transparent_ascii_shows_content_and_background(self):
        im = Image.new("RGBA", (100, 100), (0, 0, 0, 0))
        im.paste(Image.new("RGBA", (40, 40), (0, 0, 0, 255)), (30, 30))
        w, h, data = still(im).thumbnail()
        seen = {data[i : i + 3] for i in range(0, len(data), 3)}
        self.assertEqual(seen, {bytes(V.GREY), b"\0\0\0"})

    def test_zoom_keys_floor_and_cap(self):
        player = still(Image.new("RGB", (1800, 1200)))  # fit scale 0.5
        player.key(b"-")
        self.assertEqual((player.zoom_n, player.toast), (0, "fit"))
        player.key(b"+")
        self.assertEqual((player.zoom_n, player.toast), (1, "zoom 71%"))
        player.key(b"=")
        self.assertEqual((player.zoom_n, player.toast), (2, "zoom 100%"))
        for _ in range(20):
            player.key(b"+")
        self.assertEqual(player.zoom_n, 8)  # 8 screen px per image px: 0.5 * 2^(8/2) = 8
        self.assertEqual(player.toast, "zoom 800%")
        player.key(b"-")
        self.assertEqual(player.toast, "zoom 566%")
        for _ in range(20):
            player.key(b"-")
        self.assertEqual((player.zoom_n, player.toast, player.pan), (0, "fit", (0.5, 0.5)))

    def test_small_image_fits_unenlarged_and_caps_at_8x(self):
        player = still(Image.new("RGB", (100, 100)))
        self.assertEqual(player.fit_scale(), 1)
        for _ in range(20):
            player.key(b"+")
        self.assertEqual((player.zoom_n, player.toast), (6, "zoom 800%"))

    def test_zoom_redraws_clearing(self):
        player = still(Image.new("RGB", (1800, 1200)))
        player.key(b"+")
        self.assertEqual(player.stale, 2)

    def test_crop_box(self):
        player = still(Image.new("RGB", (1800, 1200)))
        self.assertEqual(player.crop_box(), (0, 0, 1800, 1200))
        player.zoom_n = 2  # 100%: the window's 900 x 600 px of the image
        self.assertEqual(player.crop_box(), (450, 300, 1350, 900))
        player.zoom_n = 4  # 200%
        self.assertEqual(player.crop_box(), (675, 450, 1125, 750))

    def test_crop_box_shows_a_short_axis_whole(self):
        player = still(Image.new("RGB", (1000, 200)), room=(400, 600))  # fit scale 0.4
        player.zoom_n = 2  # 80%: 500 px across, the whole 200 px high
        self.assertEqual(player.crop_box(), (250, 0, 750, 200))

    def test_pan_clamps_at_the_edges(self):
        player = still(Image.new("RGB", (1800, 1200)))
        player.zoom_n = 2
        player.key(b"l")  # a quarter of the 900 px view
        self.assertEqual(player.crop_box()[::2], (675, 1575))
        player.key(b"\x1b[C")
        self.assertEqual(player.crop_box()[::2], (900, 1800))
        player.key(b"l")
        self.assertEqual(player.crop_box()[::2], (900, 1800))
        for _ in range(9):
            player.key(b"h")
        self.assertEqual(player.crop_box()[::2], (0, 900))
        for k in (b"k", b"\x1b[A", b"k"):
            player.key(k)
        self.assertEqual(player.crop_box()[1::2], (0, 600))
        for _ in range(9):
            player.key(b"j")
        self.assertEqual(player.crop_box()[1::2], (600, 1200))
        self.assertEqual(player.rotation, 0)

    def test_zoom_out_keeps_the_view_inside(self):
        player = still(Image.new("RGB", (1800, 1200)))
        player.zoom_n = 4
        for _ in range(9):
            player.key(b"l")
        player.key(b"-")
        self.assertEqual(player.crop_box()[2], 1800)
        self.assertEqual(player.pan[0] * 1800, player.crop_box()[0] / 2 + player.crop_box()[2] / 2)

    def test_h_l_step_at_fit_and_pan_zoomed(self):
        player = still(Image.new("RGB", (1800, 1200)))
        with mock.patch.object(player, "step") as step:
            for k, offset in ((b"h", -1), (b"\x1b[D", -1), (b"l", 1), (b"\x1b[C", 1)):
                player.key(k)
                step.assert_called_with(offset)
            self.assertEqual(step.call_count, 4)
            player.key(b"+")
            player.key(b"h")
            player.key(b"l")
            self.assertEqual(step.call_count, 4)

    def test_shift_pans_a_sixteenth_zoomed_and_does_nothing_at_fit(self):
        player = still(Image.new("RGB", (1800, 1200)))
        with mock.patch.object(player, "step") as step, mock.patch.object(player, "rotate") as rotate:
            for k in (b"H", b"J", b"K", b"L", b"\x1b[1;2D", b"\x1b[1;2C", b"\x1b[1;2A", b"\x1b[1;2B"):
                player.key(k)
            step.assert_not_called()
            rotate.assert_not_called()
        self.assertEqual((player.zoom_n, player.pan), (0, (0.5, 0.5)))
        player.key(b"+")
        player.key(b"+")
        left, top, right, bottom = player.crop_box()
        for k, (dx, dy) in ((b"L", (1, 0)), (b"\x1b[1;2C", (1, 0)), (b"J", (0, 1)), (b"\x1b[1;2B", (0, 1)),
                            (b"H", (-1, 0)), (b"\x1b[1;2D", (-1, 0)), (b"K", (0, -1)), (b"\x1b[1;2A", (0, -1))):
            before = player.crop_box()
            player.key(k)
            after = player.crop_box()
            self.assertAlmostEqual(after[0] - before[0], dx * (right - left) / 16, msg=k)
            self.assertAlmostEqual(after[1] - before[1], dy * (bottom - top) / 16, msg=k)

    def test_j_k_rotate_at_fit_and_pan_zoomed(self):
        player = still(Image.new("RGB", (1800, 1200)))
        player.key(b"k")
        self.assertEqual(player.rotation, 1)
        player.key(b"j")
        player.key(b"j")
        self.assertEqual(player.rotation, 3)
        player.key(b"\x1b[A")
        self.assertEqual(player.rotation, 0)
        player.key(b"\x1b[B")
        self.assertEqual(player.rotation, 3)
        player.key(b"+")
        player.key(b"k")
        player.key(b"\x1b[B")
        self.assertEqual(player.rotation, 3)

    def test_rotate_resets_zoom(self):
        player = still(Image.new("RGB", (1800, 1200)))
        player.key(b"+")
        player.key(b"+")
        player.key(b"l")
        player.rotate(1)
        self.assertEqual((player.zoom_n, player.pan, player.rotation), (0, (0.5, 0.5), 1))
        self.assertEqual(player.crop_box(), (0, 0, 1200, 1800))

    def test_step_resets_zoom(self):
        with tempfile.TemporaryDirectory() as tmp:
            a, b = Path(tmp, "a.png"), Path(tmp, "b.png")
            Image.new("RGB", (1800, 1200)).save(a)
            Image.new("RGB", (800, 600)).save(b)
            player = still(V.open_picture(a), a)
            player.key(b"+")
            player.key(b"+")
            with mock.patch.object(player, "thumbnail") as thumb:
                player.stale = 0
                player.step(1)
                player.settle_step()
            thumb.assert_not_called()  # draw() cuts the frame
            self.assertEqual(player.stale, 2)
            self.assertEqual(player.path, str(b))
            self.assertEqual((player.zoom_n, player.pan, player.logical), (0, (0.5, 0.5), (800, 600)))
            self.assertEqual(player.thumbnail()[:2], (720, 540))  # 800 x 600 fitted to the ASCII frame

    def test_ascii_frame_is_the_visible_crop(self):
        im = Image.new("RGB", (1800, 1200), (0, 0, 255))
        im.paste((255, 0, 0), (0, 0, 900, 600))  # the top left quarter
        player = still(im)
        self.assertEqual(player.thumbnail()[:2], (810, 540))
        player.zoom_n = 4  # 200%: a 450 x 300 px piece, not enlarged
        player.pan = (0.75, 0.75)
        w, h, data = player.thumbnail()
        self.assertEqual((w, h), (450, 300))
        self.assertEqual({data[i : i + 3] for i in range(0, len(data), 3)}, {b"\0\0\xff"})
        player.pan = (0, 0)  # clamped to the top left corner
        w, h, data = player.thumbnail()
        self.assertEqual({data[i : i + 3] for i in range(0, len(data), 3)}, {b"\xff\0\0"})

    def test_ascii_frame_kept_until_the_view_changes(self):
        player = still(Image.new("RGB", (1800, 1200)))
        first = player.thumbnail()
        self.assertIs(player.thumbnail(), first)
        player.zoom_n = 2
        self.assertIsNot(player.thumbnail(), first)

    kitty = staticmethod(placed)

    def test_image_mode_fit_and_zoomed(self):
        im = Image.new("RGB", (1800, 1200), (0, 0, 255))
        im.paste((255, 0, 0), (0, 0, 900, 600))
        player = still(im, pixels=True)
        w, h, rgb, fmt, *_ = self.kitty(player)
        self.assertEqual((w, h, fmt, len(rgb)), (900, 600, 24, 900 * 600 * 3))
        player.zoom_n = 4  # 200%: a 450 x 300 px piece of the picture fills the window
        player.pan = (0.25, 0.25)
        w, h, rgb, *_ = self.kitty(player)
        self.assertEqual((w, h), (900, 600))
        self.assertEqual({rgb[i : i + 3] for i in range(0, len(rgb), 3)}, {b"\xff\0\0"})
        player.pan = (0.75, 0.75)
        rgb = self.kitty(player)[2]
        self.assertEqual({rgb[i : i + 3] for i in range(0, len(rgb), 3)}, {b"\0\0\xff"})

    def test_image_mode_zoom_cache_follows_the_view(self):
        player = still(Image.new("RGB", (1800, 1200)), pixels=True)
        self.kitty(player)
        first = player.kitty
        self.kitty(player)
        self.assertIs(player.kitty, first)
        player.zoom_n = 2
        self.kitty(player)
        self.assertIsNot(player.kitty, first)
        second = player.kitty
        player.pan = (0.9, 0.5)
        self.kitty(player)
        self.assertIsNot(player.kitty, second)

    def test_image_mode_zoom_steps_of_a_small_picture_show_at_their_scale(self):
        player = still(Image.new("RGB", (400, 300)), room=(1800, 1000), pixels=True)
        sizes = []
        for n in range(5):
            player.zoom_n = n
            sizes.append(self.kitty(player, 200, 50)[:2])
        self.assertEqual(sizes[:4], [(round(400 * 2 ** (n / 2)), round(300 * 2 ** (n / 2))) for n in range(4)])  # not the window's size
        self.assertEqual(sizes[4], (1600, 1000))  # 400%: 400 x 250 of the picture, as high as the window

    def test_image_mode_enlarges_by_nearest_and_reduces_smoothly(self):
        stripes = Image.frombytes("L", (40, 30), bytes(255 * ((x + y) & 1) for y in range(30) for x in range(40))).convert("RGB")
        player = still(stripes, room=(1800, 1000), pixels=True)
        player.zoom_n = 4  # 400%: each image px a 4 x 4 block
        w, h, rgb, *_ = self.kitty(player, 200, 50)
        self.assertEqual(set(rgb), {0, 255})
        self.assertEqual(rgb[:w * 3:12], bytes(255 * (x & 1) for x in range(w // 4)))
        big = still(stripes.resize((1800, 1200), Image.NEAREST), room=(900, 600), pixels=True)
        self.assertGreater(len(set(self.kitty(big, 100, 30)[2])), 2)  # the fit halves it: blended

    def test_backspace_zooms_back_to_the_fit(self):
        for k in (b"\x7f", b"\x08"):
            player = still(Image.new("RGB", (3600, 2400)))
            player.zoom_n = 5
            player.key(b"l")
            player.key(k)
            self.assertEqual((player.zoom_n, player.pan, player.toast), (0, (0.5, 0.5), "fit"))
            player.key(k)
            self.assertEqual(player.zoom_n, 0)

    def test_ascii_zoom_passes_over_steps_that_show_the_whole_picture(self):
        player = still(Image.new("RGB", (400, 300)), room=(1800, 1000))
        player.key(b"+")
        self.assertEqual(player.zoom_n, 4)  # 1 to 3 would be drawn as the fit: chafa fills the cells
        self.assertEqual(player.toast, "zoom 400%")
        player.key(b"-")
        self.assertEqual((player.zoom_n, player.toast), (0, "fit"))
        player.pixels = True  # an image shows them at their size
        player.key(b"+")
        self.assertEqual(player.zoom_n, 1)

    def test_zoom_count_follows_the_window_when_it_grows(self):
        player = still(Image.new("RGB", (3600, 2400)))
        player.zoom_n = player.zoom_limit()
        self.assertEqual((player.zoom_n, player.scale()), (10, 8))
        player.viewport = lambda: (1800, 1200)  # fit 0.5 now: ten steps would be 16 screen px an image px
        self.assertEqual(player.scale(), 8)
        player.key(b"+")
        self.assertEqual((player.zoom_n, player.toast), (8, "zoom 800%"))  # not zoomed out to it from 10
        player.zoom_n = 10
        player.key(b"-")
        self.assertEqual((player.zoom_n, player.toast), (7, "zoom 566%"))

    def test_rotate_and_step_leave_the_frame_to_draw(self):
        player = still(Image.new("RGB", (1800, 1200)))
        with mock.patch.object(player, "thumbnail") as thumb:
            for _ in range(8):
                player.key(b"k")
            player.stale = 0
            player.key(b"j")
        thumb.assert_not_called()
        self.assertEqual((player.rotation, player.stale, bool(player.still)), (3, 2, True))
        self.assertEqual(player.thumbnail()[:2], (360, 540))  # what draw() cuts: 1200 x 1800 fitted to the frame

    def test_image_mode_transparency_over_checkerboard(self):
        player = still(Image.new("RGBA", (900, 600), (0, 0, 0, 0)), pixels=True)
        w, h, rgb, *_ = self.kitty(player)
        self.assertEqual(rgb, V.checkerboard((900, 600)))
        player.zoom_n = 2
        _, _, zoomed, *_ = self.kitty(player)
        self.assertEqual(zoomed, rgb)  # squares stay 8 screen px, however far in

    def test_open_page_gets_rgb(self):
        player = still(Image.new("RGBA", (90, 60), (0, 0, 0, 0)))
        with mock.patch.object(V, "RenderPage") as page:
            player.open_page()
        self.assertEqual(page.call_args.args[1].mode, "RGB")
        self.assertEqual(page.call_args.args[1].getpixel((3, 3)), V.GREY)

    def test_screenshot_hands_convert_the_grey_composite(self):
        with tempfile.TemporaryDirectory() as tmp:
            src = Path(tmp, "clear.png")
            Image.new("RGBA", (90, 60), (0, 0, 0, 0)).save(src)
            player = still(V.open_picture(src), src)
            player.remote_w = os.pipe()[1]
            seen = []

            def fake(command, **kw):
                image = Image.open(command[-1])
                seen.append((command[-1], image.mode, image.getpixel((3, 3))))
                return subprocess.CompletedProcess(command, 1)

            with mock.patch.object(V.subprocess, "run", fake):
                player.screenshot()
                deadline = time.monotonic() + 5
                while not seen and time.monotonic() < deadline:
                    time.sleep(0.02)
            self.assertEqual(len(seen), 1)
            self.assertNotEqual(seen[0][0], str(src))
            self.assertEqual(seen[0][1:], ("RGB", V.GREY))


class SvgZoomTest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.dir = Path(self.tmp.name)
        self.fds = []

    def tearDown(self):
        for fd in self.fds:
            os.close(fd)
        self.tmp.cleanup()

    def player(self, svg, **kw):
        src = self.dir / "icon.svg"
        src.write_text(svg)
        player = still(V.open_picture(src), src, **kw)
        self.fds += os.pipe()
        player.remote_w = self.fds[-1]
        return player

    def rendered(self, player):
        """The part asked for last, once render_parts() has it."""
        deadline = time.monotonic() + 10
        while not (player.detail and player.detail[0] == player.asked) and time.monotonic() < deadline:
            time.sleep(0.01)
        return player.detail[1]

    def reference(self, src, player, width):
        """The view in image mode as a whole width px raster of src would show it, over the checkerboard."""
        full = V.open_picture(src, width).rotate(-90 * player.rotation, expand=True)
        k = width / player.logical[0]
        crop = full.crop(tuple(round(v * k) for v in player.crop_box()))
        return V.flatten(crop, Image.frombytes("RGB", crop.size, V.checkerboard(crop.size))).tobytes()

    def close(self, a, b):
        return sum(abs(x - y) for x, y in zip(a, b)) / len(a) < 2

    def test_zoom_renders_the_view_alone(self):
        player = self.player(SQUARE, pixels=True)  # logical 1600 x 800, fit scale 0.5625
        player.zoom_n = 2  # 1.125 screen px per logical px: an 800 x 533 piece fills the 900 x 600 window
        self.assertEqual(placed(player)[:2], (900, 600))  # the raster enlarged meanwhile
        self.assertEqual(player.asked[3], (900, 600))
        self.assertEqual(self.rendered(player).size, (900, 600))
        w, h, rgb, *_ = placed(player)
        self.assertEqual(player.kitty[0][0], player.asked)  # the part, not the raster
        self.assertTrue(self.close(rgb, self.reference(player.path, player, 1800)))
        self.assertEqual((player.picture.width, player.logical), (1600, (1600, 800)))  # the raster stays

    def test_part_follows_the_window_not_the_zoom(self):
        player = self.player(SQUARE, pixels=True)
        player.zoom_n = player.zoom_limit()  # 6.4 screen px per logical px: a 10000 px raster, were it whole
        placed(player)
        self.assertEqual(self.rendered(player).size, (900, 600))

    def test_not_for_other_pictures_or_fit(self):
        player = self.player(SQUARE, pixels=True)
        placed(player)
        player.zoom_n = 1  # 0.8 screen px per logical px: the raster has enough
        placed(player)
        self.assertIsNone(player.asked)
        png = still(Image.new("RGB", (1800, 1200)), pixels=True)
        png.zoom_n = 6
        placed(png)
        self.assertIsNone(png.asked)

    def test_ascii_needs_only_what_chafa_samples(self):
        player = self.player(SQUARE)  # a 100 x 30 cell window: chafa samples 8 px a cell side, 800 x 240
        player.zoom_n = 1  # a 1131 x 754 piece of the logical picture: the raster has more than 240 px for it
        player.thumbnail()
        self.assertIsNone(player.asked)
        player.zoom_n = 6  # a 200 x 133 piece, wanted at 240 px high: 1.8 px per logical px
        self.assertEqual(player.thumbnail()[:2], (200, 133))  # the raster's own px meanwhile
        self.assertEqual(self.rendered(player).size, (360, 240))  # not the 6400 px of a whole 960 x 540 frame
        self.assertEqual(player.thumbnail()[:2], (360, 240))

    def test_ascii_part_follows_the_cell_grid(self):
        small, big = self.player(SQUARE), self.player(SQUARE, room=(2700, 1600))  # 100 x 30 cells; 300 x 80
        for player in (small, big):
            player.zoom_n = 6
            player.thumbnail()
            self.rendered(player)
        self.assertEqual(small.asked[3], (360, 240))  # 1.8 px per logical px: 240 px for 133 rows
        self.assertEqual(big.asked[3], (911, 540))  # 2.7: a 337 x 200 piece in the 960 x 540 frame at most

    def test_fit_does_not_grow(self):
        icon = '<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24"><rect width="12" height="24"/></svg>'
        player = self.player(icon, pixels=True, room=(3843, 2160))  # a 4K window; logical 1600 x 1600
        shown = lambda: placed(player, 427, 108)[:2]
        sizes = [shown()]
        for k in (b"+", b"+", b"-", b"-", b"+"):
            player.key(k)
            if player.asked and player.steps():
                self.rendered(player)
            sizes.append(shown())
        self.assertEqual(sizes, [(1600, 1600), (2263, 2160), (3200, 2160), (2263, 2160), (1600, 1600), (2263, 2160)])
        self.assertEqual(player.picture.width, 1600)

    def test_rotated_svg(self):
        corner = '<svg xmlns="http://www.w3.org/2000/svg" width="100" height="50"><rect width="30" height="20"/></svg>'
        for turns in range(4):
            player = self.player(corner, pixels=True)
            player.rotate(turns)
            player.zoom_n = 4  # 1.1 to 1.6 screen px per logical px, by the turn
            placed(player)
            self.rendered(player)
            w, h, rgb, *_ = placed(player)
            self.assertEqual(player.kitty[0][0], player.asked, turns)
            left, _, right, _ = player.crop_box()
            width = round(player.logical[0] * w / (right - left))  # the part's px per logical px, whole
            self.assertTrue(self.close(rgb, self.reference(player.path, player, width)), turns)


class KittyTerminalTest(unittest.TestCase):
    def test_detected_by_window_id_or_term(self):
        for env, want in (({"KITTY_WINDOW_ID": "3", "TERM": "xterm-256color"}, True), ({"TERM": "xterm-kitty"}, True),
                          ({"TERM": "xterm-256color"}, False), ({}, False)):
            with mock.patch.dict(os.environ, env, clear=True):
                self.assertEqual(V.kitty_terminal(), want, env)


class LabelTest(unittest.TestCase):
    def test_tone_label(self):
        self.assertEqual(V.tone_label(0), "0")
        self.assertEqual(V.tone_label(15), "15% darker")
        self.assertEqual(V.tone_label(-5), "5% brighter")


class TempData(unittest.TestCase):
    """A temp dir with the trash's data home inside it, so files and trash share a device; the real
    trash is never touched."""

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.dir = Path(self.tmp.name).resolve()
        self.data = self.dir / "data"
        self.trash = self.data / "Trash"
        patch = mock.patch.dict(os.environ, {"XDG_DATA_HOME": str(self.data)})
        patch.start()
        self.addCleanup(patch.stop)
        self.addCleanup(self.tmp.cleanup)

    def folder(self, name="pics", names=("a.png", "b.png", "c.png")):
        """A folder of small PNGs, each of its own size, and their paths."""
        d = self.dir / name
        d.mkdir(exist_ok=True)
        out = []
        for i, n in enumerate(names):
            Image.new("RGB", (40 + i, 30)).save(d / n)
            out.append(d / n)
        return out

    def listing(self, sub):
        return sorted(p.name for p in (self.trash / sub).iterdir()) if (self.trash / sub).exists() else []


class SiblingImagesTest(TempData):
    def test_a_symlink_lists_its_own_folder_not_the_targets(self):
        pics, other = self.folder("pics", ("l.png", "m.png")), self.folder("other", ("z.png", "zz.png"))
        pics[0].unlink()
        pics[0].symlink_to(other[0])
        self.assertEqual([f.name for f in V.sibling_images(pics[0])], ["l.png", "m.png"])
        self.assertEqual({f.parent for f in V.sibling_images(pics[0])}, {pics[0].parent})


class TrashTest(TempData):
    def test_trash_stores_file_and_info(self):
        d = self.dir / "my pics"
        d.mkdir()
        src = d / "a b#é.png"
        src.write_bytes(b"pixels")
        entry = V.trash(src)
        self.assertFalse(src.exists())
        self.assertEqual(entry.stored, self.trash / "files" / "a b#é.png")
        self.assertEqual(entry.stored.read_bytes(), b"pixels")
        lines = entry.info.read_text().splitlines()
        self.assertEqual(entry.info, self.trash / "info" / "a b#é.png.trashinfo")
        self.assertEqual(lines[0], "[Trash Info]")
        self.assertEqual(lines[1], "Path=" + str(d).replace(" ", "%20") + "/a%20b%23%C3%A9.png")
        self.assertRegex(lines[2], r"^DeletionDate=\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d$")
        self.assertEqual(len(lines), 3)
        self.assertEqual(os.stat(self.trash).st_mode & 0o777, 0o700)

    def test_unique_names(self):
        one, two = self.dir / "one", self.dir / "two"
        one.mkdir()
        two.mkdir()
        stored = []
        for d in (one, two, one, two):  # the same name from two folders, twice over
            (d / "x.png").write_bytes(d.name.encode())
            stored.append(V.trash(d / "x.png"))
        self.assertEqual([e.stored.name for e in stored], ["x.png", "x.2.png", "x.3.png", "x.4.png"])
        self.assertEqual([e.stored.read_bytes() for e in stored], [b"one", b"two", b"one", b"two"])
        self.assertEqual(self.listing("info"), [f"{n}.trashinfo" for n in ("x.2.png", "x.3.png", "x.4.png", "x.png")])
        self.assertIn("Path=" + str(two / "x.png"), stored[1].info.read_text())

    def test_name_taken_by_a_file_without_info(self):
        (self.trash / "files").mkdir(parents=True)
        (self.trash / "files" / "x.png").write_bytes(b"orphan")
        (self.dir / "x.png").write_bytes(b"new")
        entry = V.trash(self.dir / "x.png")
        self.assertEqual(entry.stored.name, "x.2.png")
        self.assertEqual((self.trash / "files" / "x.png").read_bytes(), b"orphan")

    def test_failed_move_leaves_the_file_and_no_info(self):
        (src,) = self.folder(names=("a.png",))
        with mock.patch.object(V.os, "rename", side_effect=OSError("denied")):
            with self.assertRaises(OSError):
                V.trash(src)
        self.assertTrue(src.exists())
        self.assertEqual((self.listing("files"), self.listing("info")), ([], []))
        with self.assertRaises(OSError):  # a file that is not there is no trash either
            V.trash(self.dir / "pics" / "nope.png")
        self.assertEqual(self.listing("info"), [])

    def test_undo_restores_bytes_and_removes_info(self):
        (src,) = self.folder(names=("a.png",))
        before = src.read_bytes()
        entry = V.trash(src)
        V.untrash(entry)
        self.assertEqual(src.read_bytes(), before)
        self.assertEqual((self.listing("files"), self.listing("info")), ([], []))

    def test_undo_does_not_overwrite(self):
        (src,) = self.folder(names=("a.png",))
        entry = V.trash(src)
        src.write_bytes(b"newer")
        with self.assertRaises(FileExistsError):
            V.untrash(entry)
        self.assertEqual(src.read_bytes(), b"newer")
        self.assertTrue(entry.stored.exists() and entry.info.exists())

    def test_undo_refuses_a_slot_reused_by_another_file(self):
        one, two = self.dir / "e", self.dir / "f"
        one.mkdir()
        two.mkdir()
        (one / "x.png").write_bytes(b"from e")
        (two / "x.png").write_bytes(b"from f")
        first = V.trash(one / "x.png")
        first.stored.rename(self.dir / "restored-elsewhere.png")  # what a file manager's restore does
        first.info.unlink()
        second = V.trash(two / "x.png")
        self.assertEqual(second.stored, first.stored)  # the name was free again
        with self.assertRaises(V.TrashGone):
            V.untrash(first)
        self.assertFalse((one / "x.png").exists())
        self.assertEqual(second.stored.read_bytes(), b"from f")
        self.assertTrue(second.info.exists())
        V.untrash(second)
        self.assertEqual((two / "x.png").read_bytes(), b"from f")

    def test_undo_when_the_trash_was_emptied_or_lost_its_info(self):
        a, b = self.folder(names=("a.png", "b.png"))
        gone, bare = V.trash(a), V.trash(b)
        gone.stored.unlink()
        gone.info.unlink()
        bare.info.unlink()
        for entry in (gone, bare):
            with self.assertRaises(V.TrashGone):
                V.untrash(entry)
        self.assertFalse(a.exists() or b.exists())
        self.assertTrue(bare.stored.exists())  # left alone

    def test_undo_without_hard_links_still_checks(self):
        (src,) = self.folder(names=("a.png",))
        entry = V.trash(src)
        with mock.patch.object(V.os, "link", side_effect=PermissionError):
            src.write_bytes(b"newer")
            with self.assertRaises(FileExistsError):
                V.untrash(entry)
            src.unlink()
            V.untrash(entry)
        self.assertTrue(src.exists())
        self.assertFalse(entry.stored.exists() or entry.info.exists())

    def test_device_and_mount_lookups(self):
        self.assertTrue(V.same_device(self.dir, self.trash))  # the trash does not exist yet: its first existing parent counts
        mount = V.mount_point(self.dir)
        self.assertTrue(mount.is_mount() and os.stat(mount).st_dev == os.stat(self.dir).st_dev)
        if os.path.isdir("/dev/shm") and os.stat("/dev/shm").st_dev != os.stat(self.dir).st_dev:
            self.assertFalse(V.same_device(Path("/dev/shm"), self.trash))

    def test_symlink_moves_itself(self):
        target = self.dir / "real.png"
        target.write_bytes(b"real")
        link = self.dir / "link.png"
        link.symlink_to(target)
        entry = V.trash(link)
        self.assertTrue(target.exists() and entry.stored.is_symlink())
        V.untrash(entry)
        self.assertTrue(link.is_symlink())


class TopdirTrashTest(TempData):
    """A file on another device than the home trash: faked, with a folder standing for its mount."""

    def setUp(self):
        super().setUp()
        self.top = self.dir / "mnt"
        (self.top / "sub").mkdir(parents=True)
        self.src = self.top / "sub" / "f č.png"
        self.src.write_bytes(b"x")
        for name, value in (("same_device", lambda *a: False), ("mount_point", lambda p: self.top)):
            patch = mock.patch.object(V, name, value)
            patch.start()
            self.addCleanup(patch.stop)
        self.uid = str(os.getuid())

    def check(self, folder):
        entry = V.trash(self.src)
        self.assertEqual(entry.stored, folder / "files" / "f č.png")
        self.assertEqual(entry.stored.read_bytes(), b"x")
        self.assertEqual(entry.info.read_text().splitlines()[1], "Path=sub/f%20%C4%8D.png")  # relative to the topdir
        self.assertEqual(self.listing("files"), [])  # the home trash stays unused
        return entry

    def test_dot_trash_uid_when_sticky(self):
        (self.top / ".Trash").mkdir()
        os.chmod(self.top / ".Trash", 0o1777)
        entry = self.check(self.top / ".Trash" / self.uid)
        self.assertEqual(os.stat(self.top / ".Trash" / self.uid).st_mode & 0o777, 0o700)
        V.untrash(entry)
        self.assertTrue(self.src.exists())

    def test_dot_trash_dash_uid_without_dot_trash(self):
        self.check(self.top / f".Trash-{self.uid}")
        self.assertEqual(os.stat(self.top / f".Trash-{self.uid}").st_mode & 0o777, 0o700)

    def test_dot_trash_not_sticky_is_passed_over(self):
        (self.top / ".Trash").mkdir()
        os.chmod(self.top / ".Trash", 0o777)
        self.check(self.top / f".Trash-{self.uid}")
        self.assertEqual(list((self.top / ".Trash").iterdir()), [])

    def test_dot_trash_symlink_is_passed_over(self):
        real = self.dir / "elsewhere"
        real.mkdir()
        os.chmod(real, 0o1777)
        (self.top / ".Trash").symlink_to(real)
        self.check(self.top / f".Trash-{self.uid}")
        self.assertEqual(list(real.iterdir()), [])

    @unittest.skipIf(os.getuid() == 0, "root can make the directory")
    def test_dot_trash_uid_that_cannot_be_made_falls_back(self):
        (self.top / ".Trash").mkdir()
        os.chmod(self.top / ".Trash", 0o1555)  # sticky, but not writable: no $uid directory can be made
        self.addCleanup(os.chmod, self.top / ".Trash", 0o1777)
        self.check(self.top / f".Trash-{self.uid}")

    def test_a_symlinked_uid_directory_is_refused(self):
        real = self.dir / "elsewhere"
        real.mkdir()
        (self.top / ".Trash").mkdir()
        os.chmod(self.top / ".Trash", 0o1777)
        (self.top / ".Trash" / self.uid).symlink_to(real)
        self.check(self.top / f".Trash-{self.uid}")  # the next candidate
        self.assertEqual(list(real.iterdir()), [])

    def test_a_symlinked_dash_uid_directory_is_refused(self):
        real = self.dir / "elsewhere"
        real.mkdir()
        (self.top / f".Trash-{self.uid}").symlink_to(real)
        with self.assertRaises(OSError):
            V.trash(self.src)
        self.assertEqual((list(real.iterdir()), self.src.read_bytes()), ([], b"x"))

    def test_a_symlinked_files_directory_is_refused(self):
        real = self.dir / "elsewhere"
        real.mkdir()
        (self.top / f".Trash-{self.uid}").mkdir()
        (self.top / f".Trash-{self.uid}" / "files").symlink_to(real)
        with self.assertRaises(OSError):
            V.trash(self.src)
        self.assertEqual(list(real.iterdir()), [])

    def test_a_directory_of_another_user_is_refused(self):
        (self.top / "sub" / "g.png").write_bytes(b"g")
        with mock.patch.object(V.os, "getuid", return_value=os.getuid() + 1):
            (self.top / f".Trash-{os.getuid() + 1}").mkdir()  # made by this user, so not the uid asked for
            with self.assertRaises(PermissionError):
                V.trash(self.top / "sub" / "g.png")
        self.assertEqual((self.top / "sub" / "g.png").read_bytes(), b"g")

    def test_same_device_picks_the_home_trash(self):
        with mock.patch.object(V, "same_device", lambda *a: True):
            entry = V.trash(self.src)
        self.assertEqual(entry.stored, self.trash / "files" / "f č.png")
        self.assertIn("Path=" + V.quote(str(self.src)), entry.info.read_text())  # absolute there


def folder_player(test, names=("a.png", "b.png", "c.png"), start=0, name="pics"):
    """A Player (as still() makes one) on a real folder of images, at names[start], and the paths."""
    paths = test.folder(name, names)
    return still(V.open_picture(paths[start]), paths[start]), paths


class TrashKeysTest(TempData):
    def test_d_trashes_and_shows_the_next(self):
        player, (a, b, c) = folder_player(self)
        player.key(b"d")
        self.assertEqual((player.path, player.toast, player.stale), (str(b), "trashed a.png", 2))
        self.assertEqual((a.exists(), (self.trash / "files" / "a.png").exists()), (False, True))
        self.assertEqual((player.empty, player.picture.size), (None, (41, 30)))
        self.assertEqual(len(player.trashed), 1)

    def test_d_on_the_last_image_wraps_round(self):
        player, (a, b, c) = folder_player(self, start=2)
        player.key(b"d")
        self.assertEqual((player.path, player.toast), (str(a), "trashed c.png"))
        player.key(b"d")  # a, then b is next
        self.assertEqual(player.path, str(b))

    def test_d_skips_files_that_do_not_open(self):
        player, (a, b, c) = folder_player(self)
        b.write_bytes(b"not a picture")
        player.key(b"d")
        self.assertEqual(player.path, str(c))

    def test_d_on_the_only_image_shows_the_empty_folder(self):
        player, (a,) = folder_player(self, names=("a.png",))
        player.key(b"d")
        self.assertEqual((player.empty, player.picture, player.toast), (a.parent, None, "trashed a.png"))
        self.assertEqual(player.stale, 2)
        out = self.screen(player)
        self.assertIn(b"no images in pics", out)
        self.assertIn(b"u undo  q quit", out)
        self.assertIn(b"\x1b[2J", out)
        self.assertIn(b"trashed a.png", out)  # the toast, on the status row

    def screen(self, player, clear=False):
        """What draw() writes."""
        out = io.BytesIO()
        with mock.patch.object(V.sys, "stdout", mock.Mock(buffer=out)), mock.patch.object(V.shutil, "get_terminal_size", lambda *a: os.terminal_size((80, 24))):
            player.draw(clear=clear)
        return out.getvalue()

    def test_empty_screen_in_image_mode_clears_kitty(self):
        player, _ = folder_player(self, names=("a.png",))
        player.pixels = True
        player.key(b"d")
        out = self.screen(player)
        self.assertIn(V.KITTY_CLEAR, out)
        self.assertNotIn(b"a=T", out)

    def test_empty_screen_without_undo_offers_quit_only(self):
        player, (a,) = folder_player(self, names=("a.png",))
        player.key(b"D")
        player.key(b"y")
        self.assertEqual(player.empty, a.parent)
        out = self.screen(player)
        self.assertIn(b"q quit", out)
        self.assertNotIn(b"u undo", out)
        player.key(b"u")
        self.assertEqual(player.toast, "nothing to undo")

    def test_u_from_the_empty_folder_shows_the_image(self):
        player, (a,) = folder_player(self, names=("a.png",))
        before = a.read_bytes()
        player.key(b"d")
        player.stale = 0
        player.key(b"u")
        self.assertEqual((player.empty, player.path, player.toast, player.stale), (None, str(a), "restored a.png", 2))
        self.assertEqual((a.read_bytes(), player.picture.size, player.trashed), (before, (40, 30), []))
        self.assertEqual((self.listing("files"), self.listing("info")), ([], []))
        self.assertTrue(player.thumbnail())  # and it draws

    def test_u_undoes_in_reverse_order(self):
        player, (a, b, c) = folder_player(self)
        player.key(b"d")
        player.key(b"d")
        self.assertEqual(player.path, str(c))
        player.key(b"u")
        self.assertEqual((player.path, player.toast, b.exists(), a.exists()), (str(b), "restored b.png", True, False))
        player.key(b"u")
        self.assertEqual((player.path, player.toast, a.exists()), (str(a), "restored a.png", True))
        player.key(b"u")
        self.assertEqual(player.toast, "nothing to undo")
        self.assertEqual(player.path, str(a))

    def test_u_does_not_overwrite_and_keeps_the_entry(self):
        player, (a, b, c) = folder_player(self)
        player.key(b"d")
        a.write_bytes(b"newer")
        player.key(b"u")
        self.assertEqual((player.toast, a.read_bytes(), player.path, len(player.trashed)), ("cannot restore: a.png exists", b"newer", str(b), 1))
        a.unlink()
        player.key(b"u")
        self.assertEqual((player.toast, player.path, player.trashed), ("restored a.png", str(a), []))

    def test_u_drops_an_entry_whose_file_is_gone(self):
        player, (a, b, c) = folder_player(self)
        player.key(b"d")
        player.key(b"d")
        for sub in ("files", "info"):  # the trash was emptied
            for f in (self.trash / sub).iterdir():
                if f.name.startswith("b"):
                    f.unlink()
        player.key(b"u")
        self.assertEqual((player.toast, len(player.trashed), b.exists()), ("b.png is no longer in the trash", 1, False))
        player.key(b"u")  # the entry under it is not blocked
        self.assertEqual((player.toast, player.path, a.exists(), player.trashed), ("restored a.png", str(a), True, []))
        player.key(b"u")
        self.assertEqual(player.toast, "nothing to undo")

    def test_u_keeps_the_other_files_trash_slot(self):
        player, (a, b, c) = folder_player(self)
        player.key(b"d")
        stored = self.trash / "files" / "a.png"
        stored.rename(self.dir / "moved.png")
        (self.trash / "info" / "a.png.trashinfo").unlink()
        other = self.dir / "other"
        other.mkdir()
        (other / "a.png").write_bytes(b"other")
        V.trash(other / "a.png")  # takes the slot a.png had
        player.key(b"u")
        self.assertEqual((player.toast, a.exists(), stored.read_bytes()), ("a.png is no longer in the trash", False, b"other"))
        self.assertTrue((self.trash / "info" / "a.png.trashinfo").exists())

    def test_d_and_capital_d_on_a_symlink_keep_the_target_in_the_list(self):
        for key, toast in ((b"d", "trashed y.png"), (b"D", "deleted y.png")):
            with self.subTest(key=key):
                d = self.dir / f"links{key.decode()}"
                d.mkdir()
                Image.new("RGB", (40, 30)).save(d / "x.png")
                (d / "y.png").symlink_to("x.png")
                player = still(V.open_picture(d / "y.png"), d / "y.png")
                player.key(key)
                if key == b"D":
                    player.key(b"y")
                self.assertEqual((player.empty, player.path, player.toast), (None, str(d / "x.png"), toast))
                self.assertEqual(((d / "x.png").exists(), (d / "y.png").is_symlink()), (True, False))

    def test_d_failing_changes_nothing(self):
        player, (a, b, c) = folder_player(self)
        with mock.patch.object(V, "trash", side_effect=OSError):
            player.key(b"d")
        self.assertEqual((player.toast, player.path, a.exists(), player.trashed), ("trash failed", str(a), True, []))

    def test_empty_folder_ignores_everything_but_u_q_f(self):
        player, (a,) = folder_player(self, names=("a.png",))
        player.key(b"d")
        player.toast = ""
        names = ("step", "rotate", "zoom_by", "pan_by", "copy_image", "screenshot", "open_page", "fullscreen", "undo_trash", "trash_image", "delete_image")
        with contextlib.ExitStack() as stack:
            calls = {n: stack.enter_context(mock.patch.object(player, n)) for n in names}
            keys = [b"h", b"l", b"j", b"k", b"\x1b[A", b"\x1b[B", b"\x1b[C", b"\x1b[D", b"+", b"=", b"-", b"v", b"y", b"s", b"r", b"d", b"D",
                    b"b", b"c", b"g", b"`", b" ", b"p", b"m", b"L", b"[", b"]", b"\\", b"\x7f", b"1", b"2", b"3", b"4", b"7", b"8", b"0", b"9", b".", b",", b"x"]
            for k in keys:
                self.assertIsNone(player.key(k), k)
            self.assertEqual([n for n, c in calls.items() if c.called], [])
            self.assertEqual((player.toast, player.empty, player.confirming, player.stale), ("", a.parent, False, 2))
            player.key(b"f")
            self.assertTrue(calls["fullscreen"].called)
            player.key(b"u")
            self.assertTrue(calls["undo_trash"].called)
            with self.assertRaises(SystemExit):
                player.key(b"q")
        self.screen(player)  # still drawable

    def test_s_after_the_empty_folder_and_u_in_image_mode(self):
        player, (a,) = folder_player(self, names=("a.png",))
        player.pixels = True
        self.addCleanup(player.drop_shm)
        player.key(b"d")
        player.key(b"u")
        self.screen(player)
        with mock.patch.object(player, "screenshot") as shot:
            player.key(b"s")
        shot.assert_called_once()

    def test_other_state_survives_the_empty_folder(self):
        player, _ = folder_player(self, names=("a.png",))
        player.key(b"d")
        self.assertEqual(player.kitty, (None, b""))
        player.key(b"u")
        self.assertEqual((player.rotation, player.zoom_n), (0, 0))


class DeleteKeysTest(TempData):
    def test_prompt_waits_and_names_the_file(self):
        player, (a, b, c) = folder_player(self)
        player.say("earlier")
        player.key(b"D")
        self.assertTrue(player.confirming)
        player.toast_until = 0.0
        for _ in range(2):  # however long
            self.assertIn(b"delete a.png permanently? y/n", player.status(80, 24))
        self.assertTrue(a.exists())
        self.assertEqual(player.stale, 1)

    def test_prompt_shortens_a_long_name_not_the_question(self):
        player, (a, b, c) = folder_player(self)
        player.path = str(self.dir / ("n" * 200 + ".png"))
        player.confirming = True
        self.assertIn(b"permanently? y/n", player.status(40, 24))

    def test_y_deletes_for_good_and_shows_the_next(self):
        player, (a, b, c) = folder_player(self)
        player.key(b"D")
        player.key(b"y")
        self.assertEqual((a.exists(), player.path, player.toast, player.confirming), (False, str(b), "deleted a.png", False))
        self.assertEqual((self.listing("files"), self.listing("info"), player.trashed), ([], [], []))

    def test_delete_leaves_the_undo_stack(self):
        player, (a, b, c) = folder_player(self)
        player.key(b"d")
        player.key(b"D")
        player.key(b"y")
        self.assertEqual((player.path, len(player.trashed)), (str(c), 1))
        player.key(b"u")
        self.assertEqual((player.path, a.exists(), b.exists()), (str(a), True, False))

    def test_other_key_keeps_it_and_is_used_up(self):
        for i, k in enumerate((b"n", b"l", b"q", b"d", b"u", b"\x1b", b"Y", b"\r")):
            with self.subTest(k=k):
                player, (a, b, c) = folder_player(self, name=f"p{i}")
                with mock.patch.object(player, "step") as step:
                    player.key(b"D")
                    player.key(k)
                    step.assert_not_called()
                self.assertEqual((a.exists(), player.path, player.toast, player.confirming), (True, str(a), "kept a.png", False))
                self.assertEqual((player.trashed, self.listing("files")), ([], []))

    def test_y_does_not_copy_while_asking(self):
        player, _ = folder_player(self)
        with mock.patch.object(player, "copy_image") as copy:
            player.key(b"D")
            player.key(b"y")
            copy.assert_not_called()
            player.key(b"y")  # no question now: it copies (this is the next image)
            copy.assert_called_once()

    def test_deleting_the_last_image_shows_the_empty_folder(self):
        player, (a,) = folder_player(self, names=("a.png",))
        player.key(b"D")
        player.key(b"y")
        self.assertEqual((player.empty, a.exists(), player.toast), (a.parent, False, "deleted a.png"))

    def test_failure_is_said(self):
        player, (a, b, c) = folder_player(self)
        player.key(b"D")
        with mock.patch.object(V.Path, "unlink", side_effect=PermissionError):
            player.key(b"y")
        self.assertEqual((player.toast, player.path, a.exists(), player.confirming), ("delete failed", str(a), True, False))

    def test_d_and_capital_d_do_nothing_for_video(self):
        player = bare_player()
        for k in (b"d", b"D", b"u"):
            player.key(k)
        self.assertFalse(player.confirming)


class MinuteSeekTest(unittest.TestCase):
    def test_j_k_seek_a_minute_in_video_and_audio(self):
        for has_video in (True, False):
            player = bare_player(has_video=has_video)
            with mock.patch.object(player, "seek") as seek:
                player.key(b"k")
                seek.assert_called_once_with(60)
                seek.reset_mock()
                player.key(b"j")
                seek.assert_called_once_with(-60)
            self.assertEqual(V.SEEKS[b"k"], V.SEEKS[b"\x1b[A"])  # as the Up and Down arrows
            self.assertEqual(V.SEEKS[b"j"], V.SEEKS[b"\x1b[B"])

    def test_audio_only_accepts_them(self):
        self.assertTrue({b"j", b"k", b"\\"} <= V.AUDIO_KEYS)

    def test_held_keys_add_up_in_one_seek(self):
        player = bare_player()
        player.winch = player.remote_r = -1
        with mock.patch.object(player, "seek") as seek, mock.patch.object(player, "flush"), mock.patch.object(V.select, "select", return_value=([0], [], [])), \
                mock.patch.object(V.os, "read", return_value=b"kkj"):
            player.poll(0)
        seek.assert_called_once_with(60)

    def test_stills_keep_j_k_for_the_picture(self):
        player = still(Image.new("RGB", (1800, 1200)))
        player.seek = mock.Mock()
        player.key(b"k")
        player.key(b"j")
        player.key(b"j")
        self.assertEqual(player.rotation, 3)
        player.seek.assert_not_called()
        player.key(b"+")
        player.key(b"k")  # zoomed: pans
        self.assertEqual(player.rotation, 3)
        player.seek.assert_not_called()


class SpeedResetTest(unittest.TestCase):
    def test_backslash_resets_the_speed(self):
        for has_video in (True, False):
            for reset in (b"\\", b"\x7f"):
                player = bare_player(has_video=has_video)
                player.key(b"}")
                self.assertEqual(player.speed, 200)
                player.key(reset)
                self.assertEqual((player.speed, player.toast), (100, "speed 100%"), (has_video, reset))


if __name__ == "__main__":
    unittest.main()
