"""AI helper for Claw OS Python apps.

Every Python app that needs to talk to a model (LLM, embedding,
image-gen, TTS, STT, vision, video) must go through this helper. The
helper shells out to ``cos agent chat --app <id>`` — the single,
authoritative entry point for AI requests of every modality. The
kernel derives the modality (and the underlying caps verb) from the
shape of the request, then runs capability check, prompt-origin
allowlist, per-month budget, the safety pipeline, and audit before
letting any model see the prompt.

Apps **never** name a verb. They describe what they want and the
gate picks the verb. The helpers here are the supported Python
surface for each modality:

    ai.chat(prompt, ...)                  → ai.chat / ai.chat.untrusted
    ai.embed(prompt, ...)                 → ai.embed
    ai.image_generate(prompt, output=..)  → ai.image.generate
    ai.image_analyze(image=...)           → ai.image.analyze
    ai.vision_analyze(prompt, image=..)   → ai.vision.analyze
    ai.audio_tts(prompt, output=...)      → ai.audio.tts
    ai.audio_stt(audio=...)               → ai.audio.stt
    ai.video_generate(prompt, output=...) → ai.video.generate
    ai.video_analyze(video=..., prompt=)  → ai.video.analyze

Apps also do **not** pick the model. The machine owner configures one
provider/model in ``/etc/cos/agent.toml`` and every app's call uses
that. The helpers expose ``origin``, ``max_units``, and prompt /
artifact arguments — never a ``model`` argument.

Typical usage::

    from _lib import ai

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
a model. Centralising the request in ``cos agent chat`` means the
kernel — not the app — controls budget, safety, and audit.
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


class AiError(Exception):
    """Base class for every error this module raises."""


class AiUnavailable(AiError):
    """The ``cos`` binary could not be invoked or returned garbage."""


class AiDenied(AiError):
    """A gate (capability / origin / budget) refused the call.

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
class AiResponse:
    text: str
    model: str
    provider: str
    verb: str = ""
    embedding: List[float] = field(default_factory=list)
    output_path: Optional[str] = None
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
    max_units: Optional[int] = None,
    system: Optional[str] = None,
    app_id: Optional[str] = None,
) -> AiResponse:
    """Send a single-shot chat completion through the kernel's AI gate.

    The gate derives the verb (``ai.chat`` or ``ai.chat.untrusted``)
    from ``origin``: pass ``"external-content"`` for any third-party
    text (emails, web pages, file contents, another agent's output)
    so the strict safety pipeline kicks in.

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
    )


def embed(
    prompt: str,
    *,
    origin: str = "trusted",
    max_units: Optional[int] = None,
    app_id: Optional[str] = None,
) -> AiResponse:
    """Embed text into a vector. Result vector lives at ``response.embedding``."""
    if not prompt or not prompt.strip():
        raise AiError("embed: prompt must be non-empty")
    return _dispatch(
        modality="embed",
        prompt=prompt,
        origin=origin,
        max_units=max_units,
        app_id=app_id,
        embed=True,
    )


def image_generate(
    prompt: str,
    *,
    output: str,
    origin: str = "trusted",
    max_units: Optional[int] = None,
    app_id: Optional[str] = None,
) -> AiResponse:
    """Generate an image from a prompt; the gate writes it to ``output``."""
    if not prompt or not prompt.strip():
        raise AiError("image_generate: prompt must be non-empty")
    return _dispatch(
        modality="image.generate",
        prompt=prompt,
        origin=origin,
        max_units=max_units,
        app_id=app_id,
        image_output=output,
    )


def image_analyze(
    *,
    image: str,
    origin: str = "trusted",
    max_units: Optional[int] = None,
    app_id: Optional[str] = None,
) -> AiResponse:
    """Caption / classify an image with no prompt. Use ``vision_analyze`` for Q&A."""
    return _dispatch(
        modality="image.analyze",
        prompt=None,
        origin=origin,
        max_units=max_units,
        app_id=app_id,
        image_input=image,
    )


def vision_analyze(
    prompt: str,
    *,
    image: str,
    origin: str = "trusted",
    max_units: Optional[int] = None,
    system: Optional[str] = None,
    app_id: Optional[str] = None,
) -> AiResponse:
    """Answer a textual question about an image."""
    if not prompt or not prompt.strip():
        raise AiError("vision_analyze: prompt must be non-empty")
    return _dispatch(
        modality="vision.analyze",
        prompt=prompt,
        origin=origin,
        max_units=max_units,
        system=system,
        app_id=app_id,
        image_input=image,
    )


def audio_tts(
    prompt: str,
    *,
    output: str,
    origin: str = "trusted",
    max_units: Optional[int] = None,
    app_id: Optional[str] = None,
) -> AiResponse:
    """Synthesize speech from text; the gate writes the audio to ``output``."""
    if not prompt or not prompt.strip():
        raise AiError("audio_tts: prompt must be non-empty")
    return _dispatch(
        modality="audio.tts",
        prompt=prompt,
        origin=origin,
        max_units=max_units,
        app_id=app_id,
        audio_output=output,
    )


def audio_stt(
    *,
    audio: str,
    origin: str = "trusted",
    max_units: Optional[int] = None,
    app_id: Optional[str] = None,
) -> AiResponse:
    """Transcribe an audio file. Transcript lives at ``response.text``."""
    return _dispatch(
        modality="audio.stt",
        prompt=None,
        origin=origin,
        max_units=max_units,
        app_id=app_id,
        audio_input=audio,
    )


def video_generate(
    prompt: str,
    *,
    output: str,
    origin: str = "trusted",
    max_units: Optional[int] = None,
    app_id: Optional[str] = None,
) -> AiResponse:
    """Generate a video from a prompt; the gate writes it to ``output``."""
    if not prompt or not prompt.strip():
        raise AiError("video_generate: prompt must be non-empty")
    return _dispatch(
        modality="video.generate",
        prompt=prompt,
        origin=origin,
        max_units=max_units,
        app_id=app_id,
        video_output=output,
    )


def video_analyze(
    *,
    video: str,
    prompt: Optional[str] = None,
    origin: str = "trusted",
    max_units: Optional[int] = None,
    app_id: Optional[str] = None,
) -> AiResponse:
    """Describe or answer a question about a video file."""
    return _dispatch(
        modality="video.analyze",
        prompt=prompt,
        origin=origin,
        max_units=max_units,
        app_id=app_id,
        video_input=video,
    )


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
    embed: bool = False,
    image_input: Optional[str] = None,
    image_output: Optional[str] = None,
    audio_input: Optional[str] = None,
    audio_output: Optional[str] = None,
    video_input: Optional[str] = None,
    video_output: Optional[str] = None,
) -> AiResponse:
    """Build the `cos agent chat` command line and parse the envelope.

    All public helpers funnel through here. The kernel-side gate
    derives the caps verb from the flag combination — we never name
    one. ``modality`` is only used for error messages on this side.
    """
    app = app_id or os.environ.get("COS_APP_ID")
    if not app:
        raise AiError(
            f"{modality}: app_id is required (pass app_id= or set COS_APP_ID)"
        )

    cmd = [_cos_binary(), "agent", "chat", "--app", app, "--origin", origin]
    if prompt is not None:
        cmd.extend(["--prompt", prompt])
    if max_units is not None:
        cmd.extend(["--max-units", str(max_units)])
    if system is not None:
        cmd.extend(["--system", system])
    if embed:
        cmd.append("--embed")
    if image_input is not None:
        cmd.extend(["--image-input", image_input])
    if image_output is not None:
        cmd.extend(["--image-output", image_output])
    if audio_input is not None:
        cmd.extend(["--audio-input", audio_input])
    if audio_output is not None:
        cmd.extend(["--audio-output", audio_output])
    if video_input is not None:
        cmd.extend(["--video-input", video_input])
    if video_output is not None:
        cmd.extend(["--video-output", video_output])

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


def _parse_response(env: Mapping[str, Any]) -> AiResponse:
    usage = env.get("usage") or {}
    budget_blk = env.get("budget") or {}
    review = env.get("review") or {}
    embedding_raw = env.get("embedding") or []
    return AiResponse(
        text=env.get("text", ""),
        model=env.get("model", ""),
        provider=env.get("provider", ""),
        verb=env.get("verb", ""),
        embedding=[float(x) for x in embedding_raw] if isinstance(embedding_raw, list) else [],
        output_path=env.get("output_path"),
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
