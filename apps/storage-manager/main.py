"""storage-manager — UDisks2 control and storage health diagnostics."""

import json
import os
import re
import shutil
import subprocess
import sys

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from _shared.env_scrub import scrub_env  # noqa: E402

from cos_runtime import policy  # noqa: E402


DEVICE_RE = re.compile(r"^/dev/[A-Za-z0-9._/+:-]+$")
QUERY_TIMEOUT_SECS = int(os.environ.get("CLAW_STORAGE_TIMEOUT", "300"))
CHECK_TIMEOUT_SECS = int(os.environ.get("CLAW_STORAGE_CHECK_TIMEOUT", "2100"))


def _cos_binary():
    return os.environ.get("COS_BIN") or shutil.which("cos")


def _canonical_device(raw):
    if (
        not isinstance(raw, str)
        or len(raw) > 4096
        or DEVICE_RE.fullmatch(raw) is None
        or any(part in {"", ".", ".."} for part in raw.split("/")[2:])
    ):
        raise ValueError("device must be a canonical absolute /dev path")
    canonical = os.path.realpath(raw)
    if canonical != raw:
        raise ValueError(f"use the canonical block-device path: {canonical}")
    return canonical


def _broker(action, device=None):
    cos_bin = _cos_binary()
    if not cos_bin:
        return {"error": "cos binary not found; Storage Manager broker unavailable"}
    argv = [cos_bin, "__storage", action]
    if device is not None:
        argv.extend(["--device", device])
    try:
        result = subprocess.run(
            argv,
            capture_output=True,
            text=True,
            timeout=CHECK_TIMEOUT_SECS if action == "check" else QUERY_TIMEOUT_SECS,
            stdin=subprocess.DEVNULL,
            env=scrub_env(),
            check=False,
        )
    except (FileNotFoundError, PermissionError) as exc:
        return {"error": str(exc)}
    except subprocess.TimeoutExpired:
        return {"error": f"Storage Manager broker exceeded its timeout for {action}"}
    payload_text = (result.stdout or "").strip() or (result.stderr or "").strip()
    try:
        payload = json.loads(payload_text) if payload_text else {}
    except json.JSONDecodeError:
        return {"error": "Storage Manager broker returned invalid JSON"}
    if result.returncode != 0 and "error" not in payload:
        payload["error"] = f"Storage Manager broker exited {result.returncode}"
    return payload


def run(command, args):
    from canonical_argv import normalize_canonical_argv
    args = normalize_canonical_argv(args)
    if command == "status":
        if args:
            return {"error": "status takes no arguments"}
        policy.require("sys.observe", name="storage")
        return _broker(command)
    if command not in {"health", "check", "mount", "unmount", "eject"}:
        return {"error": f"unknown command: {command}"}
    if len(args) != 1:
        return {"error": f"{command} requires one canonical /dev block-device path"}
    try:
        device = _canonical_device(args[0])
    except ValueError as exc:
        return {"error": str(exc)}
    if command in {"health", "check"}:
        policy.require("sys.storage", name="diagnose")
    else:
        policy.require("sys.mount", path=device)
    return _broker(command, device)
