"""notify — Send notifications to the user."""

import fcntl
import json
import os
import uuid
from datetime import datetime, timezone


DATA_DIR = os.environ.get("AOS_DATA_DIR", "/var/lib/aos")
NOTIFICATIONS_FILE = os.path.join(DATA_DIR, "notifications.json")


def _load_notifications():
    """Load notifications from disk, returning a list."""
    if not os.path.isfile(NOTIFICATIONS_FILE):
        return []
    with open(NOTIFICATIONS_FILE, "r") as f:
        try:
            return json.load(f)
        except (json.JSONDecodeError, ValueError):
            return []


def _save_notifications(notifications):
    """Save notifications list to disk."""
    os.makedirs(os.path.dirname(NOTIFICATIONS_FILE), exist_ok=True)
    with open(NOTIFICATIONS_FILE, "w") as f:
        json.dump(notifications, f, indent=2)


def _with_lock(fn):
    """Run fn while holding an exclusive lock on the notifications file."""
    os.makedirs(os.path.dirname(NOTIFICATIONS_FILE), exist_ok=True)
    lock_path = NOTIFICATIONS_FILE + ".lock"
    with open(lock_path, "w") as lock_fd:
        fcntl.flock(lock_fd, fcntl.LOCK_EX)
        try:
            return fn()
        finally:
            fcntl.flock(lock_fd, fcntl.LOCK_UN)


def _cmd_send(args):
    """Send a notification. Usage: send [--urgent] <message>"""
    urgent = False
    message_parts = []

    for arg in args:
        if arg == "--urgent":
            urgent = True
        else:
            message_parts.append(arg)

    if not message_parts:
        raise ValueError("send requires a message")

    message = " ".join(message_parts)
    notification_id = uuid.uuid4().hex[:8]
    timestamp = datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%S")

    entry = {
        "id": notification_id,
        "message": message,
        "urgent": urgent,
        "timestamp": timestamp,
        "read": False,
    }

    def do_send():
        notifications = _load_notifications()
        notifications.append(entry)
        _save_notifications(notifications)

    _with_lock(do_send)

    return {
        "id": notification_id,
        "message": message,
        "urgent": urgent,
        "timestamp": timestamp,
    }


def _cmd_list(args):
    """List recent notifications. Usage: list [--limit N]"""
    limit = 20
    i = 0
    while i < len(args):
        if args[i] == "--limit" and i + 1 < len(args):
            try:
                limit = int(args[i + 1])
            except ValueError:
                raise ValueError(f"invalid limit: {args[i + 1]}")
            i += 2
        else:
            raise ValueError(f"unknown argument: {args[i]}")

    def do_list():
        return _load_notifications()

    notifications = _with_lock(do_list)
    total = len(notifications)
    recent = list(reversed(notifications))[:limit]

    return {
        "notifications": recent,
        "total": total,
    }


def run(command, args):
    """Entry point called by aos."""
    commands = {
        "send": _cmd_send,
        "list": _cmd_list,
    }
    handler = commands.get(command)
    if handler is None:
        raise ValueError(f"unknown command: {command}")
    return handler(args)
