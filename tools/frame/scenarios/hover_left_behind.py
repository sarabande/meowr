"""A hover answered after its buffer has been left is not drawn (#123, #124).

ADR-0009: an answer is re-checked when it lands and again when it is drawn. A
hover asked in `main.rs` and answered once `lib.rs` is on screen is about text
that is not there.
"""

FILES = {
    "main.rs": "old_name();\n",
    "lib.rs": "fn old_name() {}\n",
}
OPEN = "main.rs"
CONFIG = "(set-language-server 'rust \"{server}\" '())"
ENV = {"MEOWR_SLOW_DELAY": "2"}
SAYS = "SLOW SERVER SAYS"


def run(frame):
    # The control: asked and left alone, the answer is drawn. Without it the
    # refusal below would pass against a server that never answered.
    frame.keys(" kk")
    frame.expect(SAYS, within=6)
    frame.keys("\x1b")
    frame.wait(0.5)

    # Open `lib.rs`, then come back, so switching is one picker away.
    frame.keys("  lib.rs")
    frame.wait(1)
    frame.keys("\r")
    frame.expect("NORMAL lib.rs")
    frame.keys(" .main.rs\r")
    frame.expect("NORMAL main.rs")

    # Asked, and `lib.rs` shown before the answer lands.
    frame.keys(" kk .lib.rs\r")
    frame.refute(SAYS, 4)
    frame.expect("NORMAL lib.rs", within=1)
