"""security-center — sensitive read-only system security analysis."""

import json
import os
import shutil
import subprocess
import sys

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from _shared.env_scrub import scrub_env  # noqa: E402

from cos_runtime import policy  # noqa: E402


TIMEOUT_SECS = int(os.environ.get("CLAW_SECURITY_CENTER_TIMEOUT", "180"))


def _cos_binary() -> str | None:
    return os.environ.get("COS_BIN") or shutil.which("cos")


def _broker(action: str) -> dict:
    cos_bin = _cos_binary()
    if not cos_bin:
        raise FileNotFoundError(
            "cos binary not found; Security Center broker unavailable"
        )
    try:
        result = subprocess.run(
            [cos_bin, "__security", action],
            capture_output=True,
            text=True,
            timeout=TIMEOUT_SECS,
            stdin=subprocess.DEVNULL,
            env=scrub_env(),
            check=False,
        )
    except subprocess.TimeoutExpired as exc:
        raise RuntimeError(
            f"Security Center broker exceeded {TIMEOUT_SECS}s"
        ) from exc
    payload_text = (result.stdout or "").strip() or (result.stderr or "").strip()
    try:
        payload = json.loads(payload_text) if payload_text else {}
    except json.JSONDecodeError as exc:
        raise RuntimeError("Security Center broker returned invalid JSON") from exc
    if not isinstance(payload, dict):
        raise RuntimeError("Security Center broker returned a non-object result")
    error = payload.get("error")
    if result.returncode != 0 or error:
        if not isinstance(error, str) or not error:
            error = f"Security Center broker exited {result.returncode}"
        raise RuntimeError(error)
    return payload


def _inspect(action: str) -> dict:
    policy.require("sys.security", name="audit")
    return _broker(action)


def summary() -> dict:
    return _inspect("summary")


def auth() -> dict:
    return _inspect("auth")


def ssh() -> dict:
    return _inspect("ssh")


def sudo() -> dict:
    return _inspect("sudo")


def mac() -> dict:
    return _inspect("mac")


def ports() -> dict:
    return _inspect("ports")


def events() -> dict:
    return _inspect("events")
