"""`SPC f m` moves this file and `SPC f C` copies it (#178).

The first commands that change the filesystem under the editor. Checked here
because what they do is only half in the session: where a typed name lands --
a directory, a missing parent, something already there -- is the host's answer.
"""

FILES = {"main.rs": "fn main() {}\n", "other.rs": "fn other() {}\n"}
OPEN = "main.rs"
# An auto-save soon enough to watch it move with the file.
CONFIG = "(set-option 'auto-save-timeout 300)"
COLS = 120


def retype(frame, was, name):
    """Clear the seeded path and type another."""
    frame.keys("\x7f" * len(was) + name + "\r")


def files(frame):
    """What the Project picker lists, which is what is on disk."""
    frame.keys("  ")
    frame.expect("project:")
    frame.wait(1)
    shown = "\n".join(frame.screen.display)
    frame.keys("\x1b")
    frame.wait(0.3)
    return shown


def run(frame):
    # Typed into, so there is an auto-save beside it to follow the file.
    frame.keys("sx")
    frame.wait(0.3)
    frame.keys("\x1b")
    frame.wait(1)
    assert "#main.rs#" in files(frame), "an auto-save was written to follow"

    # Moved into a directory that does not exist yet.
    frame.keys(" fm")
    frame.expect("Move file to: main.rs")
    retype(frame, "main.rs", "src/main.rs")
    frame.expect("Moved main.rs to src/main.rs, creating src/.")
    frame.expect("NORMAL * src/main.rs")
    frame.wait(1)
    listed = files(frame)
    assert "#main.rs#" not in listed, f"the auto-save stayed behind:\n{listed}"

    # Copied, and the copy is what you are looking at.
    frame.keys(" fC")
    frame.expect("Copy file to: src/main.rs")
    retype(frame, "src/main.rs", "src/copy.rs")
    frame.expect("Copied src/main.rs to src/copy.rs.")
    frame.expect("NORMAL src/copy.rs")
    frame.expect("fn main() {}")
    # The buffer copied from is where it was, with its own unsaved text.
    frame.keys(" .main\r")
    frame.expect("NORMAL * src/main.rs")
    frame.expect("xfn main() {}")

    # A name that is taken is refused, and `SPC u` goes ahead anyway.
    frame.keys(" .copy\r")
    frame.expect("NORMAL src/copy.rs")
    frame.keys(" fm")
    retype(frame, "src/copy.rs", "other.rs")
    frame.expect("other.rs exists. SPC u SPC f m to move it there anyway.")
    frame.expect("NORMAL src/copy.rs", within=1)
    frame.keys(" u fm")
    retype(frame, "src/copy.rs", "other.rs")
    frame.expect("Moved src/copy.rs to other.rs.")
    frame.expect("NORMAL other.rs")

    # A write that cannot happen moves nothing: `other.rs` is a file, so
    # nothing can be put underneath it.
    frame.keys(" fm")
    retype(frame, "other.rs", "other.rs/inside.rs")
    frame.expect("other.rs could not be moved:")
    frame.expect("NORMAL other.rs", within=1)
