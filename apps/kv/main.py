"""kv — Key-value store for agent memory and state.

Uses a single JSON file as backend with file locking for safety.
"""

import fcntl
import fnmatch
import json
import os

DATA_DIR = os.environ.get("AOS_DATA_DIR", "/var/lib/aos")
STORE_PATH = os.path.join(DATA_DIR, "kv.json")


def _load():
    """Load the store from disk, returning an empty dict if missing."""
    if not os.path.isfile(STORE_PATH):
        return {}
    with open(STORE_PATH, "r") as f:
        fcntl.flock(f, fcntl.LOCK_SH)
        try:
            data = json.load(f)
        except (json.JSONDecodeError, ValueError):
            data = {}
        finally:
            fcntl.flock(f, fcntl.LOCK_UN)
    return data


def _save(data):
    """Write the store to disk with an exclusive lock."""
    os.makedirs(DATA_DIR, exist_ok=True)
    with open(STORE_PATH, "w") as f:
        fcntl.flock(f, fcntl.LOCK_EX)
        try:
            json.dump(data, f)
        finally:
            fcntl.flock(f, fcntl.LOCK_UN)


def run(command, args):
    if command == "set":
        if len(args) < 2:
            return {"error": "usage: kv set <key> <value>"}
        key = args[0]
        value = " ".join(args[1:])
        data = _load()
        data[key] = value
        _save(data)
        return {"key": key, "value": value}

    elif command == "get":
        if len(args) < 1:
            return {"error": "usage: kv get <key>"}
        key = args[0]
        data = _load()
        if key not in data:
            return {"error": f"key not found: {key}"}
        return {"key": key, "value": data[key]}

    elif command == "del":
        if len(args) < 1:
            return {"error": "usage: kv del <key>"}
        key = args[0]
        data = _load()
        if key not in data:
            return {"error": f"key not found: {key}"}
        del data[key]
        _save(data)
        return {"deleted": key}

    elif command == "list":
        pattern = args[0] if args else "*"
        data = _load()
        keys = sorted(k for k in data if fnmatch.fnmatch(k, pattern))
        return {"pattern": pattern, "keys": keys}

    elif command == "dump":
        data = _load()
        return {"count": len(data), "data": data}

    else:
        return {"error": f"unknown command: {command}"}
