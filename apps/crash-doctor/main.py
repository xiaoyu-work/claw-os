"""crash-doctor — privileged coredump and crash correlation."""

import json
import os
import re
import shutil
import subprocess
import sys

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from _shared.env_scrub import scrub_env  # noqa: E402

from cos_runtime import policy  # noqa: E402


MAX_SINCE_MINUTES = 7 * 24 * 60
MAX_LIMIT = 100
TIMEOUT_SECS = int(os.environ.get("CLAW_CRASH_DOCTOR_TIMEOUT", "300"))
COREDUMP_ID_RE = re.compile(
    r"^[0-9A-Fa-f]{32}:[1-9][0-9]{0,9}:[1-9][0-9]{0,19}$"
)


def _cos_binary() -> str | None:
    return os.environ.get("COS_BIN") or shutil.which("cos")


def _broker(
    action: str,
    since_minutes: int | None = None,
    limit: int | None = None,
    coredump_id: str | None = None,
) -> dict:
    cos_bin = _cos_binary()
    if not cos_bin:
        raise FileNotFoundError(
            "cos binary not found; Crash Doctor broker unavailable"
        )
    argv = [cos_bin, "__crash", action]
    if since_minutes is not None:
        argv.extend(["--since-minutes", str(since_minutes)])
    if limit is not None:
        argv.extend(["--limit", str(limit)])
    if coredump_id is not None:
        argv.extend(["--id", coredump_id])
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
            f"Crash Doctor broker executable not found: {cos_bin}"
        ) from exc
    except PermissionError as exc:
        raise PermissionError(
            f"permission denied launching Crash Doctor broker: {cos_bin}"
        ) from exc
    except subprocess.TimeoutExpired as exc:
        raise TimeoutError(
            f"Crash Doctor broker exceeded {TIMEOUT_SECS}s"
        ) from exc
    payload_text = (result.stdout or "").strip() or (result.stderr or "").strip()
    try:
        payload = json.loads(payload_text) if payload_text else {}
    except json.JSONDecodeError as exc:
        raise RuntimeError("Crash Doctor broker returned invalid JSON") from exc
    if not isinstance(payload, dict):
        raise RuntimeError("Crash Doctor broker returned a non-object result")
    if "error" in payload:
        error = payload["error"]
        if not isinstance(error, str) or not error:
            raise RuntimeError("Crash Doctor broker returned an invalid error payload")
        raise RuntimeError(error)
    if result.returncode != 0:
        raise RuntimeError(f"Crash Doctor broker exited {result.returncode}")
    return payload


def _validate_query_bounds(
    since_minutes: object,
    limit: object,
) -> tuple[int, int]:
    if type(since_minutes) is not int or type(limit) is not int:
        raise ValueError("since_minutes and limit must be integers")
    if not 1 <= since_minutes <= MAX_SINCE_MINUTES:
        raise ValueError(f"since_minutes must be 1..{MAX_SINCE_MINUTES}")
    if not 1 <= limit <= MAX_LIMIT:
        raise ValueError(f"limit must be 1..{MAX_LIMIT}")
    return since_minutes, limit


def _validate_coredump_id(coredump_id: object) -> str:
    if (
        not isinstance(coredump_id, str)
        or COREDUMP_ID_RE.fullmatch(coredump_id) is None
    ):
        raise ValueError(
            "id must be a <32-hex-boot-id>:<pid>:<timestamp-us> coredump id"
        )
    return coredump_id.lower()


def recent(since_minutes: int = 60, limit: int = 20) -> dict:
    since_minutes, limit = _validate_query_bounds(since_minutes, limit)
    policy.require("sys.crash", name="system")
    return _broker("recent", since_minutes, limit)


def diagnose(since_minutes: int = 60, limit: int = 20) -> dict:
    since_minutes, limit = _validate_query_bounds(since_minutes, limit)
    policy.require("sys.crash", name="system")
    return _broker("diagnose", since_minutes, limit)


def backtrace(coredump_id: str) -> dict:
    coredump_id = _validate_coredump_id(coredump_id)
    policy.require("sys.crash", name="system")
    return _broker("backtrace", coredump_id=coredump_id)
