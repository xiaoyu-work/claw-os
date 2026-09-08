#!/usr/bin/env python3
"""Build the checked-in Mail product against its pinned Mozilla platform."""

import argparse
import json
import os
from pathlib import Path
import subprocess
import sys


PRODUCT = Path(__file__).resolve().parent
ROOT = PRODUCT.parent.parent
WORK = ROOT / "build" / "mail"
GECKO = WORK / "gecko"


def run(*args, cwd=ROOT, env=None):
    subprocess.run(args, cwd=cwd, env=env, check=True)


def output(*args, cwd=ROOT):
    return subprocess.check_output(args, cwd=cwd, text=True).strip()


def prepare(pin):
    if sys.platform != "linux" or str(ROOT).startswith("/mnt/"):
        raise RuntimeError("Build Mail on a Linux filesystem, not a Windows-mounted checkout")
    WORK.mkdir(parents=True, exist_ok=True)
    if not GECKO.exists():
        run("git", "clone", "--depth=1", "--single-branch", "--branch", pin["tag"],
            pin["repository"] + ".git", str(GECKO))
    revision = output("git", "rev-parse", "HEAD", cwd=GECKO)
    if revision != pin["revision"]:
        raise RuntimeError(f"Firefox platform mismatch: expected {pin['revision']}, got {revision}")
    run("git", "diff", "--quiet", cwd=GECKO)
    run("git", "diff", "--cached", "--quiet", cwd=GECKO)
    comm = GECKO / "comm"
    if comm.is_symlink():
        if comm.resolve() != PRODUCT / "comm":
            raise RuntimeError("Platform comm symlink points to a different Mail source")
    elif comm.exists():
        raise RuntimeError("Platform comm already exists; refusing to overwrite another source tree")
    else:
        comm.symlink_to(PRODUCT / "comm", target_is_directory=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=("prepare", "bootstrap", "configure", "build", "package", "test"))
    parser.add_argument("args", nargs=argparse.REMAINDER, help="Arguments passed to the selected mach command")
    options = parser.parse_args()
    pin = json.loads((PRODUCT / "upstream.json").read_text())["firefox"]
    prepare(pin)
    if options.command == "prepare":
        print(f"Mail source: {PRODUCT / 'comm'}\nFirefox platform: {pin['revision']}")
        return
    env = os.environ.copy()
    env.update({
        "MOZCONFIG": str(PRODUCT / "mozconfig"),
        "CLAW_MAIL_OBJDIR": str(WORK / "obj"),
        "MOZBUILD_STATE_PATH": str(WORK / "state"),
        "COMM_HEAD_REPOSITORY": "https://github.com/xiaoyu-work/claw-os",
        "COMM_HEAD_REV": output("git", "rev-parse", "HEAD"),
        "GECKO_HEAD_REPOSITORY": pin["repository"],
        "GECKO_HEAD_REV": pin["revision"],
        "SOURCE_DATE_EPOCH": output("git", "show", "-s", "--format=%ct", "HEAD"),
    })
    command = {
        "bootstrap": ["bootstrap"],
        "configure": ["configure"],
        "build": ["build"],
        "package": ["build", "package"],
        "test": ["xpcshell-test"],
    }[options.command]
    run(sys.executable, str(GECKO / "mach"), *command, *options.args, cwd=GECKO, env=env)


if __name__ == "__main__":
    main()
