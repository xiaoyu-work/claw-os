"""Owner/App-bound event publication; reported data never grants execution."""

from __future__ import annotations

import json
import os
import time
from typing import Any, Callable

from .kernel import call_json_with_stdin_binary


def publish(
    event_type: str,
    key: str,
    *,
    payload: dict[str, Any] | None = None,
    entity_id: str | None = None,
    deadline_unix_ms: int | None = None,
    check_cancelled: Callable[[], None] | None = None,
    binary: str | None = None,
) -> dict[str, Any]:
    request = {"event_type": event_type, "key": key}
    if payload is not None:
        request["payload"] = payload
    if entity_id is not None:
        request["entity_id"] = entity_id
    data = json.dumps(request, ensure_ascii=False, separators=(",", ":")).encode("utf-8")
    if len(data) > 20 * 1024:
        raise ValueError("App event request exceeds 20 KiB")
    return call_json_with_stdin_binary(
        binary if binary is not None else os.environ.get("CLAW_COS_BIN", "/usr/local/bin/cos"),
        ["__app_event", "publish"],
        data,
        deadline_unix_ms=(
            time.time_ns() // 1_000_000 + 30_000 if deadline_unix_ms is None else deadline_unix_ms
        ),
        check_cancelled=check_cancelled,
    )
