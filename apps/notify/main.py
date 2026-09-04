"""notify — Send notifications to the user."""

from __future__ import annotations

import fcntl
import json
import os
import sys
import uuid
from collections.abc import Callable
from datetime import datetime, timezone
from typing import TypeVar

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from _shared.atomic import atomic_write_bytes  # noqa: E402

from cos_runtime import policy  # noqa: E402


DATA_DIR = os.environ.get("COS_DATA_DIR", "/var/lib/cos")
NOTIFICATIONS_FILE = os.path.join(DATA_DIR, "notifications.json")
_T = TypeVar("_T")


def _load_notifications() -> list[dict[str, object]]:
    """Load and validate the notifications store."""
    try:
        with open(NOTIFICATIONS_FILE, "r", encoding="utf-8") as notifications_file:
            loaded: object = json.load(notifications_file)
    except FileNotFoundError:
        return []

    if not isinstance(loaded, list):
        raise ValueError("notifications store must contain a JSON list")

    notifications: list[dict[str, object]] = []
    for index, entry in enumerate(loaded):
        if not isinstance(entry, dict):
            raise ValueError(
                f"notifications store entry {index} must be a JSON object"
            )
        notifications.append(entry)
    return notifications


def _save_notifications(notifications: list[dict[str, object]]) -> None:
    """Atomically save notifications while the caller holds the store lock."""
    payload = json.dumps(notifications, indent=2).encode("utf-8")
    atomic_write_bytes(NOTIFICATIONS_FILE, payload)


def _with_lock(fn: Callable[[], _T]) -> _T:
    """Run fn while holding an exclusive lock on the notifications file."""
    os.makedirs(os.path.dirname(NOTIFICATIONS_FILE), exist_ok=True)
    lock_path = NOTIFICATIONS_FILE + ".lock"
    with open(lock_path, "a+", encoding="utf-8") as lock_fd:
        fcntl.flock(lock_fd, fcntl.LOCK_EX)
        try:
            return fn()
        finally:
            fcntl.flock(lock_fd, fcntl.LOCK_UN)


def send(message: str, urgent: bool = False) -> dict[str, object]:
    """Persist a notification after validating and authorizing the request."""
    if not isinstance(message, str) or not message.strip():
        raise ValueError("message must be a non-empty string")
    if type(urgent) is not bool:
        raise ValueError("urgent must be a boolean")

    policy.require("ui.notify", wild=True)
    notification_id = uuid.uuid4().hex[:8]
    timestamp = datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%S")

    entry: dict[str, object] = {
        "id": notification_id,
        "message": message,
        "urgent": urgent,
        "timestamp": timestamp,
        "read": False,
    }

    def do_send() -> None:
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


def list_notifications(limit: int = 20) -> dict[str, object]:
    """Return the newest notifications first."""
    if not isinstance(limit, int) or isinstance(limit, bool):
        raise ValueError("limit must be an integer")
    if not 1 <= limit <= 100:
        raise ValueError("limit must be 1..100")

    policy.require("data.inbox.read", wild=True)
    notifications = _with_lock(_load_notifications)
    total = len(notifications)
    recent = list(reversed(notifications))[:limit]

    return {
        "notifications": recent,
        "total": total,
    }
