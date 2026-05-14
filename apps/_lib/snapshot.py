"""Filesystem snapshot helper.

Every gated ``fs.write`` / ``fs.delete`` style operation in
:mod:`apps.fs.main` calls :func:`snapshot` *before* it touches the
disk. The snapshot is a pure copy of whatever currently lives at the
target path written under
``$COS_DATA_DIR/trash/<session_id>/<seq>/`` together with a
``meta.json`` sidecar that records the original path, the operation
that triggered the snapshot, and the timestamp.

``cos perms undo <session_id>`` later walks the directory in reverse
order and replays each entry — restoring the snapshotted bytes to the
recorded path (or, for the "absent" case, deleting whatever was put
there).

The contract here is intentionally simple so a future move to
btrfs / ZFS reflink can replace the bytes-on-disk implementation
without changing the on-disk layout:

::

    $COS_DATA_DIR/trash/<sid>/
        000001/
            meta.json   # { "op": "write", "path": "...", "kind": "file"|"dir"|"absent", ... }
            blob        # the file bytes  (omitted when kind = "absent")
            blob/       # OR a directory tree (omitted when kind = "absent")
        000002/
            ...

The sequence number (``000001``, ``000002``, …) is monotonic per
session so :func:`reverse_iter` can replay newest-first. Each
directory is self-describing; we never index entries in a single
manifest because that creates a torn-write hazard during long batch
operations.

See :doc:`docs/07-design-decisions.md` § 3 for why we picked pure
copy over filesystem-specific snapshotting.
"""

from __future__ import annotations

import json
import os
import shutil
import time
from typing import Iterator, Optional


# ---------------------------------------------------------------------------
# Layout helpers
# ---------------------------------------------------------------------------


def _data_root() -> str:
    return os.environ.get("COS_DATA_DIR", "/var/lib/cos")


def _session_id() -> Optional[str]:
    sid = os.environ.get("COS_SESSION")
    return sid if sid else None


def trash_dir(session_id: str) -> str:
    return os.path.join(_data_root(), "trash", session_id)


def _next_seq(sid_dir: str) -> str:
    if not os.path.isdir(sid_dir):
        return "000001"
    existing = [name for name in os.listdir(sid_dir) if name.isdigit()]
    if not existing:
        return "000001"
    return f"{max(int(n) for n in existing) + 1:06d}"


# ---------------------------------------------------------------------------
# Public API
# ---------------------------------------------------------------------------


def snapshot(path: str, op: str, *, session_id: Optional[str] = None) -> Optional[str]:
    """Snapshot ``path`` before a mutation. Returns the seq directory
    that was created, or ``None`` if snapshotting was skipped (no
    session context, snapshotting disabled, etc.).

    ``op`` is a short label — ``"write"``, ``"rm"``, ``"rename"``,
    ``"move"``, ``"copy"`` — that gets recorded in ``meta.json`` for
    audit / undo display purposes.

    When ``path`` does not exist (a fresh ``fs.write`` to a new
    location, or a ``rename`` whose destination is empty), the
    snapshot records ``kind = "absent"`` so the undo can delete
    whatever ends up there.
    """
    if os.environ.get("COS_SNAPSHOT", "1") in ("0", "off", "false", "no"):
        return None
    sid = session_id or _session_id()
    if not sid:
        return None

    sid_dir = trash_dir(sid)
    os.makedirs(sid_dir, exist_ok=True)
    seq = _next_seq(sid_dir)
    entry_dir = os.path.join(sid_dir, seq)
    os.makedirs(entry_dir, exist_ok=True)

    abs_path = os.path.abspath(path)
    if os.path.isdir(abs_path) and not os.path.islink(abs_path):
        kind = "dir"
        shutil.copytree(abs_path, os.path.join(entry_dir, "blob"), symlinks=True)
    elif os.path.exists(abs_path) or os.path.islink(abs_path):
        kind = "file"
        # copy2 preserves stat metadata; works for regular files +
        # symlinks (follow_symlinks=False keeps the link intact)
        shutil.copy2(abs_path, os.path.join(entry_dir, "blob"), follow_symlinks=False)
    else:
        kind = "absent"

    meta = {
        "op": op,
        "path": abs_path,
        "kind": kind,
        "snapshot_at": int(time.time()),
        "session": sid,
        "seq": seq,
    }
    with open(os.path.join(entry_dir, "meta.json"), "w") as f:
        json.dump(meta, f, indent=2)
    return entry_dir


def snapshot_pair(src: str, dst: str, op: str, *, session_id: Optional[str] = None) -> None:
    """Snapshot helper for two-path operations (``rename``, ``move``,
    ``copy``). Records both the source state (so we can put it back
    if the op overwrites the destination on the wrong path) and the
    destination state (so we can revert what we created).
    """
    snapshot(src, op, session_id=session_id)
    snapshot(dst, op, session_id=session_id)


# ---------------------------------------------------------------------------
# Replay / undo (called from Rust via `cos perms undo` — kept here so
# both the Python apps and the Rust kernel agree on the directory
# layout)
# ---------------------------------------------------------------------------


def iter_entries(session_id: str) -> Iterator[dict]:
    """Yield every snapshot entry recorded for ``session_id``, oldest
    first. Each yield is the parsed ``meta.json`` augmented with
    ``"_dir"`` (the absolute entry directory).
    """
    sid_dir = trash_dir(session_id)
    if not os.path.isdir(sid_dir):
        return
    for seq in sorted(os.listdir(sid_dir)):
        entry = os.path.join(sid_dir, seq)
        meta_path = os.path.join(entry, "meta.json")
        if not os.path.isfile(meta_path):
            continue
        with open(meta_path) as f:
            meta = json.load(f)
        meta["_dir"] = entry
        yield meta


def replay_reverse(session_id: str) -> list[dict]:
    """Walk snapshot entries newest-first and restore each. Returns a
    list of per-entry ``{seq, path, action, ok, error?}`` records the
    caller can render.
    """
    entries = list(iter_entries(session_id))
    report = []
    for meta in reversed(entries):
        rec = {"seq": meta.get("seq"), "path": meta.get("path"), "op": meta.get("op")}
        target = meta.get("path")
        kind = meta.get("kind")
        try:
            if kind == "absent":
                # The original state was "nothing here" — wipe whatever
                # the gated op put there.
                if os.path.isdir(target) and not os.path.islink(target):
                    shutil.rmtree(target)
                elif os.path.exists(target) or os.path.islink(target):
                    os.remove(target)
                rec["action"] = "removed"
            elif kind == "file":
                # Restore the file (overwrite whatever exists now).
                if os.path.isdir(target) and not os.path.islink(target):
                    shutil.rmtree(target)
                parent = os.path.dirname(target)
                if parent and not os.path.isdir(parent):
                    os.makedirs(parent, exist_ok=True)
                shutil.copy2(
                    os.path.join(meta["_dir"], "blob"), target, follow_symlinks=False
                )
                rec["action"] = "restored"
            elif kind == "dir":
                if os.path.exists(target):
                    if os.path.isdir(target) and not os.path.islink(target):
                        shutil.rmtree(target)
                    else:
                        os.remove(target)
                shutil.copytree(
                    os.path.join(meta["_dir"], "blob"), target, symlinks=True
                )
                rec["action"] = "restored"
            else:
                rec["action"] = "skipped"
                rec["error"] = f"unknown kind: {kind}"
            rec["ok"] = "error" not in rec
        except Exception as exc:  # pragma: no cover — defensive
            rec["ok"] = False
            rec["error"] = str(exc)
        report.append(rec)
    return report


def gc(older_than_days: int = 30) -> int:
    """Delete trash directories whose every entry is older than
    ``older_than_days``. Returns the number of session dirs deleted.
    """
    root = os.path.join(_data_root(), "trash")
    if not os.path.isdir(root):
        return 0
    cutoff = time.time() - older_than_days * 86400
    deleted = 0
    for sid in os.listdir(root):
        sid_dir = os.path.join(root, sid)
        if not os.path.isdir(sid_dir):
            continue
        newest = 0
        for seq in os.listdir(sid_dir):
            meta_path = os.path.join(sid_dir, seq, "meta.json")
            if os.path.isfile(meta_path):
                try:
                    with open(meta_path) as f:
                        ts = json.load(f).get("snapshot_at", 0)
                    newest = max(newest, ts)
                except Exception:
                    pass
        if newest and newest < cutoff:
            shutil.rmtree(sid_dir)
            deleted += 1
    return deleted
