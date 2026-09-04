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
SNAPSHOT_ID_RE = re.compile(r"^[0-9A-Fa-f]{8,64}$")
TAG_RE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$")
TIMEOUT_SECS = int(os.environ.get("CLAW_BACKUP_CENTER_TIMEOUT", "10800"))


def _cos_binary() -> str | None:
    return os.environ.get("COS_BIN") or shutil.which("cos")


def _canonical_path(
    raw: object,
    name: str,
    *,
    allow_missing: bool = False,
) -> str:
    if (
        not isinstance(raw, str)
        or not os.path.isabs(raw)
        or len(raw) > 4096
        or "\x00" in raw
    ):
        raise ValueError(f"{name} must be an absolute path without NUL bytes")
    if os.path.lexists(raw):
        if os.path.islink(raw):
            raise ValueError(f"{name} symlinks are not allowed")
    elif not allow_missing:
        raise ValueError(f"{name} does not exist")
    canonical = os.path.realpath(raw)
    if canonical != raw:
        raise ValueError(f"use the canonical {name} path: {canonical}")
    return canonical


def _credential(raw: object) -> str:
    if (
        not isinstance(raw, str)
        or "\x00" in raw
        or CREDENTIAL_RE.fullmatch(raw) is None
    ):
        raise ValueError("credential must use namespace/name form")
    return raw


def _tag(raw: object | None) -> str | None:
    if raw is None:
        return None
    if not isinstance(raw, str) or "\x00" in raw or TAG_RE.fullmatch(raw) is None:
        raise ValueError("invalid backup tag")
    return raw


def _snapshot(raw: object, *, allow_latest: bool) -> str:
    if not isinstance(raw, str) or "\x00" in raw:
        raise ValueError("snapshot must be latest or an 8..64 character hexadecimal id")
    if allow_latest and raw == "latest":
        return raw
    if SNAPSHOT_ID_RE.fullmatch(raw) is None:
        if allow_latest:
            raise ValueError(
                "snapshot must be latest or an 8..64 character hexadecimal id"
            )
        raise ValueError("snapshot must be an exact 8..64 character hexadecimal id")
    return raw.lower()


def _retention_count(raw: object, name: str, maximum: int) -> int:
    if isinstance(raw, bool) or not isinstance(raw, int):
        raise ValueError(f"{name} must be an integer")
    if not 0 <= raw <= maximum:
        raise ValueError(f"{name} must be 0..{maximum}")
    return raw


def _require_confirmation(action: str, confirm: object) -> None:
    if confirm is not True:
        raise ValueError(f"{action} requires confirm=true")


def _parse_payload(payload_text: str) -> dict:
    try:
        payload = json.loads(payload_text)
    except json.JSONDecodeError as exc:
        raise RuntimeError("Backup Center broker returned invalid JSON") from exc
    if not isinstance(payload, dict):
        raise RuntimeError("Backup Center broker returned a non-object result")
    if "error" in payload:
        error = payload["error"]
        if not isinstance(error, str) or not error:
            raise RuntimeError(
                "Backup Center broker returned an invalid error payload"
            )
        raise RuntimeError(error)
    return payload


def _broker(
    action: str,
    repo: str,
    credential: str,
    *,
    source: str | None = None,
    destination: str | None = None,
    snapshot: str | None = None,
    tag: str | None = None,
    keep_daily: int | None = None,
    keep_weekly: int | None = None,
    keep_monthly: int | None = None,
    confirm: bool = False,
) -> dict:
    cos_bin = _cos_binary()
    if not cos_bin:
        raise FileNotFoundError(
            "cos binary not found; Backup Center broker unavailable"
        )
    argv = [cos_bin, "__backup", action, "--repo", repo, "--credential", credential]
    if source is not None:
        argv.extend(["--source", source])
    if destination is not None:
        argv.extend(["--destination", destination])
    if snapshot is not None:
        argv.extend(["--snapshot", snapshot])
    if tag is not None:
        argv.extend(["--tag", tag])
    if keep_daily is not None:
        argv.extend(["--keep-daily", f"{keep_daily}"])
    if keep_weekly is not None:
        argv.extend(["--keep-weekly", f"{keep_weekly}"])
    if keep_monthly is not None:
        argv.extend(["--keep-monthly", f"{keep_monthly}"])
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
            f"Backup Center broker executable not found: {cos_bin}"
        ) from exc
    except PermissionError as exc:
        raise PermissionError(
            f"permission denied launching Backup Center broker: {cos_bin}"
        ) from exc
    except subprocess.TimeoutExpired as exc:
        raise TimeoutError(
            f"Backup Center broker exceeded {TIMEOUT_SECS}s for {action}"
        ) from exc

    payloads = []
    for output in (
        (result.stdout or "").strip(),
        (result.stderr or "").strip(),
    ):
        if not output:
            continue
        payloads.append(_parse_payload(output))
        if result.returncode == 0:
            break
    if result.returncode != 0:
        raise RuntimeError(f"Backup Center broker exited {result.returncode}")
    if not payloads:
        raise RuntimeError("Backup Center broker returned invalid JSON")
    return payloads[0]


def _require(repo: str, credential: str, other: str | None = None) -> None:
    policy.require("data.backup", path=repo)
    if other is not None:
        policy.require("data.backup", path=other)
    policy.require("secret.read", name=credential)


def init_repository(repo: str, credential: str) -> dict:
    repo = _canonical_path(repo, "repository", allow_missing=True)
    credential = _credential(credential)
    _require(repo, credential)
    return _broker("init", repo, credential)


def snapshots(repo: str, credential: str) -> dict:
    repo = _canonical_path(repo, "repository")
    credential = _credential(credential)
    _require(repo, credential)
    return _broker("snapshots", repo, credential)


def check(repo: str, credential: str) -> dict:
    repo = _canonical_path(repo, "repository")
    credential = _credential(credential)
    _require(repo, credential)
    return _broker("check", repo, credential)


def backup(
    repo: str,
    source: str,
    credential: str,
    tag: str | None = None,
) -> dict:
    repo = _canonical_path(repo, "repository")
    source = _canonical_path(source, "source")
    credential = _credential(credential)
    tag = _tag(tag)
    _require(repo, credential, source)
    return _broker("backup", repo, credential, source=source, tag=tag)


def restore(
    repo: str,
    snapshot: str,
    destination: str,
    credential: str,
    confirm: bool,
) -> dict:
    repo = _canonical_path(repo, "repository")
    snapshot = _snapshot(snapshot, allow_latest=True)
    destination = _canonical_path(
        destination,
        "destination",
        allow_missing=True,
    )
    credential = _credential(credential)
    _require_confirmation("restore", confirm)
    _require(repo, credential, destination)
    return _broker(
        "restore",
        repo,
        credential,
        destination=destination,
        snapshot=snapshot,
        confirm=True,
    )


def forget(
    repo: str,
    snapshot: str,
    credential: str,
    confirm: bool,
) -> dict:
    repo = _canonical_path(repo, "repository")
    snapshot = _snapshot(snapshot, allow_latest=False)
    credential = _credential(credential)
    _require_confirmation("forget", confirm)
    _require(repo, credential)
    return _broker(
        "forget",
        repo,
        credential,
        snapshot=snapshot,
        confirm=True,
    )


def retention(
    repo: str,
    credential: str,
    keep_daily: int,
    keep_weekly: int,
    keep_monthly: int,
    confirm: bool,
) -> dict:
    repo = _canonical_path(repo, "repository")
    credential = _credential(credential)
    keep_daily = _retention_count(keep_daily, "keep_daily", 365)
    keep_weekly = _retention_count(keep_weekly, "keep_weekly", 260)
    keep_monthly = _retention_count(keep_monthly, "keep_monthly", 120)
    _require_confirmation("retention", confirm)
    _require(repo, credential)
    return _broker(
        "retention",
        repo,
        credential,
        keep_daily=keep_daily,
        keep_weekly=keep_weekly,
        keep_monthly=keep_monthly,
        confirm=True,
    )
