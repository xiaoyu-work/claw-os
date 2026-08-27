"""desktop-manager — COSMIC Wayland toplevel discovery and control."""

import json
import os
import re
import shutil
import subprocess
import sys

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from _shared.env_scrub import scrub_env  # noqa: E402

from cos_runtime import policy  # noqa: E402


TIMEOUT_SECS = int(os.environ.get("CLAW_DESKTOP_MANAGER_TIMEOUT", "120"))
APP_ID_RE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._-]{0,254}$")


def _cos_binary():
    return os.environ.get("COS_BIN") or shutil.which("cos")


def _valid_identifier(value):
    return (
        isinstance(value, str)
        and 1 <= len(value) <= 512
        and not value.startswith("-")
        and not any(character in "\r\n\x00" or ord(character) < 32 for character in value)
    )


def _broker(action, identifier=None, app_id=None):
    cos_bin = _cos_binary()
    if not cos_bin:
        return {"error": "cos binary not found; Desktop Manager broker unavailable"}
    argv = [cos_bin, "__desktop", action]
    if identifier is not None:
        argv.extend(["--identifier", identifier])
    if app_id is not None:
        argv.extend(["--app-id", app_id])
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
        return {"error": f"Desktop Manager broker exceeded {TIMEOUT_SECS}s"}
    payload_text = (result.stdout or "").strip() or (result.stderr or "").strip()
    try:
        payload = json.loads(payload_text) if payload_text else {}
    except json.JSONDecodeError:
        return {"error": "Desktop Manager broker returned invalid JSON"}
    if result.returncode != 0 and "error" not in payload:
        payload["error"] = f"Desktop Manager broker exited {result.returncode}"
    return payload


def run(command, args):
    if command == "list":
        if args:
            return {"error": "list takes no arguments"}
        policy.require("sys.observe", name="desktop")
        return _broker(command)
    if command in {"focus", "close"}:
        if len(args) != 1 or not _valid_identifier(args[0]):
            return {"error": f"{command} requires one identifier returned by list"}
        policy.require("desktop.window", name="control")
        return _broker(command, identifier=args[0])
    if command == "restart":
        if (
            len(args) != 2
            or not _valid_identifier(args[0])
            or APP_ID_RE.fullmatch(args[1]) is None
        ):
            return {"error": "restart requires <identifier> <exact-app-id> from list"}
        policy.require("desktop.window", name="control")
        policy.require("desktop.launch", name=args[1])
        return _broker(command, identifier=args[0], app_id=args[1])
    return {"error": f"unknown command: {command}"}
