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


def _cos_binary() -> str | None:
    return os.environ.get("COS_BIN") or shutil.which("cos")


def _validate_identifier(value: object) -> str:
    if (
        not isinstance(value, str)
        or not 1 <= len(value) <= 512
        or value.startswith("-")
        or any(character in "\r\n\x00" or ord(character) < 32 for character in value)
    ):
        raise ValueError(
            "identifier must be a valid window identifier returned by list"
        )
    return value


def _validate_app_id(value: object) -> str:
    if not isinstance(value, str) or APP_ID_RE.fullmatch(value) is None:
        raise ValueError("app_id must be an exact desktop AppID returned by list")
    return value


def _broker(
    action: str,
    identifier: str | None = None,
    app_id: str | None = None,
) -> dict:
    cos_bin = _cos_binary()
    if not cos_bin:
        raise FileNotFoundError(
            "cos binary not found; Desktop Manager broker unavailable"
        )
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
        raise RuntimeError(f"Desktop Manager broker execution failed: {exc}") from exc
    except subprocess.TimeoutExpired as exc:
        raise RuntimeError(
            f"Desktop Manager broker exceeded {TIMEOUT_SECS}s"
        ) from exc
    payload_text = (result.stdout or "").strip() or (result.stderr or "").strip()
    try:
        payload = json.loads(payload_text) if payload_text else {}
    except json.JSONDecodeError as exc:
        raise RuntimeError("Desktop Manager broker returned invalid JSON") from exc
    if not isinstance(payload, dict):
        raise RuntimeError("Desktop Manager broker returned a non-object result")
    if "error" in payload:
        error = payload["error"]
        if not isinstance(error, str) or not error:
            raise RuntimeError(
                "Desktop Manager broker returned an invalid error payload"
            )
        raise RuntimeError(error)
    if result.returncode != 0:
        raise RuntimeError(f"Desktop Manager broker exited {result.returncode}")
    return payload


def list_windows() -> dict:
    policy.require("sys.observe", name="desktop")
    return _broker("list")


def focus_window(identifier: str) -> dict:
    identifier = _validate_identifier(identifier)
    policy.require("desktop.window", name="control")
    return _broker("focus", identifier=identifier)


def close_window(identifier: str) -> dict:
    identifier = _validate_identifier(identifier)
    policy.require("desktop.window", name="control")
    return _broker("close", identifier=identifier)


def restart_application(identifier: str, app_id: str) -> dict:
    identifier = _validate_identifier(identifier)
    app_id = _validate_app_id(app_id)
    policy.require("desktop.window", name="control")
    policy.require("desktop.launch", name=app_id)
    return _broker("restart", identifier=identifier, app_id=app_id)
