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
import tempfile
from dataclasses import dataclass, field
from typing import Any, Dict, List, Mapping, Optional

from .generated import (
    WireDecodeError,
    decode_wire_json,
    validate_ai,
    validate_budget_show,
    wire_integer_to_int,
)


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
    input: Any


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
        env = decode_wire_json(text)
    except (json.JSONDecodeError, ValueError) as exc:
        raise AiUnavailable(
            f"cos agent budget show returned non-JSON output: {_truncate(text)}"
        ) from exc
    if proc.returncode != 0:
        # A non-zero exit always means failure, even if stdout happened
        # to be valid JSON — the body may be a partial frame.
        raise AiUnavailable(
            f"cos agent budget show exited {proc.returncode}: {_truncate(text)}"
        )
    try:
        validate_budget_show(env)
    except WireDecodeError as exc:
        raise AiUnavailable(f"budget response decode failed: {exc}") from exc
    return Budget(
        period=env["period"],
        units_used=wire_integer_to_int(env["units_used"]),
        units_cap=0,
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

    with tempfile.TemporaryDirectory(prefix="claw-ai-") as private_dir:
        cmd = [_cos_binary(), "ai", "chat", "--app", app, "--origin", origin]

        def add_private_file(flag: str, name: str, value: str) -> None:
            path = os.path.join(private_dir, name)
            fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
            with os.fdopen(fd, "w", encoding="utf-8") as output:
                output.write(value)
                output.flush()
                os.fsync(output.fileno())
            cmd.extend([flag, path])

        if prompt is not None:
            add_private_file("--prompt-file", "prompt", prompt)
        if max_units is not None:
            cmd.extend(["--max-units", str(max_units)])
        if system is not None:
            add_private_file("--system-file", "system", system)
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
        envelope = decode_wire_json(payload_text)
    except (json.JSONDecodeError, ValueError) as exc:
        raise AiUnavailable(
            f"cos ai chat returned non-JSON output: {_truncate(payload_text)}"
        ) from exc

    if proc.returncode != 0:
        if isinstance(envelope, Mapping):
            _raise_for_error(envelope)
        raise AiUnavailable(
            f"cos ai chat exited {proc.returncode}: {_truncate(payload_text)}"
        )
    if isinstance(envelope, Mapping) and "error" in envelope:
        _raise_for_error(envelope)

    return _parse_response(envelope)


def _parse_response(env: Any) -> AiResponse:
    try:
        validate_ai(env)
    except WireDecodeError as exc:
        raise AiUnavailable(f"ai response decode failed: {exc}") from exc
    usage = env["usage"]
    budget_blk = env["budget"]
    review = env["review"]
    raw_calls = env.get("tool_calls", [])
    parsed_calls: List[ProposedToolCall] = []
    for tc in raw_calls:
        parsed_calls.append(
            ProposedToolCall(
                id=tc["id"],
                name=tc["name"],
                input=tc["input"],
            )
        )
    return AiResponse(
        text=env["text"],
        model=env["model"],
        provider=env["provider"],
        verb=env["verb"],
        usage=Usage(
            input_tokens=wire_integer_to_int(usage["input_tokens"]),
            output_tokens=wire_integer_to_int(usage["output_tokens"]),
            units=wire_integer_to_int(usage["units"]),
        ),
        budget=Budget(
            period=budget_blk["period"],
            units_used=wire_integer_to_int(budget_blk["units_used"]),
            units_cap=wire_integer_to_int(budget_blk["units_cap"]),
        ),
        review=Review(
            safety=review["safety"],
            prompt_redacted=review["prompt_redacted"],
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
