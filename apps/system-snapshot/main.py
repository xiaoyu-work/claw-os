"""system-snapshot — full-system recovery points through clawd."""

import json
import os
import re
import shutil
import subprocess
import sys

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from _shared.env_scrub import scrub_env  # noqa: E402

from cos_runtime import policy  # noqa: E402


SNAPSHOT_ID_RE = re.compile(r"^snap_[0-9a-f]{32}$")
TIMEOUT_SECS = int(os.environ.get("CLAW_SNAPSHOT_TIMEOUT", "1800"))
DEFAULT_DESCRIPTION = "Claw OS recovery point"


def _cos_binary() -> str | None:
    return os.environ.get("COS_BIN") or shutil.which("cos")


def _broker(
    action: str,
    value: str | None = None,
    confirm: bool = False,
) -> dict:
    cos_bin = _cos_binary()
    if not cos_bin:
        raise FileNotFoundError(
            "cos binary not found; System Snapshot broker unavailable"
        )
    argv = [cos_bin, "__snapshot", action]
    if value is not None:
        argv.append(value)
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
    except (FileNotFoundError, PermissionError) as exc:
        raise RuntimeError(f"System Snapshot broker execution failed: {exc}") from exc
    except subprocess.TimeoutExpired as exc:
        raise RuntimeError(
            f"System Snapshot broker exceeded {TIMEOUT_SECS}s"
        ) from exc
    payload_text = (result.stdout or "").strip() or (result.stderr or "").strip()
    try:
        payload = json.loads(payload_text) if payload_text else {}
    except json.JSONDecodeError as exc:
        raise RuntimeError("System Snapshot broker returned invalid JSON") from exc
    if not isinstance(payload, dict):
        raise RuntimeError("System Snapshot broker returned a non-object result")
    error = payload.get("error")
    if error is not None:
        if not isinstance(error, str) or not error:
            raise RuntimeError(
                "System Snapshot broker returned an invalid error payload"
            )
        raise RuntimeError(error)
    if result.returncode != 0:
        raise RuntimeError(f"System Snapshot broker exited {result.returncode}")
    return payload


def _validate_snapshot_id(snapshot_id: str) -> str:
    if not isinstance(snapshot_id, str) or SNAPSHOT_ID_RE.fullmatch(snapshot_id) is None:
        raise ValueError("snapshot id must match snap_<32 lowercase hex digits>")
    return snapshot_id


def status() -> dict:
    policy.require("sys.observe", name="system-snapshots")
    return _broker("status")


def list_snapshots() -> dict:
    policy.require("sys.observe", name="system-snapshots")
    return _broker("list")


def create_snapshot(description: str | None = None) -> dict:
    if description is not None and not isinstance(description, str):
        raise ValueError("description must be text")
    policy.require("sys.snapshot", wild=True)
    return _broker("create", description if description is not None else DEFAULT_DESCRIPTION)


def delete_snapshot(snapshot_id: str) -> dict:
    snapshot_id = _validate_snapshot_id(snapshot_id)
    policy.require("sys.snapshot", wild=True)
    return _broker("delete", snapshot_id)


def rollback_snapshot(snapshot_id: str, confirm: bool) -> dict:
    snapshot_id = _validate_snapshot_id(snapshot_id)
    if confirm is not True:
        raise ValueError("rollback requires confirm=true")
    policy.require("sys.snapshot", wild=True)
    return _broker("rollback", snapshot_id, confirm=True)
