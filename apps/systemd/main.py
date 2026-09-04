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


def _validate_unit(unit: str) -> None:
    if (
        not isinstance(unit, str)
        or len(unit) > 255
        or UNIT_RE.fullmatch(unit) is None
    ):
        raise ValueError(
            "unit must be a valid systemd name ending in "
            ".service, .socket, .timer, .mount, .target, or .path"
        )


def _cos_binary() -> str | None:
    return os.environ.get("COS_BIN") or shutil.which("cos")


def _broker(action: str, unit: str) -> dict:
    cos_bin = _cos_binary()
    if not cos_bin:
        raise FileNotFoundError(
            "cos binary not found; privileged systemd broker unavailable"
        )
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
    except (FileNotFoundError, PermissionError) as exc:
        raise RuntimeError(f"systemd broker execution failed: {exc}") from exc
    except subprocess.TimeoutExpired as exc:
        timeout = QUERY_TIMEOUT_SECS if action == "status" else CONTROL_TIMEOUT_SECS
        raise RuntimeError(
            f"systemd broker exceeded {timeout}s for {action} {unit}"
        ) from exc

    payload_text = (result.stdout or "").strip() or (result.stderr or "").strip()
    try:
        payload = json.loads(payload_text) if payload_text else {}
    except json.JSONDecodeError as exc:
        raise RuntimeError("systemd broker returned invalid JSON") from exc
    if not isinstance(payload, dict):
        raise RuntimeError("systemd broker returned a non-object result")
    error = payload.get("error")
    if error is not None:
        if not isinstance(error, str) or not error:
            raise RuntimeError("systemd broker returned an invalid error payload")
        raise RuntimeError(error)
    if result.returncode != 0:
        raise RuntimeError(f"systemd broker exited {result.returncode}")
    return payload


def status(unit: str) -> dict:
    _validate_unit(unit)
    policy.require("sys.observe", name=unit)
    return _broker("status", unit)


def control(action: str, unit: str) -> dict:
    if action not in MUTATING:
        raise ValueError(f"unknown systemd action: {action}")
    _validate_unit(unit)
    policy.require("sys.service", name=unit)
    return _broker(action, unit)
