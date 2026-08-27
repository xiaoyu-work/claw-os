"""usb-guard — sysfs authorization, managed udev blocking, and safe eject."""

import json
import os
import re
import shutil
import subprocess
import sys

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from _shared.env_scrub import scrub_env  # noqa: E402

from cos_runtime import policy  # noqa: E402


DEVICE_RE = re.compile(r"^(?=.{3,64}$)[0-9]+-[0-9]+(?:\.[0-9]+)*$")
TOKEN_RE = re.compile(r"^[0-9A-Fa-f]{32}$")
TIMEOUT_SECS = int(os.environ.get("CLAW_USB_GUARD_TIMEOUT", "180"))


def _cos_binary():
    return os.environ.get("COS_BIN") or shutil.which("cos")


def _broker(action, device=None, state=None, rule_id=None, token=None, confirm=False):
    cos_bin = _cos_binary()
    if not cos_bin:
        return {"error": "cos binary not found; USB Guard broker unavailable"}
    argv = [cos_bin, "__usb", action]
    for flag, value in [
        ("--device", device),
        ("--state", state),
        ("--rule-id", rule_id),
        ("--token", token),
    ]:
        if value is not None:
            argv.extend([flag, value])
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
        return {"error": f"USB Guard broker exceeded {TIMEOUT_SECS}s"}
    payload_text = (result.stdout or "").strip() or (result.stderr or "").strip()
    try:
        payload = json.loads(payload_text) if payload_text else {}
    except json.JSONDecodeError:
        return {"error": "USB Guard broker returned invalid JSON"}
    if result.returncode != 0 and "error" not in payload:
        payload["error"] = f"USB Guard broker exited {result.returncode}"
    return payload


def run(command, args):
    if command == "status":
        if args:
            return {"error": "status takes no arguments"}
        policy.require("sys.observe", name="usb")
        return _broker(command)
    if command == "authorize":
        if not 2 <= len(args) <= 3 or DEVICE_RE.fullmatch(args[0]) is None or args[1] not in {"on", "off"}:
            return {"error": "authorize requires <device> on|off [--confirm for off]"}
        confirm = "--confirm" in args[2:]
        if (args[1] == "off") != confirm:
            return {"error": "deauthorization requires --confirm; authorization does not"}
        policy.require("device.usb", name="control")
        return _broker(command, device=args[0], state=args[1], confirm=confirm)
    if command in {"block", "eject"}:
        if len(args) != 2 or args[1] != "--confirm" or DEVICE_RE.fullmatch(args[0]) is None:
            return {"error": f"{command} requires <device> --confirm"}
        policy.require("device.usb", name="control")
        return _broker(command, device=args[0], confirm=True)
    if command == "unblock":
        if len(args) != 2 or args[1] != "--confirm" or TOKEN_RE.fullmatch(args[0]) is None:
            return {"error": "unblock requires <rule-id> --confirm"}
        policy.require("device.usb", name="control")
        return _broker(command, rule_id=args[0].lower(), confirm=True)
    if command == "restore":
        if len(args) != 2 or args[1] != "--confirm" or TOKEN_RE.fullmatch(args[0]) is None:
            return {"error": "restore requires <backup-token> --confirm"}
        policy.require("device.usb", name="control")
        return _broker(command, token=args[0].lower(), confirm=True)
    return {"error": f"unknown command: {command}"}
