"""Shared helper to push gateway "send" outcomes into the agent's memory.

Every gateway-app's ``send`` (Slack / Discord / Telegram / …) ultimately
returns a small dict like::

    {"ok": True, "platform": "slack", "channel_id": "C123", "ts": "..."}

Calling :func:`remember_send` once after a successful send records a
single natural-language line into the user's per-app memory, scoped to
``source="gateway-<platform>"``. The agent can later answer questions
like "what did I say to <recipient> last week?" without needing any
gateway-specific glue.

Best-effort: every failure path is swallowed. A memory hiccup must
never break the user-visible outbound action.
"""

from __future__ import annotations

from typing import Any, Iterable

try:
    from cos_runtime import memory as _memory
except Exception:  # pragma: no cover - the runtime is always present in prod
    _memory = None  # type: ignore[assignment]


def _short(text: str, limit: int = 200) -> str:
    text = str(text or "").strip()
    if len(text) <= limit:
        return text
    return text[: limit - 1] + "\u2026"


def remember_send(
    platform: str,
    result: Any,
    *,
    channel_id: str = "",
    text: str = "",
    extra_tags: Iterable[str] | None = None,
) -> None:
    """Record one line of memory for a successful gateway send.

    No-op on failed sends (``result`` missing or ``result.get("ok")`` is
    falsy) and on any internal error.
    """
    if _memory is None:
        return
    try:
        if not isinstance(result, dict) or not result.get("ok"):
            return
        plat = (platform or result.get("platform") or "").strip() or "unknown"
        source = f"gateway-{plat}"
        # Prefer the gateway's reported channel_id over the caller's hint.
        channel = result.get("channel_id") or channel_id or ""
        # Common "message id" fields across providers.
        msg_id = (
            result.get("ts")
            or result.get("message_id")
            or result.get("id")
            or result.get("update_id")
            or ""
        )
        snippet = _short(text)
        bits = [f"Sent via {plat}"]
        if channel:
            bits.append(f"to {channel}")
        if snippet:
            bits.append(f"— {snippet}")
        line = " ".join(bits)
        tags = [plat, "outbound"]
        if extra_tags:
            tags.extend(str(t) for t in extra_tags if t)
        _memory.remember(
            source=source,
            text=line,
            kind="event",
            entity_id=str(msg_id) if msg_id else None,
            tags=tags,
        )
    except getattr(_memory, "MemoryError", Exception):
        pass
    except Exception:
        # Memory must never crash a gateway send.
        pass
