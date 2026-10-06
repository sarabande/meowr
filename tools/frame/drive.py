#!/usr/bin/env python3
"""Drive meowr in a real terminal and read the frame back (#122, #123, #124).

The fidelity harness proves what a keypress does to a buffer. This proves what
the *frame* shows, which is a different question and the one three livability
tickets turned on: exactly one of four things owns the bottom of the frame, and
being wrong about which is invisible to every assertion about buffer state.

Two things it does that are not optional, both learnt the hard way:

  * It sets the window size on the pty. Without `TIOCSWINSZ` ratatui draws into
    a 0x0 frame and the capture is nothing but cursor escapes -- which reads as
    a broken feature rather than a broken harness.
  * It renders with `pyte` rather than stripping escapes with a regex. A regex
    over the raw stream gives interleaved partial redraws that cannot be read;
    `screen.display` gives the frame as it actually stands.

    tools/frame/drive.py scenario.py
    tools/frame/drive.py --rows 40 --cols 120 --cwd /tmp/repro scenario.py

A scenario is ordinary Python run with `frame` already in scope:

    frame.wait(25)                       # rust-analyzer indexing
    frame.keys(" ffmain\r")
    frame.wait(3)
    frame.keys("wwh")
    frame.show("the point rests on Widget")

and it can say what it expects rather than only print it (#170):

    frame.expect("SLOW SERVER SAYS")     # drawn within ten seconds, or fail
    frame.refute("SLOW SERVER SAYS", 4)  # drawn at no moment for four seconds

A failed expectation raises `Failure` with the frame in its message. The
checked-in scenarios are run by `run.py`, which builds each its own project.

Needs `pyte` (`uv pip install pyte --target tools/frame/lib`) and a release
build at `target/release/meowr` unless `--binary` says otherwise.
"""

import argparse
import fcntl
import os
import pty
import select
import signal
import struct
import sys
import termios
import time

sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "lib"))

try:
    import pyte
except ImportError:
    sys.exit(
        "drive.py needs pyte: uv pip install pyte --target tools/frame/lib\n"
        "A regex over the escape stream is not a substitute -- see the docstring."
    )


class Failure(AssertionError):
    """An expectation about the frame that did not hold, with the frame."""


class Frame:
    """A meowr under a pty, and the screen it is drawing."""

    def __init__(self, binary, args, cwd, config, rows, cols, env=None):
        # Kept so a scenario can ask the filesystem directly. The screen is the
        # only witness for what meowr says; for what it did to a file, a picker
        # is the wrong one -- the Project's picker lists what git tracks, so a
        # deleted file is still on it (#179).
        self.cwd = cwd
        # And where its config is, for the rows that are about that (#181): the
        # scenario writes beside `init.scm`, or takes it away to be the machine
        # that has never had one.
        self.config = config
        self.pid, self.fd = pty.fork()
        if self.pid == 0:
            if config:
                os.environ["XDG_CONFIG_HOME"] = config
            os.environ.update(env or {})
            os.environ["TERM"] = "xterm-256color"
            os.chdir(cwd)
            os.execv(binary, ["meowr", *args])
        fcntl.ioctl(self.fd, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, 0, 0))
        os.set_blocking(self.fd, False)
        self.screen = pyte.Screen(cols, rows)
        self.stream = pyte.Stream(self.screen)

    def wait(self, seconds):
        """Let the editor draw for a while. Also the debounce every idle timer
        measures, so this is how long the point has been sitting still."""
        self._draw_until(time.time() + seconds, lambda: False)

    def _draw_until(self, end, done):
        """Read what the editor draws until `end`, or until `done()` holds of
        the screen -- asked after every chunk, so a frame that is drawn and
        drawn over before the next look is still seen."""
        while time.time() < end:
            if done():
                return True
            ready, _, _ = select.select([self.fd], [], [], 0.05)
            if not ready:
                continue
            try:
                chunk = os.read(self.fd, 65536)
            except OSError:
                return done()
            if not chunk:
                return done()
            self.stream.feed(chunk.decode("utf-8", "replace"))
        return done()

    def drawn(self, text):
        """Whether `text` is anywhere on the screen as it stands."""
        return any(text in line for line in self.screen.display)

    def expect(self, text, within=10):
        """Wait for `text` to be drawn, failing after `within` seconds."""
        if not self._draw_until(time.time() + within, lambda: self.drawn(text)):
            raise Failure(f"never drawn within {within}s: {text!r}\n{self.picture()}")

    def refute(self, text, seconds):
        """Fail if `text` is drawn at any moment over the next `seconds`."""
        if self._draw_until(time.time() + seconds, lambda: self.drawn(text)):
            raise Failure(f"drawn, and should not have been: {text!r}\n{self.picture()}")

    def picture(self):
        """The frame as text, for a failure to show."""
        return "\n".join("  |" + line.rstrip() for line in self.screen.display if line.strip())

    def keys(self, text):
        """Send keys. Sent as one write when timing matters: typing a picker
        filter slower than `preview-delay` fires a preview, and a preview swaps
        the window off the buffer under test."""
        os.write(self.fd, text.encode())

    def show(self, label=""):
        print(f"\n=== {label}" if label else "")
        for line in self.screen.display:
            if line.strip():
                print("  |" + line.rstrip())
        sys.stdout.flush()

    def quit(self):
        """`SPC q Q`: quit losing whatever is unsaved, which in a scenario is
        the point -- `SPC q q` would refuse and leave the process up.

        Escape first, so the keys are not typed into a picker a scenario left
        open. Then waited on, and killed if it lingers: a meowr still writing
        its recent files after this returns would write into a directory the
        caller has gone on to remove."""
        self.keys("\x1b")
        time.sleep(0.2)
        self.keys(" qQ")
        deadline = time.time() + 3
        while time.time() < deadline:
            self._draw_until(time.time() + 0.1, lambda: False)
            pid, _ = os.waitpid(self.pid, os.WNOHANG)
            if pid:
                break
        else:
            os.kill(self.pid, signal.SIGKILL)
            os.waitpid(self.pid, 0)
        os.close(self.fd)


def main():
    parser = argparse.ArgumentParser(description="Drive meowr and read the frame back.")
    parser.add_argument("scenario", help="a Python file run with `frame` in scope")
    parser.add_argument("--binary", default="target/release/meowr")
    parser.add_argument("--cwd", default=".", help="where meowr runs; a repository, usually")
    parser.add_argument("--config", help="an XDG_CONFIG_HOME holding meowr/init.scm")
    parser.add_argument("--rows", type=int, default=24)
    parser.add_argument("--cols", type=int, default=96)
    parser.add_argument("args", nargs="*", help="arguments for meowr itself")
    options = parser.parse_args()

    frame = Frame(
        binary=os.path.abspath(options.binary),
        args=options.args,
        cwd=options.cwd,
        config=os.path.abspath(options.config) if options.config else None,
        rows=options.rows,
        cols=options.cols,
    )
    source = open(options.scenario).read()
    try:
        exec(compile(source, options.scenario, "exec"), {"frame": frame})
    finally:
        frame.quit()


if __name__ == "__main__":
    main()
