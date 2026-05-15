"""Tool helper for Claw OS Python apps.

Apps that want to fulfil a model-proposed tool call (returned in
``ai.AiResponse.tool_calls`` after ``ai.chat(..., tools=[...])``) shell
out through this helper to ``cos ai tool <name> --app <id> --args
<json>``. The kernel runs the catalog implementation under the App's
own capabilities, audits the call, and returns a structured result.

Why separate from :mod:`claw_os_sdk.ai`?
---------------------------------

``ai.chat`` is the **only** path to a language model. Tools are the
**only** path to *computer operations* the AI is allowed to request.
Keeping them in distinct modules makes that distinction visible at
the call site:

    proposal = ai.chat(prompt=..., tools=tools.for_chat("fs.read_text"))
    for call in proposal.tool_calls:
        result = tools.call(call.name, call.input)
        # ... feed result back into the next ai.chat turn ...

Apps **never** name a verb. The kernel derives the underlying caps
verb (``fs.read``, ``fs.list``, ``data.kv.read``, ...) from the tool
name and runs ``caps::require`` against the App's grant before any
side effect.

Apps also **never** call ``cos agent`` for tools. ``cos agent`` is
the kernel's own Agent product; ``cos ai tool`` is the App-facing
primitive. See ``docs/app-ai-integration.md`` §10.

Typical usage
-------------

::

    from claw_os_sdk import ai, tools

    proposal = ai.chat(
        prompt="Summarise the file at /etc/hostname",
        tools=tools.for_chat("fs.read_text"),
    )

    for call in proposal.tool_calls:
        try:
            result = tools.call(call.name, call.input)
        except tools.ToolDenied as e:
            # caps / unknown / args-shape failure — the gate audited it
            print("denied:", e.payload)
            continue
        # feed result.value back into the next turn however the app prefers
"""

from __future__ import annotations

import json
import os
import shutil
import subprocess
from dataclasses import dataclass, field
from typing import Any, Dict, List, Mapping, Optional


# ---------------------------------------------------------------------------
# Exceptions
# ---------------------------------------------------------------------------


class ToolError(Exception):
    """Base class for every error this module raises."""


class ToolUnavailable(ToolError):
    """The ``cos`` binary could not be invoked or returned garbage."""


class ToolDenied(ToolError):
    """A gate (capability / unknown tool / args shape) refused the call.

    The ``payload`` attribute holds the structured envelope
    ``cos ai tool`` returned (verb, scope, reason, hint, …) —
    suitable for forwarding back to the agent.
    """

    def __init__(self, payload: Mapping[str, Any]):
        self.payload = dict(payload)
        super().__init__(self.payload.get("error") or "Tool call denied")


# ---------------------------------------------------------------------------
# Response shape
# ---------------------------------------------------------------------------


@dataclass
class ToolResult:
    """The kernel-mediated result of one tool invocation.

    ``value`` is the JSON the catalog implementation produced. Its
    shape is documented per-tool in ``docs/app-ai-tool-catalog.md``.
    ``status`` is ``"ok"`` on success; the kernel always raises
    :class:`ToolDenied` instead of returning a non-ok status here.
    """

    name: str
    app_id: str = ""
    status: str = "ok"
    value: Any = None
    raw: Dict[str, Any] = field(default_factory=dict)


@dataclass
class CatalogEntry:
    """One row from ``cos ai tools``."""

    name: str
    summary: str
    verb: str
    stability: str = "experimental"
    args_schema: Optional[Dict[str, Any]] = None
    returns_schema: Optional[Dict[str, Any]] = None
    raw: Dict[str, Any] = field(default_factory=dict)


# ---------------------------------------------------------------------------
# Public API
# ---------------------------------------------------------------------------


def call(
    name: str,
    args: Optional[Mapping[str, Any]] = None,
    *,
    app_id: Optional[str] = None,
) -> ToolResult:
    """Invoke a catalog tool through the kernel.

    Shells to ``cos ai tool <name> --app <id> --args <json>``. The
    kernel resolves ``name`` against the catalog, derives the caps
    verb + scope, runs ``caps::require``, executes the implementation,
    and writes an audit row.

    Raises :class:`ToolDenied` for anything the gate refused
    (unknown tool, missing capability, malformed args, …) and
    :class:`ToolUnavailable` for transport problems.
    """
    if not name or not isinstance(name, str):
        raise ToolError("call: name must be a non-empty string")
    app = app_id or os.environ.get("COS_APP_ID")
    if not app:
        raise ToolError(
            f"{name}: app_id is required (pass app_id= or set COS_APP_ID)"
        )

    args_payload = json.dumps(dict(args or {}))
    cmd = [
        _cos_binary(),
        "ai",
        "tool",
        name,
        "--app",
        app,
        "--args",
        args_payload,
    ]

    proc = subprocess.run(cmd, capture_output=True, text=True, check=False)
    text = (proc.stdout or "").strip() or (proc.stderr or "").strip()
    if not text:
        raise ToolUnavailable(
            f"cos ai tool {name} returned no output (exit {proc.returncode})"
        )
    try:
        envelope = json.loads(text)
    except json.JSONDecodeError as exc:
        raise ToolUnavailable(
            f"cos ai tool {name} returned non-JSON output: {text!r}"
        ) from exc

    if proc.returncode != 0 or "error" in envelope:
        raise ToolDenied(envelope)

    return ToolResult(
        name=str(envelope.get("tool", name)),
        app_id=str(envelope.get("app_id", app)),
        status=str(envelope.get("status", "ok")),
        value=envelope.get("result"),
        raw=dict(envelope),
    )


def catalog() -> List[CatalogEntry]:
    """Return the live tool catalog as exposed by ``cos ai tools``.

    Apps shouldn't hard-code tool names at install time without
    consulting this list; the catalog evolves and a tool can be
    deprecated or renamed between releases.
    """
    cmd = [_cos_binary(), "ai", "tools"]
    proc = subprocess.run(cmd, capture_output=True, text=True, check=False)
    text = (proc.stdout or "").strip() or (proc.stderr or "").strip()
    if not text:
        raise ToolUnavailable(
            f"cos ai tools returned no output (exit {proc.returncode})"
        )
    try:
        envelope = json.loads(text)
    except json.JSONDecodeError as exc:
        raise ToolUnavailable(
            f"cos ai tools returned non-JSON output: {text!r}"
        ) from exc

    if proc.returncode != 0 or (
        isinstance(envelope, Mapping) and "error" in envelope
    ):
        raise ToolDenied(
            envelope if isinstance(envelope, Mapping) else {"error": str(envelope)}
        )

    rows = envelope.get("tools") if isinstance(envelope, Mapping) else envelope
    if not isinstance(rows, list):
        raise ToolUnavailable(
            f"cos ai tools envelope missing `tools` array: {envelope!r}"
        )

    out: List[CatalogEntry] = []
    for row in rows:
        if not isinstance(row, Mapping):
            continue
        out.append(
            CatalogEntry(
                name=str(row.get("name", "")),
                summary=str(row.get("summary", "")),
                verb=str(row.get("verb", "")),
                stability=str(row.get("stability", "experimental")),
                args_schema=_maybe_schema(row.get("args_schema")),
                returns_schema=_maybe_schema(row.get("returns_schema")),
                raw=dict(row),
            )
        )
    return out


def for_chat(*names: str) -> List[str]:
    """Return tool names ready for :func:`claw_os_sdk.ai.chat`'s ``tools=`` kwarg.

    Whitespace is trimmed and empty entries are dropped so
    ``tools.for_chat("fs.read_text", " kv.get ", "")`` collapses to
    two clean entries. The returned list contains *only* names — the
    kernel resolves them against the catalog at call time.
    """
    out: List[str] = []
    for n in names:
        if not isinstance(n, str):
            raise ToolError(f"for_chat: tool names must be strings, got {n!r}")
        s = n.strip()
        if s:
            out.append(s)
    return out


# ---------------------------------------------------------------------------
# Internals
# ---------------------------------------------------------------------------


def _maybe_schema(blob: Any) -> Optional[Dict[str, Any]]:
    """Schemas come over the wire as either parsed JSON or raw text."""
    if blob is None:
        return None
    if isinstance(blob, Mapping):
        return dict(blob)
    if isinstance(blob, str):
        try:
            parsed = json.loads(blob)
        except json.JSONDecodeError:
            return None
        if isinstance(parsed, Mapping):
            return dict(parsed)
    return None


def _cos_binary() -> str:
    override = os.environ.get("COS_BIN")
    if override:
        return override
    found = shutil.which("cos")
    if found is None:
        raise ToolUnavailable(
            "the `cos` binary is not on PATH; cannot reach the AI gate"
        )
    return found
