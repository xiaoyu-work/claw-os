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


def _cos_binary():
    return os.environ.get("COS_BIN") or shutil.which("cos")


def _canonical_target(raw):
    if not isinstance(raw, str) or not raw.startswith("/") or len(raw) > 4096:
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


def _canonical_source(raw):
    if not isinstance(raw, str) or not os.path.isabs(raw) or len(raw) > 4096:
        raise ValueError("source must be an absolute file path")
    if os.path.islink(raw):
        raise ValueError("source symlinks are not allowed")
    canonical = os.path.realpath(raw)
    if canonical != raw:
        raise ValueError(f"use the canonical source path: {canonical}")
    return canonical


def _broker(action, target, source=None, token=None, confirm=False):
    cos_bin = _cos_binary()
    if not cos_bin:
        return {"error": "cos binary not found; Safe Config Editor broker unavailable"}
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
    except (FileNotFoundError, PermissionError) as exc:
        return {"error": str(exc)}
    except subprocess.TimeoutExpired:
        return {"error": f"Safe Config Editor broker exceeded {TIMEOUT_SECS}s"}
    payload_text = (result.stdout or "").strip() or (result.stderr or "").strip()
    try:
        payload = json.loads(payload_text) if payload_text else {}
    except json.JSONDecodeError:
        return {"error": "Safe Config Editor broker returned invalid JSON"}
    if result.returncode != 0 and "error" not in payload:
        payload["error"] = f"Safe Config Editor broker exited {result.returncode}"
    return payload


def run(command, args):
    if command == "__schema__":
        return _schema()
    if command == "inspect":
        if len(args) != 1:
            return {"error": "inspect requires <target>"}
        try:
            target = _canonical_target(args[0])
        except ValueError as exc:
            return {"error": str(exc)}
        policy.require("sys.config", path=target)
        return _broker(command, target)
    if command in {"validate", "apply"}:
        confirm = "--confirm" in args
        values = [value for value in args if value != "--confirm"]
        if len(values) != 2 or (command == "apply" and not confirm) or (command == "validate" and confirm):
            return {"error": f"{command} requires <target> <source>" + (" --confirm" if command == "apply" else "")}
        try:
            target = _canonical_target(values[0])
            source = _canonical_source(values[1])
        except ValueError as exc:
            return {"error": str(exc)}
        policy.require("sys.config", path=target)
        policy.require("fs.read", path=source)
        return _broker(command, target, source=source, confirm=confirm)
    if command == "restore":
        confirm = "--confirm" in args
        values = [value for value in args if value != "--confirm"]
        if (
            len(values) != 2
            or not confirm
            or TOKEN_RE.fullmatch(values[1]) is None
        ):
            return {"error": "restore requires <target> <backup-token> --confirm"}
        try:
            target = _canonical_target(values[0])
        except ValueError as exc:
            return {"error": str(exc)}
        policy.require("sys.config", path=target)
        return _broker(command, target, token=values[1].lower(), confirm=True)
    return {"error": f"unknown command: {command}"}


def _schema():
    return {
        "inspect": {"description": "Read a supported /etc config", "parameters": []},
        "validate": {"description": "Validate staged content without applying it", "parameters": []},
        "apply": {"description": "Back up, validate, and atomically apply staged content", "parameters": []},
        "restore": {"description": "Restore a backup if the target has not changed since apply", "parameters": []},
    }
