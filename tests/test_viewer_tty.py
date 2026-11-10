"""ascii-viewer run in a pseudo-terminal, as a user drives it: keys in, screen out."""

import fcntl
import os
import pty
import select
import struct
import tempfile
import termios
import time
import unittest
from pathlib import Path

from PIL import Image
from support import ROOT, gradient

VIEWER = str(ROOT / "bin" / "ascii-viewer")


KITTY_OK = b"\x1b_Gi=31;OK\x1b\\\x1b[?62;22c"  # kitty's reply to the viewer's file probe, then to device attributes
# kitty's reply when the file is not there for it to read (a kitty on another machine), then to device attributes
KITTY_EBADF = (b"\x1b_Gi=31;EBADF:Failed to open file for graphics transmission: /dev/shm/tty-graphics-protocol-x with error:"
               b" [2] No such file or directory\x1b\\\x1b[?62;22c")


class Session:
    """The viewer on path in a 200x60 pty with 9x20 px cells, kitty's variables unset and TERM not kitty, extra in
    its environment. reply answers the viewer's kitty probe (kitty_reads_files), as kitty does by default, delay
    seconds after it."""

    def __init__(self, path, reply=KITTY_OK, delay=0.0, **extra):
        env = {**{k: v for k, v in os.environ.items() if not k.startswith("KITTY")}, "TERM": "xterm-256color", **extra}
        self.pid, self.fd = pty.fork()
        if self.pid == 0:
            os.execve(VIEWER, [VIEWER, str(path)], env)
        fcntl.ioctl(self.fd, termios.TIOCSWINSZ, struct.pack("HHHH", 60, 200, 1800, 1200))
        self.output, self.reply, self.delay, self.due = b"", reply, delay, None

    def read(self, seconds):
        """Collect the screen output for that long, answering the probe when it is due; False once the viewer has
        gone."""
        end = time.monotonic() + seconds
        while time.monotonic() < end:
            if self.reply and self.due is not None and time.monotonic() >= self.due:
                os.write(self.fd, self.reply)
                self.reply = None
            if select.select([self.fd], [], [], 0.05)[0]:
                try:
                    data = os.read(self.fd, 1 << 16)
                except OSError:
                    return False
                if not data:
                    return False
                self.output += data
                if self.due is None and b"\x1b_Gi=31," in self.output:
                    self.due = time.monotonic() + self.delay
        return True

    def send(self, keys):
        os.write(self.fd, keys)

    def wait(self, limit):
        """Seconds until the viewer exits, at most limit; then its exit code."""
        start = time.monotonic()
        while self.read(0.05) and time.monotonic() - start < limit:
            pass
        elapsed = time.monotonic() - start
        os.kill(self.pid, 9) if elapsed >= limit else None
        _, status = os.waitpid(self.pid, 0)
        os.close(self.fd)
        return elapsed, os.waitstatus_to_exitcode(status)


class ViewerTtyTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.tmp = tempfile.TemporaryDirectory()
        cls.image = Path(cls.tmp.name, "pic.png")
        gradient(800, 400).save(cls.image)

    @classmethod
    def tearDownClass(cls):
        cls.tmp.cleanup()

    def test_held_key_backlog_does_not_block(self):
        s = Session(self.image)
        s.read(1.5)  # first frame
        s.send(b"3" * 300 + b"q")
        elapsed, code = s.wait(10)
        self.assertEqual(code, 0)
        self.assertLess(elapsed, 1.0)

    def test_held_zoom_and_pan_do_not_block(self):
        s = Session(self.image)
        s.read(1.5)
        s.send(b"+++" + b"l\x1b[B" * 150 + b"+-" * 50 + b"q")
        elapsed, code = s.wait(10)
        self.assertEqual(code, 0)
        self.assertLess(elapsed, 1.0)

    def test_screenshot_writes_beside_the_image(self):
        out = self.image.with_name("pic-ascii.png")
        s = Session(self.image)
        s.read(1.5)
        s.send(b"3s")
        deadline = time.monotonic() + 20
        while not out.exists() and time.monotonic() < deadline:
            s.read(0.2)
        s.send(b"q")
        s.wait(10)
        self.assertTrue(out.exists())

    def test_ascii_by_default_and_zoom_toasts(self):
        s = Session(self.image)
        s.read(1.5)
        self.assertFalse(b"a=T" in s.output)  # no kitty: no image placed
        s.send(b"+")
        s.read(1.0)
        # the 800 x 400 picture fits 1800 x 1200 px 2.25 times over; chafa fills the cells either way,
        # so the steps up to 2.25 are passed over and the first press crops
        self.assertTrue(b"zoom 283%" in s.output and b"zoom 141%" not in s.output)
        s.send(b"++l-")
        s.read(1.0)
        s.send(b"----")
        s.read(1.0)
        s.send(b"q")
        s.wait(10)
        self.assertTrue(b" fit " in s.output)

    def test_kitty_opens_on_the_image_itself(self):
        s = Session(self.image, KITTY_WINDOW_ID="1")
        s.read(1.5)
        self.assertTrue(b"\x1b_Ga=T,f=24,t=t,s=800,v=400" in s.output)  # 800 x 400 px, not enlarged
        s.send(b"+")
        s.read(1.0)
        self.assertTrue(b"zoom 141%" in s.output)
        s.send(b"v")  # to ASCII
        s.read(1.0)
        s.send(b"q")
        elapsed, code = s.wait(10)
        self.assertEqual(code, 0)

    def test_a_resize_redraws_at_the_new_size(self):
        s = Session(self.image, KITTY_WINDOW_ID="1")
        s.read(1.5)
        self.assertIn(b"a=T,f=24,t=t,s=800,v=400", s.output)
        before = len(s.output)
        fcntl.ioctl(s.fd, termios.TIOCSWINSZ, struct.pack("HHHH", 20, 50, 450, 400))  # 50 x 20 cells, still 9 x 20 px
        s.read(1.0)
        self.assertIn(b"a=T,f=24,t=t,s=450,v=225", s.output[before:])  # the 800 x 400 picture fitted to 450 px
        s.send(b"3")
        s.read(0.5)
        self.assertIn(b"\x1b[20;1H\x1b[7m level 5% darker", s.output[before:])  # the toast on the new bottom row
        s.send(b"q")
        elapsed, code = s.wait(10)
        self.assertEqual(code, 0)

    def test_kitty_that_cannot_read_the_file_gets_ascii(self):
        for reply in (b"\x1b_Gi=31;EBADF:could not read\x1b\\\x1b[?62;22c", None):  # an error, or no answer at all
            s = Session(self.image, reply=reply, KITTY_WINDOW_ID="1")
            s.read(2.5)
            s.send(b"q")
            elapsed, code = s.wait(10)
            self.assertEqual(code, 0, reply)
            self.assertIn(b"a=q,t=t", s.output)
            self.assertNotIn(b"a=T", s.output, reply)  # no image placed: the ASCII picture instead

    def test_a_late_kitty_reply_is_no_keys(self):
        # kitty's answer after the viewer stopped waiting for it (kitty over a slow link): among its letters are d
        # (trash), s (screenshot), r (render page), f (fullscreen) and l (next image); none may act
        root = Path(self.tmp.name, "late")
        folder, data, stubs = root / "pics", root / "data", root / "bin"
        for d in (folder, data, stubs):
            d.mkdir(parents=True)
        for i in range(3):
            gradient(80, 40).save(folder / f"{i}.png")
        for name in ("hyprctl", "wl-copy"):  # never the real ones: f would toggle this window's fullscreen
            (stubs / name).write_text(f'#!/bin/sh\necho {name} >> {root}/calls\ncat > /dev/null\n')
            (stubs / name).chmod(0o755)
        s = Session(folder / "0.png", KITTY_EBADF, delay=1.6, KITTY_WINDOW_ID="1", XDG_DATA_HOME=str(data),
                    PATH=f"{stubs}:{os.environ['PATH']}")
        s.read(3.5)  # the probe gives up after 1 s; its answer comes after 1.6
        s.send(b"q")
        elapsed, code = s.wait(10)
        self.assertEqual(code, 0)  # q quit: no render page took it
        self.assertEqual(sorted(p.name for p in folder.iterdir()), ["0.png", "1.png", "2.png"])  # no trash, no screenshot
        self.assertEqual((list(data.iterdir()), (root / "calls").exists()), ([], False))
        self.assertNotIn(b"\x1b[7m", s.output)  # no toast: nothing was done

    def test_y_copies_the_image_itself(self):
        bin_dir, out = Path(self.tmp.name, "bin"), Path(self.tmp.name, "clipboard")
        bin_dir.mkdir()
        stub = bin_dir / "wl-copy"
        stub.write_text(f'#!/bin/sh\nprintf "%s\\n" "$*" > {out}.args\ncat > {out}\n')
        stub.chmod(0o755)
        s = Session(self.image, PATH=f"{bin_dir}:{os.environ['PATH']}")
        s.read(1.5)
        s.send(b"y")
        deadline = time.monotonic() + 10
        while b"copied" not in s.output and time.monotonic() < deadline:
            s.read(0.2)
        s.send(b"q")
        s.wait(10)
        self.assertIn(b"copied 800x400", s.output)
        self.assertEqual(Path(f"{out}.args").read_text(), "--type image/png\n")
        self.assertEqual(Image.open(out).tobytes(), Image.open(self.image).tobytes())


class TrashTtyTest(unittest.TestCase):
    """d, u and the empty folder, as a user meets them; the trash is a temp dir's."""

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        root = Path(self.tmp.name)
        self.pics, self.trash = root / "pics", root / "data" / "Trash"
        self.pics.mkdir()
        self.a, self.b = self.pics / "a.png", self.pics / "b.png"
        gradient(800, 400).save(self.a)  # kitty is told the size of the image it is sent: 800x400, then 400x800
        gradient(400, 800).save(self.b)

    def expect(self, s, text, since=0, limit=10):
        """Read until text shows in the output after offset since; the output's length then."""
        end = time.monotonic() + limit
        while text not in s.output[since:] and time.monotonic() < end:
            s.read(0.1)
        self.assertIn(text, s.output[since:])
        return len(s.output)

    def test_d_d_u_q(self):
        s = Session(self.a, KITTY_WINDOW_ID="1", XDG_DATA_HOME=str(self.trash.parent))
        self.expect(s, b"s=800,v=400")  # a.png
        before = len(s.output)
        s.send(b"d")
        self.expect(s, b"trashed a.png", before)
        self.expect(s, b"s=400,v=800", before)  # b.png is on screen now
        self.assertFalse(self.a.exists())
        self.assertTrue((self.trash / "files" / "a.png").exists())
        before = len(s.output)
        s.send(b"d")
        self.expect(s, b"trashed b.png", before)
        self.expect(s, b"no images in pics", before)
        self.expect(s, b"u undo  q quit", before)
        self.assertEqual(sorted(p.name for p in self.pics.iterdir()), [])
        s.send(b"jkl+-\x1b[A")  # nothing to act on
        s.read(0.3)
        before = len(s.output)
        s.send(b"u")
        self.expect(s, b"restored b.png", before)
        self.expect(s, b"s=400,v=800", before)  # and b.png is back on screen
        s.send(b"q")
        elapsed, code = s.wait(10)
        self.assertEqual(code, 0)
        self.assertEqual(sorted(p.name for p in self.pics.iterdir()), ["b.png"])
        self.assertEqual(sorted(p.name for p in (self.trash / "files").iterdir()), ["a.png"])
        self.assertEqual(sorted(p.name for p in (self.trash / "info").iterdir()), ["a.png.trashinfo"])
        self.assertIn(f"Path={self.a}", (self.trash / "info" / "a.png.trashinfo").read_text())

    def test_capital_d_asks_then_deletes(self):
        s = Session(self.a, XDG_DATA_HOME=str(self.trash.parent))
        s.read(1.5)
        before = len(s.output)
        s.send(b"D")
        self.expect(s, b"delete a.png permanently? y/n", before)
        s.read(2.0)  # no timeout: the question outlasts a toast
        before = len(s.output)
        s.send(b"n")
        self.expect(s, b"kept a.png", before)
        self.assertTrue(self.a.exists())
        before = len(s.output)
        s.send(b"Dy")
        self.expect(s, b"deleted a.png", before)
        s.send(b"q")
        s.wait(10)
        self.assertEqual(sorted(p.name for p in self.pics.iterdir()), ["b.png"])
        self.assertFalse(self.trash.exists())


if __name__ == "__main__":
    unittest.main()
