"""Filesystem behavior for the manifest-bound fs MCP tools."""

import base64
import json
import os
import shutil
import stat as file_stat
import subprocess
import sys

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from _shared.atomic import atomic_write_bytes, atomic_write_json  # noqa: E402
from _shared.env_scrub import scrub_env  # noqa: E402
from _shared.paths import safe_realpath  # noqa: E402
from cos_runtime import policy, snapshot  # noqa: E402


WORKSPACE = "/workspace"
META_FILENAME = ".cos-meta.json"
MAX_READ_BYTES = 1_000_000
# MCP duplicates structured results as text; leave room for both representations
# in its 16 MiB frame and for the text result in the 8 MiB Host frame.
MAX_READ_BYTES_BINARY = 4 * 1024 * 1024
MAX_LINE_RANGE_BYTES = 1_000_000
SEARCH_TIMEOUT = 30


def _text(value: str, name: str, *, empty: bool = False) -> None:
    if not isinstance(value, str) or (not empty and not value):
        raise ValueError(f"{name} must be {'a' if empty else 'a nonempty'} string")


def _abs(path: str) -> str:
    _text(path, "path")
    if "\0" in path:
        raise ValueError("path must not contain NUL")
    return safe_realpath(path)


def _integer(value: int, name: str, minimum: int = 0) -> None:
    if type(value) is not int or value < minimum:
        raise ValueError(f"{name} must be an integer >= {minimum}")


def _open_nofollow(path: str, flags: int, mode: int = 0o644) -> int:
    # The resolved path is checked by policy; reject replacement leaf symlinks.
    return os.open(path, flags | os.O_NOFOLLOW, mode)


def _open_file(path: str):
    fd = _open_nofollow(path, os.O_RDONLY | os.O_NONBLOCK)
    if not file_stat.S_ISREG(os.fstat(fd).st_mode):
        os.close(fd)
        raise ValueError(f"not a regular file: {path}")
    return os.fdopen(fd, "rb")


def _meta_path(directory: str) -> str:
    path = os.path.join(directory, META_FILENAME)
    if os.path.islink(path):
        raise ValueError(f"metadata sidecar must not be a symlink: {path}")
    return path


def _load_meta(directory: str) -> dict:
    path = _meta_path(directory)
    try:
        stream = _open_file(path)
    except FileNotFoundError:
        return {}
    with stream:
        meta = json.load(stream)
    if not isinstance(meta, dict) or any(
        not isinstance(entry, dict)
        or (
            "tags" in entry
            and (
                not isinstance(entry["tags"], list)
                or any(not isinstance(tag, str) for tag in entry["tags"])
            )
        )
        for entry in meta.values()
    ):
        raise ValueError(f"invalid metadata sidecar: {path}")
    return meta


def ls(path: str = ".") -> dict:
    path = _abs(path)
    policy.require("fs.read", path=path)
    with os.scandir(path) as entries:
        files = [
            {"name": entry.name, "is_dir": entry.is_dir(follow_symlinks=False)}
            for entry in entries
        ]
    return {"path": path, "files": sorted(files, key=lambda entry: entry["name"])}


def _read_lines(path: str, start: int, end: int | None) -> dict:
    import io

    chunks = []
    size = total_lines = selected_count = 0
    line_number = 1
    last_selected = 0
    truncated = False
    with _open_file(path) as raw, io.TextIOWrapper(
        raw, encoding="utf-8", errors="replace"
    ) as stream:
        # Bounded fragments also handle a file containing one enormous line.
        while chunk := stream.readline(64 * 1024):
            total_lines = line_number
            if start <= line_number and (end is None or line_number <= end):
                data = chunk.encode("utf-8")
                room = MAX_LINE_RANGE_BYTES - size
                selected = data[:room].decode("utf-8", errors="ignore") if room else ""
                if selected and not truncated:
                    chunks.append(selected)
                    size += len(selected.encode("utf-8"))
                    if last_selected != line_number:
                        selected_count += 1
                        last_selected = line_number
                if len(data) > room:
                    truncated = True
            if chunk.endswith("\n"):
                line_number += 1
    result = {
        "path": path,
        "content": "".join(chunks),
        "start_line": start,
        "end_line": end if end is not None else total_lines,
        "total_lines": total_lines,
        "lines_returned": selected_count,
    }
    if truncated:
        result["truncated"] = True
    return result


def _read_slice(path: str, offset: int, limit: int) -> tuple[bytes, int, bool]:
    with _open_file(path) as stream:
        total_size = os.fstat(stream.fileno()).st_size
        stream.seek(offset)
        raw = stream.read(limit + 1)
    return raw[:limit], total_size, len(raw) > limit


def read(
    path: str,
    offset: int = 0,
    limit: int = MAX_READ_BYTES,
    start: int | None = None,
    end: int | None = None,
) -> dict:
    path = _abs(path)
    _integer(offset, "offset")
    _integer(limit, "limit", 1)
    if start is not None:
        _integer(start, "start", 1)
    if end is not None:
        _integer(end, "end", 1)
        if start is None or end < start:
            raise ValueError("end requires start and must be >= start")
    if start is not None and (offset != 0 or limit != MAX_READ_BYTES):
        raise ValueError("line ranges cannot be combined with byte offset/limit")
    policy.require("fs.read", path=path)
    if start is not None:
        return _read_lines(path, start, end)
    raw, total_size, truncated = _read_slice(path, offset, min(limit, MAX_READ_BYTES))
    result = {"path": path, "content": raw.decode("utf-8", errors="replace")}
    if offset:
        result["offset"] = offset
    if truncated:
        result.update(truncated=True, total_size=total_size)
    return result


def read_bytes(path: str, offset: int = 0, limit: int = MAX_READ_BYTES_BINARY) -> dict:
    path = _abs(path)
    _integer(offset, "offset")
    _integer(limit, "limit", 1)
    policy.require("fs.read", path=path)
    raw, total_size, truncated = _read_slice(path, offset, min(limit, MAX_READ_BYTES_BINARY))
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


def _snapshot(path: str, operation: str, session_id: str | None) -> None:
    # A persistent MCP service's process environment is not the caller's session.
    if session_id is not None:
        snapshot.snapshot(path, operation, session_id=session_id)


def _write(path: str, data: bytes, operation: str, session_id: str | None) -> dict:
    policy.require("fs.write", path=path)
    _snapshot(path, operation, session_id)
    atomic_write_bytes(path, data)
    return {"path": path, "bytes": len(data)}


def write(path: str, content: str, *, session_id: str | None = None) -> dict:
    path = _abs(path)
    _text(content, "content", empty=True)
    return _write(path, content.encode("utf-8"), "write", session_id)


def write_bytes(path: str, content: str, *, session_id: str | None = None) -> dict:
    path = _abs(path)
    _text(content, "content", empty=True)
    data = base64.b64decode(content, validate=True)
    return _write(path, data, "write_bytes", session_id)


def rm(path: str, *, session_id: str | None = None) -> dict:
    path = _abs(path)
    policy.require("fs.delete", path=path)
    st = os.lstat(path)
    _snapshot(path, "rm", session_id)
    if file_stat.S_ISDIR(st.st_mode):
        shutil.rmtree(path)
    else:
        os.remove(path)
    return {"removed": path}


def mkdir(path: str, *, session_id: str | None = None) -> dict:
    path = _abs(path)
    policy.require("fs.write", path=path)
    _snapshot(path, "mkdir", session_id)
    os.makedirs(path, exist_ok=True)
    return {"created": path}


def stat(path: str) -> dict:
    path = _abs(path)
    policy.require("fs.meta", path=path)
    st = os.lstat(path)
    result = {
        "path": path,
        "size": st.st_size,
        "is_dir": file_stat.S_ISDIR(st.st_mode),
        "is_file": file_stat.S_ISREG(st.st_mode),
        "modified": st.st_mtime,
        "created": st.st_ctime,
        "permissions": oct(st.st_mode),
    }
    if result["is_file"]:
        directory = os.path.dirname(path)
        policy.require("fs.read", path=directory)
        entry = _load_meta(directory).get(os.path.basename(path), {})
        if "tags" in entry:
            result["tags"] = entry["tags"]
    return result


def _walk_error(error: OSError) -> None:
    raise error


def _rg_text(field: dict) -> str:
    if "text" in field:
        return field["text"]
    return base64.b64decode(field["bytes"], validate=True).decode("utf-8", errors="replace")


def search(query: str, path: str = WORKSPACE) -> dict:
    _text(query, "query")
    if "\0" in query:
        raise ValueError("query must not contain NUL")
    path = _abs(path)
    policy.require("fs.read", path=path)
    result = subprocess.run(
        ["rg", "--json", "--color", "never", "--", query, path],
        capture_output=True,
        text=True,
        timeout=SEARCH_TIMEOUT,
        stdin=subprocess.DEVNULL,
        env=scrub_env(),
        check=False,
    )
    if result.returncode not in (0, 1):
        raise RuntimeError(f"ripgrep exited {result.returncode}: {result.stderr.strip()}")
    matches = []
    for line in result.stdout.splitlines():
        record = json.loads(line)
        if record["type"] == "match":
            data = record["data"]
            matches.append({
                "path": _rg_text(data["path"]),
                "line": data["line_number"],
                "text": _rg_text(data["lines"]).rstrip("\n"),
            })
    matched_paths = {match["path"] for match in matches}
    if os.path.isfile(path):
        candidates = [(os.path.dirname(path), [], [os.path.basename(path)])]
    else:
        candidates = os.walk(path, onerror=_walk_error, followlinks=False)
    for directory, _, filenames in candidates:
        for name in filenames:
            full = os.path.join(directory, name)
            if query.lower() in name.lower() and full not in matched_paths:
                matches.append({"path": full, "line": 0, "text": f"[filename match: {name}]"})
    return {"query": query, "matches": matches}


def tag(path: str, tags: list[str], *, session_id: str | None = None) -> dict:
    path = _abs(path)
    if not isinstance(tags, list) or not tags:
        raise ValueError("tags must be a nonempty list of strings")
    for value in tags:
        _text(value, "tag")
    directory = os.path.dirname(path)
    policy.require("fs.meta", path=path)
    policy.require("fs.read", path=directory)
    policy.require("fs.write", path=directory)
    if not file_stat.S_ISREG(os.lstat(path).st_mode):
        raise ValueError(f"not a regular file: {path}")
    meta = _load_meta(directory)
    entry = meta.setdefault(os.path.basename(path), {})
    existing = entry.setdefault("tags", [])
    for value in tags:
        if value not in existing:
            existing.append(value)
    meta_path = _meta_path(directory)
    _snapshot(meta_path, "tag", session_id)
    atomic_write_json(meta_path, meta)
    return {"path": path, "tags": existing}


def recent(n: int = 10) -> dict:
    _integer(n, "n")
    root = _abs(WORKSPACE)
    policy.require("fs.read", path=root)
    files = []
    for directory, dirs, names in os.walk(root, onerror=_walk_error, followlinks=False):
        dirs[:] = [name for name in dirs if not name.startswith(".")]
        for name in names:
            if name.startswith("."):
                continue
            path = os.path.join(directory, name)
            st = os.lstat(path)
            if file_stat.S_ISREG(st.st_mode):
                files.append({"path": path, "modified": st.st_mtime})
    files.sort(key=lambda entry: entry["modified"], reverse=True)
    return {"files": files[:n]}


def rename(src: str, dst: str, *, session_id: str | None = None) -> dict:
    src, dst = _abs(src), _abs(dst)
    if src != dst and os.path.commonpath([src, dst]) == src:
        raise ValueError("rename destination must not be inside the source")
    policy.require("fs.delete", path=src)
    policy.require("fs.write", path=dst)
    os.lstat(src)
    if os.path.lexists(dst):
        raise FileExistsError(f"destination already exists: {dst}")
    if session_id is not None:
        snapshot.snapshot_pair(src, dst, "rename", session_id=session_id)
    os.makedirs(os.path.dirname(dst), exist_ok=True)
    os.rename(src, dst)
    return {"from": src, "to": dst}


def move(src: str, dst: str, *, session_id: str | None = None) -> dict:
    return rename(src, dst, session_id=session_id)


def _check_tree(path: str) -> None:
    for directory, dirs, files in os.walk(path, onerror=_walk_error, followlinks=False):
        for name in dirs + files:
            entry = os.path.join(directory, name)
            st = os.lstat(entry)
            if not (file_stat.S_ISREG(st.st_mode) or file_stat.S_ISDIR(st.st_mode)):
                raise ValueError(f"copy refuses symlinks and special files: {entry}")


def _copy_file(src: str, dst: str) -> str:
    with _open_file(src) as source:
        st = os.fstat(source.fileno())
        fd = _open_nofollow(dst, os.O_WRONLY | os.O_CREAT | os.O_EXCL)
        with os.fdopen(fd, "wb") as destination:
            shutil.copyfileobj(source, destination)
            destination.flush()
            os.fchmod(destination.fileno(), file_stat.S_IMODE(st.st_mode))
            os.utime(destination.fileno(), ns=(st.st_atime_ns, st.st_mtime_ns))
    return dst


def copy(src: str, dst: str, *, session_id: str | None = None) -> dict:
    src, dst = _abs(src), _abs(dst)
    if os.path.commonpath([src, dst]) == src:
        raise ValueError("copy destination must not be inside the source")
    policy.require("fs.read", path=src)
    policy.require("fs.write", path=dst)
    st = os.lstat(src)
    if os.path.lexists(dst):
        raise FileExistsError(f"destination already exists: {dst}")
    is_dir = file_stat.S_ISDIR(st.st_mode)
    if is_dir:
        _check_tree(src)
    elif not file_stat.S_ISREG(st.st_mode):
        raise ValueError(f"not a regular file: {src}")
    _snapshot(dst, "copy", session_id)
    os.makedirs(os.path.dirname(dst), exist_ok=True)
    if is_dir:
        # Never dereference a link inserted after the preflight walk.
        shutil.copytree(src, dst, symlinks=True, copy_function=_copy_file)
        try:
            _check_tree(dst)
        except ValueError:
            shutil.rmtree(dst)
            raise
    else:
        _copy_file(src, dst)
    return {"from": src, "to": dst, "kind": "dir" if is_dir else "file"}
