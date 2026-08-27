"""accessibility-manager — COSMIC Wayland and AT-SPI accessibility control."""

import json
import os
import shutil
import subprocess
import sys

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from _shared.env_scrub import scrub_env  # noqa: E402

from cos_runtime import policy  # noqa: E402


FILTERS = frozenset({"off", "greyscale", "protanopia", "deuteranopia", "tritanopia"})
TIMEOUT_SECS = int(os.environ.get("CLAW_ACCESSIBILITY_MANAGER_TIMEOUT", "120"))


def _cos_binary():
    return os.environ.get("COS_BIN") or shutil.which("cos")


def _broker(action, value=None):
    cos_bin = _cos_binary()
    if not cos_bin:
        return {"error": "cos binary not found; Accessibility Manager broker unavailable"}
    argv = [cos_bin, "__accessibility", action]
    if value is not None:
        argv.extend(["--value", value])
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
        return {"error": f"Accessibility Manager broker exceeded {TIMEOUT_SECS}s"}
    payload_text = (result.stdout or "").strip() or (result.stderr or "").strip()
    try:
        payload = json.loads(payload_text) if payload_text else {}
    except json.JSONDecodeError:
        return {"error": "Accessibility Manager broker returned invalid JSON"}
    if result.returncode != 0 and "error" not in payload:
        payload["error"] = f"Accessibility Manager broker exited {result.returncode}"
    return payload


def run(command, args):
    from canonical_argv import normalize_canonical_argv
    args = normalize_canonical_argv(args)
    if command == "status":
        if args:
            return {"error": "status takes no arguments"}
        policy.require("sys.observe", name="accessibility")
        return _broker(command)
    if command in {"screen-reader", "magnifier", "invert"}:
        if len(args) != 1 or args[0] not in {"on", "off"}:
            return {"error": f"{command} requires on|off"}
        policy.require("ui.accessibility", name="control")
        return _broker(command, args[0])
    if command == "filter":
        if len(args) != 1 or args[0] not in FILTERS:
            return {"error": f"filter requires one of: {', '.join(sorted(FILTERS))}"}
        policy.require("ui.accessibility", name="control")
        return _broker(command, args[0])
    return {"error": f"unknown command: {command}"}
