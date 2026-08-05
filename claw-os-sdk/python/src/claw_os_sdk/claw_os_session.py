"""
session.py — minimal third-party agent contract for Claw OS durable sessions.

This module is the *demonstration* that the Phase 2-5 design point holds: a
session is a directory on disk, not an in-memory object. Any agent runtime
— ours, yours, anyone's — can attach to a session by reading and appending
to the same files. There is no RPC contract to break, no daemon to keep
in sync, no language-specific binding to vendor.

Concretely, you can use this module to:

  - List durable sessions:   `Session.list()`
  - Open one:                `s = Session.open(sid)`
  - Read its conversation:   `s.turns()` — newest last
  - Append a turn:           `s.append_turn(role, content, runtime="my-bot")`
  - Read the inverse log:    `s.mutations()`
  - Record a fs mutation:    `s.record_fs_write(path, prev_bytes)` (etc.)

Everything goes through the same JSON file schema documented in
`skills/claw-os/sessions.md`. No subprocess into `cos`, no network call.
The only system primitive used here is `flock(LOCK_EX)` on the JSONL
files so concurrent appenders can't interleave half-lines.

Usage from a third-party Python agent:

    from claw_os_session import Session

    sid = "ses_0019e25..."
    s = Session.open(sid)
    for t in s.turns()[-5:]:
        print(t["role"], t["content"])
    s.append_turn("assistant", "I'm taking over from here.", runtime="my-bot")
"""

from __future__ import annotations

import dataclasses
import datetime as _dt
import errno
import fcntl
import json
import math
import os
import pathlib
import re
import threading
import time
import typing as _t
import uuid

# ---------------------------------------------------------------------------
# Disk layout
# ---------------------------------------------------------------------------

_SID_RE = re.compile(r"^ses_[0-9a-f]{13}_[0-9a-f]{12}$")


def data_dir() -> pathlib.Path:
    """Resolve $COS_DATA_DIR with the same fallback as the Rust kernel."""
    raw = os.environ.get("COS_DATA_DIR")
    if raw:
        return pathlib.Path(raw)
    return pathlib.Path("/var/lib/cos")


def sessions_root() -> pathlib.Path:
    return data_dir() / "sessions"


def session_dir(sid: str) -> pathlib.Path:
    if not _SID_RE.match(sid):
        raise ValueError(
            f"invalid session id: {sid!r} (expected ses_<13 hex>_<12 hex>)"
        )
    return sessions_root() / sid


# ---------------------------------------------------------------------------
# Schema types — all field names match the Rust serde derive.
# ---------------------------------------------------------------------------


@dataclasses.dataclass
class Meta:
    id: str
    purpose: str
    status: str
    role: str
    parent_session: _t.Optional[str]
    creator_runtime: _t.Optional[str]
    budget: dict
    created_at: str
    ended_at: _t.Optional[str]


@dataclasses.dataclass
class Lease:
    pid: int
    runtime: _t.Optional[str]
    started_at: str
    heartbeat_at: str


class SessionCorruptError(RuntimeError):
    """A durable-session JSONL log violates the shared seq protocol."""


# ---------------------------------------------------------------------------
# IO helpers
# ---------------------------------------------------------------------------


def _now_rfc3339() -> str:
    return _dt.datetime.now(_dt.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def _atomic_write_json(path: pathlib.Path, payload) -> None:
    """tmp + fsync + rename; matches `crate::filelock::write_locked` semantics.

    The tmp suffix combines pid + native thread id + uuid so two
    threads in the same process (or two processes that happen to
    collide on pid after a fork) cannot race each other to the same
    ``.tmp.<pid>`` filename and then both ``os.replace`` partial
    writes over the target.

    We fsync the tmp file before renaming and fsync the parent
    directory after, so a power loss between write() and replace()
    leaves either the old file or the new file — never a torn write.
    """
    path.parent.mkdir(parents=True, exist_ok=True)
    suffix = f".tmp.{os.getpid()}.{threading.get_native_id()}.{uuid.uuid4().hex}"
    tmp = path.with_suffix(path.suffix + suffix)
    _validate_json_value(payload)
    data = json.dumps(payload, separators=(",", ":"), allow_nan=False).encode("utf-8")
    fd = os.open(tmp, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
    try:
        _write_all(fd, data)
        os.fsync(fd)
    finally:
        os.close(fd)
    os.replace(tmp, path)
    # Sync the directory so the rename is recorded on stable storage.
    try:
        dir_fd = os.open(str(path.parent), os.O_RDONLY | getattr(os, "O_DIRECTORY", 0))
    except OSError:
        return
    try:
        try:
            os.fsync(dir_fd)
        except OSError:
            pass
    finally:
        os.close(dir_fd)


def _read_json(path: pathlib.Path):
    with open(path, "r") as f:
        return json.load(f)


def _append_jsonl_with_seq(path: pathlib.Path, record: dict, seq_field: str = "seq") -> int:
    """Validate and append under flock; mirrors Rust's session store.

    JSONL is the only sequence source of truth. Under the same exclusive
    lock we verify strict ``seq == 0..N-1``, repair only an invalid
    trailing fragment, allocate ``N``, write the complete line, and
    fsync before unlocking.
    """
    path.parent.mkdir(parents=True, exist_ok=True)
    fd = os.open(path, os.O_RDWR | os.O_CREAT, 0o600)
    try:
        fcntl.flock(fd, fcntl.LOCK_EX)
        os.fchmod(fd, 0o600)
        records, truncate_to, append_newline = _scan_jsonl_fd(fd, path)
        if truncate_to is not None:
            os.ftruncate(fd, truncate_to)
        elif append_newline:
            os.lseek(fd, 0, os.SEEK_END)
            _write_all(fd, b"\n")
        seq = len(records)
        record = dict(record)
        record[seq_field] = seq
        _validate_json_value(record)
        line = json.dumps(record, separators=(",", ":"), allow_nan=False) + "\n"
        _validate_jsonl_record(path, line[:-1].encode("utf-8"), seq)
        os.lseek(fd, 0, os.SEEK_END)
        _write_all(fd, line.encode("utf-8"))
        os.fsync(fd)
        _fsync_directory(path.parent)
        return seq
    finally:
        try:
            fcntl.flock(fd, fcntl.LOCK_UN)
        finally:
            os.close(fd)


def _write_all(fd: int, payload: bytes) -> None:
    view = memoryview(payload)
    while view:
        written = os.write(fd, view)
        if written <= 0:
            raise OSError(errno.EIO, "short write to durable-session log")
        view = view[written:]


def _read_all(fd: int) -> bytes:
    os.lseek(fd, 0, os.SEEK_SET)
    chunks: _t.List[bytes] = []
    while True:
        chunk = os.read(fd, 1024 * 1024)
        if not chunk:
            return b"".join(chunks)
        chunks.append(chunk)


def _fsync_directory(path: pathlib.Path) -> None:
    fd = os.open(str(path), os.O_RDONLY | getattr(os, "O_DIRECTORY", 0))
    try:
        os.fsync(fd)
    finally:
        os.close(fd)


def _reject_json_constant(value: str):
    raise ValueError(f"non-finite JSON number: {value}")


def _parse_json_int(value: str) -> int:
    if value == "-0":
        raise ValueError("JSON integer -0 is not accepted by serde_json as u64 seq")
    parsed = int(value)
    if parsed < -(1 << 63) or parsed > (1 << 64) - 1:
        raise ValueError(f"JSON integer out of serde_json range: {value}")
    return parsed


def _parse_json_float(value: str) -> float:
    parsed = float(value)
    if not math.isfinite(parsed):
        raise ValueError(f"non-finite JSON number: {value}")
    return parsed


def _validate_json_value(value) -> None:
    if value is None or isinstance(value, (str, bool)):
        return
    if isinstance(value, int):
        if value < -(1 << 63) or value > (1 << 64) - 1:
            raise ValueError(f"JSON integer out of serde_json range: {value}")
        return
    if isinstance(value, float):
        if not math.isfinite(value):
            raise ValueError(f"non-finite JSON number: {value}")
        return
    if isinstance(value, list):
        for item in value:
            _validate_json_value(item)
        return
    if isinstance(value, dict):
        for key, item in value.items():
            if not isinstance(key, str):
                raise ValueError("JSON object keys must be strings")
            _validate_json_value(item)
        return
    raise ValueError(f"value is not JSON serializable: {type(value).__name__}")


def _validate_jsonl_record(path: pathlib.Path, line: bytes, expected_seq: int) -> dict:
    if not line:
        raise SessionCorruptError(f"{path}: empty record at seq {expected_seq}")
    try:
        decoded = line.decode("utf-8")
        record = json.loads(
            decoded,
            parse_constant=_reject_json_constant,
            parse_int=_parse_json_int,
            parse_float=_parse_json_float,
        )
    except (UnicodeDecodeError, json.JSONDecodeError, ValueError) as exc:
        raise SessionCorruptError(
            f"{path}: invalid JSON at seq {expected_seq}: {exc}"
        ) from exc
    if not isinstance(record, dict):
        raise SessionCorruptError(f"{path}: record at seq {expected_seq} is not an object")
    seq = record.get("seq")
    if isinstance(seq, bool) or not isinstance(seq, int) or seq < 0:
        raise SessionCorruptError(
            f"{path}: record at seq {expected_seq} has no unsigned seq"
        )
    if seq != expected_seq:
        raise SessionCorruptError(
            f"{path}: expected seq {expected_seq}, found {seq}"
        )
    return record


def _scan_jsonl_fd(
    fd: int, path: pathlib.Path
) -> _t.Tuple[_t.List[dict], _t.Optional[int], bool]:
    data = _read_all(fd)
    records: _t.List[dict] = []
    start = 0
    while True:
        end = data.find(b"\n", start)
        if end < 0:
            break
        records.append(_validate_jsonl_record(path, data[start:end], len(records)))
        start = end + 1

    tail = data[start:]
    if not tail:
        return records, None, False
    try:
        record = _validate_jsonl_record(path, tail, len(records))
    except SessionCorruptError as exc:
        try:
            tail.decode("utf-8")
            json.loads(
                tail,
                parse_constant=_reject_json_constant,
                parse_int=_parse_json_int,
                parse_float=_parse_json_float,
            )
        except (UnicodeDecodeError, json.JSONDecodeError, ValueError):
            return records, start, False
        raise exc
    records.append(record)
    return records, None, True


def _read_jsonl(path: pathlib.Path) -> _t.List[dict]:
    if not path.exists():
        return []
    fd = os.open(path, os.O_RDONLY)
    try:
        fcntl.flock(fd, fcntl.LOCK_SH)
        records, _, _ = _scan_jsonl_fd(fd, path)
        return records
    finally:
        try:
            fcntl.flock(fd, fcntl.LOCK_UN)
        finally:
            os.close(fd)


def _new_blob_id() -> str:
    return uuid.uuid4().hex


# ---------------------------------------------------------------------------
# Public API
# ---------------------------------------------------------------------------


class Session:
    """Read/write handle for one durable session.

    Construction is cheap; nothing is loaded until you call a method. The
    handle does not hold any flock — a third-party agent that wants to
    claim the session as the sole runner should also try to acquire the
    [`Lease`] sentinel, but that is optional. Nothing in this module
    races against the Rust kernel for IO, because every JSONL writer
    takes the same flock.
    """

    def __init__(self, sid: str) -> None:
        self.sid = sid
        self.dir = session_dir(sid)

    # ------------------------------------------------------------------
    # Discovery
    # ------------------------------------------------------------------

    @classmethod
    def list(cls) -> _t.List["Session"]:
        root = sessions_root()
        if not root.exists():
            return []
        out: _t.List[Session] = []
        for d in sorted(root.iterdir()):
            if d.name.startswith(".") or not d.is_dir():
                continue
            if not _SID_RE.match(d.name):
                continue
            if not (d / "meta.json").exists():
                continue
            out.append(cls(d.name))
        return out

    @classmethod
    def open(cls, sid: str) -> "Session":
        s = cls(sid)
        if not s.dir.exists() or not (s.dir / "meta.json").exists():
            raise FileNotFoundError(f"no such session: {sid}")
        return s

    # ------------------------------------------------------------------
    # meta.json + lease.json
    # ------------------------------------------------------------------

    def meta(self) -> Meta:
        raw = _read_json(self.dir / "meta.json")
        return Meta(
            id=raw["id"],
            purpose=raw["purpose"],
            status=raw["status"],
            role=raw["role"],
            parent_session=raw.get("parent_session"),
            creator_runtime=raw.get("creator_runtime"),
            budget=raw.get("budget", {}),
            created_at=raw["created_at"],
            ended_at=raw.get("ended_at"),
        )

    def lease(self) -> _t.Optional[Lease]:
        path = self.dir / "lease.json"
        if not path.exists():
            return None
        try:
            raw = _read_json(path)
        except json.JSONDecodeError:
            # Concurrent writer mid-rename; safe to treat as no holder.
            return None
        return Lease(
            pid=raw["pid"],
            runtime=raw.get("runtime"),
            started_at=raw["started_at"],
            heartbeat_at=raw["heartbeat_at"],
        )

    # ------------------------------------------------------------------
    # turns.jsonl
    # ------------------------------------------------------------------

    def turns(self) -> _t.List[dict]:
        """Return every turn, oldest first. Tolerates a trailing half-line.

        Validation is performed against one shared-locked snapshot.
        """
        return list(self.iter_turns())

    def iter_turns(self) -> _t.Iterator[dict]:
        """Yield validated turns, oldest first.

        A single invalid trailing fragment is ignored; corruption in
        any complete record raises :class:`SessionCorruptError`.
        """
        path = self.dir / "turns.jsonl"
        yield from _read_jsonl(path)

    def append_turn(
        self,
        role: str,
        content: str,
        *,
        runtime: _t.Optional[str] = None,
        tool_calls: _t.Optional[_t.List[dict]] = None,
        tool_call_id: _t.Optional[str] = None,
        usage: _t.Optional[dict] = None,
    ) -> int:
        """Append one turn. Returns the assigned `seq`.

        Field names match `core::session::turn::Turn` exactly so a Rust
        reader can deserialize what we write without translation.
        """
        if role not in ("user", "assistant", "system", "tool"):
            raise ValueError(
                f"invalid role: {role!r} (expected user|assistant|system|tool)"
            )
        record: dict = {
            "at": _now_rfc3339(),
            "role": role,
            "content": content,
        }
        if runtime is not None:
            record["runtime"] = runtime
        if tool_calls:
            record["tool_calls"] = tool_calls
        if tool_call_id is not None:
            record["tool_call_id"] = tool_call_id
        if usage is not None:
            record["usage"] = usage
        return _append_jsonl_with_seq(self.dir / "turns.jsonl", record)

    # ------------------------------------------------------------------
    # mutations.jsonl + files/inverse/
    # ------------------------------------------------------------------

    def mutations(self) -> _t.List[dict]:
        """Return every mutation from one validated, shared-locked snapshot."""
        return list(self.iter_mutations())

    def iter_mutations(self) -> _t.Iterator[dict]:
        """Yield validated mutation records lazily, oldest first."""
        path = self.dir / "mutations.jsonl"
        yield from _read_jsonl(path)

    def record_fs_write(
        self,
        path: str,
        *,
        prev_bytes: _t.Optional[bytes] = None,
        runtime: _t.Optional[str] = None,
    ) -> int:
        prev_blob = self._stash_blob(prev_bytes) if prev_bytes is not None else None
        return self._record(
            {"kind": "fs-write", "path": path, "prev_blob": prev_blob},
            runtime=runtime,
        )

    def record_fs_delete(
        self,
        path: str,
        *,
        prev_bytes: bytes,
        runtime: _t.Optional[str] = None,
    ) -> int:
        blob_id = self._stash_blob(prev_bytes)
        return self._record(
            {"kind": "fs-delete", "path": path, "blob_id": blob_id},
            runtime=runtime,
        )

    def record_fs_rename(
        self,
        from_path: str,
        to_path: str,
        *,
        runtime: _t.Optional[str] = None,
    ) -> int:
        return self._record(
            {"kind": "fs-rename", "from": from_path, "to": to_path},
            runtime=runtime,
        )

    def record_opaque(
        self,
        verb: str,
        forward: dict,
        inverse: dict,
        *,
        runtime: _t.Optional[str] = None,
    ) -> int:
        """For ops the kernel can't safely undo automatically.

        `cos agent undo` will surface these as `Skipped` so a human can
        decide what to do; we still record them for the audit trail.
        """
        return self._record(
            {
                "kind": "opaque",
                "verb": verb,
                "forward": forward,
                "inverse": inverse,
            },
            runtime=runtime,
        )

    # ------------------------------------------------------------------
    # internals
    # ------------------------------------------------------------------

    def _stash_blob(self, payload: bytes) -> str:
        blob_id = _new_blob_id()
        blob_dir = self.dir / "files" / "inverse"
        blob_dir.mkdir(parents=True, exist_ok=True)
        target = blob_dir / f"{blob_id}.bin"
        # Unique tmp suffix so two threads writing different blobs to
        # the same dir cannot collide on a shared ".bin.tmp" path.
        tmp = target.with_suffix(
            f".bin.tmp.{os.getpid()}.{threading.get_native_id()}.{uuid.uuid4().hex}"
        )
        fd = os.open(tmp, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
        try:
            _write_all(fd, payload)
            os.fsync(fd)
        finally:
            os.close(fd)
        os.replace(tmp, target)
        _fsync_directory(blob_dir)
        _fsync_directory(blob_dir.parent)
        return blob_id

    def _record(self, mutation: dict, *, runtime: _t.Optional[str]) -> int:
        record = {"at": _now_rfc3339(), "mutation": mutation}
        if runtime is not None:
            record["runtime"] = runtime
        return _append_jsonl_with_seq(self.dir / "mutations.jsonl", record)


__all__ = [
    "Session",
    "Meta",
    "Lease",
    "SessionCorruptError",
    "data_dir",
    "sessions_root",
    "session_dir",
]
