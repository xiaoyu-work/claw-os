"""Atomic file-write helpers for gateway state files.

State files in gateway apps (telegram's ``state.json`` offset, any
PID files, JSONL queues for inbound dedup) must survive a crash in
the middle of a write. The naive ``open(..., 'w'); f.write(...)``
pattern leaks zero-length / half-written files when the process dies
during the write — and that file is exactly the one the next run
will refuse to parse.

The recipe used here is the standard one:

1. Write payload bytes to ``<target>.tmp``.
2. Flush the FD and ``fsync`` it so the bytes hit the disk.
3. Atomically ``rename`` ``.tmp`` over ``<target>``.
4. ``fsync`` the *directory* so the rename is durable across power
   loss too. (POSIX only — Windows doesn't expose dir fsync and we
   fall back to the rename-is-atomic semantics it does provide.)

That sequence guarantees a reader sees either the previous complete
version or the new complete version, never a half file.
"""

from __future__ import annotations

import json
import os
import sys
from typing import Any, Iterable


def _fsync_dir(path: str) -> None:
    """fsync the directory containing ``path``. POSIX only."""
    if sys.platform == "win32":
        return
    dirpath = os.path.dirname(os.path.abspath(path)) or "."
    try:
        fd = os.open(dirpath, os.O_RDONLY)
    except OSError:
        # Some filesystems (e.g. /proc) don't permit dir-opens. Best
        # effort: skip and rely on rename atomicity.
        return
    try:
        try:
            os.fsync(fd)
        except OSError:
            pass
    finally:
        os.close(fd)


def atomic_write_bytes(path: str, data: bytes, *, mode: int = 0o600) -> None:
    """Write ``data`` to ``path`` atomically.

    Args:
        path: Final destination path.
        data: Payload bytes.
        mode: Permission bits applied to the tmp file before rename.
              Default ``0o600`` so credentials and state never widen
              past the owner. Callers that need a wider mode (e.g.
              ``0o644`` for a public-readable PID file) pass it.
    """
    parent = os.path.dirname(os.path.abspath(path))
    if parent:
        os.makedirs(parent, exist_ok=True)
    tmp = f"{path}.tmp"
    fd = os.open(tmp, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, mode)
    try:
        with os.fdopen(fd, "wb") as f:
            f.write(data)
            f.flush()
            try:
                os.fsync(f.fileno())
            except OSError:
                # Some VFS layers (tmpfs in containers, certain NFS
                # mounts) don't implement fsync. The rename is still
                # the atomic step; missing fsync only weakens
                # durability across crashes, not consistency.
                pass
    except Exception:
        # Best-effort cleanup on failure so we don't leave .tmp
        # turds behind.
        try:
            os.remove(tmp)
        except FileNotFoundError:
            pass
        raise
    os.replace(tmp, path)
    _fsync_dir(path)


def atomic_write_text(path: str, text: str, *, mode: int = 0o600) -> None:
    """Convenience wrapper around :func:`atomic_write_bytes` for text."""
    atomic_write_bytes(path, text.encode("utf-8"), mode=mode)


def atomic_write_json(path: str, payload: Any, *, mode: int = 0o600) -> None:
    """Serialise ``payload`` as JSON, then :func:`atomic_write_bytes`."""
    raw = json.dumps(payload, ensure_ascii=False).encode("utf-8")
    atomic_write_bytes(path, raw, mode=mode)


def atomic_write_jsonl(
    path: str, records: Iterable[Any], *, mode: int = 0o600
) -> None:
    """Write each ``records`` element as one JSON line, atomically."""
    lines = []
    for rec in records:
        lines.append(json.dumps(rec, ensure_ascii=False))
    blob = ("\n".join(lines) + ("\n" if lines else "")).encode("utf-8")
    atomic_write_bytes(path, blob, mode=mode)


__all__ = [
    "atomic_write_bytes",
    "atomic_write_text",
    "atomic_write_json",
    "atomic_write_jsonl",
]
