"""`SPC f D` deletes this file (#179).

Checked here because the two halves are in different layers and only the real
binary has both: the session refuses, says what it would delete and decides
what happens to the buffer, and the filesystem is the one that answers whether
the file is actually gone.

The screen is not asked that last question. The Project's picker lists what git
tracks, so a file this deletes is still on it; `frame.cwd` is asked instead.

`other.rs` is opened with an auto-save already beside it -- what #37 leaves
after a crash -- because an unmodified buffer is the only one that can have one
without the timer writing another the moment its file goes.
"""

import os

FILES = {
    "main.rs": "fn main() {}\n",
    "other.rs": "fn other() {}\n",
    "#other.rs#": "fn other() { unrecovered() }\n",
}
OPEN = "main.rs"
# An auto-save soon enough that a buffer which survives its file writes one.
CONFIG = "(set-option 'auto-save-timeout 300)"
# The sentences are long, and a wrapped one is not the one that was said.
COLS = 120


def run(frame):
    def on_disk(name):
        return os.path.exists(os.path.join(frame.cwd, name))

    # Typed into, so this buffer is the modified one.
    frame.keys("sx")
    frame.wait(0.3)
    frame.keys("\x1b")
    frame.expect("NORMAL * main.rs")

    # Alone, it deletes nothing, and names the file and the way past.
    frame.keys(" fD")
    frame.expect("main.rs would be deleted, not trashed, and nothing here can undo it.")
    frame.expect("SPC u SPC f D to delete it.")
    assert on_disk("main.rs"), "nothing should have been deleted"

    # The go-ahead. A modified buffer keeps its text, because that text is now
    # the only copy of the work anywhere.
    frame.keys(" u fD")
    frame.expect("Deleted main.rs.")
    frame.expect(
        "This buffer is still open and modified, so its text is the only copy of that work left."
    )
    frame.expect("NORMAL * main.rs")
    frame.expect("xfn main() {}")
    assert not on_disk("main.rs"), "the file should be gone"
    # The auto-save went with the file. Typing again writes another beside the
    # name the buffer still holds, which is the truth: that work is unsaved,
    # and now it is unsaved with nowhere to be saved to.
    assert not on_disk("#main.rs#"), "the auto-save should have gone with the file"
    frame.keys("sy")
    frame.wait(0.3)
    frame.keys("\x1b")
    frame.wait(2)
    assert on_disk("#main.rs#"), "the surviving buffer's work should still be kept"

    # An unmodified buffer goes with its file, and so does the auto-save that
    # was sitting beside it. `SPC f F` rather than the buffer picker: it is not
    # open yet. Typing the name puts the file above that auto-save.
    frame.keys(" fF")
    frame.expect("find: ")
    frame.keys("other\r")
    frame.expect("NORMAL other.rs")
    assert on_disk("#other.rs#"), "the auto-save this is about"

    frame.keys(" u fD")
    frame.expect("Deleted other.rs.")
    # No second sentence: there is no buffer left to say anything about.
    # Back in the one that survived, which is the only one there is.
    frame.expect("NORMAL * main.rs")
    frame.wait(1)
    assert not on_disk("other.rs"), "the file should be gone"
    assert not on_disk("#other.rs#"), "the auto-save should have gone with it"
