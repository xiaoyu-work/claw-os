"""config-editor — validated, atomic, rollback-capable /etc edits."""

import json
import os
import re
import shutil
import subprocess
import sys

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from _shared.env_scrub import scrub_env  # noqa: E402

from cos_runtime import policy  # noqa: E402


TOKEN_RE = re.compile(r"^[0-9A-Fa-f]{32}$")
TIMEOUT_SECS = int(os.environ.get("CLAW_CONFIG_EDITOR_TIMEOUT", "180"))


def _cos_binary() -> str | None:
    return os.environ.get("COS_BIN") or shutil.which("cos")


def _canonical_target(raw: object) -> str:
    if (
        not isinstance(raw, str)
        or not raw.startswith("/")
        or len(raw) > 4096
        or "\x00" in raw
    ):
        raise ValueError("target must be an absolute /etc path")
    if os.path.lexists(raw):
        if os.path.islink(raw):
            raise ValueError("target symlinks are not allowed")
        canonical = os.path.realpath(raw)
    else:
        parent = os.path.realpath(os.path.dirname(raw))
        canonical = os.path.join(parent, os.path.basename(raw))
    if canonical == "/etc" or not canonical.startswith("/etc/"):
        raise ValueError("target must resolve below /etc")
    if canonical != raw:
        raise ValueError(f"use the canonical target path: {canonical}")
    return canonical


def _canonical_source(raw: object) -> str:
    if (
        not isinstance(raw, str)
        or not os.path.isabs(raw)
        or len(raw) > 4096
        or "\x00" in raw
    ):
        raise ValueError("source must be an absolute file path")
    if os.path.islink(raw):
        raise ValueError("source symlinks are not allowed")
    canonical = os.path.realpath(raw)
    if canonical != raw:
        raise ValueError(f"use the canonical source path: {canonical}")
    return canonical


def _backup_token(raw: object) -> str:
    if not isinstance(raw, str) or TOKEN_RE.fullmatch(raw) is None:
        raise ValueError("backup_token must be exactly 32 hexadecimal characters")
    return raw.lower()


def _require_confirmation(action: str, confirm: object) -> None:
    if confirm is not True:
        raise ValueError(f"{action} requires confirm=true")


def _broker(
    action: str,
    target: str,
    *,
    source: str | None = None,
    token: str | None = None,
    confirm: bool = False,
) -> dict:
    cos_bin = _cos_binary()
    if not cos_bin:
        raise FileNotFoundError(
            "cos binary not found; Safe Config Editor broker unavailable"
        )
    argv = [cos_bin, "__config", action, "--target", target]
    if source is not None:
        argv.extend(["--source", source])
    if token is not None:
        argv.extend(["--token", token])
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
    except FileNotFoundError as exc:
        raise FileNotFoundError(
            f"Safe Config Editor broker executable not found: {cos_bin}"
        ) from exc
    except PermissionError as exc:
        raise PermissionError(
            f"permission denied launching Safe Config Editor broker: {cos_bin}"
        ) from exc
    except subprocess.TimeoutExpired as exc:
        raise TimeoutError(
            f"Safe Config Editor broker exceeded {TIMEOUT_SECS}s for {action}"
        ) from exc
    payload_text = (result.stdout or "").strip() or (result.stderr or "").strip()
    try:
        payload = json.loads(payload_text) if payload_text else {}
    except json.JSONDecodeError as exc:
        raise RuntimeError(
            "Safe Config Editor broker returned invalid JSON"
        ) from exc
    if not isinstance(payload, dict):
        raise RuntimeError(
            "Safe Config Editor broker returned a non-object result"
        )
    if "error" in payload:
        error = payload["error"]
        if not isinstance(error, str) or not error:
            raise RuntimeError(
                "Safe Config Editor broker returned an invalid error payload"
            )
        raise RuntimeError(error)
    if result.returncode != 0:
        raise RuntimeError(
            f"Safe Config Editor broker exited {result.returncode}"
        )
    return payload


def inspect(target: str) -> dict:
    target = _canonical_target(target)
    policy.require("sys.config", path=target)
    return _broker("inspect", target)


def validate(target: str, source: str) -> dict:
    target = _canonical_target(target)
    source = _canonical_source(source)
    policy.require("sys.config", path=target)
    policy.require("fs.read", path=source)
    return _broker("validate", target, source=source)


def apply(target: str, source: str, confirm: bool) -> dict:
    target = _canonical_target(target)
    source = _canonical_source(source)
    _require_confirmation("apply", confirm)
    policy.require("sys.config", path=target)
    policy.require("fs.read", path=source)
    return _broker("apply", target, source=source, confirm=True)


def restore(target: str, backup_token: str, confirm: bool) -> dict:
    target = _canonical_target(target)
    backup_token = _backup_token(backup_token)
    _require_confirmation("restore", confirm)
    policy.require("sys.config", path=target)
    return _broker("restore", target, token=backup_token, confirm=True)
