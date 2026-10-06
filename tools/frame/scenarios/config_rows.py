"""The config rows: `SPC f P` opens it, `SPC f p` finds a file beside it (#181).

Checked here because both rows are about a path the session is *told* and never
works out -- `config::find` reads `XDG_CONFIG_HOME`, which is the environment,
which is the host's -- so only the real binary knows whether the row opened the
file meowr would actually read. And because the interesting case is a config
that is not there: an empty buffer and a buffer for a file that does not exist
look identical on screen, and the Echo Area is the only thing that tells them
apart.
"""

import os

FILES = {"main.rs": "fn main() {}\n"}
OPEN = "main.rs"
COLS = 120


def run(frame):
    beside = os.path.join(frame.config, "meowr")
    init = os.path.join(beside, "init.scm")
    # run.py writes one for every scenario; this row is about the machine that
    # has never had a config at all, so the file goes and the directory with
    # it -- a config directory nobody has made is the ordinary state of one.
    os.remove(init)
    os.rmdir(beside)

    # Opened anyway, and said to be new -- an empty buffer otherwise implies an
    # empty config.
    frame.keys(" fP")
    frame.expect("init.scm is new")
    frame.expect("NORMAL")

    # Typed into and written: the file did not exist and now does, with what was
    # typed in it -- and the directory it needed was made on the way, which is
    # the thing a config directory nobody has created yet is for.
    frame.keys("s(set-option 'auto-save #t)\r")
    frame.wait(0.3)
    frame.keys("\x1b")
    frame.keys("\x18\x13")  # C-x C-s, which is `write-file`
    # The directory was made on the way, and said, as a copy and a move say it.
    frame.expect("creating")
    with open(init) as file:
        assert file.read() == "(set-option 'auto-save #t)\n", "the config was not written"

    # A second press is the same buffer, not a second one over the first, and
    # nothing is new about it now.
    frame.keys(" bb")
    frame.expect("NORMAL main.rs")
    frame.keys(" fP")
    frame.expect("auto-save")
    shown = "\n".join(frame.screen.display)
    assert "is new" not in shown, f"a config that is there is not new:\n{shown}"

    # What is beside it, which is `SPC f F`'s listing rooted at the config.
    with open(os.path.join(beside, "keys.scm"), "w") as file:
        file.write(";; the author's own\n")
    frame.keys(" fp")
    frame.expect("keys.scm")
    frame.keys("keys\r")
    frame.expect("the author's own")

    # And when there is no config directory at all, the row refuses by name
    # rather than opening an empty picker: an empty list reads as success.
    os.remove(os.path.join(beside, "keys.scm"))
    os.remove(init)
    os.rmdir(beside)
    frame.keys(" fp")
    frame.expect("config/meowr/ could not be read")
