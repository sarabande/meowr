"""Code actions are dropped when the text moves under them, and applied when it
does not -- one with its edit, one resolved first (#169).
"""

FILES = {"main.rs": "old_name();\n"}
OPEN = "main.rs"
CONFIG = "(set-language-server 'rust \"{server}\" '())"
ENV = {"MEOWR_SLOW_DELAY": "2"}


def run(frame):
    frame.keys(" kasx")
    frame.wait(0.3)
    frame.keys("\x1b")
    frame.expect("main.rs changed while slow-server.py was working, so no action was offered.", within=6)
    frame.refute("Slow fix", 0.5)
    frame.keys("u")
    frame.wait(0.5)

    # The control: the picker of titles, and the chosen action's edit.
    frame.keys(" ka")
    frame.expect("Slow note: resolve first", within=6)
    frame.keys("fix\r")
    frame.expect("// slow fix")

    # And one that has to be resolved before it has an edit.
    frame.keys(" ka")
    frame.expect("Slow note: resolve first", within=6)
    frame.keys("note\r")
    frame.expect("// slow note", within=6)
