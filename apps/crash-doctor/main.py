"""crash-doctor — privileged coredump and crash correlation."""

import json
import os
import re
import shutil
import subprocess
import sys

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from _shared.env_scrub import scrub_env  # noqa: E402

from cos_runtime import policy  # noqa: E402


MAX_SINCE_MINUTES = 7 * 24 * 60
MAX_LIMIT = 100
TIMEOUT_SECS = int(os.environ.get("CLAW_CRASH_DOCTOR_TIMEOUT", "300"))
COREDUMP_ID_RE = re.compile(
    r"^[0-9A-Fa-f]{32}:[1-9][0-9]{0,9}:[1-9][0-9]{0,19}$"
)


def _cos_binary():
    return os.environ.get("COS_BIN") or shutil.which("cos")


def _broker(action, since_minutes=None, limit=None, coredump_id=None):
    cos_bin = _cos_binary()
    if not cos_bin:
        return {"error": "cos binary not found; Crash Doctor broker unavailable"}
    argv = [cos_bin, "__crash", action]
    if since_minutes is not None:
        argv.extend(["--since-minutes", str(since_minutes)])
    if limit is not None:
        argv.extend(["--limit", str(limit)])
    if coredump_id is not None:
        argv.extend(["--id", coredump_id])
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
        return {"error": f"Crash Doctor broker exceeded {TIMEOUT_SECS}s"}
    payload_text = (result.stdout or "").strip() or (result.stderr or "").strip()
    try:
        payload = json.loads(payload_text) if payload_text else {}
    except json.JSONDecodeError:
        return {"error": "Crash Doctor broker returned invalid JSON"}
    if result.returncode != 0 and "error" not in payload:
        payload["error"] = f"Crash Doctor broker exited {result.returncode}"
    return payload


def _query_bounds(args):
    if len(args) > 2:
        raise ValueError("expected [since_minutes] [limit]")
    try:
        since_minutes = int(args[0]) if args else 60
        limit = int(args[1]) if len(args) == 2 else 20
    except ValueError as exc:
        raise ValueError("since_minutes and limit must be integers") from exc
    if not 1 <= since_minutes <= MAX_SINCE_MINUTES:
        raise ValueError(f"since_minutes must be 1..{MAX_SINCE_MINUTES}")
    if not 1 <= limit <= MAX_LIMIT:
        raise ValueError(f"limit must be 1..{MAX_LIMIT}")
    return since_minutes, limit


def run(command, args):
    if command == "__schema__":
        return _schema()
    if command in {"recent", "diagnose"}:
        try:
            since_minutes, limit = _query_bounds(args)
        except ValueError as exc:
            return {"error": str(exc)}
        policy.require("sys.crash", name="system")
        return _broker(command, since_minutes, limit)
    if command == "backtrace":
        if len(args) != 1 or COREDUMP_ID_RE.fullmatch(args[0]) is None:
            return {
                "error": (
                    "backtrace requires one "
                    "<32-hex-boot-id>:<pid>:<timestamp-us> coredump id"
                )
            }
        policy.require("sys.crash", name="system")
        return _broker(command, coredump_id=args[0].lower())
    return {"error": f"unknown command: {command}"}


def _schema():
    query_parameters = [
        {
            "name": "since_minutes",
            "type": "integer",
            "kind": "positional",
            "required": False,
            "default": 60,
        },
        {
            "name": "limit",
            "type": "integer",
            "kind": "positional",
            "required": False,
            "default": 20,
        },
    ]
    return {
        "recent": {
            "description": "List recent system coredumps",
            "parameters": query_parameters,
            "example": "cos app crash-doctor recent 60 20",
        },
        "diagnose": {
            "description": "Correlate coredumps with OOM, segfault, and journal evidence",
            "parameters": query_parameters,
            "example": "cos app crash-doctor diagnose 60 20",
        },
        "backtrace": {
            "description": "Return recorded and constrained live backtraces for a coredump id",
            "parameters": [
                {
                    "name": "id",
                    "type": "string",
                    "kind": "positional",
                    "required": True,
                }
            ],
            "example": "cos app crash-doctor backtrace <boot-id>:<pid>:<timestamp-us>",
        },
    }
