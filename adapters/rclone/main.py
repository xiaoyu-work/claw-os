"""rclone adapter — exposes ``cloud.listremotes``, ``cloud.ls``,
``cloud.size`` and ``cloud.copy`` so the system Agent can inspect and
move data between the user's configured rclone remotes (Google Drive,
S3, Dropbox, OneDrive, WebDAV, SFTP, ...).

Upstream: https://rclone.org/ (MIT). The adapter shells out to the
``rclone`` binary — it does NOT touch the user's rclone config file
directly. Remote configuration / credentials are entirely the user's
job (via ``rclone config``).
"""

from __future__ import annotations

import json as _json
import os
import pathlib
import re
import shutil
import subprocess
import sys

_HERE = pathlib.Path(__file__).resolve().parent
_CANDIDATES = [
    pathlib.Path(os.environ["CLAW_PYTHON_LIB"]) if os.environ.get("CLAW_PYTHON_LIB") else None,
    _HERE.parent.parent / "apps",
    pathlib.Path("/opt/claw/python"),
    pathlib.Path("/usr/lib/claw/python"),
]
for _cand in _CANDIDATES:
    if _cand and (_cand / "_lib").is_dir():
        sys.path.insert(0, str(_cand))
        break

from _lib.serve import App  # noqa: E402


def _rclone_bin() -> str:
    explicit = os.environ.get("CLAW_RCLONE_BIN")
    if explicit:
        return explicit
    found = shutil.which("rclone")
    if found is None:
        raise FileNotFoundError(
            "rclone not found on PATH; install the `rclone` package or set "
            "CLAW_RCLONE_BIN"
        )
    return found


def _run(cmd: list[str]) -> str:
    proc = subprocess.run(cmd, capture_output=True, text=True, check=False)
    if proc.returncode != 0:
        msg = proc.stderr.strip() or proc.stdout.strip() or "rclone failed"
        raise RuntimeError(f"rclone (exit {proc.returncode}): {msg}")
    return proc.stdout


_REMOTE_NAME_RE = re.compile(r"^[A-Za-z0-9_][A-Za-z0-9_.\-]{0,63}$")


def _validate_remote_name(name: str) -> str:
    if not _REMOTE_NAME_RE.match(name):
        raise ValueError(
            f"invalid rclone remote name '{name}'. Allowed: A-Z a-z 0-9 _ . - "
            "(must start with alnum or underscore, max 64 chars)"
        )
    return name


def _validate_path_fragment(value: str, label: str) -> str:
    if not value:
        raise ValueError(f"{label} must not be empty")
    if "\0" in value:
        raise ValueError(f"{label} contains NUL byte")
    if value.startswith("-"):
        raise ValueError(
            f"{label} '{value}' starts with '-'; refusing to pass as rclone arg"
        )
    return value


def _split_target(target: str, label: str) -> tuple[str, str]:
    """Split 'remote:path/sub' into ('remote', 'path/sub'). The remote half
    is validated; the path half is left alone (rclone treats it opaquely)
    except for a NUL / leading-dash check."""
    _validate_path_fragment(target, label)
    if ":" not in target:
        raise ValueError(
            f"{label} '{target}' is not in 'remote:path' form. "
            "Use cloud.listremotes() to discover configured remotes."
        )
    remote, _, path = target.partition(":")
    _validate_remote_name(remote)
    if path and "\0" in path:
        raise ValueError(f"{label} path contains NUL byte")
    return remote, path


app = App()


@app.tool(
    "cloud.listremotes",
    summary=(
        "List the rclone remotes configured on this machine. Each returned "
        "string is suitable as the `remote:` prefix in cloud.ls / cloud.size "
        "/ cloud.copy."
    ),
    args={},
)
def cloud_listremotes() -> list:
    out = _run([_rclone_bin(), "listremotes"])
    remotes = []
    for line in out.splitlines():
        line = line.strip()
        if not line:
            continue
        if line.endswith(":"):
            line = line[:-1]
        remotes.append(line)
    return remotes


@app.tool(
    "cloud.ls",
    summary=(
        "List entries at a remote path. Returns the JSON array reported by "
        "`rclone lsjson` (Path, Name, Size, ModTime, IsDir, MimeType, ...)."
    ),
    args={
        "target": {
            "type": "string",
            "description": (
                "Remote path in 'remote:path/sub' form. The path half may "
                "be empty (e.g. 'mydrive:') to list the remote root."
            ),
        },
    },
    required=["target"],
)
def cloud_ls(target: str) -> list:
    _split_target(target, "target")
    out = _run([_rclone_bin(), "lsjson", target])
    try:
        data = _json.loads(out)
    except _json.JSONDecodeError as e:
        raise RuntimeError(f"rclone lsjson returned invalid JSON: {e}")
    if not isinstance(data, list):
        raise RuntimeError("rclone lsjson returned non-array")
    return data


@app.tool(
    "cloud.size",
    summary=(
        "Report total object count and byte size at a remote path. Calls "
        "`rclone size --json` so the answer is exact (full recursive walk) — "
        "may be slow on large trees."
    ),
    args={
        "target": {
            "type": "string",
            "description": "Remote path in 'remote:path' form.",
        },
    },
    required=["target"],
)
def cloud_size(target: str) -> dict:
    _split_target(target, "target")
    out = _run([_rclone_bin(), "size", "--json", target])
    try:
        data = _json.loads(out)
    except _json.JSONDecodeError as e:
        raise RuntimeError(f"rclone size returned invalid JSON: {e}")
    if not isinstance(data, dict):
        raise RuntimeError("rclone size returned non-object")
    return data


@app.tool(
    "cloud.copy",
    summary=(
        "Copy files from source to destination using `rclone copy` "
        "(non-destructive — never deletes from source; existing destination "
        "files of the same size+mtime are skipped). Either side may be a "
        "local path or a 'remote:path'. Pass dry_run=true to preview without "
        "transferring."
    ),
    args={
        "source": {
            "type": "string",
            "description": "Source: 'remote:path' or local absolute path.",
        },
        "destination": {
            "type": "string",
            "description": "Destination: 'remote:path' or local absolute path.",
        },
        "dry_run": {
            "type": "boolean",
            "description": "If true, pass --dry-run (preview only).",
            "default": False,
        },
    },
    required=["source", "destination"],
)
def cloud_copy(source: str, destination: str, dry_run: bool = False) -> dict:
    _validate_path_fragment(source, "source")
    _validate_path_fragment(destination, "destination")
    if ":" in source:
        _split_target(source, "source")
    if ":" in destination:
        _split_target(destination, "destination")
    cmd = [_rclone_bin(), "copy"]
    if dry_run:
        cmd.append("--dry-run")
    cmd.extend([source, destination])
    proc = subprocess.run(cmd, capture_output=True, text=True, check=False)
    if proc.returncode != 0:
        msg = proc.stderr.strip() or proc.stdout.strip() or "rclone copy failed"
        raise RuntimeError(f"rclone copy (exit {proc.returncode}): {msg}")
    return {
        "source": source,
        "destination": destination,
        "dry_run": bool(dry_run),
        "stderr_tail": "\n".join(proc.stderr.splitlines()[-20:]),
    }


if __name__ == "__main__":
    app.serve()
