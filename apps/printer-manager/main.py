"""printer-manager — CUPS discovery, queue, print, and cancel."""

import json
import os
import re
import shutil
import subprocess
import sys

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from _shared.env_scrub import scrub_env  # noqa: E402

from cos_runtime import policy  # noqa: E402


NAME_RE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9_.-]{0,126}$")
JOB_RE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9_.-]{0,126}-[0-9]+$")
MEDIA_RE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9_.-]{0,127}$")
SIDES = frozenset({"one-sided", "two-sided-long-edge", "two-sided-short-edge"})
TIMEOUT_SECS = int(os.environ.get("CLAW_PRINTER_MANAGER_TIMEOUT", "600"))


def _cos_binary():
    return os.environ.get("COS_BIN") or shutil.which("cos")


def _broker(action, **values):
    cos_bin = _cos_binary()
    if not cos_bin:
        return {"error": "cos binary not found; Printer Manager broker unavailable"}
    argv = [cos_bin, "__printer", action]
    for key in ["printer", "source", "job_id", "title", "media", "sides", "copies"]:
        value = values.get(key)
        if value is not None:
            argv.extend([f"--{key.replace('_', '-')}", str(value)])
    if values.get("confirm"):
        argv.append("--confirm")
    try:
        result = subprocess.run(
            argv,
            capture_output=True,
            text=True,
            timeout=TIMEOUT_SECS,
            stdin=subprocess.DEVNULL,
            env=scrub_env(),
            check=False,
        )
    except (FileNotFoundError, PermissionError) as exc:
        return {"error": str(exc)}
    except subprocess.TimeoutExpired:
        return {"error": f"Printer Manager broker exceeded {TIMEOUT_SECS}s"}
    payload_text = (result.stdout or "").strip() or (result.stderr or "").strip()
    try:
        payload = json.loads(payload_text) if payload_text else {}
    except json.JSONDecodeError:
        return {"error": "Printer Manager broker returned invalid JSON"}
    if result.returncode != 0 and "error" not in payload:
        payload["error"] = f"Printer Manager broker exited {result.returncode}"
    return payload


def run(command, args):
    from canonical_argv import normalize_canonical_argv
    args = normalize_canonical_argv(args, bool_flags={"confirm"})
    if command == "status":
        if args:
            return {"error": "status takes no arguments"}
        policy.require("sys.observe", name="printing")
        return _broker(command)
    if command == "jobs":
        if len(args) > 1 or (args and NAME_RE.fullmatch(args[0]) is None):
            return {"error": "jobs accepts an optional printer name"}
        policy.require("device.printer", name="observe")
        return _broker(command, printer=args[0] if args else None)
    if command == "capabilities":
        if len(args) != 1 or NAME_RE.fullmatch(args[0]) is None:
            return {"error": "capabilities requires one printer name"}
        policy.require("sys.observe", name="printing")
        return _broker(command, printer=args[0])
    if command == "print":
        if len(args) < 2 or NAME_RE.fullmatch(args[0]) is None:
            return {"error": "print requires <printer> <source> [options]"}
        source = os.path.realpath(args[1])
        if source != args[1] or os.path.islink(args[1]) or not os.path.isabs(source):
            return {"error": "print source must be a canonical non-symlink path"}
        copies = 1
        title = None
        media = None
        sides = None
        index = 2
        while index < len(args):
            if args[index] == "--copies" and index + 1 < len(args):
                try:
                    copies = int(args[index + 1])
                except ValueError:
                    return {"error": "copies must be an integer"}
                index += 2
            elif args[index] == "--title" and index + 1 < len(args):
                title = args[index + 1]
                index += 2
            elif args[index] == "--media" and index + 1 < len(args):
                media = args[index + 1]
                index += 2
            elif args[index] == "--sides" and index + 1 < len(args):
                sides = args[index + 1]
                index += 2
            else:
                return {"error": f"unexpected print argument: {args[index]}"}
        if not 1 <= copies <= 100:
            return {"error": "copies must be 1..100"}
        if title is not None and (not title or len(title) > 128 or any(ord(ch) < 32 for ch in title)):
            return {"error": "invalid print title"}
        if media is not None and MEDIA_RE.fullmatch(media) is None:
            return {"error": "invalid media option"}
        if sides is not None and sides not in SIDES:
            return {"error": "invalid sides option"}
        policy.require("device.printer", name="print")
        policy.require("fs.read", path=source)
        return _broker(command, printer=args[0], source=source, copies=copies, title=title, media=media, sides=sides)
    if command == "cancel":
        if len(args) != 2 or args[1] != "--confirm" or JOB_RE.fullmatch(args[0]) is None:
            return {"error": "cancel requires <job-id> --confirm"}
        policy.require("device.printer", name="control")
        return _broker(command, job_id=args[0], confirm=True)
    return {"error": f"unknown command: {command}"}
