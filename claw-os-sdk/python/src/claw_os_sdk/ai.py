"""AI helper for Claw OS Python apps.

``chat`` is the stable model API and shells out to
``cos ai chat --app <id>``. The kernel derives ``ai.chat`` or
``ai.chat.untrusted`` from the prompt origin, then runs capability
checks, budget, safety, and audit before a model sees the prompt.

``cos ai chat`` is deliberately separate from ``cos agent chat``.
``cos agent`` is the kernel's *own* Agent product (REPL, memory,
skills, hooks, sessions, recall). Apps must not use it. ``cos ai``
is the App-developer-facing primitive: raw, gated LLM access with
no loop and no kernel state.

Apps **never** name a verb. ``embed``, image, vision, audio, and video
helpers retain their signatures as deprecated, experimental
compatibility shims. They are currently unsupported and always raise
``AiUnsupported`` before invoking ``cos``.

Apps also do **not** pick the model. The machine owner configures one
provider/model in ``/etc/cos/agent.toml`` and every app's call uses
that. ``chat`` exposes ``origin``, ``max_units``, and prompt controls,
never a ``model`` argument.

Typical usage::

    from claw_os_sdk import ai

    def handle_summarize(args):
        result = ai.chat(
            prompt=args["body"],
            origin="external-content",   # text came from outside
            max_units=2000,
        )
        return {"summary": result.text, "usage": result.usage}

The helper makes ``app_id`` explicit when the env var
``COS_APP_ID`` is missing — that way unit tests can target a specific
app without having to set up a full kernel session.

Why a subprocess and not an in-process check?
---------------------------------------------

Apps are **not** allowed to import provider SDKs (openai, anthropic,
google.generativeai, ...). The kernel enforces this at install time
via ``cos app lint``; the AI helper is the *only* sanctioned route to
a model. Centralising the request in ``cos ai chat`` means the
kernel — not the app — controls budget, safety, and audit.
"""

from __future__ import annotations

import json
import os
import shutil
import subprocess
from dataclasses import dataclass, field
from typing import Any, Dict, List, Mapping, Optional


# Subprocess timeout — covers every shell-out to the `cos` binary. The
# default is long enough for slow providers but bounded so a hung child
# never blocks the calling app forever.
_DEFAULT_TIMEOUT_S = 60


def _truncate(value: Any, limit: int = 200) -> str:
    """Return ``str(value)`` truncated to ``limit`` chars with an ellipsis
    marker. Used to keep large response payloads — which routinely
    contain the user's prompt and the model's reply — out of exception
    strings that flow into logs.
    """
    s = repr(value) if not isinstance(value, str) else value
    if len(s) <= limit:
        return s
    return s[:limit] + f"... [{len(s) - limit} more bytes elided]"


# ---------------------------------------------------------------------------
# Exceptions
# ---------------------------------------------------------------------------


class AiError(Exception):
    """Base class for every error this module raises."""


class AiUnavailable(AiError):
    """The ``cos`` binary could not be invoked or returned garbage."""


class AiUnsupported(AiError):
    """An experimental compatibility modality is currently unsupported."""

    def __init__(self, modality: str):
        self.modality = modality
        super().__init__(
            f"{modality}: currently unsupported; "
            "only chat/chat-untrusted are stable"
        )


class AiDenied(AiError):
    """A gate (capability / origin / budget) refused the call.

    The ``payload`` attribute holds the structured envelope
    ``cos ai chat`` returned (verb, scope, reason, hint, …) —
    suitable for forwarding back to the agent.
    """

    def __init__(self, payload: Mapping[str, Any]):
        self.payload = dict(payload)
        super().__init__(self.payload.get("error") or "AI call denied")


class AiBudgetExceeded(AiDenied):
    """The per-app monthly budget was exhausted."""


class AiSafetyViolation(AiDenied):
    """The safety pipeline refused the request."""


# ---------------------------------------------------------------------------
# Response shape
# ---------------------------------------------------------------------------


@dataclass
class Usage:
    input_tokens: int = 0
    output_tokens: int = 0
    units: int = 0


@dataclass
class Budget:
    period: str = ""
    units_used: int = 0
    units_cap: int = 0


@dataclass
class Review:
    safety: str = "strict"
    prompt_redacted: bool = False


@dataclass
class ProposedToolCall:
    """A tool call the model proposed but the gate did **not** execute.

    The kernel surfaces these in ``AiResponse.tool_calls``. Apps
    decide whether to fulfil any of them by calling
    :func:`claw_os_sdk.tools.call` with the same ``name`` and ``input``.
    The ``id`` echoes back to the provider on the next turn.
    """

    id: str
    name: str
    input: Dict[str, Any] = field(default_factory=dict)


@dataclass
class AiResponse:
    text: str
    model: str
    provider: str
    verb: str = ""
    usage: Usage = field(default_factory=Usage)
    budget: Budget = field(default_factory=Budget)
    review: Review = field(default_factory=Review)
    tool_calls: List[ProposedToolCall] = field(default_factory=list)
    raw: Dict[str, Any] = field(default_factory=dict)


# ---------------------------------------------------------------------------
# Public API
# ---------------------------------------------------------------------------


def chat(
    prompt: str,
    *,
    origin: str = "trusted",
    max_units: Optional[int] = None,
    system: Optional[str] = None,
    app_id: Optional[str] = None,
    tools: Optional[List[str]] = None,
) -> AiResponse:
    """Send a single-shot chat completion through the kernel's AI gate.

    The gate derives the verb (``ai.chat`` or ``ai.chat.untrusted``)
    from ``origin``: pass ``"external-content"`` for any third-party
    text (emails, web pages, file contents, another agent's output)
    so the strict safety pipeline kicks in.

    ``tools`` is a list of catalog tool names (e.g.
    ``["fs.read_text", "kv.get"]``) the model may *propose* calling.
    The gate **never** executes them — proposed calls come back in
    :attr:`AiResponse.tool_calls`. Apps inspect them and re-call the
    kernel via :func:`claw_os_sdk.tools.call` for whichever they choose.
    Use :mod:`claw_os_sdk.tools` to look up the catalog at runtime.

    Returns an :class:`AiResponse`. Raises :class:`AiBudgetExceeded`,
    :class:`AiSafetyViolation`, :class:`AiDenied`, or
    :class:`AiUnavailable` on failure.
    """
    if not prompt or not prompt.strip():
        raise AiError("chat: prompt must be non-empty")
    return _dispatch(
        modality="chat",
        prompt=prompt,
        origin=origin,
        max_units=max_units,
        system=system,
        app_id=app_id,
        tools=tools,
    )


def embed(
    prompt: str,
    *,
    origin: str = "trusted",
    max_units: Optional[int] = None,
    app_id: Optional[str] = None,
) -> AiResponse:
    """Deprecated experimental compatibility shim; currently unsupported."""
    raise AiUnsupported("embed")


def image_generate(
    prompt: str,
    *,
    output: str,
    origin: str = "trusted",
    max_units: Optional[int] = None,
    app_id: Optional[str] = None,
) -> AiResponse:
    """Deprecated experimental compatibility shim; currently unsupported."""
    raise AiUnsupported("image.generate")


def image_analyze(
    *,
    image: str,
    origin: str = "trusted",
    max_units: Optional[int] = None,
    app_id: Optional[str] = None,
) -> AiResponse:
    """Deprecated experimental compatibility shim; currently unsupported."""
    raise AiUnsupported("image.analyze")


def vision_analyze(
    prompt: str,
    *,
    image: str,
    origin: str = "trusted",
    max_units: Optional[int] = None,
    system: Optional[str] = None,
    app_id: Optional[str] = None,
) -> AiResponse:
    """Deprecated experimental compatibility shim; currently unsupported."""
    raise AiUnsupported("vision.analyze")


def audio_tts(
    prompt: str,
    *,
    output: str,
    origin: str = "trusted",
    max_units: Optional[int] = None,
    app_id: Optional[str] = None,
) -> AiResponse:
    """Deprecated experimental compatibility shim; currently unsupported."""
    raise AiUnsupported("audio.tts")


def audio_stt(
    *,
    audio: str,
    origin: str = "trusted",
    max_units: Optional[int] = None,
    app_id: Optional[str] = None,
) -> AiResponse:
    """Deprecated experimental compatibility shim; currently unsupported."""
    raise AiUnsupported("audio.stt")


def video_generate(
    prompt: str,
    *,
    output: str,
    origin: str = "trusted",
    max_units: Optional[int] = None,
    app_id: Optional[str] = None,
) -> AiResponse:
    """Deprecated experimental compatibility shim; currently unsupported."""
    raise AiUnsupported("video.generate")


def video_analyze(
    *,
    video: str,
    prompt: Optional[str] = None,
    origin: str = "trusted",
    max_units: Optional[int] = None,
    app_id: Optional[str] = None,
) -> AiResponse:
    """Deprecated experimental compatibility shim; currently unsupported."""
    raise AiUnsupported("video.analyze")


def budget(app_id: Optional[str] = None) -> Budget:
    """Return the current-period budget snapshot for an app."""
    app = app_id or os.environ.get("COS_APP_ID")
    if not app:
        raise AiError("budget: app_id is required")
    cmd = [_cos_binary(), "agent", "budget", "show", app]
    try:
        proc = subprocess.run(
            cmd,
            capture_output=True,
            text=True,
            check=False,
            timeout=_DEFAULT_TIMEOUT_S,
        )
    except subprocess.TimeoutExpired as exc:
        raise AiUnavailable(
            f"cos agent budget show timed out after {_DEFAULT_TIMEOUT_S}s"
        ) from exc
    text = (proc.stdout or "").strip() or (proc.stderr or "").strip()
    if not text:
        raise AiUnavailable(
            f"cos agent budget show returned no output (exit {proc.returncode})"
        )
    try:
        env = json.loads(text)
    except json.JSONDecodeError as exc:
        raise AiUnavailable(
            f"cos agent budget show returned non-JSON output: {_truncate(text)}"
        ) from exc
    if proc.returncode != 0:
        # A non-zero exit always means failure, even if stdout happened
        # to be valid JSON — the body may be a partial frame.
        raise AiUnavailable(
            f"cos agent budget show exited {proc.returncode}: {_truncate(text)}"
        )
    return Budget(
        period=env.get("period", ""),
        units_used=int(env.get("units_used", 0) or 0),
        units_cap=int(env.get("units_cap", 0) or 0),
    )


# ---------------------------------------------------------------------------
# Internals
# ---------------------------------------------------------------------------


def _dispatch(
    *,
    modality: str,
    prompt: Optional[str],
    origin: str,
    max_units: Optional[int],
    app_id: Optional[str],
    system: Optional[str] = None,
    tools: Optional[List[str]] = None,
) -> AiResponse:
    """Build the stable ``cos ai chat`` command and parse its envelope."""
    app = app_id or os.environ.get("COS_APP_ID")
    if not app:
        raise AiError(
            f"{modality}: app_id is required (pass app_id= or set COS_APP_ID)"
        )

    cmd = [_cos_binary(), "ai", "chat", "--app", app, "--origin", origin]
    if prompt is not None:
        cmd.extend(["--prompt", prompt])
    if max_units is not None:
        cmd.extend(["--max-units", str(max_units)])
    if system is not None:
        cmd.extend(["--system", system])
    if tools:
        cmd.extend(["--tools", ",".join(tools)])

    try:
        proc = subprocess.run(
            cmd,
            capture_output=True,
            text=True,
            check=False,
            timeout=_DEFAULT_TIMEOUT_S,
        )
    except subprocess.TimeoutExpired as exc:
        raise AiUnavailable(
            f"cos ai {modality} timed out after {_DEFAULT_TIMEOUT_S}s"
        ) from exc
    payload_text = (proc.stdout or "").strip() or (proc.stderr or "").strip()
    if not payload_text:
        raise AiUnavailable(
            f"cos ai chat returned no output (exit {proc.returncode})"
        )

    try:
        envelope = json.loads(payload_text)
    except json.JSONDecodeError as exc:
        raise AiUnavailable(
            f"cos ai chat returned non-JSON output: {_truncate(payload_text)}"
        ) from exc

    if proc.returncode != 0 or "error" in envelope:
        _raise_for_error(envelope)

    return _parse_response(envelope)


def _parse_response(env: Mapping[str, Any]) -> AiResponse:
    usage = env.get("usage") or {}
    budget_blk = env.get("budget") or {}
    review = env.get("review") or {}
    raw_calls = env.get("tool_calls") or []
    parsed_calls: List[ProposedToolCall] = []
    if isinstance(raw_calls, list):
        for tc in raw_calls:
            if not isinstance(tc, Mapping):
                continue
            parsed_calls.append(
                ProposedToolCall(
                    id=str(tc.get("id", "")),
                    name=str(tc.get("name", "")),
                    input=dict(tc.get("input") or {}),
                )
            )
    return AiResponse(
        text=env.get("text", ""),
        model=env.get("model", ""),
        provider=env.get("provider", ""),
        verb=env.get("verb", ""),
        usage=Usage(
            input_tokens=int(usage.get("input_tokens", 0) or 0),
            output_tokens=int(usage.get("output_tokens", 0) or 0),
            units=int(usage.get("units", 0) or 0),
        ),
        budget=Budget(
            period=budget_blk.get("period", ""),
            units_used=int(budget_blk.get("units_used", 0) or 0),
            units_cap=int(budget_blk.get("units_cap", 0) or 0),
        ),
        review=Review(
            safety=review.get("safety", "strict"),
            prompt_redacted=bool(review.get("prompt_redacted", False)),
        ),
        tool_calls=parsed_calls,
        raw=dict(env),
    )


def _raise_for_error(env: Mapping[str, Any]) -> None:
    msg = (env.get("error") or "").lower()
    if "budget" in msg and ("exceed" in msg or "over" in msg):
        raise AiBudgetExceeded(env)
    if "safety" in msg or "redact" in msg or "injection" in msg:
        raise AiSafetyViolation(env)
    raise AiDenied(env)


def _cos_binary() -> str:
    override = os.environ.get("COS_BIN")
    if override:
        return override
    found = shutil.which("cos")
    if found is None:
        raise AiUnavailable(
            "the `cos` binary is not on PATH; cannot reach the AI gate"
        )
    return found
