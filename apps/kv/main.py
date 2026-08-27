"""kv — Key-value store for agent memory and state.

Uses a single JSON file as backend with file locking for safety.
"""

import fcntl
import fnmatch
import json
import os
import sys

# Pull in the shared atomic-write helper. Each app runs as its own
# Python process so we splice the parent of this app onto sys.path
# before the import.
sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from _shared.atomic import atomic_write_bytes  # noqa: E402

from cos_runtime import policy  # noqa: E402

DATA_DIR = os.environ.get("COS_DATA_DIR", "/var/lib/cos")
STORE_PATH = os.path.join(DATA_DIR, "kv.json")
LOCK_PATH = STORE_PATH + ".lock"


def _lock_path():
    os.makedirs(DATA_DIR, exist_ok=True)
    return LOCK_PATH


def _load():
    """Load the store from disk, returning an empty dict if missing.

    Holds the shared lock on a *separate lock file* — never on the
    JSON file itself, because the writer atomically replaces the
    JSON file via ``os.replace`` (so any flock held on it would be
    against a now-unlinked inode).
    """
    if not os.path.isfile(STORE_PATH):
        return {}
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
    return data if isinstance(data, dict) else {}


def _save(data):
    """Write the store to disk under an exclusive lock.

    SECURITY/CONSISTENCY: the previous implementation opened the
    store file with mode ``"w"`` which **truncates the file before**
    the ``flock`` call could acquire the lock — between truncate and
    write any concurrent reader would see an empty store. We now:

    1. Take an exclusive lock on a dedicated ``.lock`` sibling file.
    2. Write the new contents via ``atomic_write_bytes`` (tmp +
       fsync + ``os.replace`` + fsync(parent)).

    This way readers either see the old store (pre-replace) or the
    full new store (post-replace) — never an empty file.
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


def run(command, args):
    from canonical_argv import normalize_canonical_argv
    args = normalize_canonical_argv(args)
    try:
        if command == "set":
            if len(args) < 2:
                return {"error": "usage: kv set <key> <value>"}
            key = args[0]
            value = " ".join(args[1:])
            policy.require("data.kv.write", name=key)
            data = _load()
            data[key] = value
            _save(data)
            return {"key": key, "value": value}

        elif command == "get":
            if len(args) < 1:
                return {"error": "usage: kv get <key>"}
            key = args[0]
            policy.require("data.kv.read", name=key)
            data = _load()
            if key not in data:
                return {"error": f"key not found: {key}"}
            return {"key": key, "value": data[key]}

        elif command == "del":
            if len(args) < 1:
                return {"error": "usage: kv del <key>"}
            key = args[0]
            policy.require("data.kv.delete", name=key)
            data = _load()
            if key not in data:
                return {"error": f"key not found: {key}"}
            del data[key]
            _save(data)
            return {"deleted": key}

        elif command == "list":
            policy.require("data.kv.read", wild=True)
            pattern = args[0] if args else "*"
            data = _load()
            keys = sorted(k for k in data if fnmatch.fnmatch(k, pattern))
            return {"pattern": pattern, "keys": keys}

        elif command == "dump":
            policy.require("data.kv.read", wild=True)
            data = _load()
            return {"count": len(data), "data": data}

        else:
            return {"error": f"unknown command: {command}"}
    except policy.PermissionDenied as denied:
        return {"error": str(denied), "denial": denied.denial}
    except policy.PolicyUnavailable as exc:
        return {"error": f"capability check failed: {exc}"}
