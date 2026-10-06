"""Restarting: `SPC q r` (#182).

Only the real binary can be checked for this at all. A restart is `exec` on
itself, so there is no return value and no state left behind to assert on --
what happened is that the same process is now a new editor, drawing a screen it
built from nothing. The pty is the only witness: the terminal has to have been
handed back before the `exec`, or the new meowr inherits a screen it did not
draw, and what was on screen before it has to be gone, because nothing is
restored.

The restart is made from `other.rs` on purpose. meowr was started on `main.rs`,
so coming back to `main.rs` is the *argument* being replayed rather than the
session being restored -- and those two are indistinguishable from a scenario
that restarts while sitting in the file it was launched on.
"""

import os

FILES = {"main.rs": "fn main() {}\n", "other.rs": "fn other() {}\n"}
OPEN = "main.rs"
COLS = 120
# So that there is an auto-save for the restart to be rid of, and so that one
# edit is enough to write it.
CONFIG = "(set-option 'auto-save #t)\n(set-option 'auto-save-interval 1)"


def run(frame):
    # Somewhere other than the file meowr was started on, typed into so there is
    # unsaved work and an auto-save beside it.
    frame.keys(" ff")
    frame.keys("other")
    frame.wait(0.4)
    frame.keys("\r")
    frame.expect("NORMAL other.rs")
    frame.keys("sZZTOP")
    frame.wait(0.3)
    frame.keys("\x1b")
    frame.expect("NORMAL * other.rs")

    auto = os.path.join(frame.cwd, "#other.rs#")
    frame.wait(0.5)
    assert os.path.exists(auto), "the auto-save this is about was never written"

    # Refused: a restart is stopped by what a quit is stopped by, and names the
    # go-ahead rather than a second command.
    frame.keys(" qr")
    frame.expect("Unsaved: other.rs.")
    frame.expect("SPC u SPC q r")
    shown = "\n".join(frame.screen.display)
    assert "ZZTOP" in shown, f"it refused, so the work is still here:\n{shown}"

    # Past it, and the editor comes back: the same process, a screen it drew
    # itself, and the file it was *started* on rather than the one it was in.
    frame.keys(" u q")
    frame.keys("r")
    frame.expect("NORMAL main.rs", within=20)
    frame.expect("fn main() {}")
    shown = "\n".join(frame.screen.display)
    assert "ZZTOP" not in shown, f"nothing is restored, not even the edit:\n{shown}"
    assert "NORMAL * " not in shown, f"the buffer it came back to is the file:\n{shown}"

    # The auto-saves were dealt with as a quit deals with them: gone, because
    # what they held was thrown away on purpose and one left behind would offer
    # to recover work nobody lost.
    assert not os.path.exists(auto), "the auto-save outlived the restart"

    # And it is an editor, not a picture of one.
    frame.keys(" fx")
    frame.expect("NORMAL *scratch*")
    frame.keys(" fx")
    frame.expect("NORMAL main.rs")

    # The recent files went the other way -- written on the way out, read on the
    # way back in -- which is the other half of "as a quit deals with them".
    # `other.rs` was never opened by *this* editor; the one before it opened it.
    # Left open: `frame.quit` presses Escape before it presses anything else.
    frame.keys(" \r")
    frame.expect("other.rs")
