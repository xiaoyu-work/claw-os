"""event-center — persistent system event subscriptions and pidfd watches."""

import json
import os
import shutil
import subprocess
import sys

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from _shared.env_scrub import scrub_env  # noqa: E402

from cos_runtime import policy  # noqa: E402


SOURCES = frozenset({"udev", "systemd", "journal", "storage", "security", "process"})
TIMEOUT_SECS = int(os.environ.get("CLAW_EVENT_CENTER_TIMEOUT", "120"))


def _cos_binary() -> str | None:
    return os.environ.get("COS_BIN") or shutil.which("cos")


def _broker(
    action: str,
    source: str | None = None,
    limit: int | None = None,
    pid: int | None = None,
) -> dict:
    cos_bin = _cos_binary()
    if not cos_bin:
        raise FileNotFoundError(
            "cos binary not found; Event Center broker unavailable"
        )
    argv = [cos_bin, "__events", action]
    if source is not None:
        argv.extend(["--source", source])
    if limit is not None:
        argv.extend(["--limit", str(limit)])
    if pid is not None:
        argv.extend(["--pid", str(pid)])
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
    except FileNotFoundError as exc:
        raise FileNotFoundError(
            f"Event Center broker executable not found: {cos_bin}"
        ) from exc
    except PermissionError as exc:
        raise PermissionError(
            f"permission denied launching Event Center broker: {cos_bin}"
        ) from exc
    except subprocess.TimeoutExpired as exc:
        raise TimeoutError(f"Event Center broker exceeded {TIMEOUT_SECS}s") from exc
    payload_text = (result.stdout or "").strip() or (result.stderr or "").strip()
    try:
        payload = json.loads(payload_text) if payload_text else {}
    except json.JSONDecodeError as exc:
        raise RuntimeError("Event Center broker returned invalid JSON") from exc
    if not isinstance(payload, dict):
        raise RuntimeError("Event Center broker returned a non-object result")
    if "error" in payload:
        error = payload["error"]
        if not isinstance(error, str) or not error:
            raise RuntimeError("Event Center broker returned an invalid error payload")
        raise RuntimeError(error)
    if result.returncode != 0:
        raise RuntimeError(f"Event Center broker exited {result.returncode}")
    return payload


def status() -> dict:
    policy.require("sys.events", name="observe")
    return _broker("status")


def recent(limit: int = 100, source: str | None = None) -> dict:
    if not isinstance(limit, int) or isinstance(limit, bool):
        raise ValueError("event limit must be an integer")
    if not 1 <= limit <= 1000:
        raise ValueError("recent limit must be 1..1000")
    if source is not None and (not isinstance(source, str) or source not in SOURCES):
        raise ValueError(f"unknown event source: {source}")
    policy.require("sys.events", name="observe")
    return _broker("recent", source=source, limit=limit)


def watch_pid(pid: int) -> dict:
    if not isinstance(pid, int) or isinstance(pid, bool):
        raise ValueError("pid must be an integer")
    if not 1 <= pid <= 2**32 - 1:
        raise ValueError("pid must be a positive 32-bit integer")
    policy.require("sys.events", name="observe")
    return _broker("watch-pid", pid=pid)
