"""The scratch rows: `SPC f x` `f X` and a Project's `SPC p x` `p X` (#180).

Checked here because a Project's scratch is named for a repository the session
never sees -- the host resolves ADR-0010's root and the name is the last
segment of it -- so only the real binary knows what the buffer is called. The
scenario runs in a fresh repository of its own, so it reads the name off
`frame.cwd` rather than hard-coding one.
"""

import os

FILES = {"main.rs": "fn main() {}\n"}
OPEN = "main.rs"
COLS = 120


def run(frame):
    # What this Project is called: the root's own name, which is Doom's
    # `doom-project-name`. run.py puts the repository at `<tmp>/work`, so this
    # is always "work" -- read off the directory rather than written down, so
    # that moving the repository does not quietly stop testing the name.
    project = os.path.basename(frame.cwd)
    named = f"*scratch: {project}*"

    # The global one toggles into a window below and out again.
    frame.keys(" fx")
    frame.expect("NORMAL *scratch*")
    frame.keys(" fx")
    frame.expect("NORMAL main.rs")

    # The Project's is a different buffer with a name of its own.
    frame.keys(" px")
    frame.expect(f"NORMAL {named}")
    frame.keys("sproject notes")
    frame.wait(0.3)
    frame.keys("\x1b")
    frame.expect(f"NORMAL * {named}")

    # Toggled off from inside it, which it can only do by carrying its own
    # root: it has no file for a Project to be found from.
    frame.keys(" px")
    frame.expect("NORMAL main.rs")

    # And switched back to, in this window, with what was typed still in it.
    frame.keys(" pX")
    frame.expect(f"NORMAL * {named}")
    frame.expect("project notes")

    # The global scratch is not this one: a separate buffer, still empty.
    frame.keys(" fX")
    frame.expect("NORMAL *scratch*")
    shown = "\n".join(frame.screen.display)
    assert "project notes" not in shown, f"the two scratches are one buffer:\n{shown}"

    # Neither is persisted, and nothing goes quietly: a modified scratch is
    # named among the unsaved when you try to quit (#166).
    frame.keys(" qq")
    frame.expect(named)
    frame.expect("to quit anyway")
