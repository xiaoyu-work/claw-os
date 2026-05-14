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
    """tmp + rename; matches `crate::filelock::write_locked` semantics."""
    tmp = path.with_suffix(path.suffix + f".tmp.{os.getpid()}")
    tmp.write_text(json.dumps(payload, separators=(",", ":")))
    os.replace(tmp, path)


def _read_json(path: pathlib.Path):
    with open(path, "r") as f:
        return json.load(f)


def _count_lines(path: pathlib.Path) -> int:
    if not path.exists():
        return 0
    n = 0
    with open(path, "rb") as f:
        for _ in f:
            n += 1
    return n


def _append_jsonl_with_seq(path: pathlib.Path, record: dict, seq_field: str = "seq") -> int:
    """Atomic count+append under flock; mirrors `store::append_jsonl_with_seq`.

    Returns the seq number assigned. The same file lock that observes the
    line count covers the append, so two writers cannot collide on seq.
    """
    path.parent.mkdir(parents=True, exist_ok=True)
    fd = os.open(path, os.O_RDWR | os.O_CREAT | os.O_APPEND, 0o644)
    try:
        fcntl.flock(fd, fcntl.LOCK_EX)
        seq = _count_lines(path)
        record = dict(record)
        record[seq_field] = seq
        # Write through a fresh fd that respects the O_APPEND flag.
        line = json.dumps(record, separators=(",", ":")) + "\n"
        os.write(fd, line.encode("utf-8"))
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
        """Return every turn, oldest first. Tolerates a trailing half-line."""
        path = self.dir / "turns.jsonl"
        if not path.exists():
            return []
        out: _t.List[dict] = []
        with open(path, "r", encoding="utf-8", errors="replace") as f:
            for line in f:
                line = line.strip()
                if not line:
                    continue
                try:
                    out.append(json.loads(line))
                except json.JSONDecodeError:
                    # Half-written tail line — skip per Phase 1.5 contract.
                    continue
        return out

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
        path = self.dir / "mutations.jsonl"
        if not path.exists():
            return []
        out: _t.List[dict] = []
        with open(path, "r", encoding="utf-8", errors="replace") as f:
            for line in f:
                line = line.strip()
                if not line:
                    continue
                try:
                    out.append(json.loads(line))
                except json.JSONDecodeError:
                    continue
        return out

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
        tmp = target.with_suffix(".bin.tmp")
        tmp.write_bytes(payload)
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
