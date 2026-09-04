"""hardware-center — structured system-wide hardware inventory."""

import json
import os
import shutil
import subprocess
import sys

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from _shared.env_scrub import scrub_env  # noqa: E402

from cos_runtime import policy  # noqa: E402


COMMANDS = frozenset(
    {"summary", "cpu", "gpu", "pci", "usb", "memory", "storage", "drivers", "thermal"}
)
TIMEOUT_SECS = int(os.environ.get("CLAW_HARDWARE_CENTER_TIMEOUT", "180"))


def _cos_binary():
    return os.environ.get("COS_BIN") or shutil.which("cos")


def _broker(action):
    cos_bin = _cos_binary()
    if not cos_bin:
        raise FileNotFoundError(
            "cos binary not found; Hardware Center broker unavailable"
        )
    try:
        result = subprocess.run(
            [cos_bin, "__hardware", action],
            capture_output=True,
            text=True,
            timeout=TIMEOUT_SECS,
            stdin=subprocess.DEVNULL,
            env=scrub_env(),
            check=False,
        )
    except subprocess.TimeoutExpired as exc:
        raise RuntimeError(
            f"Hardware Center broker exceeded {TIMEOUT_SECS}s"
        ) from exc
    payload_text = (result.stdout or "").strip() or (result.stderr or "").strip()
    try:
        payload = json.loads(payload_text) if payload_text else {}
    except json.JSONDecodeError as exc:
        raise RuntimeError("Hardware Center broker returned invalid JSON") from exc
    if not isinstance(payload, dict):
        raise RuntimeError("Hardware Center broker returned a non-object result")
    error = payload.get("error")
    if result.returncode != 0 or error:
        if not isinstance(error, str) or not error:
            error = f"Hardware Center broker exited {result.returncode}"
        raise RuntimeError(error)
    return payload


def inspect(command):
    if command not in COMMANDS:
        raise ValueError(f"unknown hardware command: {command}")
    policy.require("sys.observe", name="hardware")
    return _broker(command)
