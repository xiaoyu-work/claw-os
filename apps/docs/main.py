"""Local document full-text search through the owner's Recoll index.

The App service has a private ``$HOME``. ``clawd`` supplies the verified
owner home through ``COS_OWNER_HOME`` so every tool addresses the owner's
``~/.recoll`` state without widening filesystem authority. Recoll runs from
fixed system paths and receives the owner home explicitly.
"""

from __future__ import annotations

import os
import pathlib
import shlex
import subprocess
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from _shared.atomic import atomic_write_text  # noqa: E402
from _shared.env_scrub import scrub_env  # noqa: E402

from cos_runtime import policy  # noqa: E402


def _owner_home() -> pathlib.Path:
    raw = os.environ.get("COS_OWNER_HOME")
    if not raw:
        raise RuntimeError("COS_OWNER_HOME is required")
    supplied = pathlib.Path(raw)
    resolved = pathlib.Path(os.path.realpath(raw))
    if not supplied.is_absolute() or supplied != resolved:
        raise RuntimeError("COS_OWNER_HOME must be an absolute canonical path")
    return resolved


OWNER_HOME = _owner_home()
RECOLL_DIR = OWNER_HOME / ".recoll"
RECOLL_CONF = RECOLL_DIR / "recoll.conf"
RECOLL_DB = RECOLL_DIR / "xapiandb"
RECOLLQ_BIN = "/usr/bin/recollq"
RECOLLINDEX_BIN = "/usr/bin/recollindex"

DEFAULT_TOPDIRS = ["~/Documents", "~/Desktop", "~/Downloads"]
DEFAULT_MAX_RESULTS = 20
MAX_MAX_RESULTS = 200
INDEX_TIMEOUT_SECS = 1800
QUERY_TIMEOUT_SECS = 60


def _path_exists(path: pathlib.Path) -> bool:
    try:
        path.stat()
    except FileNotFoundError:
        return False
    return True


def _recoll_env() -> dict[str, str]:
    environment = dict(scrub_env())
    environment["HOME"] = str(OWNER_HOME)
    return environment


def _process_error(name: str, process: subprocess.CompletedProcess[str]) -> RuntimeError:
    detail = (process.stderr or process.stdout or "").strip()
    if not detail:
        detail = "no diagnostic"
    return RuntimeError(f"{name} exited {process.returncode}: {detail}")


def _parse_recollq_line(line: str) -> dict[str, str]:
    try:
        tokens = shlex.split(line)
    except ValueError as exc:
        raise RuntimeError("recollq returned a malformed result line") from exc
    if len(tokens) < 4:
        raise RuntimeError("recollq returned a result with missing fields")
    url, mime, mtime, *abstract = tokens
    path = url[7:] if url.startswith("file://") else url
    return {
        "path": path,
        "mime": mime,
        "mtime": mtime,
        "snippet": " ".join(abstract),
    }


def search(
    query: str,
    max_results: int = DEFAULT_MAX_RESULTS,
) -> dict[str, object]:
    if not isinstance(query, str) or not query.strip():
        raise ValueError("query must be a non-empty string")
    if type(max_results) is not int or not 1 <= max_results <= MAX_MAX_RESULTS:
        raise ValueError(f"max_results must be between 1 and {MAX_MAX_RESULTS}")

    policy.require("proc.spawn", name="recollq")
    policy.require("fs.read", path=str(RECOLL_DIR))

    if not _path_exists(RECOLL_DB):
        return {
            "results": [],
            "count": 0,
            "hint": (
                "No Recoll index found at ~/.recoll/xapiandb yet. The "
                "claw-recoll-index.service (systemd --user) builds it "
                "automatically about 60 seconds after the first graphical "
                "login and then keeps it live via inotify. To start an "
                "incremental pass now, run `cos app docs index`."
            ),
        }

    try:
        process = subprocess.run(
            [
                RECOLLQ_BIN,
                "-c",
                str(RECOLL_DIR),
                "-t",
                "-n",
                f"{max_results}:0",
                "-F",
                "url mtype mtime abstract",
                query,
            ],
            capture_output=True,
            text=True,
            check=False,
            timeout=QUERY_TIMEOUT_SECS,
            stdin=subprocess.DEVNULL,
            env=_recoll_env(),
        )
    except subprocess.TimeoutExpired as exc:
        raise TimeoutError(f"recollq exceeded {QUERY_TIMEOUT_SECS}s") from exc
    if process.returncode != 0:
        raise _process_error("recollq", process)

    results = [
        _parse_recollq_line(line)
        for line in process.stdout.splitlines()
        if line.strip()
    ]
    return {
        "query": query,
        "count": len(results),
        "results": results,
    }


def index() -> dict[str, object]:
    policy.require("proc.spawn", name="recollindex")
    policy.require("fs.read", wild=True)
    policy.require("fs.write", path=str(RECOLL_DIR))

    if not _path_exists(RECOLL_CONF):
        raise FileNotFoundError(
            "No ~/.recoll/recoll.conf; run `cos app docs configure` first"
        )

    started = time.monotonic()
    try:
        process = subprocess.run(
            [RECOLLINDEX_BIN, "-c", str(RECOLL_DIR)],
            capture_output=True,
            text=True,
            check=False,
            timeout=INDEX_TIMEOUT_SECS,
            stdin=subprocess.DEVNULL,
            env=_recoll_env(),
        )
    except subprocess.TimeoutExpired as exc:
        raise TimeoutError(f"recollindex exceeded {INDEX_TIMEOUT_SECS}s") from exc
    elapsed = time.monotonic() - started
    if process.returncode != 0:
        raise _process_error("recollindex", process)
    return {
        "ok": True,
        "exit": 0,
        "elapsed_secs": round(elapsed, 2),
        "stderr_tail": "\n".join((process.stderr or "").splitlines()[-20:]),
    }


def status() -> dict[str, object]:
    policy.require("fs.read", path=str(RECOLL_DIR))

    config_exists = _path_exists(RECOLL_CONF)
    index_exists = _path_exists(RECOLL_DB)
    info: dict[str, object] = {
        "config_path": str(RECOLL_CONF),
        "config_exists": config_exists,
        "index_path": str(RECOLL_DB),
        "index_exists": index_exists,
        "topdirs": [],
        "last_indexed": None,
    }

    if config_exists:
        for raw in RECOLL_CONF.read_text(encoding="utf-8").splitlines():
            line = raw.strip()
            if line.startswith("#") or "=" not in line:
                continue
            key, _, value = line.partition("=")
            if key.strip() == "topdirs":
                info["topdirs"] = shlex.split(value.strip())
                break

    if index_exists:
        info["last_indexed"] = int(RECOLL_DB.stat().st_mtime)
        info["index_files"] = sorted(path.name for path in RECOLL_DB.iterdir())

    return info


def configure() -> dict[str, object]:
    policy.require("fs.write", path=str(RECOLL_DIR))

    RECOLL_DIR.mkdir(parents=True, exist_ok=True)
    if _path_exists(RECOLL_CONF):
        return {
            "created": False,
            "config_path": str(RECOLL_CONF),
            "message": (
                "~/.recoll/recoll.conf already exists; left untouched. "
                "Edit it directly to change topdirs."
            ),
        }

    lines = [
        "# Generated by Claw OS `cos app docs configure`.",
        "# Edit freely; Recoll re-reads this on every run.",
        "#",
        "# topdirs lists the directories Recoll walks during indexing.",
        "",
        "topdirs = " + " ".join(DEFAULT_TOPDIRS),
        "",
    ]
    atomic_write_text(str(RECOLL_CONF), "\n".join(lines))
    return {
        "created": True,
        "config_path": str(RECOLL_CONF),
        "topdirs": DEFAULT_TOPDIRS,
    }
