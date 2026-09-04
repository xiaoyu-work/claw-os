"""log — System audit log: every cos command is recorded automatically.

The cos CLI writes an audit entry for every command execution.
This app lets you read, tail, and search that log.
You can also write manual entries.
"""

from __future__ import annotations

import json
import os
import re
from datetime import datetime, timezone

from cos_runtime import policy

DATA_DIR = os.environ.get("COS_DATA_DIR", "/var/lib/cos")
LOG_DIR = os.path.join(DATA_DIR, "logs")
LOG_FILE = os.path.join(LOG_DIR, "audit.jsonl")

APP_ID_RE = re.compile(r"^[a-z][a-z0-9_-]*$")
VALID_LEVELS = ("debug", "info", "warn", "error")
VALID_STATUSES = ("ok", "error")


def _validate_count(value: object, name: str) -> int:
    if isinstance(value, bool) or not isinstance(value, int):
        raise ValueError(f"{name} must be an integer")
    if not 1 <= value <= 1000:
        raise ValueError(f"{name} must be 1..1000")
    return value


def _validate_app(app: object) -> str | None:
    if app is None:
        return None
    if not isinstance(app, str) or APP_ID_RE.fullmatch(app) is None:
        raise ValueError("app must be a non-empty app ID string")
    return app


def _validate_choice(
    value: object,
    name: str,
    choices: tuple[str, ...],
) -> str:
    if not isinstance(value, str) or value not in choices:
        raise ValueError(f"{name} must be one of: {', '.join(choices)}")
    return value


def _validate_text(value: object, name: str) -> str:
    if not isinstance(value, str) or not value.strip():
        raise ValueError(f"{name} must be a non-empty string")
    return value


def _read_entries() -> list[dict[str, object]]:
    """Read and validate all log entries, or return empty for a missing log."""
    entries: list[dict[str, object]] = []
    try:
        log_file = open(LOG_FILE, "r", encoding="utf-8")
    except FileNotFoundError:
        return entries

    with log_file:
        for line_number, line in enumerate(log_file, start=1):
            try:
                entry: object = json.loads(line)
            except json.JSONDecodeError as error:
                raise ValueError(
                    f"audit log line {line_number} contains invalid JSON: "
                    f"{error.msg}"
                ) from error
            if not isinstance(entry, dict):
                raise ValueError(
                    f"audit log line {line_number} must contain a JSON object"
                )
            entries.append(entry)
    return entries


def write(message: str, level: str = "info") -> dict[str, object]:
    """Append one manual log entry."""
    message = _validate_text(message, "message")
    level = _validate_choice(level, "level", VALID_LEVELS)
    policy.require("data.log.write", wild=True)

    entry = {
        "timestamp": datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
        "source": "user",
        "level": level,
        "message": message,
    }

    os.makedirs(LOG_DIR, exist_ok=True)
    with open(LOG_FILE, "a", encoding="utf-8") as log_file:
        log_file.write(json.dumps(entry) + "\n")

    return entry


def read(
    limit: int = 20,
    app: str | None = None,
    status: str | None = None,
) -> dict[str, object]:
    """Read matching log entries newest first."""
    limit = _validate_count(limit, "limit")
    app = _validate_app(app)
    if status is not None:
        status = _validate_choice(status, "status", VALID_STATUSES)
    policy.require("data.log.read", wild=True)
    entries = _read_entries()

    if app is not None:
        entries = [entry for entry in entries if entry.get("app") == app]
    if status is not None:
        entries = [entry for entry in entries if entry.get("status") == status]

    total = len(entries)
    entries = list(reversed(entries))[:limit]

    return {"entries": entries, "total": total}


def tail(n: int = 10) -> dict[str, object]:
    """Return the final N log entries in chronological order."""
    n = _validate_count(n, "n")
    policy.require("data.log.read", wild=True)
    entries = _read_entries()
    return {"entries": entries[-n:]}


def search(
    query: str,
    limit: int = 20,
    app: str | None = None,
) -> dict[str, object]:
    """Search matching log entries in their stored order."""
    query = _validate_text(query, "query")
    limit = _validate_count(limit, "limit")
    app = _validate_app(app)
    policy.require("data.log.read", wild=True)
    entries = _read_entries()

    if app is not None:
        entries = [entry for entry in entries if entry.get("app") == app]

    lowered_query = query.lower()
    matches = [
        entry
        for entry in entries
        if lowered_query in json.dumps(entry).lower()
    ]

    return {"entries": matches[:limit], "total": len(matches)}
