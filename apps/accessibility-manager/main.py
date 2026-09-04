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
TOGGLES = frozenset({"screen-reader", "magnifier", "invert"})
TIMEOUT_SECS = int(os.environ.get("CLAW_ACCESSIBILITY_MANAGER_TIMEOUT", "120"))


def _cos_binary():
    return os.environ.get("COS_BIN") or shutil.which("cos")


def _broker(action, value=None):
    cos_bin = _cos_binary()
    if not cos_bin:
        raise FileNotFoundError(
            "cos binary not found; Accessibility Manager broker unavailable"
        )
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
    except subprocess.TimeoutExpired as exc:
        raise RuntimeError(
            f"Accessibility Manager broker exceeded {TIMEOUT_SECS}s"
        ) from exc
    payload_text = (result.stdout or "").strip() or (result.stderr or "").strip()
    try:
        payload = json.loads(payload_text) if payload_text else {}
    except json.JSONDecodeError as exc:
        raise RuntimeError(
            "Accessibility Manager broker returned invalid JSON"
        ) from exc
    if not isinstance(payload, dict):
        raise RuntimeError("Accessibility Manager broker returned a non-object result")
    error = payload.get("error")
    if result.returncode != 0 or error:
        if not isinstance(error, str) or not error:
            error = f"Accessibility Manager broker exited {result.returncode}"
        raise RuntimeError(error)
    return payload


def status():
    policy.require("sys.observe", name="accessibility")
    return _broker("status")


def set_toggle(toggle, state):
    if toggle not in TOGGLES:
        raise ValueError(f"unknown accessibility toggle: {toggle}")
    if state not in {"on", "off"}:
        raise ValueError(f"{toggle} requires on|off")
    policy.require("ui.accessibility", name="control")
    return _broker(toggle, state)


def set_filter(value):
    if value not in FILTERS:
        raise ValueError(f"filter requires one of: {', '.join(sorted(FILTERS))}")
    policy.require("ui.accessibility", name="control")
    return _broker("filter", value)
