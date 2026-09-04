"""location-manager — consent-gated GeoClue fixes and timezone suggestions."""

import json
import os
import shutil
import subprocess
import sys

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from _shared.env_scrub import scrub_env  # noqa: E402

from cos_runtime import policy  # noqa: E402


ACCURACIES = frozenset({"country", "city", "neighborhood", "street", "exact"})
TIMEOUT_SECS = int(os.environ.get("CLAW_LOCATION_MANAGER_TIMEOUT", "45"))


def _cos_binary():
    return os.environ.get("COS_BIN") or shutil.which("cos")


def _broker(action, accuracy):
    cos_bin = _cos_binary()
    if not cos_bin:
        raise FileNotFoundError(
            "cos binary not found; Location Manager broker unavailable"
        )
    argv = [cos_bin, "__location", action, "--accuracy", accuracy]
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
            f"Location Manager broker exceeded {TIMEOUT_SECS}s"
        ) from exc
    payload_text = (result.stdout or "").strip() or (result.stderr or "").strip()
    try:
        payload = json.loads(payload_text) if payload_text else {}
    except json.JSONDecodeError as exc:
        raise RuntimeError("Location Manager broker returned invalid JSON") from exc
    if not isinstance(payload, dict):
        raise RuntimeError("Location Manager broker returned a non-object result")
    error = payload.get("error")
    if result.returncode != 0 or error:
        if not isinstance(error, str) or not error:
            error = f"Location Manager broker exited {result.returncode}"
        raise RuntimeError(error)
    return payload


def query(action, accuracy="city"):
    if action not in {"locate", "timezone"}:
        raise ValueError(f"unknown location action: {action}")
    if accuracy not in ACCURACIES:
        raise ValueError(
            "accuracy must be country|city|neighborhood|street|exact"
        )
    policy.require("device.location", wild=True)
    return _broker(action, accuracy)
