"""fs — Agent-native file system with metadata and search."""

import json
import os
import shutil
import subprocess
import sys
import time


WORKSPACE = "/workspace"
META_FILENAME = ".cos-meta.json"
MAX_READ_BYTES = 1_000_000  # 1 MB output limit for file reads


def _abs(path):
    """Return absolute path."""
    return os.path.abspath(path)


def _load_meta(directory):
    """Load the .cos-meta.json sidecar from a directory."""
    meta_path = os.path.join(directory, META_FILENAME)
    if os.path.isfile(meta_path):
        with open(meta_path) as f:
            return json.load(f)
    return {}


def _save_meta(directory, meta):
    """Save the .cos-meta.json sidecar to a directory."""
    meta_path = os.path.join(directory, META_FILENAME)
    with open(meta_path, "w") as f:
        json.dump(meta, f, indent=2)


# ── Command handlers ─────────────────────────────────────────────


def cmd_ls(args):
    path = _abs(args[0]) if args else os.getcwd()
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

    if not os.path.isfile(path):
        return {"error": f"file not found: {path}"}

    # Line range mode: --start N [--end M]
    if start_line is not None:
        with open(path, "r", errors="replace") as f:
            lines = f.readlines()
        total_lines = len(lines)
        # 1-indexed, inclusive
        s = max(0, start_line - 1)
        e = end_line if end_line is not None else total_lines
        selected = lines[s:e]
        content = "".join(selected)
        if len(content) > MAX_READ_BYTES:
            content = content[:MAX_READ_BYTES]
            return {
                "path": path,
                "content": content,
                "start_line": start_line,
                "end_line": e,
                "total_lines": total_lines,
                "truncated": True,
            }
        return {
            "path": path,
            "content": content,
            "start_line": start_line,
            "end_line": e,
            "total_lines": total_lines,
            "lines_returned": len(selected),
        }

    # Byte offset mode (original behavior)
    total_size = os.path.getsize(path)
    effective_limit = min(limit, MAX_READ_BYTES)

    with open(path, "rb") as f:
        if offset > 0:
            f.seek(offset)
        raw = f.read(effective_limit + 1)

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
    with open(path, "w") as f:
        n = f.write(content)
    return {"path": path, "bytes": n}


def cmd_rm(args):
    if not args:
        raise Exception("rm requires a path argument")
    path = _abs(args[0])
    if not os.path.exists(path):
        return {"error": f"not found: {path}"}
    if os.path.isdir(path):
        shutil.rmtree(path)
    else:
        os.remove(path)
    return {"removed": path}


def cmd_mkdir(args):
    if not args:
        raise Exception("mkdir requires a path argument")
    path = _abs(args[0])
    os.makedirs(path, exist_ok=True)
    return {"created": path}


def cmd_stat(args):
    if not args:
        raise Exception("stat requires a path argument")
    path = _abs(args[0])
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
    query = args[0]
    search_path = _abs(args[1]) if len(args) > 1 else WORKSPACE
    if not os.path.exists(search_path):
        return {"error": f"path not found: {search_path}"}
    matches = []
    # Use ripgrep for content search
    try:
        result = subprocess.run(
            ["rg", "--no-heading", "--line-number", "--color", "never", query, search_path],
            capture_output=True,
            text=True,
            timeout=30,
        )
        for line in result.stdout.splitlines():
            # rg output: path:line_number:content
            parts = line.split(":", 2)
            if len(parts) >= 3:
                matches.append({
                    "path": parts[0],
                    "line": int(parts[1]),
                    "text": parts[2],
                })
            elif len(parts) == 2:
                matches.append({
                    "path": parts[0],
                    "line": int(parts[1]),
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
    if not os.path.exists(path):
        return {"error": f"not found: {path}"}
    directory = os.path.dirname(path) if os.path.isfile(path) else path
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


# ── Dispatch ──────────────────────────────────────────────────────

COMMANDS = {
    "ls": cmd_ls,
    "read": cmd_read,
    "write": cmd_write,
    "rm": cmd_rm,
    "mkdir": cmd_mkdir,
    "stat": cmd_stat,
    "search": cmd_search,
    "tag": cmd_tag,
    "recent": cmd_recent,
}


def run(command, args):
    handler = COMMANDS.get(command)
    if handler is None:
        return {"error": f"unknown command: {command}"}
    return handler(args)
