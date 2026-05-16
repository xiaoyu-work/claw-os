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

Durable session mirroring
-------------------------

When ``COS_SESSION`` points at a *durable* session — i.e., a directory
exists at ``$COS_DATA_DIR/sessions/<sid>/`` with a ``meta.json`` —
every snapshot is **also** mirrored into the durable session's
``mutations.jsonl`` log + ``files/inverse/<blob_id>.bin`` blob store.
This is the cross-runtime contract the Rust kernel reads from
``cos perms undo`` (which then routes through
``core/src/session/rollback.rs``) and the future ``cos-apid`` socket.

The Python and Rust sides agree purely through the file format — see
``core/src/session/{mutation,recorder,inverse}.rs`` for the shapes.
We do not fork-exec into ``cos`` here: that would multiply the cost
of every fs.write by an interpreter startup, and it would defeat the
point of "agents talk through shared files, not RPC".
"""

from __future__ import annotations

import fcntl
import json
import os
import shutil
import stat
import time
import uuid
from typing import Iterator, Optional, Tuple


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


def _allocate_seq_dir(sid_dir: str) -> Tuple[str, str]:
    """Allocate the next sequence directory under ``sid_dir`` and
    return ``(seq, entry_dir_path)``.

    Pre-fix the snapshot code called ``_next_seq`` (which scanned
    ``listdir`` and picked ``max+1``) and then ``os.makedirs(
    entry_dir, exist_ok=True)``. Two snapshots running concurrently
    inside the same session — easy to hit because each app process
    is its own snapshot writer and the trash dir is shared — could
    both compute the same next seq, both succeed under
    ``exist_ok=True``, and stomp each other's ``blob`` /
    ``meta.json`` files. ``exist_ok=True`` masked the collision.

    The fix: probe with ``os.mkdir`` which is atomic on POSIX and
    raises ``FileExistsError`` on collision. On collision, advance
    past the conflict (by rescanning ``listdir`` so we also catch
    seq dirs created by other processes since our last scan) and
    retry. After a bounded number of attempts we give up — a real
    bug, not a thundering herd.
    """
    os.makedirs(sid_dir, exist_ok=True)

    def _scan_max() -> int:
        try:
            existing = [name for name in os.listdir(sid_dir) if name.isdigit()]
        except FileNotFoundError:
            return 0
        if not existing:
            return 0
        return max(int(n) for n in existing)

    next_n = _scan_max() + 1
    for _ in range(64):
        seq = f"{next_n:06d}"
        entry_dir = os.path.join(sid_dir, seq)
        try:
            os.mkdir(entry_dir)
            return seq, entry_dir
        except FileExistsError:
            # Another writer (this process or another) grabbed this
            # seq between our scan and our mkdir. Rescan rather than
            # blindly +1, because they may have taken multiple slots
            # while we were retrying.
            next_n = max(next_n + 1, _scan_max() + 1)
    raise RuntimeError(
        f"snapshot: could not allocate sequence directory in {sid_dir} "
        f"after 64 retries"
    )


# ---------------------------------------------------------------------------
# Durable session mirroring
# ---------------------------------------------------------------------------


def _durable_session_dir(session_id: str) -> Optional[str]:
    """Return the absolute path of the durable session directory if
    ``session_id`` names one, else ``None``. Defined as: a directory
    at ``$COS_DATA_DIR/sessions/<sid>/`` containing a ``meta.json``.
    """
    candidate = os.path.join(_data_root(), "sessions", session_id)
    if os.path.isfile(os.path.join(candidate, "meta.json")):
        return candidate
    return None


def _new_blob_id() -> str:
    """Match ``core/src/session/inverse.rs::new_blob_id``: uuid v4
    simple hex, 32 lowercase chars, no dashes.
    """
    return uuid.uuid4().hex


def _write_inverse_blob(session_dir: str, data: bytes) -> str:
    """Write ``data`` to ``<session_dir>/files/inverse/<id>.bin`` via
    tmp+rename. Returns the blob id. Mirrors the Rust
    :func:`session::inverse::write_blob` API and on-disk shape.
    """
    inv_dir = os.path.join(session_dir, "files", "inverse")
    os.makedirs(inv_dir, exist_ok=True)
    for _ in range(4):
        blob_id = _new_blob_id()
        target = os.path.join(inv_dir, f"{blob_id}.bin")
        if os.path.exists(target):
            continue
        tmp = target + ".tmp"
        with open(tmp, "wb") as f:
            f.write(data)
        os.rename(tmp, target)
        return blob_id
    raise RuntimeError("uuid collision four times in a row — the universe is broken")


def _now_rfc3339() -> str:
    # Include nanoseconds so concurrent snapshots in the same second
    # still get distinct timestamps. The `Z` suffix keeps the string
    # compatible with `serde(format = "rfc3339")` on the Rust side.
    now = time.time()
    secs = int(now)
    nanos = int(round((now - secs) * 1_000_000_000))
    # Clamp in case rounding pushed us into the next second.
    if nanos >= 1_000_000_000:
        secs += 1
        nanos -= 1_000_000_000
    base = time.strftime("%Y-%m-%dT%H:%M:%S", time.gmtime(secs))
    return f"{base}.{nanos:09d}Z"


def _append_mutation_record(
    session_dir: str,
    mutation: dict,
    *,
    runtime: Optional[str] = None,
    turn_seq: Optional[int] = None,
) -> int:
    """Append one record to ``<session_dir>/mutations.jsonl`` under an
    exclusive ``flock`` so a concurrent Rust ``record_mutation`` call
    can never race us. Returns the seq number assigned to this entry.

    Schema mirrors ``core::session::mutation::MutationRecord``:

    .. code-block:: json

        {
          "seq": 7,
          "at": "2026-05-12T12:34:56Z",
          "mutation": { "kind": "fs-write", "path": "...", "prev_blob": "..." },
          "runtime": "cos-app-fs",     // optional
          "turn_seq": 3                // optional
        }

    The kebab-case ``kind`` discriminator follows
    ``serde(tag = "kind", rename_all = "kebab-case")`` on the Rust
    enum.
    """
    path = os.path.join(session_dir, "mutations.jsonl")
    # Open r+ so the same fd both counts and appends; create if absent.
    fd = os.open(path, os.O_RDWR | os.O_CREAT | os.O_APPEND, 0o644)
    try:
        # Exclusive lock — every appender (Rust or Python) takes it.
        fcntl.flock(fd, fcntl.LOCK_EX)
        # Count existing lines for the seq. We use a fresh open to read
        # because the appending fd's position is unspecified after
        # O_APPEND on some platforms.
        try:
            with open(path, "rb") as rf:
                seq = sum(1 for _ in rf)
        except FileNotFoundError:  # pragma: no cover — we just created it
            seq = 0
        record = {
            "seq": seq,
            "at": _now_rfc3339(),
            "mutation": mutation,
        }
        if runtime is not None:
            record["runtime"] = runtime
        if turn_seq is not None:
            record["turn_seq"] = turn_seq
        line = json.dumps(record, separators=(",", ":")) + "\n"
        os.write(fd, line.encode("utf-8"))
        return seq
    finally:
        try:
            fcntl.flock(fd, fcntl.LOCK_UN)
        except OSError:  # pragma: no cover
            pass
        os.close(fd)


def _mirror_to_durable_session(
    session_id: str,
    op: str,
    abs_path: str,
    kind: str,
    *,
    runtime: Optional[str] = None,
) -> None:
    """If ``session_id`` points at a durable session, append the
    matching ``Mutation::{FsWrite,FsDelete}`` record (and its inverse
    blob, if needed) so ``cos perms undo`` can replay it via the Rust
    rollback engine.

    Best-effort: any IO failure is swallowed with a warning printed to
    stderr. The trash-dir snapshot remains the authoritative undo path
    for the legacy CLI flow, so a mirror failure does not break the
    user-visible undo.
    """
    session_dir = _durable_session_dir(session_id)
    if not session_dir:
        return

    try:
        if op in ("write", "rename", "move", "copy"):
            # The gated app is about to write `abs_path`. Snapshot the
            # current bytes (if the path existed and is a file) into
            # the inverse store, then record FsWrite.
            if kind == "file":
                with open(abs_path, "rb") as f:
                    blob_id = _write_inverse_blob(session_dir, f.read())
                mutation = {
                    "kind": "fs-write",
                    "path": abs_path,
                    "prev_blob": blob_id,
                }
            elif kind == "absent":
                mutation = {
                    "kind": "fs-write",
                    "path": abs_path,
                    "prev_blob": None,
                }
            else:
                # kind == "dir": the typed Rust schema doesn't have a
                # FsWrite-on-directory variant yet. Emit Opaque so the
                # rollback engine surfaces it as "manual review" rather
                # than silently losing the snapshot.
                mutation = {
                    "kind": "opaque",
                    "verb": f"fs.{op}.dir",
                    "forward": {"path": abs_path},
                    "inverse": {"hint": "directory state was snapshotted to trash"},
                }
        elif op == "rm":
            if kind == "file":
                with open(abs_path, "rb") as f:
                    blob_id = _write_inverse_blob(session_dir, f.read())
                mutation = {
                    "kind": "fs-delete",
                    "path": abs_path,
                    "blob_id": blob_id,
                }
            else:
                mutation = {
                    "kind": "opaque",
                    "verb": f"fs.rm.{kind}",
                    "forward": {"path": abs_path},
                    "inverse": {"hint": "non-file delete; check trash"},
                }
        else:
            mutation = {
                "kind": "opaque",
                "verb": f"fs.{op}",
                "forward": {"path": abs_path},
                "inverse": {},
            }

        _append_mutation_record(session_dir, mutation, runtime=runtime)
    except Exception as exc:  # pragma: no cover — best-effort mirror
        import sys

        print(
            f"snapshot: durable mirror failed for {abs_path}: {exc}",
            file=sys.stderr,
        )
        # Leave a marker in the session dir so a later validator
        # (or `cos perms undo`) can surface the inconsistency
        # instead of silently lying about the rollback path.
        try:
            sd = _durable_session_dir(session_id)
            if sd:
                marker_dir = os.path.join(sd, "mirror-errors")
                os.makedirs(marker_dir, exist_ok=True)
                marker = os.path.join(
                    marker_dir,
                    f"{int(time.time())}-{uuid.uuid4().hex[:8]}.json",
                )
                with open(marker, "w") as mf:
                    json.dump(
                        {
                            "at": _now_rfc3339(),
                            "op": op,
                            "path": abs_path,
                            "kind": kind,
                            "error": str(exc),
                        },
                        mf,
                    )
        except Exception:
            # If even the marker write fails there's nothing more
            # we can do without crashing the gated op.
            pass


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
    seq, entry_dir = _allocate_seq_dir(sid_dir)

    abs_path = os.path.abspath(path)
    # Classify the snapshot kind without ever following the final
    # symlink: an attacker who plants a symlink at a sensitive
    # absolute path (e.g. /etc/shadow) and then triggers a gated
    # write would otherwise dereference the link and snapshot the
    # symlink's *target* contents (or in `replay_reverse`, restore
    # those target contents over the link). Using `os.lstat` keeps
    # symlinks opaque to snapshotting.
    try:
        st = os.lstat(abs_path)
    except FileNotFoundError:
        st = None

    if st is None:
        kind = "absent"
    elif stat.S_ISLNK(st.st_mode):
        # Symlink at the leaf: snapshot the link itself.
        kind = "file"
        shutil.copy2(abs_path, os.path.join(entry_dir, "blob"), follow_symlinks=False)
    elif stat.S_ISDIR(st.st_mode):
        kind = "dir"
        shutil.copytree(abs_path, os.path.join(entry_dir, "blob"), symlinks=True)
    else:
        kind = "file"
        # copy2 preserves stat metadata; works for regular files +
        # symlinks (follow_symlinks=False keeps the link intact)
        shutil.copy2(abs_path, os.path.join(entry_dir, "blob"), follow_symlinks=False)

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

    # Also mirror the snapshot into the durable session log if the sid
    # names one. Best-effort; the trash dir is still authoritative for
    # the legacy CLI undo path.
    _mirror_to_durable_session(sid, op, abs_path, kind)

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


def _safe_to_delete_root(target: str) -> bool:
    """Return True iff ``target`` is inside a root we're willing to
    delete on undo. Used by :func:`replay_reverse` for ``kind == 'absent'``:
    if the gated op originally created a path at ``target``, undo
    should remove whatever is there now — but only if it's somewhere
    we have a legitimate claim to. The allowlist is intentionally
    narrow: the user's home dir, the data dir, the workspace dir, and
    anything explicitly opted-in via ``$COS_UNDO_DELETE_ROOTS`` (colon
    list).
    """
    target_abs = os.path.realpath(target)
    roots: list[str] = []
    home = os.environ.get("HOME")
    if home:
        roots.append(os.path.realpath(home))
    roots.append(os.path.realpath(_data_root()))
    workspace = os.environ.get("COS_WORKSPACE")
    if workspace:
        roots.append(os.path.realpath(workspace))
    extra = os.environ.get("COS_UNDO_DELETE_ROOTS", "")
    for r in extra.split(":"):
        r = r.strip()
        if r:
            roots.append(os.path.realpath(r))
    # Reject targets outside of every allowlisted root.
    for root in roots:
        if not root:
            continue
        # Make sure we don't accidentally allowlist "/" via an empty
        # env var.
        if root in ("/", ""):
            continue
        try:
            rel = os.path.relpath(target_abs, root)
        except ValueError:
            continue
        if not rel.startswith(".."):
            return True
    return False


def replay_reverse(session_id: str) -> list[dict]:
    """Walk snapshot entries newest-first and restore each. Returns a
    list of per-entry ``{seq, path, action, ok, error?}`` records the
    caller can render.

    If any individual entry fails we record the failure but continue
    with the remaining entries so the caller can see the full report.
    On any failure we also stash a marker file under
    ``<trash_dir>/.replay-failure-<timestamp>.json`` so a follow-up
    operator can find a half-restored tree and resume manually rather
    than silently move on. ``kind == "absent"`` entries require the
    target path to live inside an allowlisted root
    (see :func:`_safe_to_delete_root`); deletions of foreign-looking
    trees are skipped with a `denied` status.
    """
    entries = list(iter_entries(session_id))
    report: list[dict] = []
    any_failure = False
    for meta in reversed(entries):
        rec = {"seq": meta.get("seq"), "path": meta.get("path"), "op": meta.get("op")}
        target = meta.get("path")
        kind = meta.get("kind")
        try:
            if kind == "absent":
                if not target or not _safe_to_delete_root(target):
                    rec["action"] = "skipped"
                    rec["ok"] = False
                    rec["error"] = (
                        "refusing to delete outside allowlisted roots; "
                        "set $COS_UNDO_DELETE_ROOTS to opt in"
                    )
                else:
                    # The original state was "nothing here" — wipe
                    # whatever the gated op put there.
                    if os.path.isdir(target) and not os.path.islink(target):
                        shutil.rmtree(target)
                    elif os.path.lexists(target):
                        os.remove(target)
                    rec["action"] = "removed"
                    rec["ok"] = True
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
                rec["ok"] = True
            elif kind == "dir":
                if os.path.lexists(target):
                    if os.path.isdir(target) and not os.path.islink(target):
                        shutil.rmtree(target)
                    else:
                        os.remove(target)
                shutil.copytree(
                    os.path.join(meta["_dir"], "blob"), target, symlinks=True
                )
                rec["action"] = "restored"
                rec["ok"] = True
            else:
                rec["action"] = "skipped"
                rec["ok"] = False
                rec["error"] = f"unknown kind: {kind}"
        except Exception as exc:  # pragma: no cover — defensive
            rec["ok"] = False
            rec["error"] = str(exc)
        if rec.get("ok") is False:
            any_failure = True
        report.append(rec)
    if any_failure:
        # Best-effort marker so a follow-up validator / human can find
        # the half-restored state.
        try:
            sid_dir = trash_dir(session_id)
            marker_path = os.path.join(
                sid_dir,
                f".replay-failure-{int(time.time())}-{uuid.uuid4().hex[:8]}.json",
            )
            os.makedirs(sid_dir, exist_ok=True)
            with open(marker_path, "w") as mf:
                json.dump({"at": _now_rfc3339(), "report": report}, mf)
        except Exception:
            pass
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
