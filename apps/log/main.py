"""log — System audit log: every aos command is recorded automatically.

The aos CLI writes an audit entry for every command execution.
This app lets you read, tail, and search that log.
You can also write manual entries.
"""

import json
import os
from datetime import datetime, timezone

DATA_DIR = os.environ.get("AOS_DATA_DIR", "/var/lib/aos")
LOG_DIR = os.path.join(DATA_DIR, "logs")
LOG_FILE = os.path.join(LOG_DIR, "audit.jsonl")

VALID_LEVELS = ("debug", "info", "warn", "error")


def _read_entries():
    """Read all log entries from disk. Returns empty list if file missing."""
    if not os.path.isfile(LOG_FILE):
        return []
    entries = []
    with open(LOG_FILE, "r") as f:
        for line in f:
            line = line.strip()
            if not line:
                continue
            try:
                entries.append(json.loads(line))
            except (json.JSONDecodeError, ValueError):
                continue
    return entries


def _cmd_write(args):
    """Write a manual log entry. Usage: write <message> [--level LEVEL]"""
    level = "info"
    message_parts = []

    i = 0
    while i < len(args):
        if args[i] == "--level" and i + 1 < len(args):
            level = args[i + 1].lower()
            if level not in VALID_LEVELS:
                return {"error": f"invalid level: {level} (must be one of {', '.join(VALID_LEVELS)})"}
            i += 2
        else:
            message_parts.append(args[i])
            i += 1

    if not message_parts:
        return {"error": "usage: log write <message> [--level LEVEL]"}

    entry = {
        "timestamp": datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
        "source": "user",
        "level": level,
        "message": " ".join(message_parts),
    }

    os.makedirs(LOG_DIR, exist_ok=True)
    with open(LOG_FILE, "a") as f:
        f.write(json.dumps(entry) + "\n")

    return entry


def _cmd_read(args):
    """Read recent log entries. Usage: read [--limit N] [--app NAME] [--status ok|error]"""
    limit = 20
    app_filter = None
    status_filter = None

    i = 0
    while i < len(args):
        if args[i] == "--limit" and i + 1 < len(args):
            try:
                limit = int(args[i + 1])
            except ValueError:
                return {"error": f"invalid limit: {args[i + 1]}"}
            i += 2
        elif args[i] == "--app" and i + 1 < len(args):
            app_filter = args[i + 1]
            i += 2
        elif args[i] == "--status" and i + 1 < len(args):
            status_filter = args[i + 1]
            i += 2
        else:
            return {"error": f"unknown argument: {args[i]}"}

    entries = _read_entries()

    if app_filter:
        entries = [e for e in entries if e.get("app") == app_filter]
    if status_filter:
        entries = [e for e in entries if e.get("status") == status_filter]

    total = len(entries)
    entries = list(reversed(entries))[:limit]

    return {"entries": entries, "total": total}


def _cmd_tail(args):
    """Show last N log entries. Usage: tail [N]"""
    n = 10
    if args:
        try:
            n = int(args[0])
        except ValueError:
            return {"error": f"invalid number: {args[0]}"}

    entries = _read_entries()
    return {"entries": entries[-n:]}


def _cmd_search(args):
    """Search logs by keyword. Usage: search <query> [--limit N] [--app NAME]"""
    limit = 20
    app_filter = None
    query_parts = []

    i = 0
    while i < len(args):
        if args[i] == "--limit" and i + 1 < len(args):
            try:
                limit = int(args[i + 1])
            except ValueError:
                return {"error": f"invalid limit: {args[i + 1]}"}
            i += 2
        elif args[i] == "--app" and i + 1 < len(args):
            app_filter = args[i + 1]
            i += 2
        else:
            query_parts.append(args[i])
            i += 1

    if not query_parts:
        return {"error": "usage: log search <query> [--limit N] [--app NAME]"}

    query = " ".join(query_parts).lower()
    entries = _read_entries()

    if app_filter:
        entries = [e for e in entries if e.get("app") == app_filter]

    # Search across all text fields in each entry
    matches = []
    for e in entries:
        text = json.dumps(e).lower()
        if query in text:
            matches.append(e)

    return {"entries": matches[:limit], "total": len(matches)}


def run(command, args):
    """Entry point called by aos."""
    commands = {
        "write": _cmd_write,
        "read": _cmd_read,
        "tail": _cmd_tail,
        "search": _cmd_search,
    }
    handler = commands.get(command)
    if handler is None:
        return {"error": f"unknown command: {command}"}
    return handler(args)
