"""backup-center — mounted local Restic backup lifecycle."""

import json
import os
import re
import shutil
import subprocess
import sys

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from _shared.env_scrub import scrub_env  # noqa: E402

from cos_runtime import policy  # noqa: E402


CREDENTIAL_RE = re.compile(r"^[A-Za-z0-9_.:-]+/[A-Za-z0-9_.:-]+$")
SNAPSHOT_RE = re.compile(r"^(?:latest|[0-9A-Fa-f]{8,64})$")
TAG_RE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$")
TIMEOUT_SECS = int(os.environ.get("CLAW_BACKUP_CENTER_TIMEOUT", "10800"))


def _cos_binary():
    return os.environ.get("COS_BIN") or shutil.which("cos")


def _canonical(raw, name, allow_missing=False):
    if not isinstance(raw, str) or not os.path.isabs(raw) or len(raw) > 4096:
        raise ValueError(f"{name} must be an absolute path")
    if os.path.lexists(raw):
        if os.path.islink(raw):
            raise ValueError(f"{name} symlinks are not allowed")
        canonical = os.path.realpath(raw)
    elif allow_missing:
        canonical = os.path.join(os.path.realpath(os.path.dirname(raw)), os.path.basename(raw))
    else:
        raise ValueError(f"{name} does not exist")
    if canonical != raw:
        raise ValueError(f"use the canonical {name} path: {canonical}")
    return canonical


def _credential(raw):
    if CREDENTIAL_RE.fullmatch(raw or "") is None:
        raise ValueError("credential must use namespace/name form")
    return raw


def _broker(action, repo, credential, **values):
    cos_bin = _cos_binary()
    if not cos_bin:
        return {"error": "cos binary not found; Backup Center broker unavailable"}
    argv = [cos_bin, "__backup", action, "--repo", repo, "--credential", credential]
    for key in [
        "source",
        "destination",
        "snapshot",
        "tag",
        "keep_daily",
        "keep_weekly",
        "keep_monthly",
    ]:
        value = values.get(key)
        if value is not None:
            argv.extend([f"--{key.replace('_', '-')}", str(value)])
    if values.get("confirm"):
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
        return {"error": f"Backup Center broker exceeded {TIMEOUT_SECS}s"}
    payload_text = (result.stdout or "").strip() or (result.stderr or "").strip()
    try:
        payload = json.loads(payload_text) if payload_text else {}
    except json.JSONDecodeError:
        return {"error": "Backup Center broker returned invalid JSON"}
    if result.returncode != 0 and "error" not in payload:
        payload["error"] = f"Backup Center broker exited {result.returncode}"
    return payload


def run(command, args):
    if command == "__schema__":
        return _schema()
    confirm = "--confirm" in args
    values = [value for value in args if value != "--confirm"]
    try:
        if command in {"init", "snapshots", "check"}:
            if len(values) != 2 or confirm:
                raise ValueError(f"{command} requires <repo> <credential>")
            repo = _canonical(values[0], "repository", allow_missing=command == "init")
            credential = _credential(values[1])
            _require(repo, credential)
            return _broker(command, repo, credential)
        if command == "backup":
            if not 3 <= len(values) <= 4 or confirm:
                raise ValueError("backup requires <repo> <source> <credential> [tag]")
            repo = _canonical(values[0], "repository")
            source = _canonical(values[1], "source")
            credential = _credential(values[2])
            tag = values[3] if len(values) == 4 else None
            if tag is not None and TAG_RE.fullmatch(tag) is None:
                raise ValueError("invalid backup tag")
            _require(repo, credential, source)
            return _broker(command, repo, credential, source=source, tag=tag)
        if command == "restore":
            if len(values) != 4 or not confirm or SNAPSHOT_RE.fullmatch(values[1]) is None:
                raise ValueError("restore requires <repo> <snapshot> <destination> <credential> --confirm")
            repo = _canonical(values[0], "repository")
            destination = _canonical(values[2], "destination", allow_missing=True)
            credential = _credential(values[3])
            _require(repo, credential, destination)
            return _broker(command, repo, credential, snapshot=values[1].lower(), destination=destination, confirm=True)
        if command == "forget":
            if len(values) != 3 or not confirm or not re.fullmatch(r"[0-9A-Fa-f]{8,64}", values[1]):
                raise ValueError("forget requires <repo> <snapshot-id> <credential> --confirm")
            repo = _canonical(values[0], "repository")
            credential = _credential(values[2])
            _require(repo, credential)
            return _broker(command, repo, credential, snapshot=values[1].lower(), confirm=True)
        if command == "retention":
            if len(values) != 5 or not confirm:
                raise ValueError("retention requires <repo> <credential> <daily> <weekly> <monthly> --confirm")
            repo = _canonical(values[0], "repository")
            credential = _credential(values[1])
            daily, weekly, monthly = map(int, values[2:])
            if not (0 <= daily <= 365 and 0 <= weekly <= 260 and 0 <= monthly <= 120):
                raise ValueError("retention values exceed allowed bounds")
            _require(repo, credential)
            return _broker(command, repo, credential, keep_daily=daily, keep_weekly=weekly, keep_monthly=monthly, confirm=True)
    except (ValueError, TypeError) as exc:
        return {"error": str(exc)}
    return {"error": f"unknown command: {command}"}


def _require(repo, credential, other=None):
    policy.require("data.backup", path=repo)
    if other is not None:
        policy.require("data.backup", path=other)
    policy.require("secret.read", name=credential)


def _schema():
    return {
        command: {"description": f"Restic {command} operation", "parameters": []}
        for command in ["init", "snapshots", "check", "backup", "restore", "forget", "retention"]
    }
