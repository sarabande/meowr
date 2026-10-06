"""`SPC q F` kills every buffer (#176).

Checked here because the row is three of Doom's steps in one key and the middle
one is the host's: `save-some-buffers` hands its writes out and the kill waits
for them, so under a pty this is the only place the whole sequence happens in
the order a person would see it.

The refusal is the interesting half. `save-some-buffers` writes everything it
can without asking, so what is left modified afterwards is work with nowhere to
go -- and killing that is the one thing this row will not do unasked.
"""

import os

FILES = {"main.rs": "fn main() {}\n", "other.rs": "fn other() {}\n"}
OPEN = "main.rs"
# The refusal names a buffer and a key, and a wrapped sentence is not the one
# that was said.
COLS = 120


def run(frame):
    def on_disk(name):
        with open(os.path.join(frame.cwd, name)) as handle:
            return handle.read()

    # A second buffer, and a second window, so there is something for both
    # `delete-other-windows` and the kill to do.
    frame.keys(" fF")
    frame.expect("find: ")
    frame.keys("other\r")
    frame.expect("NORMAL other.rs")
    frame.keys("\x18\x32")  # C-x 2
    frame.wait(0.5)

    # Typed into, so `save-some-buffers` has a write to make.
    frame.keys("sx")
    frame.wait(0.3)
    frame.keys("\x1b")
    frame.expect("NORMAL * other.rs")

    frame.keys(" qF")
    frame.expect("Saved 1 buffer, killed 2.")
    frame.expect("NORMAL *scratch*")
    assert on_disk("other.rs") == "xfn other() {}\n", "the save went out before the kill"

    # Work with nowhere to go stops the whole thing, and says what and how.
    frame.keys("snotes")
    frame.wait(0.3)
    frame.keys("\x1b")
    frame.expect("NORMAL * *scratch*")
    frame.keys(" qF")
    frame.expect("*scratch* has no file. SPC u SPC q F to kill them anyway.")
    frame.expect("NORMAL * *scratch*", within=1)
    frame.expect("notes")

    # And the go-ahead takes it.
    frame.keys(" u qF")
    frame.expect("Killed 1 buffer; *scratch* has no file.")
    frame.expect("NORMAL *scratch*")
