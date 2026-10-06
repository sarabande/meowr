#!/usr/bin/env python3
"""A formatter that takes its time (#170).

apheleia's plumbing, as meowr runs a formatter: the buffer's text on stdin, the
formatted text on stdout. This one waits, then puts a line at the top, so a
format that landed is visible and one that was dropped visibly is not.

    (set-formatter 'rust "tools/frame/slow-format.py" '())
    MEOWR_SLOW_DELAY=2.0          # seconds; 2 by default
"""

import os
import sys
import time

text = sys.stdin.read()
time.sleep(float(os.environ.get("MEOWR_SLOW_DELAY", "2.0")))
sys.stdout.write(text if text.startswith("// formatted\n") else "// formatted\n" + text)
