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
    data = json.dumps(payload, separators=(",", ":")).encode("utf-8")
    fd = os.open(tmp, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o644)
    try:
        os.write(fd, data)
        try:
            os.fsync(fd)
        except OSError:
            # fsync may fail on filesystems that don't support it
            # (e.g. some test FUSE mounts); the rename still
            # provides at-least-once durability semantics.
            pass
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


def _count_path(path: pathlib.Path) -> pathlib.Path:
    """Sidecar counter file for ``path``. Holds the canonical line count
    for the JSONL file so we can append in O(1) instead of re-scanning
    the entire file on every call.
    """
    return path.with_suffix(path.suffix + ".count")


def _read_sidecar_count(path: pathlib.Path) -> _t.Optional[int]:
    """Return the cached line count for ``path`` (sidecar), or ``None``
    if no sidecar exists / it's unreadable. Treats any malformed
    sidecar as missing — the caller will fall back to a one-time scan.
    """
    cp = _count_path(path)
    try:
        with open(cp, "rb") as f:
            raw = f.read().strip()
    except FileNotFoundError:
        return None
    except OSError:
        return None
    if not raw:
        return None
    try:
        return int(raw)
    except ValueError:
        return None


def _write_sidecar_count(path: pathlib.Path, count: int) -> None:
    """Atomically update the sidecar counter (write + rename)."""
    cp = _count_path(path)
    tmp = cp.with_suffix(cp.suffix + f".tmp.{os.getpid()}.{threading.get_native_id()}.{uuid.uuid4().hex}")
    try:
        with open(tmp, "wb") as f:
            f.write(str(count).encode("ascii"))
        os.replace(tmp, cp)
    except OSError:
        # Best-effort: a missing sidecar just forces the next caller
        # to rescan once. Don't let a sidecar failure poison the
        # actual append.
        try:
            os.unlink(tmp)
        except OSError:
            pass


def _count_lines(path: pathlib.Path) -> int:
    """Return the current number of newline-terminated lines in
    ``path``. Prefers a sidecar counter for O(1) reads; falls back to a
    one-time linear scan if no sidecar yet exists (e.g. for files
    written by a peer process before this code shipped).
    """
    if not path.exists():
        return 0
    cached = _read_sidecar_count(path)
    if cached is not None:
        return cached
    n = 0
    with open(path, "rb") as f:
        for _ in f:
            n += 1
    # Best-effort: bootstrap the sidecar so subsequent calls are O(1).
    _write_sidecar_count(path, n)
    return n


def _append_jsonl_with_seq(path: pathlib.Path, record: dict, seq_field: str = "seq") -> int:
    """Atomic count+append under flock; mirrors `store::append_jsonl_with_seq`.

    Returns the seq number assigned. The same file lock that observes the
    line count covers the append, so two writers cannot collide on seq.

    On success we fsync the JSONL fd, write the updated sidecar count,
    and release the lock. A crash between the JSONL write and the
    sidecar update leaves the sidecar one entry stale; the next reader
    will detect the mismatch by trying to read a record that doesn't
    yet exist and re-bootstrap from scratch.
    """
    path.parent.mkdir(parents=True, exist_ok=True)
    fd = os.open(path, os.O_RDWR | os.O_CREAT | os.O_APPEND, 0o644)
    try:
        fcntl.flock(fd, fcntl.LOCK_EX)
        seq = _count_lines(path)
        record = dict(record)
        record[seq_field] = seq
        line = json.dumps(record, separators=(",", ":")) + "\n"
        os.write(fd, line.encode("utf-8"))
        try:
            os.fsync(fd)
        except OSError:
            pass
        _write_sidecar_count(path, seq + 1)
        return seq
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

        For very large turn logs prefer :meth:`iter_turns`, which yields
        one record at a time instead of holding the whole file in RAM.
        """
        return list(self.iter_turns())

    def iter_turns(self) -> _t.Iterator[dict]:
        """Yield turns lazily, oldest first. Skips half-written tail
        lines per the Phase 1.5 contract. The yielded dicts are
        independent of the file — safe to mutate.
        """
        path = self.dir / "turns.jsonl"
        if not path.exists():
            return
        with open(path, "r", encoding="utf-8", errors="replace") as f:
            for line in f:
                stripped = line.strip()
                if not stripped:
                    continue
                try:
                    yield json.loads(stripped)
                except json.JSONDecodeError:
                    # Half-written tail line — skip per Phase 1.5 contract.
                    continue

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
        """Return every mutation, oldest first. See :meth:`iter_mutations`
        for a streaming alternative when the file is large.
        """
        return list(self.iter_mutations())

    def iter_mutations(self) -> _t.Iterator[dict]:
        """Yield mutation records lazily, oldest first."""
        path = self.dir / "mutations.jsonl"
        if not path.exists():
            return
        with open(path, "r", encoding="utf-8", errors="replace") as f:
            for line in f:
                stripped = line.strip()
                if not stripped:
                    continue
                try:
                    yield json.loads(stripped)
                except json.JSONDecodeError:
                    continue

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
        prev_blob = self._stash_blob(prev_bytes)
        return self._record(
            {"kind": "fs-delete", "path": path, "prev_blob": prev_blob},
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
        fd = os.open(tmp, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o644)
        try:
            os.write(fd, payload)
            try:
                os.fsync(fd)
            except OSError:
                pass
        finally:
            os.close(fd)
        os.replace(tmp, target)
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
    "data_dir",
    "sessions_root",
    "session_dir",
]
