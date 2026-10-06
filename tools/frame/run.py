#!/usr/bin/env python3
"""Run the checked-in frame scenarios, each in a project of its own (#170).

    tools/frame/run.py                        # every scenario
    tools/frame/run.py rename format          # those whose file name contains either
    tools/frame/run.py --binary path/to/meowr

A scenario is a module in `scenarios/` that says what its project holds and
what to do in it:

    FILES = {"main.rs": "old_name();\\n"}      # written into a fresh git repository
    OPEN = "main.rs"                          # the file meowr is started on
    CONFIG = "(set-language-server 'rust \\"{server}\\" '())"
    ENV = {"MEOWR_SLOW_DELAY": "2"}           # for the slow programs
    ROWS, COLS = 20, 100                      # the terminal; these are the defaults
    SETTLE = 1.5                              # seconds before `run`, for the server to start

    def run(frame):
        frame.keys(" kk")
        frame.expect("SLOW SERVER SAYS")

`{server}` and `{formatter}` in `CONFIG` name `slow-server.py` and
`slow-format.py` by absolute path. Everything meowr writes -- auto-saves, the
recent files, the config it reads -- lands in the scenario's temporary
directory, which is removed afterwards. Exits non-zero if any scenario fails,
printing the frame it failed on.
"""

import argparse
import importlib.util
import os
import subprocess
import sys
import tempfile
import time
import traceback

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)

from drive import Failure, Frame  # noqa: E402


def load(path):
    spec = importlib.util.spec_from_file_location(os.path.basename(path)[:-3], path)
    if not (spec and spec.loader):
        raise ImportError(f"not a scenario module: {path}")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def project(directory, scenario):
    """The scenario's files in a git repository -- meowr's Project is one
    (ADR-0010) -- and its config beside them."""
    work = os.path.join(directory, "work")
    for name, text in scenario.FILES.items():
        path = os.path.join(work, name)
        os.makedirs(os.path.dirname(path), exist_ok=True)
        with open(path, "w") as file:
            file.write(text)
    subprocess.run(["git", "init", "-q", work], check=True)
    config = os.path.join(directory, "config")
    os.makedirs(os.path.join(config, "meowr"))
    with open(os.path.join(config, "meowr", "init.scm"), "w") as file:
        file.write(getattr(scenario, "CONFIG", "").format(
            server=os.path.join(HERE, "slow-server.py"),
            formatter=os.path.join(HERE, "slow-format.py"),
        ))
    return work, config


def run_one(path, binary):
    scenario = load(path)
    with tempfile.TemporaryDirectory(prefix="meowr-frame-") as directory:
        work, config = project(directory, scenario)
        env = {
            "XDG_STATE_HOME": os.path.join(directory, "state"),
            "XDG_CACHE_HOME": os.path.join(directory, "cache"),
            **getattr(scenario, "ENV", {}),
        }
        frame = Frame(
            binary=binary,
            args=[scenario.OPEN] if getattr(scenario, "OPEN", None) else [],
            cwd=work,
            config=config,
            rows=getattr(scenario, "ROWS", 20),
            cols=getattr(scenario, "COLS", 100),
            env=env,
        )
        try:
            # The server has to have started and been told about the file
            # before a question means anything; a second and a half is plenty
            # for the slow server, which is slow only at answering.
            frame.wait(getattr(scenario, "SETTLE", 1.5))
            scenario.run(frame)
        finally:
            frame.quit()


def main():
    parser = argparse.ArgumentParser(description="Run the checked-in frame scenarios.")
    parser.add_argument("only", nargs="*", help="run scenarios whose file name contains one of these")
    parser.add_argument("--binary", default="target/release/meowr")
    options = parser.parse_args()

    binary = os.path.abspath(options.binary)
    if not os.access(binary, os.X_OK):
        sys.exit(f"no meowr at {binary}: cargo build --release -p meowr")
    directory = os.path.join(HERE, "scenarios")
    paths = sorted(
        os.path.join(directory, name)
        for name in os.listdir(directory)
        if name.endswith(".py") and (not options.only or any(o in name for o in options.only))
    )
    failed = []
    for path in paths:
        name = os.path.basename(path)[:-3]
        started = time.time()
        try:
            run_one(path, binary)
            print(f"ok      {name} ({time.time() - started:.1f}s)")
        except Failure as failure:
            failed.append(name)
            print(f"FAILED  {name} ({time.time() - started:.1f}s)\n{failure}\n")
        # A scenario that is itself broken -- a typo, a file it cannot write --
        # fails as itself rather than stopping the run before the summary.
        except Exception:
            failed.append(name)
            print(f"BROKEN  {name} ({time.time() - started:.1f}s)\n{traceback.format_exc()}")
        sys.stdout.flush()
    print(f"\n{len(paths) - len(failed)} passed, {len(failed)} failed")
    sys.exit(1 if failed else 0)


if __name__ == "__main__":
    main()
