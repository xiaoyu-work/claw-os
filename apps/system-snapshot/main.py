"""system-snapshot — full-system recovery points through clawd."""

import json
import os
import re
import shutil
import subprocess
import sys

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from _shared.env_scrub import scrub_env  # noqa: E402

from cos_runtime import policy  # noqa: E402


SNAPSHOT_ID_RE = re.compile(r"^snap_[0-9a-f]{32}$")
TIMEOUT_SECS = int(os.environ.get("CLAW_SNAPSHOT_TIMEOUT", "1800"))


def _cos_binary():
    return os.environ.get("COS_BIN") or shutil.which("cos")


def _broker(action, value=None, confirm=False):
    cos_bin = _cos_binary()
    if not cos_bin:
        return {"error": "cos binary not found; snapshot broker unavailable"}
    argv = [cos_bin, "__snapshot", action]
    if value is not None:
        argv.append(value)
    if confirm:
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
        return {"error": f"snapshot broker exceeded {TIMEOUT_SECS}s"}
    payload_text = (result.stdout or "").strip() or (result.stderr or "").strip()
    try:
        payload = json.loads(payload_text) if payload_text else {}
    except json.JSONDecodeError:
        return {"error": "snapshot broker returned invalid JSON"}
    if result.returncode != 0 and "error" not in payload:
        payload["error"] = f"snapshot broker exited {result.returncode}"
    return payload


def run(command, args):
    if command in ("status", "list"):
        if args:
            return {"error": f"{command} takes no arguments"}
        policy.require("sys.observe", name="system-snapshots")
        return _broker(command)
    if command == "create":
        if len(args) > 1:
            return {"error": "create accepts at most one description"}
        policy.require("sys.snapshot", wild=True)
        return _broker("create", args[0] if args else "Claw OS recovery point")
    if command == "delete":
        if len(args) != 1 or SNAPSHOT_ID_RE.fullmatch(args[0]) is None:
            return {"error": "delete requires a valid snapshot id"}
        policy.require("sys.snapshot", wild=True)
        return _broker("delete", args[0])
    if command == "rollback":
        confirm = "--confirm" in args
        ids = [value for value in args if value != "--confirm"]
        if len(ids) != 1 or SNAPSHOT_ID_RE.fullmatch(ids[0]) is None or not confirm:
            return {"error": "rollback requires <snapshot-id> --confirm"}
        policy.require("sys.snapshot", wild=True)
        return _broker("rollback", ids[0], confirm=True)
    return {"error": f"unknown command: {command}"}
