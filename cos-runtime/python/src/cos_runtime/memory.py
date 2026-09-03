"""App→agent-memory helper for Claw OS Python apps.

Apps voluntarily push searchable summaries of their own activity into
the agent's memory so the agent can later answer cross-app questions
(``"what did I expense at hotels last year?"``) without re-reading
every per-app store.

Every call gates through the kernel ``memory.write`` capability. An
app's manifest binds that verb to its own app id, so apps cannot
impersonate each other (the kernel enforces ``self:<source>`` on every
request).

Typical usage::

    from cos_runtime import memory

    memory.remember(
        text=(
            "Booked Hyatt SFO for 3 nights, total $612 charged to "
            "personal card."
        ),
        source="expense-tracker",
        kind="event",
        entity_id="expense-12839",
        tags=["hotel", "travel", "2025"],
        link="cos app expense-tracker show 12839",
    )

Apps are expected to push *summaries*, not raw rows. The user inspects
or redacts what has been stored via ``cos agent memory``.
"""

from __future__ import annotations

import json
import os
import shutil
import subprocess
from typing import Any, Mapping, Optional, Sequence

from claw_os_sdk._transport import (
    WIRE_ARG,
    WireDenied,
    WireUnavailable,
    decode_response,
)


# Subprocess timeout for every shell-out to the hidden memory bridge.
# ``remember`` may compute a semantic embedding, which on a cold-start
# CPU embedder can take a few seconds — be generous.
_DEFAULT_TIMEOUT_S = 60


# ---------------------------------------------------------------------------
# Exceptions
# ---------------------------------------------------------------------------


class MemoryError(Exception):
    """Base class for every error this module raises."""


class PermissionDenied(MemoryError):
    """The kernel refused the ``memory.write`` capability check."""

    def __init__(self, denial: Mapping[str, Any]):
        self.denial = dict(denial)
        super().__init__(self.denial.get("summary") or "memory.write denied")


class MemoryUnavailable(MemoryError):
    """The ``cos`` binary could not be invoked or returned garbage.

    Apps that treat memory as best-effort (the common case) can catch
    this and continue without surfacing an error to the user.
    """


# ---------------------------------------------------------------------------
# Public API
# ---------------------------------------------------------------------------


def remember(
    text: str,
    *,
    source: str,
    kind: Optional[str] = None,
    entity_id: Optional[str] = None,
    tags: Optional[Sequence[str]] = None,
    link: Optional[str] = None,
    indexable: bool = True,
) -> dict:
    """Push one structured summary into the agent's memory.

    ``source`` must match the app's own id; the kernel rejects writes
    to any other namespace. ``text`` is the human-readable summary
    (required, max 32 KiB). The optional fields help the agent recall
    the entry later:

    * ``kind`` — free-form category (``"event"``, ``"fact"``,
      ``"preference"``, ``"note"`` …). Lowercased server-side.
    * ``entity_id`` — stable id the app uses for the underlying record
      (e.g. ``"expense-12839"``). Lets the agent dedupe and link back.
    * ``tags`` — up to 8 lowercase tags, 48 chars each.
    * ``link`` — optional shell command line the agent (or user) can
      run to inspect the underlying record.
    * ``indexable`` — when ``False`` only the FTS index is updated and
      the semantic embedding is skipped. Useful for high-volume
      writes where vector recall isn't worth the embed latency.

    Returns the kernel's outcome envelope::

        {
          "ok": true,
          "row_id": 42,
          "session_id": "app:expense-tracker",
          "stored_bytes": 178,
          "indexed_semantic": true,
          "text": "..."
        }

    Raises :class:`PermissionDenied` if the app is not authorised,
    :class:`MemoryUnavailable` if the kernel is unreachable, and
    :class:`MemoryError` for validation failures (empty text, bad
    source).
    """
    if not isinstance(text, str) or not text.strip():
        raise MemoryError("memory.remember: text must be a non-empty string")
    if not isinstance(source, str) or not source:
        raise MemoryError("memory.remember: source is required")

    payload: dict[str, Any] = {
        "source": source,
        "text": text,
        "indexable": bool(indexable),
    }
    if kind is not None:
        payload["kind"] = kind
    if entity_id is not None:
        payload["entity_id"] = entity_id
    if tags is not None:
        payload["tags"] = list(tags)
    if link is not None:
        payload["link"] = link

    return _invoke("remember", ["--json", json.dumps(payload)])


def forget(*, source: Optional[str] = None, row_id: Optional[int] = None) -> dict:
    """Delete app-emitted memory rows.

    Pass exactly one of ``source`` (delete every row for that source)
    or ``row_id`` (delete one specific row). Apps usually call this
    when the underlying record is itself deleted so memory stays in
    sync.

    Returns the kernel envelope, e.g. ``{"removed": 12, "source": "expense-tracker"}``
    or ``{"removed": 1, "row_id": 42}``.
    """
    if (source is None) == (row_id is None):
        raise MemoryError("memory.forget: pass exactly one of source= or row_id=")
    args: list[str] = []
    if source is not None:
        args.extend(["--source", source])
    if row_id is not None:
        args.extend(["--row", str(int(row_id))])
    return _invoke("forget", args)


# ---------------------------------------------------------------------------
# Internals
# ---------------------------------------------------------------------------


def _invoke(subcommand: str, args: Sequence[str]) -> dict:
    cmd = [_cos_binary(), WIRE_ARG, "__memory", subcommand, *args]
    try:
        proc = subprocess.run(
            cmd,
            capture_output=True,
            text=True,
            check=False,
            timeout=_DEFAULT_TIMEOUT_S,
        )
    except subprocess.TimeoutExpired as exc:
        raise MemoryUnavailable(
            f"memory {subcommand} timed out after {_DEFAULT_TIMEOUT_S}s"
        ) from exc

    stdout = (proc.stdout or "").strip()
    try:
        return decode_response(
            stdout,
            proc.returncode,
            f"memory {subcommand}",
        )
    except WireDenied as error:
        detail = error.payload.get("detail")
        if (
            error.payload.get("code") == "PERMISSION_DENIED"
            and isinstance(detail, dict)
            and detail.get("decision") == "deny"
        ):
            raise PermissionDenied(detail) from error
        raise MemoryUnavailable(
            f"memory {subcommand} denied: {error}"
        ) from error
    except WireUnavailable as error:
        raise MemoryUnavailable(str(error)) from error


def _cos_binary() -> str:
    """Locate the ``cos`` binary. Honours ``CLAW_COS_BIN`` for tests."""
    override = os.environ.get("CLAW_COS_BIN")
    if override:
        return override
    found = shutil.which("cos")
    if found is None:
        raise MemoryUnavailable(
            "the `cos` binary is not on PATH; cannot push to agent memory"
        )
    return found
