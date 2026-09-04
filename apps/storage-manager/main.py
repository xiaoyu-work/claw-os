"""storage-manager — UDisks2 control and storage health diagnostics."""

import json
import os
import re
import shutil
import subprocess
import sys
from typing import Literal

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from _shared.env_scrub import scrub_env  # noqa: E402

from cos_runtime import policy  # noqa: E402


DEVICE_RE = re.compile(r"^/dev/[A-Za-z0-9._/+:-]+$")
QUERY_TIMEOUT_SECS = int(os.environ.get("CLAW_STORAGE_TIMEOUT", "300"))
CHECK_TIMEOUT_SECS = int(os.environ.get("CLAW_STORAGE_CHECK_TIMEOUT", "2100"))
StorageAction = Literal["health", "check", "mount", "unmount", "eject"]
STORAGE_ACTIONS = frozenset({"health", "check", "mount", "unmount", "eject"})
DIAGNOSTIC_ACTIONS = frozenset({"health", "check"})


def _cos_binary() -> str | None:
    return os.environ.get("COS_BIN") or shutil.which("cos")


def _canonical_device(raw: object) -> str:
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


def _broker(action: str, device: str | None = None) -> dict:
    cos_bin = _cos_binary()
    if not cos_bin:
        raise FileNotFoundError(
            "cos binary not found; Storage Manager broker unavailable"
        )
    argv = [cos_bin, "__storage", action]
    if device is not None:
        argv.extend(["--device", device])
    timeout = CHECK_TIMEOUT_SECS if action == "check" else QUERY_TIMEOUT_SECS
    try:
        result = subprocess.run(
            argv,
            capture_output=True,
            text=True,
            timeout=timeout,
            stdin=subprocess.DEVNULL,
            env=scrub_env(),
            check=False,
        )
    except (FileNotFoundError, PermissionError) as exc:
        raise RuntimeError(f"Storage Manager broker execution failed: {exc}") from exc
    except subprocess.TimeoutExpired as exc:
        raise RuntimeError(
            f"Storage Manager broker exceeded {timeout}s for {action}"
        ) from exc
    payload_text = (result.stdout or "").strip() or (result.stderr or "").strip()
    try:
        payload = json.loads(payload_text) if payload_text else {}
    except json.JSONDecodeError as exc:
        raise RuntimeError("Storage Manager broker returned invalid JSON") from exc
    if not isinstance(payload, dict):
        raise RuntimeError("Storage Manager broker returned a non-object result")
    if "error" in payload:
        error = payload["error"]
        if not isinstance(error, str) or not error:
            raise RuntimeError(
                "Storage Manager broker returned an invalid error payload"
            )
        raise RuntimeError(error)
    if result.returncode != 0:
        raise RuntimeError(f"Storage Manager broker exited {result.returncode}")
    return payload


def status() -> dict:
    policy.require("sys.observe", name="storage")
    return _broker("status")


def _device_action(action: StorageAction, device: str) -> dict:
    if action not in STORAGE_ACTIONS:
        raise ValueError(f"unknown storage action: {action}")
    device = _canonical_device(device)
    if action in DIAGNOSTIC_ACTIONS:
        policy.require("sys.storage", name="diagnose")
    else:
        policy.require("sys.mount", path=device)
    return _broker(action, device)


def health(device: str) -> dict:
    return _device_action("health", device)


def check(device: str) -> dict:
    return _device_action("check", device)


def mount(device: str) -> dict:
    return _device_action("mount", device)


def unmount(device: str) -> dict:
    return _device_action("unmount", device)


def eject(device: str) -> dict:
    return _device_action("eject", device)
