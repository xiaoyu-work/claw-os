"""Bounded App-owned file plans; applying always requires fresh path authority."""

from __future__ import annotations

import contextlib
import datetime
import difflib
import fcntl
import hashlib
import json
import os
import stat
import time
import uuid

from _shared.atomic import atomic_write_json
from canonical_argv import parse_canonical_argv
from claw_os_sdk.objects import format_reference
from cos_runtime import policy, snapshot
from cos_runtime.file_changes import FileChangeError, validate_path

MAX_TEXT_BYTES = 65536
MAX_PLAN_BYTES = 1024 * 1024
MAX_DIFF_BYTES = 65536
MAX_PLANS = 64
STORE_SCOPE = "fs-change-plans"
STATES = {"draft", "applying", "applied", "conflicted", "indeterminate", "expired"}
STATE_KEYS = {"sha256", "size", "device", "inode", "mode", "modified_ns", "changed_ns"}
RECORD_KEYS = {
    "schema", "kind", "plan_id", "path", "before", "before_text", "after_text",
    "created_at", "expires_at", "review", "state", "snapshot", "applied_at",
    "changed", "diagnostic",
}


class PlanError(Exception):
    def __init__(self, code, message):
        super().__init__(message)
        self.code = code


def _hash(data):
    return "sha256:" + hashlib.sha256(data).hexdigest()


def _now():
    return int(time.time())


def _timestamp(seconds):
    return datetime.datetime.fromtimestamp(
        seconds, datetime.timezone.utc
    ).isoformat().replace("+00:00", "Z")


def _seconds(value):
    if not isinstance(value, str) or not value.endswith("Z"):
        raise PlanError("plan_invalid", "plan timestamps must be UTC")
    try:
        parsed = datetime.datetime.fromisoformat(value.replace("Z", "+00:00"))
        return int(parsed.timestamp())
    except (ValueError, OverflowError) as error:
        raise PlanError("plan_invalid", "invalid plan timestamp") from error


def _identifier(value):
    try:
        parsed = uuid.UUID(value)
    except (ValueError, TypeError, AttributeError) as error:
        raise PlanError("invalid_args", "plan ID must be a canonical UUID") from error
    if str(parsed) != value:
        raise PlanError("invalid_args", "plan ID must be a canonical UUID")
    return value


def _digest(value):
    return isinstance(value, str) and len(value) == 71 and value.startswith(
        "sha256:"
    ) and all(character in "0123456789abcdef" for character in value[7:])


def _path(value):
    try:
        validate_path(value)
        path = os.path.realpath(value)
        validate_path(path)
    except FileChangeError as error:
        raise PlanError("invalid_args", str(error)) from error
    if len(path.encode("utf-8")) > 1024:
        raise PlanError("unsupported_file", "file plan paths must fit the 1024-byte object identity limit")
    return path


def _text_bytes(value):
    if not isinstance(value, str):
        raise PlanError("invalid_args", "file plan content must be text")
    try:
        data = value.encode("utf-8")
    except UnicodeError as error:
        raise PlanError("invalid_args", "file plan content must be valid UTF-8") from error
    if len(data) > MAX_TEXT_BYTES or b"\0" in data:
        raise PlanError("unsupported_file", "file plans support UTF-8 text without NUL, up to 64 KiB")
    return data


def _identity(metadata):
    return {
        "size": metadata.st_size,
        "device": metadata.st_dev,
        "inode": metadata.st_ino,
        "mode": metadata.st_mode,
        "modified_ns": metadata.st_mtime_ns,
        "changed_ns": metadata.st_ctime_ns,
    }


def _read_target(path):
    # An exact read mount exposes an existing file, not an absent file's parent.
    # The broker checks host absence and parent existence before creating it.
    try:
        fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    except FileNotFoundError:
        return None, ""
    with os.fdopen(fd, "rb") as stream:
        before = os.fstat(stream.fileno())
        if not stat.S_ISREG(before.st_mode) or before.st_nlink != 1:
            raise PlanError("unsupported_file", "file plans require a regular single-link target")
        if before.st_mode & (stat.S_ISUID | stat.S_ISGID | stat.S_ISVTX):
            raise PlanError("unsupported_file", "special permission bits are not supported")
        if before.st_size > MAX_TEXT_BYTES:
            raise PlanError("unsupported_file", "target exceeds the 64 KiB file plan limit")
        if os.listxattr(stream.fileno()):
            raise PlanError("unsupported_file", "file plans do not discard extended attributes")
        data = stream.read(MAX_TEXT_BYTES + 1)
        after = os.fstat(stream.fileno())
    if _identity(before) != _identity(after) or len(data) != after.st_size:
        raise PlanError("plan_conflict", "target changed while it was being read")
    try:
        text = data.decode("utf-8")
    except UnicodeError as error:
        raise PlanError("unsupported_file", "target is not UTF-8 text") from error
    _text_bytes(text)
    return dict(_identity(after), sha256=_hash(data)), text


def _private_directory(path, create=False):
    if create:
        try:
            os.mkdir(path, 0o700)
        except FileExistsError:
            pass
    metadata = os.stat(path, follow_symlinks=False)
    if not stat.S_ISDIR(metadata.st_mode) or metadata.st_uid != os.geteuid():
        raise PlanError("plan_storage", "plan directory must be a real owner-owned directory")
    if stat.S_IMODE(metadata.st_mode) & 0o077:
        raise PlanError("plan_storage", "plan directory must be private (0700)")
    return path


def _bucket(path, create=False):
    root = os.environ.get("COS_DATA_DIR")
    if not root or not os.path.isabs(root):
        raise PlanError("plan_storage", "App data directory is not configured")
    _private_directory(root)
    plans = _private_directory(os.path.join(root, "file-change-plans"), create)
    name = hashlib.sha256(path.encode("utf-8")).hexdigest()
    return _private_directory(os.path.join(plans, name), create)


def _private_file(fd):
    metadata = os.fstat(fd)
    if not stat.S_ISREG(metadata.st_mode) or metadata.st_nlink != 1:
        raise PlanError("plan_storage", "plan storage must use regular single-link files")
    if metadata.st_uid != os.geteuid() or stat.S_IMODE(metadata.st_mode) & 0o077:
        raise PlanError("plan_storage", "plan storage must be private and owner-owned")
    return metadata


@contextlib.contextmanager
def _locked(path, *, create=False, exclusive=False):
    directory = _bucket(path, create)
    flags = os.O_RDWR | os.O_NOFOLLOW | os.O_NONBLOCK
    if create:
        flags |= os.O_CREAT
    fd = os.open(os.path.join(directory, "bucket.lock"), flags, 0o600)
    try:
        _private_file(fd)
        fcntl.flock(fd, fcntl.LOCK_EX if exclusive else fcntl.LOCK_SH)
        yield directory
    finally:
        os.close(fd)


def _pairs(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise PlanError("plan_invalid", "duplicate field in stored plan")
        result[key] = value
    return result


def _review(plan):
    bound = {
        "schema": 1,
        "plan_id": plan["plan_id"],
        "path": plan["path"],
        "before": plan["before"],
        "after_sha256": _hash(_text_bytes(plan["after_text"])),
        "created_at": plan["created_at"],
        "expires_at": plan["expires_at"],
    }
    return _hash(json.dumps(bound, sort_keys=True, separators=(",", ":"), ensure_ascii=False).encode("utf-8"))


def _validate(plan, path, plan_id):
    if not isinstance(plan, dict) or set(plan) != RECORD_KEYS:
        raise PlanError("plan_invalid", "stored plan has an invalid shape")
    if type(plan["schema"]) is not int or plan["schema"] != 1 or plan["kind"] != "file_change_plan_record":
        raise PlanError("plan_invalid", "unsupported stored plan format")
    if plan["plan_id"] != plan_id or plan["path"] != path or not isinstance(plan["state"], str) or plan["state"] not in STATES:
        raise PlanError("plan_invalid", "stored plan identity or state does not match")
    before = _text_bytes(plan["before_text"])
    _text_bytes(plan["after_text"])
    state = plan["before"]
    if state is None:
        if before:
            raise PlanError("plan_invalid", "absent baseline cannot contain bytes")
    elif not isinstance(state, dict) or set(state) != STATE_KEYS:
        raise PlanError("plan_invalid", "invalid baseline fingerprint")
    else:
        for key in STATE_KEYS - {"sha256"}:
            if type(state[key]) is not int:
                raise PlanError("plan_invalid", "baseline metadata must contain integers")
        if any(state[key] < 0 for key in ("size", "device", "inode", "mode")):
            raise PlanError("plan_invalid", "baseline metadata is outside its domain")
        if any(state[key] > (1 << 64) - 1 for key in ("device", "inode")) or state["mode"] > (1 << 32) - 1:
            raise PlanError("plan_invalid", "baseline identity is outside its domain")
        if any(not -(1 << 63) <= state[key] < (1 << 63) for key in ("modified_ns", "changed_ns")):
            raise PlanError("plan_invalid", "baseline timestamps are outside their domain")
        if not stat.S_ISREG(state["mode"]) or state["size"] != len(before) or state["sha256"] != _hash(before):
            raise PlanError("plan_invalid", "baseline content does not match its fingerprint")
    duration = _seconds(plan["expires_at"]) - _seconds(plan["created_at"])
    if not 60 <= duration <= 86400:
        raise PlanError("plan_invalid", "plan lifetime must be between one minute and one day")
    if plan["review"] != _review(plan):
        raise PlanError("plan_invalid", "stored proposal does not match its review fingerprint")
    for field in ("snapshot", "applied_at", "diagnostic"):
        value = plan[field]
        if value is not None and (not isinstance(value, str) or len(value.encode("utf-8")) > 4096):
            raise PlanError("plan_invalid", "invalid plan lifecycle metadata")
    if plan["changed"] is not None and type(plan["changed"]) is not bool:
        raise PlanError("plan_invalid", "invalid change outcome")
    if plan["state"] == "applied":
        _seconds(plan["applied_at"])
        if type(plan["changed"]) is not bool:
            raise PlanError("plan_invalid", "applied plan is missing its reported change outcome")
    elif plan["applied_at"] is not None or plan["changed"] is not None:
        raise PlanError("plan_invalid", "unfinished plan cannot claim a completed apply outcome")


def _load(directory, path, plan_id):
    filename = os.path.join(directory, _identifier(plan_id) + ".json")
    fd = os.open(filename, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    with os.fdopen(fd, "rb") as stream:
        metadata = _private_file(stream.fileno())
        if metadata.st_size > MAX_PLAN_BYTES:
            raise PlanError("plan_invalid", "stored plan exceeds its size bound")
        raw = stream.read(MAX_PLAN_BYTES + 1)
    if len(raw) > MAX_PLAN_BYTES:
        raise PlanError("plan_invalid", "stored plan exceeds its size bound")
    try:
        plan = json.loads(raw.decode("utf-8"), object_pairs_hook=_pairs)
    except (UnicodeError, ValueError) as error:
        raise PlanError("plan_invalid", "stored plan is not valid JSON") from error
    _validate(plan, path, plan_id)
    return plan


def _save(directory, plan):
    _validate(plan, plan["path"], plan["plan_id"])
    atomic_write_json(
        os.path.join(directory, plan["plan_id"] + ".json"), plan, mode=0o600, strict=True
    )


def _ids(directory):
    ids = []
    for name in os.listdir(directory):
        if name == "bucket.lock":
            continue
        if not name.endswith(".json"):
            raise PlanError("plan_storage", "unexpected entry in the plan directory")
        ids.append(_identifier(name[:-5]))
    return ids


def _public(plan):
    before = _text_bytes(plan["before_text"])
    after = _text_bytes(plan["after_text"])
    lines = difflib.unified_diff(
        plan["before_text"].splitlines(keepends=True),
        plan["after_text"].splitlines(keepends=True),
        fromfile=plan["path"] + " (before)", tofile=plan["path"] + " (proposed)",
    )
    diff = "".join(
        line if line.endswith("\n") else line + "\n\\ No newline at end of file\n"
        for line in lines
    )
    encoded = diff.encode("utf-8")
    truncated = len(encoded) > MAX_DIFF_BYTES
    if truncated:
        end = MAX_DIFF_BYTES
        while encoded[end] & 0xC0 == 0x80:
            end -= 1
        diff = encoded[:end].decode("utf-8")
    state = plan["state"]
    if state == "draft" and _now() >= _seconds(plan["expires_at"]):
        state = "expired"
    warnings = [
        "App-reported file plan, not authorization or OS-confirmed effects.",
        "Apply requires an explicit path, review fingerprint, confirmation, and fresh file permissions.",
        "Cooperative calls are serialized, but uncooperative external writers can race after the final check.",
        "Recovery remains unknown; a snapshot reference is not proof that an OS rollback is available.",
    ]
    if state in ("applying", "indeterminate"):
        warnings.append("The apply outcome is unknown; automatic replay is refused.")
    return {
        "schema": 1, "kind": "file_change_plan", "plan_id": plan["plan_id"],
        "path": plan["path"], "state": state,
        "before_exists": plan["before"] is not None,
        "before_sha256": None if plan["before"] is None else plan["before"]["sha256"],
        "after_sha256": _hash(after), "before_bytes": len(before), "after_bytes": len(after),
        "would_change": plan["before"] is None or before != after,
        "review": plan["review"],
        "reference": format_reference({
            "app_id": "fs", "object_type": "change-plan", "object_id": plan["path"],
            "revision": plan["plan_id"],
        }),
        "diff": diff, "diff_truncated": truncated,
        "created_at": plan["created_at"], "expires_at": plan["expires_at"],
        "warnings": warnings, "snapshot": plan["snapshot"], "applied_at": plan["applied_at"],
        "changed": plan["changed"], "diagnostic": plan["diagnostic"],
    }


def _prepare(path, options):
    if "content" in options:
        content = options["content"]
    else:
        import sys
        raw = sys.stdin.buffer.read(MAX_TEXT_BYTES + 1)
        if not raw:
            raise PlanError("invalid_args", "provide --content (which may be empty) or nonempty explicit stdin")
        try:
            content = raw.decode("utf-8")
        except UnicodeError as error:
            raise PlanError("invalid_args", "stdin must be UTF-8 text") from error
    _text_bytes(content)
    try:
        ttl = int(options.get("ttl_seconds", "3600"))
    except ValueError as error:
        raise PlanError("invalid_args", "ttl-seconds must be an integer") from error
    if not 60 <= ttl <= 86400:
        raise PlanError("invalid_args", "ttl-seconds must be between 60 and 86400")
    policy.require("fs.read", path=path)
    policy.require("data.db.write", name=STORE_SCOPE)
    with _locked(path, create=True, exclusive=True) as directory:
        if len(_ids(directory)) >= MAX_PLANS:
            raise PlanError("plan_limit", "plan limit reached; prune consumed or expired plans explicitly")
        before, before_text = _read_target(path)
        now = _now()
        plan = {
            "schema": 1, "kind": "file_change_plan_record",
            "plan_id": str(uuid.uuid4()), "path": path, "before": before,
            "before_text": before_text, "after_text": content,
            "created_at": _timestamp(now), "expires_at": _timestamp(now + ttl),
            "review": "", "state": "draft", "snapshot": None,
            "applied_at": None, "changed": None, "diagnostic": None,
        }
        plan["review"] = _review(plan)
        _save(directory, plan)
        return _public(plan)


def _show(path, options):
    plan_id = _identifier(options["plan"]) if "plan" in options else None
    policy.require("fs.read", path=path)
    policy.require("data.db.read", name=STORE_SCOPE)
    with _locked(path) as directory:
        if plan_id is None:
            plans = [_load(directory, path, candidate) for candidate in _ids(directory)]
            if not plans:
                raise PlanError("plan_not_found", "no file plans exist for this path")
            plan = max(plans, key=lambda item: (item["created_at"], item["plan_id"]))
        else:
            plan = _load(directory, path, plan_id)
        return _public(plan)


def _apply(path, options):
    if options.get("confirm") is not True:
        raise PlanError("invalid_args", "plan_apply requires --confirm=true")
    plan_id = _identifier(options.get("plan"))
    review = options.get("review")
    if not _digest(review):
        raise PlanError("invalid_args", "plan_apply requires the exact --review fingerprint")
    policy.require("fs.read", path=path)
    policy.require("fs.write", path=path)
    policy.require("data.db.read", name=STORE_SCOPE)
    policy.require("data.db.write", name=STORE_SCOPE)
    with _locked(path, exclusive=True) as directory:
        plan = _load(directory, path, plan_id)
        if plan["state"] != "draft":
            code = "plan_indeterminate" if plan["state"] in ("applying", "indeterminate") else "plan_consumed"
            raise PlanError(code, "this plan cannot be replayed; inspect its state and create a fresh plan")
        if _now() >= _seconds(plan["expires_at"]):
            raise PlanError("plan_expired", "file plan has expired")
        if review != plan["review"]:
            raise PlanError("plan_conflict", "proposal changed after review")
        current, _ = _read_target(path)
        if current != plan["before"]:
            plan["state"] = "conflicted"
            plan["diagnostic"] = "Target changed since this plan was prepared; no target write was attempted."
            _save(directory, plan)
            raise PlanError("plan_conflict", plan["diagnostic"])
        from cos_runtime import file_changes
        plan["state"] = "applying"
        _save(directory, plan)
        try:
            plan["snapshot"] = snapshot.snapshot(path, "write")
            content = _text_bytes(plan["after_text"])
            result = file_changes.replace_file(path, plan["before"], content)
            if result.get("path") != path or result.get("sha256") != _hash(content) or result.get("bytes") != len(content) or type(result.get("changed")) is not bool:
                raise PlanError("plan_indeterminate", "file broker returned an inconsistent result")
            plan["state"] = "applied"
            plan["applied_at"] = _timestamp(_now())
            plan["changed"] = result["changed"]
            _save(directory, plan)
        except (OSError, RuntimeError, file_changes.FileChangeError, PlanError) as error:
            plan["state"] = "indeterminate"
            plan["applied_at"] = None
            plan["changed"] = None
            detail = str(error).encode("utf-8", errors="backslashreplace")
            reason = detail[:2048].decode("utf-8", errors="ignore")
            if len(detail) > 2048:
                reason += " [truncated]"
            plan["diagnostic"] = (
                "Apply may have taken effect; automatic replay is disabled. " + reason
            )
            try:
                _save(directory, plan)
            except OSError as storage_error:
                raise PlanError("plan_indeterminate", "Apply outcome and final plan persistence are uncertain; preserve the plan and inspect the target") from storage_error
            raise PlanError("plan_indeterminate", plan["diagnostic"]) from error
        return _public(plan)


def _prune(path, options):
    if options.get("confirm") is not True:
        raise PlanError("invalid_args", "plan_prune requires --confirm=true")
    try:
        keep = int(options.get("keep", "10"))
    except ValueError as error:
        raise PlanError("invalid_args", "keep must be an integer") from error
    if not 0 <= keep <= MAX_PLANS:
        raise PlanError("invalid_args", "keep must be between 0 and 64")
    policy.require("fs.read", path=path)
    policy.require("data.db.read", name=STORE_SCOPE)
    policy.require("data.db.write", name=STORE_SCOPE)
    with _locked(path, exclusive=True) as directory:
        plans = [_load(directory, path, plan_id) for plan_id in _ids(directory)]
        retired = sorted(
            (plan for plan in plans if plan["state"] in ("applied", "conflicted", "expired") or
             (plan["state"] == "draft" and _now() >= _seconds(plan["expires_at"]))),
            key=lambda plan: (plan["created_at"], plan["plan_id"]), reverse=True,
        )
        removed = []
        for plan in retired[keep:]:
            os.unlink(os.path.join(directory, plan["plan_id"] + ".json"))
            removed.append(plan["plan_id"])
        if removed:
            fd = os.open(directory, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
            try:
                os.fsync(fd)
            finally:
                os.close(fd)
        return {"removed": removed, "kept": len(plans) - len(removed), "target_changed": False}


def run(command, args):
    flags = {
        "plan_write": (("content", "ttl-seconds"), ()),
        "plan_show": (("plan",), ()),
        "plan_apply": (("plan", "review"), ("confirm",)),
        "plan_prune": (("keep",), ("confirm",)),
    }
    if command not in flags:
        return {"error": "unknown file plan operation", "code": "invalid_args"}
    try:
        positionals, options = parse_canonical_argv(
            args, value_flags=flags[command][0], bool_flags=flags[command][1]
        )
        if len(positionals) != 1:
            raise PlanError("invalid_args", "file plan operations require exactly one target path")
        path = _path(positionals[0])
        return {"plan_write": _prepare, "plan_show": _show, "plan_apply": _apply, "plan_prune": _prune}[command](path, options)
    except PlanError as error:
        return {"error": str(error), "code": error.code}
    except ValueError as error:
        return {"error": str(error), "code": "invalid_args"}
    except OSError as error:
        return {"error": f"file plan I/O failed: {error}", "code": "file_plan_io"}
