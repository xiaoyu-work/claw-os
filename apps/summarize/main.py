"""Summarize explicitly provided text through the Claw OS AI gate."""

from __future__ import annotations

from claw_os_sdk import ai
from cos_runtime import memory, policy


INPUT_SOURCE = "<input>"
SYSTEM_PROMPT = (
    "You are a concise summariser. Read the user's text and reply with "
    "exactly 3 short lines, one bullet per line, no preamble."
)


def _remember_summary(summary: str) -> None:
    head = summary.strip().splitlines()[0]
    if len(head) > 200:
        head = head[:197] + "..."
    memory.remember(
        source="summarize",
        text=f"Summarised {INPUT_SOURCE}: {head}",
        kind="note",
        tags=["summarize"],
    )


def summarize(text: str) -> dict:
    if not isinstance(text, str) or not text.strip():
        raise ValueError("text must be a non-empty string")

    policy.require("ai.chat.untrusted", wild=True)
    response = ai.chat(
        prompt=text,
        origin="external-content",
        system=SYSTEM_PROMPT,
        max_units=4000,
    )
    summary = response.text
    if not isinstance(summary, str) or not summary.strip():
        raise RuntimeError("AI returned an empty summary")

    result = {
        "summary": summary,
        "source": INPUT_SOURCE,
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
    _remember_summary(summary)
    return result
