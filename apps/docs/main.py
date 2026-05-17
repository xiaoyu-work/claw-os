"""docs — local document full-text search via Recoll / Xapian.

This is the **AI-agent** surface for "find that document I wrote about
X". It wraps three Recoll CLI tools:

* ``recollq``      — query the Xapian index
* ``recollindex``  — build / update the index
* ``recoll``       — (GUI; not used here)

Recoll handles PDF, all LibreOffice / MS Office formats, .eml mail,
HTML, plain text, markdown, source code, EPUB and a long tail of other
formats via Xapian + format-specific filters (poppler, antiword,
unrtf, libreoffice headless, ...). All of these run **locally** — no
network, no third-party services.

Operations
==========

* ``search --query "..."``  full-text search, returns ranked results.
* ``index``                 incremental rebuild (safe to call often).
* ``status``                whether an index exists + summary stats.
* ``configure``             one-time bootstrap of ~/.recoll/recoll.conf.

Configuration
=============

Recoll keeps its config + Xapian database under ``~/.recoll/``:

* ``recoll.conf`` — at minimum a ``topdirs`` setting listing the
  directories to crawl. The ``configure`` operation writes a sensible
  default (Documents / Desktop / Downloads) on first run.
* ``xapiandb/``   — the Xapian index itself, written by ``recollindex``.

Binary discovery
================

By default this app shells out to ``recollq`` and ``recollindex``
discovered on ``$PATH``. For tests / packaging overrides, set:

* ``CLAW_RECOLLQ_BIN``      — absolute path to a recollq stand-in.
* ``CLAW_RECOLLINDEX_BIN``  — absolute path to a recollindex stand-in.
"""

from __future__ import annotations

import argparse
import os
import pathlib
import shlex
import shutil
import subprocess
import sys
import time

# Pull in scrub_env so recollq / recollindex children don't inherit
# provider API keys.
sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from _shared.env_scrub import scrub_env  # noqa: E402

from cos_runtime import policy  # noqa: E402


HOME = pathlib.Path(os.path.expanduser("~"))
RECOLL_DIR = HOME / ".recoll"
RECOLL_CONF = RECOLL_DIR / "recoll.conf"
RECOLL_DB = RECOLL_DIR / "xapiandb"

DEFAULT_TOPDIRS = ["~/Documents", "~/Desktop", "~/Downloads"]
DEFAULT_MAX_RESULTS = 20
MAX_MAX_RESULTS = 200
INDEX_TIMEOUT_SECS = 1800  # 30 minutes; recollindex on first run can be slow
QUERY_TIMEOUT_SECS = 60  # recollq should be instant; cap it so a wedged child can't hang the agent


# ---------------------------------------------------------------------------
# Binary discovery
# ---------------------------------------------------------------------------


def _recoll_bin(name: str, env_var: str) -> str:
    """Resolve a recoll-family binary, honouring tests / packaging overrides."""
    explicit = os.environ.get(env_var)
    if explicit:
        return explicit
    found = shutil.which(name)
    if found is None:
        raise RuntimeError(
            f"{name} not found on PATH. Install the `recoll` package (it ships "
            f"recollq + recollindex) or set {env_var}."
        )
    return found


# ---------------------------------------------------------------------------
# Result parsing
# ---------------------------------------------------------------------------


def _strip_quotes(token: str) -> str:
    """``recollq -F`` emits double-quoted, backslash-escaped fields."""
    if len(token) >= 2 and token[0] == '"' and token[-1] == '"':
        token = token[1:-1]
    return token.replace('\\"', '"').replace("\\\\", "\\")


def _parse_recollq_line(line: str):
    """Parse one ``recollq -F 'url mtype mtime abstract'`` line.

    The format string puts each field as a double-quoted token separated by
    spaces; ``shlex`` happily decodes that.
    """
    try:
        tokens = shlex.split(line)
    except ValueError:
        return None
    if len(tokens) < 4:
        return None
    url, mtype, mtime, *rest = tokens
    abstract = " ".join(rest)
    path = url[7:] if url.startswith("file://") else url
    return {
        "path": path,
        "mime": mtype,
        "mtime": mtime,
        "snippet": abstract,
    }


# ---------------------------------------------------------------------------
# Operations
# ---------------------------------------------------------------------------


def cmd_search(args):
    parser = argparse.ArgumentParser(prog="docs search", add_help=False)
    parser.add_argument("--query", required=True)
    parser.add_argument("--max-results", type=int, default=DEFAULT_MAX_RESULTS)
    opts = parser.parse_args(args)

    if not opts.query.strip():
        return {"error": "query is empty"}
    max_results = max(1, min(MAX_MAX_RESULTS, opts.max_results))

    policy.require("proc.spawn", name="recollq")
    policy.require("fs.read", path=str(RECOLL_DIR))

    if not RECOLL_DB.exists():
        return {
            "results": [],
            "count": 0,
            "hint": (
                "No Recoll index found at ~/.recoll/xapiandb yet. The "
                "claw-recoll-index.service (systemd --user) builds it "
                "automatically ~60 s after the first graphical login and "
                "then keeps it live via inotify. If you just logged in, "
                "retry in a minute. To kick it off manually run "
                "`cos app docs index`."
            ),
        }

    bin_ = _recoll_bin("recollq", "CLAW_RECOLLQ_BIN")
    try:
        proc = subprocess.run(
            [
                bin_,
                "-t",
                "-n",
                f"{max_results}:0",
                "-F",
                "url mtype mtime abstract",
                opts.query,
            ],
            capture_output=True,
            text=True,
            check=False,
            timeout=QUERY_TIMEOUT_SECS,
            stdin=subprocess.DEVNULL,
            env=scrub_env(),
        )
    except subprocess.TimeoutExpired:
        return {"error": f"recollq exceeded {QUERY_TIMEOUT_SECS}s"}
    if proc.returncode != 0:
        err = (proc.stderr or proc.stdout or "").strip() or "recollq failed"
        return {"error": f"recollq (exit {proc.returncode}): {err}"}

    results = []
    for line in proc.stdout.splitlines():
        if not line.strip():
            continue
        parsed = _parse_recollq_line(line)
        if parsed is not None:
            results.append(parsed)

    return {
        "query": opts.query,
        "count": len(results),
        "results": results,
    }


def cmd_index(args):
    if args:
        return {"error": f"index takes no arguments, got: {args!r}"}

    policy.require("proc.spawn", name="recollindex")
    policy.require("fs.read", wild=True)
    policy.require("fs.write", path=str(RECOLL_DIR))

    bin_ = _recoll_bin("recollindex", "CLAW_RECOLLINDEX_BIN")

    if not RECOLL_CONF.exists():
        return {
            "error": (
                "No ~/.recoll/recoll.conf — run `cos app docs configure` "
                "first to bootstrap a default config."
            )
        }

    started = time.time()
    try:
        proc = subprocess.run(
            [bin_],
            capture_output=True,
            text=True,
            check=False,
            timeout=INDEX_TIMEOUT_SECS,
            stdin=subprocess.DEVNULL,
            env=scrub_env(),
        )
    except subprocess.TimeoutExpired:
        return {
            "ok": False,
            "error": f"recollindex exceeded {INDEX_TIMEOUT_SECS}s",
            "elapsed_secs": round(time.time() - started, 2),
        }
    elapsed = time.time() - started
    ok = proc.returncode == 0
    return {
        "ok": ok,
        "exit": proc.returncode,
        "elapsed_secs": round(elapsed, 2),
        "stderr_tail": "\n".join((proc.stderr or "").splitlines()[-20:]),
    }


def cmd_status(args):
    if args:
        return {"error": f"status takes no arguments, got: {args!r}"}

    policy.require("fs.read", path=str(RECOLL_DIR))

    info = {
        "config_path": str(RECOLL_CONF),
        "config_exists": RECOLL_CONF.exists(),
        "index_path": str(RECOLL_DB),
        "index_exists": RECOLL_DB.exists(),
        "topdirs": [],
        "doc_count": None,
        "last_indexed": None,
    }

    if RECOLL_CONF.exists():
        try:
            for raw in RECOLL_CONF.read_text(errors="replace").splitlines():
                line = raw.strip()
                if line.startswith("#") or "=" not in line:
                    continue
                key, _, val = line.partition("=")
                if key.strip() == "topdirs":
                    info["topdirs"] = shlex.split(val.strip())
                    break
        except OSError:
            pass

    if RECOLL_DB.exists():
        try:
            info["last_indexed"] = int(RECOLL_DB.stat().st_mtime)
        except OSError:
            pass
        index_file = RECOLL_DB / "iamflint"
        try:
            entries = list(RECOLL_DB.iterdir())
            info["index_files"] = sorted(p.name for p in entries)
        except OSError:
            info["index_files"] = []
        _ = index_file  # marker for older Xapian backends; nothing else to do.

    return info


def cmd_configure(args):
    if args:
        return {"error": f"configure takes no arguments, got: {args!r}"}

    policy.require("fs.write", path=str(RECOLL_DIR))

    RECOLL_DIR.mkdir(parents=True, exist_ok=True)
    if RECOLL_CONF.exists():
        return {
            "created": False,
            "config_path": str(RECOLL_CONF),
            "message": (
                "~/.recoll/recoll.conf already exists; left untouched. "
                "Edit it by hand if you want to change topdirs."
            ),
        }

    lines = [
        "# Generated by ClawOS `cos app docs configure`.",
        "# Edit freely; Recoll re-reads this on every run.",
        "#",
        "# topdirs lists the directories Recoll will walk on `recollindex`.",
        "# Tilde expands to $HOME. Add or remove entries to taste.",
        "",
        "topdirs = " + " ".join(DEFAULT_TOPDIRS),
        "",
    ]
    RECOLL_CONF.write_text("\n".join(lines))
    return {
        "created": True,
        "config_path": str(RECOLL_CONF),
        "topdirs": DEFAULT_TOPDIRS,
    }


# ---------------------------------------------------------------------------
# Schema + entry point
# ---------------------------------------------------------------------------


COMMANDS = {
    "search": cmd_search,
    "index": cmd_index,
    "status": cmd_status,
    "configure": cmd_configure,
}


def _schema():
    return {
        "search": {
            "description": "Full-text search of the local Recoll index",
            "parameters": [
                {"name": "--query", "type": "string", "required": True, "description": "Query string (Recoll plaintext mode).", "kind": "flag"},
                {"name": "--max-results", "type": "integer", "required": False, "description": f"Max hits to return (1..{MAX_MAX_RESULTS}, default {DEFAULT_MAX_RESULTS}).", "kind": "flag"},
            ],
            "example": "cos app docs search --query 'budget Q3' --max-results 10",
        },
        "index": {
            "description": "Incremental Recoll index (re)build",
            "parameters": [],
            "example": "cos app docs index",
        },
        "status": {
            "description": "Show index + config status",
            "parameters": [],
            "example": "cos app docs status",
        },
        "configure": {
            "description": "Bootstrap ~/.recoll/recoll.conf with default topdirs",
            "parameters": [],
            "example": "cos app docs configure",
        },
    }


def run(command, args):
    """Entry point called by cos."""
    if command == "__schema__":
        return _schema()

    handler = COMMANDS.get(command)
    if handler is None:
        return {"error": f"unknown command: {command}"}
    try:
        return handler(args)
    except policy.PermissionDenied as denied:
        return {"error": str(denied), "denial": denied.denial}
    except policy.PolicyUnavailable as exc:
        return {"error": f"capability check failed: {exc}"}
    except subprocess.TimeoutExpired as exc:
        return {"error": f"recoll subprocess timed out after {exc.timeout}s"}


if __name__ == "__main__":
    # Allow invoking directly for ad-hoc testing.
    if len(sys.argv) < 2:
        print("usage: main.py <command> [args...]", file=sys.stderr)
        sys.exit(2)
    import json as _json
    out = run(sys.argv[1], sys.argv[2:])
    print(_json.dumps(out, indent=2, sort_keys=True))
