"""AI helper for Claw OS Python apps.

Every Python app that needs to talk to a model (LLM, embedding,
image-gen, TTS, STT, vision) must go through this helper. The helper
shells out to ``cos agent chat --app <id>`` (or its sibling commands),
which is the kernel's authoritative entry point for AI requests. The
kernel applies capability checks, the app's manifest ``ai`` policy
(model allowlist, prompt-origin allowlist), per-month budget
enforcement, the safety pipeline, and audit before letting any model
see the prompt.

Typical usage::

    from _lib import ai

    def handle_summarize(args):
        result = ai.chat(
            prompt=args["body"],
            origin="external-content",   # text came from outside
            verb="ai.chat.untrusted",
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
a model. Centralising the request in ``cos agent chat`` means the
kernel — not the app — controls budget, safety, and audit.
"""

from __future__ import annotations

import json
import os
import shutil
import subprocess
from dataclasses import dataclass, field
from typing import Any, Dict, Mapping, Optional


# ---------------------------------------------------------------------------
# Exceptions
# ---------------------------------------------------------------------------


class AiError(Exception):
    """Base class for every error this module raises."""


class AiUnavailable(AiError):
    """The ``cos`` binary could not be invoked or returned garbage."""


class AiDenied(AiError):
    """A gate (capability / origin / model glob / budget) refused the call.

    The ``payload`` attribute holds the structured envelope
    ``cos agent chat`` returned (verb, scope, reason, hint, …) —
    suitable for forwarding back to the agent.
    """

    def __init__(self, payload: Mapping[str, Any]):
        self.payload = dict(payload)
        super().__init__(self.payload.get("error") or "AI call denied")


class AiBudgetExceeded(AiDenied):
    """The per-app monthly budget was exhausted."""


class AiSafetyViolation(AiDenied):
    """The safety pipeline refused the request."""


class AiModelNotAllowed(AiDenied):
    """The app's manifest does not allow the requested model."""


# ---------------------------------------------------------------------------
# Response shape
# ---------------------------------------------------------------------------


@dataclass
class Usage:
    input_tokens: int = 0
    output_tokens: int = 0
    units: int = 0
    usd: float = 0.0


@dataclass
class Budget:
    period: str = ""
    units_used: int = 0
    units_cap: int = 0
    usd_used: float = 0.0
    usd_cap: float = 0.0


@dataclass
class Review:
    safety: str = "strict"
    prompt_redacted: bool = False


@dataclass
class AiResponse:
    text: str
    model: str
    provider: str
    usage: Usage = field(default_factory=Usage)
    budget: Budget = field(default_factory=Budget)
    review: Review = field(default_factory=Review)
    raw: Dict[str, Any] = field(default_factory=dict)


# ---------------------------------------------------------------------------
# Public API
# ---------------------------------------------------------------------------


def chat(
    prompt: str,
    *,
    origin: str = "trusted",
    verb: str = "ai.chat",
    model: Optional[str] = None,
    max_units: Optional[int] = None,
    system: Optional[str] = None,
    app_id: Optional[str] = None,
) -> AiResponse:
    """Send a single-shot chat completion through the kernel's AI gate.

    Parameters mirror ``cos agent chat --app <id>`` exactly. ``origin``
    defaults to ``"trusted"`` — apps that feed in third-party text
    (emails, web pages, file contents, another agent's output) MUST
    pass ``"external-content"`` and use ``verb="ai.chat.untrusted"``
    so the strict safety pipeline kicks in.

    Returns an :class:`AiResponse`. Raises :class:`AiBudgetExceeded`,
    :class:`AiModelNotAllowed`, :class:`AiSafetyViolation`,
    :class:`AiDenied`, or :class:`AiUnavailable` on failure.
    """
    if not prompt.strip():
        raise AiError("chat: prompt must be non-empty")

    app = app_id or os.environ.get("COS_APP_ID")
    if not app:
        raise AiError(
            "chat: app_id is required (pass app_id= or set COS_APP_ID)"
        )

    cmd = [_cos_binary(), "agent", "chat", "--app", app, "--prompt", prompt,
           "--origin", origin, "--verb", verb]
    if model is not None:
        cmd.extend(["--model", model])
    if max_units is not None:
        cmd.extend(["--max-units", str(max_units)])
    if system is not None:
        cmd.extend(["--system", system])

    proc = subprocess.run(cmd, capture_output=True, text=True, check=False)
    payload_text = (proc.stdout or "").strip() or (proc.stderr or "").strip()
    if not payload_text:
        raise AiUnavailable(
            f"cos agent chat returned no output (exit {proc.returncode})"
        )

    try:
        envelope = json.loads(payload_text)
    except json.JSONDecodeError as exc:
        raise AiUnavailable(
            f"cos agent chat returned non-JSON output: {payload_text!r}"
        ) from exc

    if proc.returncode != 0 or "error" in envelope:
        _raise_for_error(envelope)

    return _parse_response(envelope)


def budget(app_id: Optional[str] = None) -> Budget:
    """Return the current-period budget snapshot for an app."""
    app = app_id or os.environ.get("COS_APP_ID")
    if not app:
        raise AiError("budget: app_id is required")
    cmd = [_cos_binary(), "agent", "budget", "show", app]
    proc = subprocess.run(cmd, capture_output=True, text=True, check=False)
    text = (proc.stdout or "").strip() or (proc.stderr or "").strip()
    if not text:
        raise AiUnavailable(
            f"cos agent budget show returned no output (exit {proc.returncode})"
        )
    try:
        env = json.loads(text)
    except json.JSONDecodeError as exc:
        raise AiUnavailable(
            f"cos agent budget show returned non-JSON output: {text!r}"
        ) from exc
    return Budget(
        period=env.get("period", ""),
        units_used=int(env.get("units_used", 0) or 0),
        units_cap=0,
        usd_used=float(env.get("usd_used", 0.0) or 0.0),
        usd_cap=0.0,
    )


# ---------------------------------------------------------------------------
# Internals
# ---------------------------------------------------------------------------


def _parse_response(env: Mapping[str, Any]) -> AiResponse:
    usage = env.get("usage") or {}
    budget_blk = env.get("budget") or {}
    review = env.get("review") or {}
    return AiResponse(
        text=env.get("text", ""),
        model=env.get("model", ""),
        provider=env.get("provider", ""),
        usage=Usage(
            input_tokens=int(usage.get("input_tokens", 0) or 0),
            output_tokens=int(usage.get("output_tokens", 0) or 0),
            units=int(usage.get("units", 0) or 0),
            usd=float(usage.get("usd", 0.0) or 0.0),
        ),
        budget=Budget(
            period=budget_blk.get("period", ""),
            units_used=int(budget_blk.get("units_used", 0) or 0),
            units_cap=int(budget_blk.get("units_cap", 0) or 0),
            usd_used=float(budget_blk.get("usd_used", 0.0) or 0.0),
            usd_cap=float(budget_blk.get("usd_cap", 0.0) or 0.0),
        ),
        review=Review(
            safety=review.get("safety", "strict"),
            prompt_redacted=bool(review.get("prompt_redacted", False)),
        ),
        raw=dict(env),
    )


def _raise_for_error(env: Mapping[str, Any]) -> None:
    msg = (env.get("error") or "").lower()
    if "budget" in msg and ("exceed" in msg or "over" in msg):
        raise AiBudgetExceeded(env)
    if "model" in msg and ("not allowed" in msg or "does not match" in msg):
        raise AiModelNotAllowed(env)
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
