"""A rename is dropped when the text moves under it, and lands when it does not
(#168, ADR-0013).

Twice dropped: once while the server prepares, and once while it renames. Both
are said, because a rename computed before the last keystroke has to be asked
again and the message says why.
"""

FILES = {
    "main.rs": "old_name();\nold_name();\n",
    "lib.rs": "fn old_name() {}\n",
}
OPEN = "main.rs"
CONFIG = "(set-language-server 'rust \"{server}\" '())"
ENV = {"MEOWR_SLOW_DELAY": "2"}


def run(frame):
    # Typed over while the server prepares.
    frame.keys(" krsx")
    frame.wait(0.3)
    frame.keys("\x1b")
    frame.expect("main.rs changed while slow-server.py was working, so nothing was renamed.", within=6)
    frame.refute("Rename old_name to:", 0.5)
    frame.keys("u")
    frame.wait(0.5)

    # Typed over while the server renames. The rename goes out as Enter is
    # read and the typing after it, in the same write, so the server has the
    # question before it hears of the change.
    frame.keys(" kr")
    frame.expect("Rename old_name to: ", within=6)
    frame.keys("\x7f" * len("old_name") + "fresh\r" + "sx")
    frame.wait(0.3)
    frame.keys("\x1b")
    frame.expect("main.rs changed while slow-server.py was working, so its edit was not applied.", within=6)
    frame.refute("fresh", 0.5)
    frame.keys("u")
    frame.wait(0.5)

    # The control: left alone, every use is renamed, `lib.rs` opened unshown.
    frame.keys(" kr")
    frame.expect("Rename old_name to: ", within=6)
    frame.keys("\x7f" * len("old_name") + "fresh\r")
    frame.expect("slow-server.py changed 2 files; 2 are unsaved", within=6)
    frame.expect("1 fresh();")
    frame.expect("2 fresh();")
    frame.expect("NORMAL * main.rs")
    frame.keys(" .lib.rs\r")
    frame.expect("fn fresh() {}")
    frame.expect("NORMAL * lib.rs")
