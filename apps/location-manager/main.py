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
        return {"error": "cos binary not found; Location Manager broker unavailable"}
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
    except (FileNotFoundError, PermissionError) as exc:
        return {"error": str(exc)}
    except subprocess.TimeoutExpired:
        return {"error": f"Location Manager broker exceeded {TIMEOUT_SECS}s"}
    payload_text = (result.stdout or "").strip() or (result.stderr or "").strip()
    try:
        payload = json.loads(payload_text) if payload_text else {}
    except json.JSONDecodeError:
        return {"error": "Location Manager broker returned invalid JSON"}
    if result.returncode != 0 and "error" not in payload:
        payload["error"] = f"Location Manager broker exited {result.returncode}"
    return payload


def run(command, args):
    from canonical_argv import normalize_canonical_argv
    args = normalize_canonical_argv(args)
    if command not in {"locate", "timezone"}:
        return {"error": f"unknown command: {command}"}
    if len(args) > 1:
        return {"error": f"{command} accepts at most one accuracy"}
    accuracy = args[0] if args else "city"
    if accuracy not in ACCURACIES:
        return {"error": "accuracy must be country|city|neighborhood|street|exact"}
    policy.require("device.location", wild=True)
    return _broker(command, accuracy)
