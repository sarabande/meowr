"""A lookup answered while a picker is open leaves the picker alone (#146).

`found`'s rule: a widget opened since the question owns the Bottom, and neither
a picker opened over it nor a jump behind it is something the user asked for
mid-way through another.
"""

FILES = {
    "main.rs": "old_name();\n",
    "lib.rs": "fn old_name() {}\n",
}
OPEN = "main.rs"
CONFIG = "(set-language-server 'rust \"{server}\" '())"
ENV = {"MEOWR_SLOW_DELAY": "2"}


def run(frame):
    # A picker opened before the definition lands.
    frame.keys(" kd .")
    frame.expect("buffers:", within=2)
    # Across the whole time the answer lands in: a jump made behind the picker
    # would show on the mode line, which the picker leaves on screen.
    frame.refute("NORMAL lib.rs", 3.5)
    frame.expect("buffers:", within=0.5)
    frame.keys("\x1b")
    # And not held back for when the picker closes.
    frame.refute("NORMAL lib.rs", 1)

    # The control: left alone, the definition is jumped to.
    frame.keys(" kd")
    frame.expect("NORMAL lib.rs", within=6)
    frame.expect("fn old_name")

    # And references, which come back as a picker of every use.
    frame.keys("w")
    frame.wait(0.3)
    frame.keys(" kD")
    frame.expect("main.rs:1", within=6)
    frame.expect("lib.rs:1")
