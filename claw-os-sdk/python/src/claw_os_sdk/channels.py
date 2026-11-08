"""Control only the authenticated App's human-configured message channel."""

from __future__ import annotations

import json
import os
import time
from typing import Any, Callable

from .kernel import call_json_with_stdin_binary


def control(
    action: str,
    *,
    event_id: str | None = None,
    text: str | None = None,
    confirm: bool | None = None,
    deadline_unix_ms: int | None = None,
    check_cancelled: Callable[[], None] | None = None,
    binary: str | None = None,
) -> dict[str, Any]:
    request: dict[str, Any] = {"action": action}
    for name, value in (("event_id", event_id), ("text", text), ("confirm", confirm)):
        if value is not None:
            request[name] = value
    data = json.dumps(request, ensure_ascii=False, separators=(",", ":")).encode("utf-8")
    if len(data) > 20 * 1024:
        raise ValueError("channel control exceeds 20 KiB")
    return call_json_with_stdin_binary(
        binary if binary is not None else os.environ.get("CLAW_COS_BIN", "/usr/local/bin/cos"),
        ["__channel", "control"], data,
        deadline_unix_ms=time.time_ns() // 1_000_000 + 30_000 if deadline_unix_ms is None else deadline_unix_ms,
        check_cancelled=check_cancelled,
    )
