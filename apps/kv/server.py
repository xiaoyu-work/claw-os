"""kv — MCP session server for the agent.

Mirrors the verbs in `main.py` (the CLI/one-shot path) but stays live
across calls so the agent can chain `kv.set` → `kv.get` → `kv.list`
without re-loading the store JSON each round-trip.

The kernel runs the cap check before forwarding any `tools/call` to
us (using `app.json`'s `session.tools[].needs[]`), so handlers here
don't repeat `policy.require` — they trust the bridge to have gated
the call already.
"""

from __future__ import annotations

import fcntl
import fnmatch
import json
import os
import sys
from typing import Dict, Optional

# Use the shared atomic-write helper so both kv entry points (this
# session server + the one-shot main.py) commit to disk under the
# same lock-then-replace discipline.
sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from _shared.atomic import atomic_write_bytes  # noqa: E402

from claw_os_sdk.serve import App  # noqa: E402

DATA_DIR = os.environ.get("COS_DATA_DIR", "/var/lib/cos")
STORE_PATH = os.path.join(DATA_DIR, "kv.json")
LOCK_PATH = STORE_PATH + ".lock"

app = App()


_cache: Optional[Dict[str, str]] = None


def _lock_path() -> str:
    os.makedirs(DATA_DIR, exist_ok=True)
    return LOCK_PATH


def _load() -> Dict[str, str]:
    """Return the in-memory cache, loading from disk on first access.

    Caching is what makes the session worth opening: callers that
    string together several ops on one attach amortise the JSON parse
    cost across the whole session.
    """
    global _cache
    if _cache is not None:
        return _cache
    if not os.path.isfile(STORE_PATH):
        _cache = {}
        return _cache
    lock = _lock_path()
    with open(lock, "a+") as lock_fd:
        fcntl.flock(lock_fd, fcntl.LOCK_SH)
        try:
            try:
                with open(STORE_PATH, "r") as f:
                    data = json.load(f)
            except (OSError, json.JSONDecodeError, ValueError):
                data = {}
        finally:
            fcntl.flock(lock_fd, fcntl.LOCK_UN)
    _cache = data if isinstance(data, dict) else {}
    return _cache


def _save(data: Dict[str, str]) -> None:
    """Persist ``data`` atomically: acquire the exclusive lock, then
    write via ``atomic_write_bytes`` so a concurrent reader never sees
    a truncated store. The old code opened the file in ``"w"`` mode,
    which truncates *before* the flock could be acquired.
    """
    os.makedirs(DATA_DIR, exist_ok=True)
    lock = _lock_path()
    with open(lock, "a+") as lock_fd:
        fcntl.flock(lock_fd, fcntl.LOCK_EX)
        try:
            payload = json.dumps(data, ensure_ascii=False).encode("utf-8")
            atomic_write_bytes(STORE_PATH, payload)
        finally:
            fcntl.flock(lock_fd, fcntl.LOCK_UN)


@app.tool(
    "kv.get",
    summary="Read the value stored under `key`. Returns an empty string if missing.",
    args={"key": {"type": "string", "description": "Key to look up."}},
    required=["key"],
)
def kv_get(key: str) -> str:
    return _load().get(key, "")


@app.tool(
    "kv.set",
    summary="Store `value` under `key`, overwriting any prior value.",
    args={
        "key": {"type": "string", "description": "Key name."},
        "value": {"type": "string", "description": "Value to store."},
    },
    required=["key", "value"],
)
def kv_set(key: str, value: str) -> dict:
    data = _load()
    data[key] = value
    _save(data)
    return {"key": key, "value": value}


@app.tool(
    "kv.del",
    summary="Delete `key`. Idempotent; missing keys still succeed.",
    args={"key": {"type": "string", "description": "Key to delete."}},
    required=["key"],
)
def kv_del(key: str) -> dict:
    data = _load()
    existed = key in data
    if existed:
        del data[key]
        _save(data)
    return {"key": key, "deleted": existed}


@app.tool(
    "kv.list",
    summary="List keys matching the glob `pattern` (default '*'). Returns a sorted name list.",
    args={
        "pattern": {
            "type": "string",
            "description": "Glob pattern, e.g. 'user/*' or '*'.",
            "default": "*",
        }
    },
)
def kv_list(pattern: str = "*") -> dict:
    data = _load()
    keys = sorted(k for k in data if fnmatch.fnmatch(k, pattern))
    return {"pattern": pattern, "keys": keys}


@app.tool(
    "kv.dump",
    summary="Return every key/value pair in the store. Caller is expected to hold a wild cap.",
    args={},
)
def kv_dump() -> dict:
    data = _load()
    return {"count": len(data), "data": data}


if __name__ == "__main__":
    app.serve()
