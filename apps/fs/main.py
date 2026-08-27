"""fs — Agent-native file system with metadata and search."""

import base64
import json
import os
import shutil
import subprocess
import sys

# Import the shared helper package living at ``apps/_shared``. Each app
# runs as its own Python process so we splice the parent of this app
# directory onto sys.path before the import.
sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from _shared.atomic import atomic_write_bytes, atomic_write_json  # noqa: E402,F401
from _shared.env_scrub import scrub_env  # noqa: E402

from cos_runtime import policy  # noqa: E402
from cos_runtime import snapshot  # noqa: E402


WORKSPACE = "/workspace"
META_FILENAME = ".cos-meta.json"
MAX_READ_BYTES = 1_000_000  # 1 MB output limit for file reads
SEARCH_TIMEOUT = 30  # seconds
COPY_TIMEOUT = 120  # bounded for the rare shutil-uses-subprocess path

# Cap on how many bytes a single line-range read may return.
MAX_LINE_RANGE_BYTES = 16 * 1024 * 1024  # 16 MiB


def _abs(path):
    """Resolve ``path`` to its real, symlink-followed absolute form.

    SECURITY: every path that flows to ``policy.require`` MUST be
    realpath-resolved here first, otherwise a symlink like
    ``/workspace/escape -> /etc/shadow`` would pass an
    ``fs.read /workspace/escape`` capability check and then leak
    ``/etc/shadow`` on ``open()``. ``os.path.realpath`` resolves the
    final filename component too, closing the TOCTOU window between
    the cap check and the open.

    For paths that do not yet exist (write / mkdir / rename
    destinations), ``realpath`` resolves whichever leading components
    do exist and leaves the tail alone, which is exactly what we
    want.
    """
    return os.path.realpath(path)


def _open_nofollow(path, flags, mode=0o644):
    """``os.open`` with ``O_NOFOLLOW`` so a symlink swapped in after
    the cap check fails the open instead of silently following.

    ``O_NOFOLLOW`` only affects the *final* component; we rely on
    :func:`_abs` to have resolved every intermediate dir. The pair
    is the standard defence against symlink-race sandbox escapes.
    """
    nofollow = getattr(os, "O_NOFOLLOW", 0)
    return os.open(path, flags | nofollow, mode)


def _load_meta(directory):
    """Load the .cos-meta.json sidecar from a directory."""
    meta_path = os.path.join(directory, META_FILENAME)
    if os.path.isfile(meta_path):
        try:
            with open(meta_path) as f:
                return json.load(f)
        except (OSError, json.JSONDecodeError, ValueError):
            return {}
    return {}


def _save_meta(directory, meta):
    """Save the .cos-meta.json sidecar to a directory atomically."""
    meta_path = os.path.join(directory, META_FILENAME)
    atomic_write_json(meta_path, meta)


# ── Command handlers ─────────────────────────────────────────────


def cmd_ls(args):
    if not args:
        raise Exception("ls path default was not bound by the app bridge")
    path = _abs(args[0])
    policy.require("fs.read", path=path)
    if not os.path.isdir(path):
        return {"error": f"not a directory: {path}"}
    entries = sorted(os.listdir(path))
    files = []
    for name in entries:
        full = os.path.join(path, name)
        files.append({
            "name": name,
            "is_dir": os.path.isdir(full),
        })
    return {"path": path, "files": files}


def cmd_read(args):
    if not args:
        raise Exception("read requires a path argument")

    # Parse positional and optional args
    path = None
    offset = 0
    limit = MAX_READ_BYTES
    start_line = None
    end_line = None
    rest = list(args)

    # First positional arg is the path
    positional = []
    i = 0
    while i < len(rest):
        if rest[i] == "--offset" and i + 1 < len(rest):
            offset = int(rest[i + 1])
            i += 2
        elif rest[i] == "--limit" and i + 1 < len(rest):
            limit = int(rest[i + 1])
            i += 2
        elif rest[i] == "--start" and i + 1 < len(rest):
            start_line = int(rest[i + 1])
            i += 2
        elif rest[i] == "--end" and i + 1 < len(rest):
            end_line = int(rest[i + 1])
            i += 2
        else:
            positional.append(rest[i])
            i += 1

    if not positional:
        raise Exception("read requires a path argument")
    path = _abs(positional[0])
    policy.require("fs.read", path=path)

    if not os.path.isfile(path):
        return {"error": f"file not found: {path}"}

    # Line range mode: --start N [--end M]
    if start_line is not None:
        # SECURITY/OOM: stream line-by-line and only collect the
        # requested range, capped by MAX_LINE_RANGE_BYTES so we never
        # hand the agent a 4GB blob even if start/end span a huge
        # file. The old code did ``f.readlines()`` which materialised
        # the whole file in memory before slicing.
        s = max(1, start_line)
        e = end_line if end_line is not None else None
        if e is not None and e < s:
            return {
                "path": path,
                "content": "",
                "start_line": start_line,
                "end_line": e,
                "total_lines": 0,
                "lines_returned": 0,
            }
        selected_chunks = []
        selected_count = 0
        total_lines = 0
        truncated = False
        size = 0
        try:
            with open(path, "r", errors="replace") as f:
                for total_lines, line in enumerate(f, start=1):
                    if total_lines < s:
                        continue
                    if e is not None and total_lines > e:
                        # Still need to keep counting to report total_lines accurately.
                        continue
                    if not truncated:
                        new_size = size + len(line)
                        if new_size > MAX_LINE_RANGE_BYTES:
                            room = MAX_LINE_RANGE_BYTES - size
                            if room > 0:
                                selected_chunks.append(line[:room])
                                size = MAX_LINE_RANGE_BYTES
                                selected_count += 1
                            truncated = True
                        else:
                            selected_chunks.append(line)
                            size = new_size
                            selected_count += 1
        except OSError as exc:
            return {"error": f"could not read {path}: {exc}"}
        content = "".join(selected_chunks)
        result = {
            "path": path,
            "content": content,
            "start_line": start_line,
            "end_line": e if e is not None else total_lines,
            "total_lines": total_lines,
            "lines_returned": selected_count,
        }
        if truncated:
            result["truncated"] = True
        return result

    # Byte offset mode (original behavior)
    total_size = os.path.getsize(path)
    effective_limit = min(limit, MAX_READ_BYTES)

    try:
        fd = _open_nofollow(path, os.O_RDONLY)
    except OSError as exc:
        return {"error": f"could not open {path}: {exc}"}
    try:
        with os.fdopen(fd, "rb", closefd=True) as f:
            if offset > 0:
                f.seek(offset)
            raw = f.read(effective_limit + 1)
    except OSError as exc:
        return {"error": f"could not read {path}: {exc}"}

    truncated = len(raw) > effective_limit
    if truncated:
        raw = raw[:effective_limit]

    content = raw.decode("utf-8", errors="replace")
    result = {"path": path, "content": content}
    if offset > 0:
        result["offset"] = offset
    if truncated:
        result["truncated"] = True
        result["total_size"] = total_size
    return result


def cmd_write(args):
    if not args:
        raise Exception("write requires a path argument")
    path = _abs(args[0])
    policy.require("fs.write", path=path)
    # Parse --content flag
    content = None
    rest = args[1:]
    for i, arg in enumerate(rest):
        if arg == "--content" and i + 1 < len(rest):
            content = rest[i + 1]
            break
    if content is None:
        # Read from stdin
        content = sys.stdin.read()
    # Ensure parent directory exists
    parent = os.path.dirname(path)
    if parent and not os.path.isdir(parent):
        os.makedirs(parent, exist_ok=True)
    snapshot.snapshot(path, "write")
    # Atomic write — tmp + fsync + replace + fsync(parent). Prevents a
    # concurrent reader from seeing a half-written file and keeps the
    # original on the disk on crash.
    data = content.encode("utf-8") if isinstance(content, str) else bytes(content)
    atomic_write_bytes(path, data)
    return {"path": path, "bytes": len(data)}


def cmd_rm(args):
    if not args:
        raise Exception("rm requires a path argument")
    path = _abs(args[0])
    policy.require("fs.delete", path=path)
    if not os.path.exists(path):
        return {"error": f"not found: {path}"}
    snapshot.snapshot(path, "rm")
    if os.path.isdir(path):
        shutil.rmtree(path)
    else:
        os.remove(path)
    return {"removed": path}


def cmd_mkdir(args):
    if not args:
        raise Exception("mkdir requires a path argument")
    path = _abs(args[0])
    policy.require("fs.write", path=path)
    os.makedirs(path, exist_ok=True)
    return {"created": path}


def cmd_stat(args):
    if not args:
        raise Exception("stat requires a path argument")
    path = _abs(args[0])
    policy.require("fs.meta", path=path)
    if not os.path.exists(path):
        return {"error": f"not found: {path}"}
    st = os.stat(path)
    result = {
        "path": path,
        "size": st.st_size,
        "is_dir": os.path.isdir(path),
        "is_file": os.path.isfile(path),
        "modified": st.st_mtime,
        "created": st.st_ctime,
        "permissions": oct(st.st_mode),
    }
    # Include tags if present
    directory = os.path.dirname(path) if os.path.isfile(path) else path
    basename = os.path.basename(path)
    meta = _load_meta(directory)
    if basename in meta and "tags" in meta[basename]:
        result["tags"] = meta[basename]["tags"]
    return result


def cmd_search(args):
    if not args:
        raise Exception("search requires a query argument")
    if len(args) < 2:
        raise Exception("search path default was not bound by the app bridge")
    query = args[0]
    search_path = _abs(args[1])
    policy.require("fs.read", path=search_path)
    if not os.path.exists(search_path):
        return {"error": f"path not found: {search_path}"}
    matches = []
    # Use ripgrep with --json so we don't have to guess at separator
    # parsing on filenames that happen to contain `:` (URLs, Windows
    # drive letters mounted into the sandbox, etc.).
    try:
        result = subprocess.run(
            ["rg", "--json", "--color", "never", query, search_path],
            capture_output=True,
            text=True,
            timeout=SEARCH_TIMEOUT,
            stdin=subprocess.DEVNULL,
            env=scrub_env(),
            check=False,
        )
        for line in result.stdout.splitlines():
            line = line.strip()
            if not line:
                continue
            try:
                record = json.loads(line)
            except (json.JSONDecodeError, ValueError):
                continue
            if record.get("type") != "match":
                continue
            data = record.get("data") or {}
            path_field = (data.get("path") or {}).get("text") or ""
            line_no = data.get("line_number") or 0
            line_text = (data.get("lines") or {}).get("text") or ""
            matches.append({
                "path": path_field,
                "line": int(line_no),
                "text": line_text.rstrip("\n"),
            })
    except FileNotFoundError:
        # rg not installed, fall back to filename search only
        pass
    except subprocess.TimeoutExpired:
        pass
    # Also search filenames
    for dirpath, dirnames, filenames in os.walk(search_path):
        for fname in filenames:
            if query.lower() in fname.lower():
                full = os.path.join(dirpath, fname)
                # Avoid duplicating paths already found by rg
                if not any(m["path"] == full for m in matches):
                    matches.append({"path": full, "line": 0, "text": f"[filename match: {fname}]"})
    return {"query": query, "matches": matches}


def cmd_tag(args):
    if len(args) < 2:
        raise Exception("tag requires a path and at least one tag")
    path = _abs(args[0])
    new_tags = args[1:]
    if not os.path.isfile(path):
        return {"error": f"not a file: {path}"}
    directory = os.path.dirname(path)
    policy.require("fs.write", path=directory)
    basename = os.path.basename(path)
    meta = _load_meta(directory)
    if basename not in meta:
        meta[basename] = {}
    existing = meta[basename].get("tags", [])
    # Merge tags, avoiding duplicates
    for t in new_tags:
        if t not in existing:
            existing.append(t)
    meta[basename]["tags"] = existing
    _save_meta(directory, meta)
    return {"path": path, "tags": existing}


def cmd_recent(args):
    n = int(args[0]) if args else 10
    policy.require("fs.read", path=WORKSPACE)
    files = []
    for dirpath, dirnames, filenames in os.walk(WORKSPACE):
        # Skip hidden directories
        dirnames[:] = [d for d in dirnames if not d.startswith(".")]
        for fname in filenames:
            if fname.startswith("."):
                continue
            full = os.path.join(dirpath, fname)
            try:
                mtime = os.path.getmtime(full)
                files.append({"path": full, "modified": mtime})
            except OSError:
                pass
    # Sort by modified time descending
    files.sort(key=lambda x: x["modified"], reverse=True)
    return {"files": files[:n]}


def cmd_rename(args):
    """Rename / move a file or directory. Requires fs.delete on the
    source (we're removing it from the source path) AND fs.write on
    the destination (we're creating it there).

    Args: <src> <dst>
    """
    if len(args) < 2:
        raise Exception("rename requires <src> and <dst> arguments")
    src = _abs(args[0])
    dst = _abs(args[1])
    policy.require("fs.delete", path=src)
    policy.require("fs.write", path=dst)
    if not os.path.exists(src):
        return {"error": f"not found: {src}"}
    if os.path.exists(dst):
        return {"error": f"destination already exists: {dst}"}
    parent = os.path.dirname(dst)
    if parent and not os.path.isdir(parent):
        os.makedirs(parent, exist_ok=True)
    snapshot.snapshot_pair(src, dst, "rename")
    os.rename(src, dst)
    return {"from": src, "to": dst}


# Alias: "move" is what users / GUI menus call this. Same enforcement
# story (delete src + write dst) so we route through the same handler.
def cmd_move(args):
    return cmd_rename(args)


def cmd_copy(args):
    """Copy a file or directory tree. Requires fs.read on the source
    and fs.write on the destination.

    Args: <src> <dst>
    """
    if len(args) < 2:
        raise Exception("copy requires <src> and <dst> arguments")
    src = _abs(args[0])
    dst = _abs(args[1])
    policy.require("fs.read", path=src)
    policy.require("fs.write", path=dst)
    if not os.path.exists(src):
        return {"error": f"not found: {src}"}
    if os.path.exists(dst):
        return {"error": f"destination already exists: {dst}"}
    parent = os.path.dirname(dst)
    if parent and not os.path.isdir(parent):
        os.makedirs(parent, exist_ok=True)
    snapshot.snapshot(dst, "copy")
    if os.path.isdir(src):
        # SECURITY: pass ``symlinks=False`` so copytree follows symlinks
        # rather than preserving them. ``ignore_dangling_symlinks=True``
        # ensures a dangling symlink in the tree does not abort the
        # whole copy. After the copy, walk the destination and verify
        # every entry's realpath stays inside ``dst`` — refuses to leak
        # a symlink target outside the destination root.
        try:
            shutil.copytree(
                src,
                dst,
                symlinks=False,
                ignore_dangling_symlinks=True,
            )
        except shutil.Error as exc:
            return {"error": f"copy failed: {exc}"}
        dst_root = os.path.realpath(dst)
        leaked = []
        for dirpath, dirnames, filenames in os.walk(dst, followlinks=False):
            for name in dirnames + filenames:
                full = os.path.join(dirpath, name)
                try:
                    real = os.path.realpath(full)
                except OSError:
                    continue
                try:
                    common = os.path.commonpath([dst_root, real])
                except ValueError:
                    common = ""
                if common != dst_root:
                    leaked.append(full)
        if leaked:
            # Refuse the copy: tear down the destination so the agent
            # can't read through an escape link.
            try:
                shutil.rmtree(dst)
            except OSError:
                pass
            return {
                "error": "refused: destination contained symlinks pointing outside scope",
                "leaked": leaked,
            }
        return {"from": src, "to": dst, "kind": "dir"}
    shutil.copy2(src, dst)
    return {"from": src, "to": dst, "kind": "file"}


# Cap on a single read_bytes response so we don't hand a GUI a 4GB
# blob in one shot. Larger files are read in pages by passing
# successive --offset values.
MAX_READ_BYTES_BINARY = 8 * 1024 * 1024  # 8 MiB


def cmd_read_bytes(args):
    """Read a slice of a file as base64 — the binary-safe counterpart
    to ``cmd_read``. Doesn't decode as UTF-8, so it works for images
    and other binary content.

    Args: <path> [--offset N] [--limit N]
    """
    if not args:
        raise Exception("read_bytes requires a path argument")
    path = _abs(args[0])
    offset = 0
    limit = MAX_READ_BYTES_BINARY
    rest = args[1:]
    i = 0
    while i < len(rest):
        if rest[i] == "--offset" and i + 1 < len(rest):
            offset = int(rest[i + 1])
            i += 2
        elif rest[i] == "--limit" and i + 1 < len(rest):
            limit = int(rest[i + 1])
            i += 2
        else:
            i += 1
    policy.require("fs.read", path=path)
    if not os.path.isfile(path):
        return {"error": f"file not found: {path}"}
    total_size = os.path.getsize(path)
    effective_limit = min(limit, MAX_READ_BYTES_BINARY)
    try:
        fd = _open_nofollow(path, os.O_RDONLY)
    except OSError as exc:
        return {"error": f"could not open {path}: {exc}"}
    try:
        with os.fdopen(fd, "rb", closefd=True) as f:
            if offset > 0:
                f.seek(offset)
            raw = f.read(effective_limit + 1)
    except OSError as exc:
        return {"error": f"could not read {path}: {exc}"}
    truncated = len(raw) > effective_limit
    if truncated:
        raw = raw[:effective_limit]
    result = {
        "path": path,
        "offset": offset,
        "bytes_returned": len(raw),
        "total_size": total_size,
        "base64": base64.b64encode(raw).decode("ascii"),
    }
    if truncated:
        result["truncated"] = True
    return result


def cmd_write_bytes(args):
    """Write raw bytes (base64-encoded on the wire) to a file. The
    binary-safe counterpart to ``cmd_write``. Use this for clipboard
    paste of images, drag-drop of binary content, archive extraction,
    and any other path where the GUI has bytes rather than text.

    Args: <path> [--content <base64>]
    If ``--content`` is omitted, base64 is read from stdin.
    """
    if not args:
        raise Exception("write_bytes requires a path argument")
    path = _abs(args[0])
    policy.require("fs.write", path=path)
    content_b64 = None
    rest = args[1:]
    for i, arg in enumerate(rest):
        if arg == "--content" and i + 1 < len(rest):
            content_b64 = rest[i + 1]
            break
    if content_b64 is None:
        content_b64 = sys.stdin.read()
    try:
        data = base64.b64decode(content_b64, validate=False)
    except Exception as exc:
        return {"error": f"invalid base64: {exc}"}
    parent = os.path.dirname(path)
    if parent and not os.path.isdir(parent):
        os.makedirs(parent, exist_ok=True)
    snapshot.snapshot(path, "write_bytes")
    atomic_write_bytes(path, data)
    return {"path": path, "bytes": len(data)}


# ── Dispatch ──────────────────────────────────────────────────────

COMMANDS = {
    "ls": cmd_ls,
    "read": cmd_read,
    "read_bytes": cmd_read_bytes,
    "write": cmd_write,
    "write_bytes": cmd_write_bytes,
    "rm": cmd_rm,
    "mkdir": cmd_mkdir,
    "stat": cmd_stat,
    "search": cmd_search,
    "tag": cmd_tag,
    "recent": cmd_recent,
    "rename": cmd_rename,
    "move": cmd_move,
    "copy": cmd_copy,
}


def run(command, args):
    from canonical_argv import normalize_canonical_argv
    args = normalize_canonical_argv(args)
    handler = COMMANDS.get(command)
    if handler is None:
        return {"error": f"unknown command: {command}"}
    try:
        return handler(args)
    except policy.PermissionDenied as denied:
        return {"error": str(denied), "denial": denied.denial}
    except policy.PolicyUnavailable as exc:
        return {"error": f"capability check failed: {exc}"}
