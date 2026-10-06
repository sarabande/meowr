"""A format is dropped when the text moves under it, and lands when it does not
(#164, ADR-0013, ADR-0014).
"""

FILES = {"main.rs": "old_name();\n"}
OPEN = "main.rs"
CONFIG = """
(set-language-server 'rust "{server}" '())
(set-formatter 'rust "{formatter}" '())
"""
ENV = {"MEOWR_SLOW_DELAY": "2"}
# Wide enough for the refusal, which names the formatter by its configured path.
COLS = 200


def run(frame):
    frame.keys(" kfsx")
    frame.wait(0.3)
    frame.keys("\x1b")
    frame.expect("slow-format.py was working, so its edit was not applied.", within=6)
    frame.refute("// formatted", 0.5)
    frame.keys("u")
    frame.wait(0.5)

    # The control: left alone, the formatter's output replaces the text.
    frame.keys(" kf")
    frame.expect("// formatted", within=6)
