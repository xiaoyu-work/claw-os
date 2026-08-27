"""power-manager — UPower status and critical logind power actions."""

import json
import os
import shutil
import subprocess
import sys

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from _shared.env_scrub import scrub_env  # noqa: E402

from cos_runtime import policy  # noqa: E402


TIMEOUT_SECS = int(os.environ.get("CLAW_POWER_MANAGER_TIMEOUT", "120"))
ACTIONS = frozenset(
    {
        "suspend",
        "hibernate",
        "hybrid-sleep",
        "suspend-then-hibernate",
        "reboot",
        "poweroff",
    }
)


def _cos_binary():
    return os.environ.get("COS_BIN") or shutil.which("cos")


def _broker(action, confirm=False):
    cos_bin = _cos_binary()
    if not cos_bin:
        return {"error": "cos binary not found; Power Manager broker unavailable"}
    argv = [cos_bin, "__power", action]
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
        return {"error": f"Power Manager broker exceeded {TIMEOUT_SECS}s"}
    payload_text = (result.stdout or "").strip() or (result.stderr or "").strip()
    try:
        payload = json.loads(payload_text) if payload_text else {}
    except json.JSONDecodeError:
        return {"error": "Power Manager broker returned invalid JSON"}
    if result.returncode != 0 and "error" not in payload:
        payload["error"] = f"Power Manager broker exited {result.returncode}"
    return payload


def run(command, args):
    from canonical_argv import normalize_canonical_argv
    args = normalize_canonical_argv(args, bool_flags={"confirm"})
    if command == "status":
        if args:
            return {"error": "status takes no arguments"}
        policy.require("sys.observe", name="power")
        return _broker(command)
    if command in ACTIONS:
        if args != ["--confirm"]:
            return {"error": f"{command} requires --confirm"}
        policy.require("sys.power", wild=True)
        return _broker(command, confirm=True)
    return {"error": f"unknown command: {command}"}
