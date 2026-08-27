"""systemd — native service control through the privileged clawd broker."""

import json
import os
import re
import shutil
import subprocess
import sys

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from _shared.env_scrub import scrub_env  # noqa: E402

from cos_runtime import policy  # noqa: E402


UNIT_RE = re.compile(
    r"^[A-Za-z0-9_][A-Za-z0-9_.@:-]*\.(service|socket|timer|mount|target|path)$"
)
QUERY_TIMEOUT_SECS = 30
CONTROL_TIMEOUT_SECS = int(os.environ.get("CLAW_SYSTEMD_TIMEOUT", "180"))
MUTATING = frozenset({"start", "stop", "restart", "reload", "enable", "disable"})


def _valid_unit(unit):
    return (
        isinstance(unit, str)
        and len(unit) <= 255
        and UNIT_RE.fullmatch(unit) is not None
    )


def _cos_binary():
    return os.environ.get("COS_BIN") or shutil.which("cos")


def _broker(action, unit):
    cos_bin = _cos_binary()
    if not cos_bin:
        return {"error": "cos binary not found; privileged systemd broker unavailable"}
    try:
        result = subprocess.run(
            [cos_bin, "__systemd", action, unit],
            capture_output=True,
            text=True,
            timeout=QUERY_TIMEOUT_SECS if action == "status" else CONTROL_TIMEOUT_SECS,
            stdin=subprocess.DEVNULL,
            env=scrub_env(),
            check=False,
        )
    except FileNotFoundError as exc:
        return {"error": str(exc)}
    except subprocess.TimeoutExpired:
        return {"error": f"systemd broker exceeded its timeout for {action} {unit}"}

    payload_text = (result.stdout or "").strip() or (result.stderr or "").strip()
    try:
        payload = json.loads(payload_text) if payload_text else {}
    except json.JSONDecodeError:
        return {
            "error": "systemd broker returned invalid JSON",
            "exit_code": result.returncode,
        }
    if result.returncode != 0 and "error" not in payload:
        payload["error"] = f"systemd broker exited {result.returncode}"
    return payload


def run(command, args):
    if command not in MUTATING | {"status"}:
        return {"error": f"unknown command: {command}"}
    if len(args) != 1 or not _valid_unit(args[0]):
        return {
            "error": (
                f"{command} requires one valid unit name ending in "
                ".service, .socket, .timer, .mount, .target, or .path"
            )
        }

    unit = args[0]
    if command == "status":
        policy.require("sys.observe", name=unit)
    else:
        policy.require("sys.service", name=unit)
    return _broker(command, unit)
