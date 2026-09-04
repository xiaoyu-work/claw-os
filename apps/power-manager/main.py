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


def _cos_binary() -> str | None:
    return os.environ.get("COS_BIN") or shutil.which("cos")


def _broker(action: str, confirm: bool = False) -> dict:
    cos_bin = _cos_binary()
    if not cos_bin:
        raise FileNotFoundError(
            "cos binary not found; Power Manager broker unavailable"
        )
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
    except subprocess.TimeoutExpired as exc:
        raise RuntimeError(
            f"Power Manager broker exceeded {TIMEOUT_SECS}s"
        ) from exc
    payload_text = (result.stdout or "").strip() or (result.stderr or "").strip()
    try:
        payload = json.loads(payload_text) if payload_text else {}
    except json.JSONDecodeError as exc:
        raise RuntimeError("Power Manager broker returned invalid JSON") from exc
    if not isinstance(payload, dict):
        raise RuntimeError("Power Manager broker returned a non-object result")
    error = payload.get("error")
    if result.returncode != 0 or error:
        if not isinstance(error, str) or not error:
            error = f"Power Manager broker exited {result.returncode}"
        raise RuntimeError(error)
    return payload


def status() -> dict:
    policy.require("sys.observe", name="power")
    return _broker("status")


def request_power(action: str, confirm: bool) -> dict:
    if action not in ACTIONS:
        raise ValueError(f"unknown power action: {action}")
    if confirm is not True:
        raise ValueError(f"{action} requires confirm=true")
    policy.require("sys.power", wild=True)
    return _broker(action, confirm=True)
