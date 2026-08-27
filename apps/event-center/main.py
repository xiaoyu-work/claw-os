"""event-center — persistent system event subscriptions and pidfd watches."""

import json
import os
import shutil
import subprocess
import sys

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from _shared.env_scrub import scrub_env  # noqa: E402

from cos_runtime import policy  # noqa: E402


SOURCES = frozenset({"udev", "systemd", "journal", "storage", "security", "process"})
TIMEOUT_SECS = int(os.environ.get("CLAW_EVENT_CENTER_TIMEOUT", "120"))


def _cos_binary():
    return os.environ.get("COS_BIN") or shutil.which("cos")


def _broker(action, source=None, limit=None, pid=None):
    cos_bin = _cos_binary()
    if not cos_bin:
        return {"error": "cos binary not found; Event Center broker unavailable"}
    argv = [cos_bin, "__events", action]
    if source is not None:
        argv.extend(["--source", source])
    if limit is not None:
        argv.extend(["--limit", str(limit)])
    if pid is not None:
        argv.extend(["--pid", str(pid)])
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
        return {"error": f"Event Center broker exceeded {TIMEOUT_SECS}s"}
    payload_text = (result.stdout or "").strip() or (result.stderr or "").strip()
    try:
        payload = json.loads(payload_text) if payload_text else {}
    except json.JSONDecodeError:
        return {"error": "Event Center broker returned invalid JSON"}
    if result.returncode != 0 and "error" not in payload:
        payload["error"] = f"Event Center broker exited {result.returncode}"
    return payload


def run(command, args):
    from canonical_argv import normalize_canonical_argv
    args = normalize_canonical_argv(args)
    if command == "status":
        if args:
            return {"error": "status takes no arguments"}
        policy.require("sys.events", name="observe")
        return _broker(command)
    if command == "recent":
        source = None
        limit = 100
        positionals = []
        index = 0
        while index < len(args):
            if args[index] == "--source" and index + 1 < len(args):
                source = args[index + 1]
                index += 2
            else:
                positionals.append(args[index])
                index += 1
        if source is not None and source not in SOURCES:
            return {"error": f"unknown event source: {source}"}
        if len(positionals) > 1:
            return {"error": "recent accepts [limit] [--source SOURCE]"}
        if positionals:
            try:
                limit = int(positionals[0])
            except ValueError:
                return {"error": "event limit must be an integer"}
        if not 1 <= limit <= 1000:
            return {"error": "recent limit must be 1..1000"}
        policy.require("sys.events", name="observe")
        return _broker(command, source=source, limit=limit)
    if command == "watch-pid":
        if len(args) != 1:
            return {"error": "watch-pid requires one pid"}
        try:
            pid = int(args[0])
        except ValueError:
            return {"error": "pid must be an integer"}
        if not 1 <= pid <= 2**32 - 1:
            return {"error": "pid must be a positive 32-bit integer"}
        policy.require("sys.events", name="observe")
        return _broker(command, pid=pid)
    return {"error": f"unknown command: {command}"}
