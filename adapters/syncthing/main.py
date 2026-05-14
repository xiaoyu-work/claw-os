"""Syncthing adapter — exposes ``sync.folders``, ``sync.devices`` and
``sync.rescan`` so the system Agent can inspect / nudge a running
Syncthing daemon without learning its REST API directly.

Upstream: https://syncthing.net/ (MPL-2.0). The adapter calls the
bundled ``syncthing cli`` subcommand which auto-discovers the running
daemon's API key from its config (no manual key management needed).
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


def _syncthing_bin() -> str:
    explicit = os.environ.get("CLAW_SYNCTHING_BIN")
    if explicit:
        return explicit
    found = shutil.which("syncthing")
    if found is None:
        raise FileNotFoundError(
            "syncthing not found on PATH; install the `syncthing` package"
        )
    return found


def _run(cmd: list[str]) -> str:
    proc = subprocess.run(cmd, capture_output=True, text=True, check=False)
    if proc.returncode != 0:
        msg = proc.stderr.strip() or proc.stdout.strip() or "syncthing cli failed"
        raise RuntimeError(f"syncthing (exit {proc.returncode}): {msg}")
    return proc.stdout


_FOLDER_ID_RE = re.compile(r"^[A-Za-z0-9_.-]{1,64}$")


def _validate_folder_id(folder_id: str) -> str:
    if not _FOLDER_ID_RE.match(folder_id):
        raise ValueError(
            f"invalid folder id '{folder_id}'. Allowed chars: A-Z a-z 0-9 _ . -"
        )
    return folder_id


def _parse_json(out: str, kind: str) -> object:
    try:
        return _json.loads(out)
    except _json.JSONDecodeError as e:
        raise RuntimeError(f"syncthing cli returned invalid JSON for {kind}: {e}")


app = App()


@app.tool(
    "sync.folders",
    summary=(
        "List the folders Syncthing is configured to share. Returns the JSON "
        "array reported by `syncthing cli config folders list`."
    ),
    args={},
)
def sync_folders() -> list:
    out = _run([_syncthing_bin(), "cli", "config", "folders", "list"])
    data = _parse_json(out, "folders")
    if not isinstance(data, list):
        raise RuntimeError("syncthing cli returned non-array for folders list")
    return data


@app.tool(
    "sync.devices",
    summary="List the peer devices Syncthing knows about (paired or pending).",
    args={},
)
def sync_devices() -> list:
    out = _run([_syncthing_bin(), "cli", "config", "devices", "list"])
    data = _parse_json(out, "devices")
    if not isinstance(data, list):
        raise RuntimeError("syncthing cli returned non-array for devices list")
    return data


@app.tool(
    "sync.rescan",
    summary="Force Syncthing to rescan a folder right now.",
    args={
        "folder_id": {
            "type": "string",
            "description": (
                "Folder identifier from sync.folders (the `.id` field, "
                "e.g. 'default')."
            ),
        },
    },
    required=["folder_id"],
)
def sync_rescan(folder_id: str) -> dict:
    fid = _validate_folder_id(folder_id)
    _run([_syncthing_bin(), "cli", "operations", "rescan", "--folder", fid])
    return {"rescanned": fid}


if __name__ == "__main__":
    app.serve()
