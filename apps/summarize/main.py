"""summarize — condense a block of text into a 3-line summary.

This is the canonical demo app for the App–AI Gate (Phase 7). It does
*not* import any provider SDK (openai, anthropic, …); the only way it
can reach a model is through ``claw_os_sdk.ai.chat``, which shells out to
``cos ai chat --app summarize``. The kernel applies the capability
check, budget enforcement, safety pipeline, and audit before the
prompt ever leaves the box.
"""

from __future__ import annotations

import sys

from claw_os_sdk import ai
from cos_runtime import memory, policy


SYSTEM_PROMPT = (
    "You are a concise summariser. Read the user's text and reply with "
    "exactly 3 short lines, one bullet per line, no preamble."
)


def _read_input(args):
    file_path = None
    rest = []
    i = 0
    while i < len(args):
        if args[i] == "--file" and i + 1 < len(args):
            file_path = args[i + 1]
            i += 2
        else:
            rest.append(args[i])
            i += 1
    if file_path:
        with open(file_path, "r", encoding="utf-8") as fh:
            return fh.read(), file_path
    if rest:
        return " ".join(rest), None
    # Fall back to stdin if nothing else was supplied.
    data = sys.stdin.read() if not sys.stdin.isatty() else ""
    return data, None


def _cmd_run(args):
    """Summarize the input text into 3 short lines."""
    text, file_path = _read_input(args)
    if not text.strip():
        return {"error": "no input — supply --file PATH or pipe text on stdin"}

    try:
        response = ai.chat(
            prompt=text,
            origin="external-content",
            system=SYSTEM_PROMPT,
            max_units=4000,
        )
    except ai.AiBudgetExceeded as exc:
        return {"error": "AI budget exceeded for this app", "detail": exc.payload}
    except ai.AiSafetyViolation as exc:
        return {"error": "safety violation", "detail": exc.payload}
    except ai.AiDenied as exc:
        return {"error": "AI call denied", "detail": exc.payload}
    except ai.AiUnavailable as exc:
        return {"error": f"AI unavailable: {exc}"}
    except ai.AiError as exc:
        return {"error": str(exc)}

    out = {
        "summary": response.text,
        "source": file_path or "<stdin>",
        "model": response.model,
        "provider": response.provider,
        "usage": {
            "input_tokens": response.usage.input_tokens,
            "output_tokens": response.usage.output_tokens,
            "units": response.usage.units,
        },
        "budget": {
            "period": response.budget.period,
            "units_used": response.budget.units_used,
            "units_cap": response.budget.units_cap,
        },
        "review": {
            "safety": response.review.safety,
            "prompt_redacted": response.review.prompt_redacted,
        },
    }
    _remember_run(out["source"], out["summary"])
    return out


def _remember_run(source, summary):
    try:
        if not summary:
            return
        first = summary.strip().splitlines()
        head = first[0] if first else ""
        if len(head) > 200:
            head = head[:197] + "..."
        link = f"cos summarize run --file {source}" if source and source != "<stdin>" else None
        memory.remember(
            source="summarize",
            text=f"Summarised {source}: {head}",
            kind="note",
            entity_id=source if source != "<stdin>" else None,
            tags=["summarize"],
            link=link,
        )
    except memory.MemoryError:
        pass


def run(command, args):
    """Entry point called by cos."""
    commands = {"run": _cmd_run}
    handler = commands.get(command)
    if handler is None:
        return {"error": f"unknown command: {command}"}
    try:
        # The coarse-grained capability is also re-checked here so we
        # fail fast on a denied agent without paying for a subprocess
        # boot of the gate.
        policy.require("ai.chat.untrusted", wild=True)
        return handler(args)
    except policy.PermissionDenied as denied:
        return {"error": str(denied), "denial": denied.denial}
    except policy.PolicyUnavailable as exc:
        return {"error": f"capability check failed: {exc}"}
