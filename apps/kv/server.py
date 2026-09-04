"""kv — persistent MCP service for the agent.

The service stays live across calls so the agent can chain
`kv.set` → `kv.get` → `kv.list` without re-loading the store JSON
each round-trip.

The kernel runs the cap check before forwarding any `tools/call` to
us (using `app.json`'s `mcp.tools[].needs[]`), so handlers here
don't repeat `policy.require` — they trust the bridge to have gated
the call already.
"""

from __future__ import annotations

import fcntl
import fnmatch
import json
import os
import sys

# Use the shared atomic-write helper to commit store updates under
# the same lock-then-replace discipline used by other bundled apps.
sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from _shared.atomic import atomic_write_bytes  # noqa: E402

from claw_os_sdk.mcp import App  # noqa: E402

DATA_DIR = os.environ.get("COS_DATA_DIR", "/var/lib/cos")
STORE_PATH = os.path.join(DATA_DIR, "kv.json")
LOCK_PATH = STORE_PATH + ".lock"

app = App.from_manifest()


_cache: dict[str, str] | None = None


def _lock_path() -> str:
    os.makedirs(DATA_DIR, exist_ok=True)
    return LOCK_PATH


def _load() -> dict[str, str]:
    """Return the in-memory cache, loading from disk on first access.

    Caching lets callers chain operations through one lazy service
    process without reloading the JSON store for each call.
    """
    global _cache
    if _cache is not None:
        return _cache
    try:
        os.stat(STORE_PATH)
    except FileNotFoundError:
        _cache = {}
        return _cache
    lock = _lock_path()
    with open(lock, "a+") as lock_fd:
        fcntl.flock(lock_fd, fcntl.LOCK_SH)
        try:
            try:
                with open(STORE_PATH, "r", encoding="utf-8") as f:
                    loaded: object = json.load(f)
            except FileNotFoundError:
                loaded = {}
        finally:
            fcntl.flock(lock_fd, fcntl.LOCK_UN)
    if not isinstance(loaded, dict):
        raise ValueError("kv store must contain a JSON object")
    data: dict[str, str] = {}
    for key, value in loaded.items():
        if not isinstance(key, str) or not isinstance(value, str):
            raise ValueError("kv store must contain only string keys and values")
        data[key] = value
    _cache = data
    return _cache


def _save(data: dict[str, str]) -> None:
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


@app.tool("kv.get")
def kv_get(key: str) -> str:
    return _load().get(key, "")


@app.tool("kv.set")
def kv_set(key: str, value: str) -> dict:
    data = _load()
    data[key] = value
    _save(data)
    return {"key": key, "value": value}


@app.tool("kv.del")
def kv_del(key: str) -> dict:
    data = _load()
    existed = key in data
    if existed:
        del data[key]
        _save(data)
    return {"key": key, "deleted": existed}


@app.tool("kv.list")
def kv_list(pattern: str = "*") -> dict:
    data = _load()
    keys = sorted(k for k in data if fnmatch.fnmatch(k, pattern))
    return {"pattern": pattern, "keys": keys}


@app.tool("kv.dump")
def kv_dump() -> dict:
    data = _load()
    return {"count": len(data), "data": data}


if __name__ == "__main__":
    app.serve()
