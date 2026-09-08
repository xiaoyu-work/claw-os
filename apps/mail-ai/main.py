"""Shared Mail AI business functions for MCP and Thunderbird Native Messaging.

Both transports call these typed functions in process. AI access goes through
the kernel gate via ``claw_os_sdk.ai``; credentials, consent, budgets, safety
and audit stay there. Summary and notable-triage memory writes retain the
existing ``mail-ai`` self scope. No mailbox ownership or App-to-App calls live
here: this is the preparatory M1 foundation, not a unified Mail product yet.

Operations
----------
- ``summarize``     : email body → one-line summary + key points + action items
- ``smart_reply``   : thread → three reply drafts (formal / casual / short)
- ``smart_compose`` : intent + partial draft → completion
- ``translate``     : text + target language → translated text
- ``triage``        : sender + subject + snippet → category + tags
- ``chat``          : question + context messages → grounded answer

Every operation returns a JSON dict. The shape is stable enough that
``native_host.py`` can forward the result unchanged to the extension.
"""

from __future__ import annotations

import json
import re

from claw_os_sdk import ai
from cos_runtime import memory, policy


# ---------------------------------------------------------------------------
# Limits
# ---------------------------------------------------------------------------
# These caps protect the per-app monthly AI budget and keep prompts well
# inside the provider's context window. The extension trims aggressive
# bodies before sending; this is the second line of defence.

MAX_BODY_CHARS = 12_000      # ~3k tokens of email body per request
MAX_THREAD_CHARS = 24_000    # whole thread (for smart_reply / chat)
MAX_DRAFT_CHARS = 4_000      # current draft snippet for smart_compose
MAX_CONTEXT_MESSAGES = 20    # for chat

CATEGORIES = (
    "important",
    "personal",
    "work",
    "newsletter",
    "promo",
    "receipt",
    "calendar",
    "notification",
    "other",
)


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

def _truncate(text: str, limit: int) -> str:
    if not text:
        return ""
    if len(text) <= limit:
        return text
    head = limit - 200
    return text[:head] + "\n\n[…truncated…]\n\n" + text[-180:]


def _strip_quoted(text: str) -> str:
    """Drop the most common forms of quoted-reply chrome.

    Helps keep the prompt focused on the message at hand instead of
    dumping the whole thread history into a summarise call. We do
    not try to be clever — just chop after the first reliable marker
    and let the caller supply the thread separately when context
    matters (smart_reply / chat).
    """
    if not text:
        return ""
    markers = (
        "\n-- \n",
        "\nOn ",
        "\n> ",
        "\nFrom: ",
        "\n_____",
    )
    cut = len(text)
    for m in markers:
        i = text.find(m)
        if i != -1 and i < cut:
            cut = i
    return text[:cut].strip() or text.strip()


def _safe_loads(s: str) -> dict | None:
    """Try to extract a JSON object from a model response.

    Models occasionally wrap JSON in code fences, prepend prose, or
    append a trailing period. We strip fences and search for the
    outermost ``{...}`` slab.
    """
    if not s:
        return None
    t = s.strip()
    if t.startswith("```"):
        t = re.sub(r"^```[a-zA-Z]*\n?", "", t)
        t = re.sub(r"\n?```\s*$", "", t)
    first = t.find("{")
    last = t.rfind("}")
    if first == -1 or last == -1 or last <= first:
        return None
    try:
        return json.loads(t[first:last + 1])
    except json.JSONDecodeError:
        return None


def _ai_call(prompt: str, *, system: str, max_units: int) -> dict:
    """Single chokepoint for every model call in this app."""
    try:
        policy.require("ai.chat.untrusted", wild=True)
        response = ai.chat(
            prompt=prompt,
            origin="external-content",
            system=system,
            max_units=max_units,
        )
    except policy.PermissionDenied as denied:
        return {"error": str(denied), "denial": denied.denial}
    except policy.PolicyUnavailable as exc:
        return {"error": f"capability check failed: {exc}"}
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

    return {
        "text": response.text,
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


def _wrap(result: dict, payload: dict) -> dict:
    """Merge a verb's structured output with the canonical AI metadata."""
    if "error" in result:
        return result
    out = dict(payload)
    out["model"] = result["model"]
    out["provider"] = result["provider"]
    out["usage"] = result["usage"]
    out["budget"] = result["budget"]
    out["review"] = result["review"]
    return out


# ---------------------------------------------------------------------------
# Shared input validation
# ---------------------------------------------------------------------------

def _text_error(*, required: tuple[str, ...] = (), **values: str) -> dict | None:
    for name, value in values.items():
        if not isinstance(value, str):
            return {"error": f"{name} must be a string"}
        if name in required and not value.strip():
            return {"error": f"{name} must be non-empty"}
    return None


def _remember_summary(subject: str, sender: str, payload: dict) -> None:
    """Push the summary of an email into the agent's memory."""
    try:
        subject = subject or "(no subject)"
        sender = sender or "(unknown)"
        summary = (payload.get("summary") or "").strip()
        action_items = payload.get("action_items") or []
        if not summary and not action_items:
            return  # nothing useful to remember
        text = f"Summarized email from {sender} — {subject}: {summary}".strip()
        if action_items:
            text += " | actions: " + "; ".join(action_items[:3])
        tags = ["mail-ai", "summary"]
        sentiment = payload.get("sentiment")
        if sentiment:
            tags.append(sentiment)
        memory.remember(
            source="mail-ai",
            text=text,
            kind="note",
            tags=tags,
        )
    except memory.MemoryError:
        pass


def _remember_triage(subject: str, sender: str, payload: dict) -> None:
    """Push a triage decision into the agent's memory (only when notable)."""
    try:
        priority = payload.get("priority")
        category = payload.get("category")
        # Skip low-signal triage to keep memory clean.
        if priority not in ("high",) and category in ("other", "newsletter", "marketing"):
            return
        subject = subject or "(no subject)"
        sender = sender or "(unknown)"
        reason = payload.get("reason") or ""
        text = f"Triaged email from {sender} — {subject}: {category} (priority={priority})"
        if reason:
            text += f" — {reason}"
        tags = ["mail-ai", "triage", category, priority]
        memory.remember(
            source="mail-ai",
            text=text,
            kind="event",
            tags=tags,
        )
    except memory.MemoryError:
        pass


# ---------------------------------------------------------------------------
# Operation: summarize
# ---------------------------------------------------------------------------

_SUMMARIZE_SYSTEM = (
    "You are an email assistant. Read the email body and return a single JSON "
    "object — no prose, no code fences — with exactly these keys:\n"
    "  summary       : one short sentence describing what the email is about\n"
    "  key_points    : up to 5 bullet-point strings\n"
    "  action_items  : up to 5 strings — each a concrete thing the recipient "
    "must do (or [] if none)\n"
    "  sentiment     : one of positive | neutral | negative | urgent\n"
    "Reply only with the JSON object."
)


def summarize(*, body: str, subject: str = "", sender: str = "", lang: str = "en") -> dict:
    error = _text_error(required=("body", "lang"), body=body, subject=subject, sender=sender, lang=lang)
    if error:
        return error
    body = _truncate(_strip_quoted(body), MAX_BODY_CHARS)
    if not body.strip():
        return {"error": "body must be non-empty after quote-stripping"}

    prompt = (
        f"Email metadata:\n"
        f"  From:    {sender or '(unknown)'}\n"
        f"  Subject: {subject or '(no subject)'}\n"
        f"Reply language: {lang}\n\n"
        f"--- email body ---\n{body}\n--- end ---"
    )

    result = _ai_call(prompt, system=_SUMMARIZE_SYSTEM, max_units=3000)
    if "error" in result:
        return result

    parsed = _safe_loads(result["text"]) or {}
    payload = {
        "summary": str(parsed.get("summary") or "").strip(),
        "key_points": [str(x) for x in (parsed.get("key_points") or [])][:5],
        "action_items": [str(x) for x in (parsed.get("action_items") or [])][:5],
        "sentiment": str(parsed.get("sentiment") or "neutral"),
        "raw": result["text"] if not parsed else "",
    }
    _remember_summary(subject, sender, payload)
    return _wrap(result, payload)


# ---------------------------------------------------------------------------
# Operation: smart_reply
# ---------------------------------------------------------------------------

_SMART_REPLY_SYSTEM = (
    "You are an email assistant. Read the conversation and return three "
    "reply suggestions in different tones. Return a single JSON object — "
    "no prose, no code fences — with exactly these keys:\n"
    "  formal  : a polite, professional reply (3-6 sentences)\n"
    "  casual  : a friendly, conversational reply (2-4 sentences)\n"
    "  short   : a brief acknowledgement (1-2 sentences)\n"
    "Each value is the email body only — no subject, no salutation labels, "
    "no preamble. Match the recipient's language unless overridden."
)


def smart_reply(
    *, thread: str, subject: str = "", sender: str = "", intent: str = "", lang: str = "en",
) -> dict:
    error = _text_error(
        required=("thread", "lang"), thread=thread, subject=subject, sender=sender, intent=intent, lang=lang,
    )
    if error:
        return error
    thread = _truncate(thread, MAX_THREAD_CHARS)

    intent_hint = (
        f"User wants the reply to: {intent}\n" if intent.strip() else ""
    )
    prompt = (
        f"Conversation metadata:\n"
        f"  Last sender: {sender or '(unknown)'}\n"
        f"  Subject:     {subject or '(no subject)'}\n"
        f"  Reply language: {lang}\n"
        f"{intent_hint}\n"
        f"--- thread (oldest first) ---\n{thread}\n--- end ---"
    )

    result = _ai_call(prompt, system=_SMART_REPLY_SYSTEM, max_units=4000)
    if "error" in result:
        return result

    parsed = _safe_loads(result["text"]) or {}
    return _wrap(result, {
        "suggestions": {
            "formal": str(parsed.get("formal") or "").strip(),
            "casual": str(parsed.get("casual") or "").strip(),
            "short": str(parsed.get("short") or "").strip(),
        },
        "raw": result["text"] if not parsed else "",
    })


# ---------------------------------------------------------------------------
# Operation: smart_compose
# ---------------------------------------------------------------------------

_SMART_COMPOSE_STYLES = {
    "formal": "Write in a polite, professional tone with complete sentences and a clear sign-off.",
    "casual": "Write in a friendly, conversational tone — warm but still clear.",
    "short":  "Write 2-3 sentences maximum — direct and to the point.",
}


def smart_compose(
    *, intent: str, subject: str = "", recipient: str = "", draft: str = "",
    style: str = "formal", lang: str = "en",
) -> dict:
    error = _text_error(
        required=("intent", "lang"), intent=intent, subject=subject, recipient=recipient,
        draft=draft, style=style, lang=lang,
    )
    if error:
        return error
    if style not in _SMART_COMPOSE_STYLES:
        return {"error": "style must be one of formal, casual, short"}
    draft = _truncate(draft, MAX_DRAFT_CHARS)
    style_hint = _SMART_COMPOSE_STYLES[style]

    system = (
        "You are an email assistant. Produce a complete email body the user "
        "can send as-is. Return JSON — no prose, no code fences — with keys:\n"
        "  body    : the email body only (no subject, no salutation labels)\n"
        "  subject : a one-line subject suggestion (may be empty if already set)\n"
        f"Style guidance: {style_hint}"
    )

    prompt = (
        f"Recipient: {recipient or '(unknown)'}\n"
        f"Subject:   {subject or '(no subject yet)'}\n"
        f"Language:  {lang}\n"
        f"User intent: {intent}\n\n"
        f"--- current draft (may be empty) ---\n{draft}\n--- end ---"
    )

    result = _ai_call(prompt, system=system, max_units=4000)
    if "error" in result:
        return result

    parsed = _safe_loads(result["text"]) or {}
    body = str(parsed.get("body") or "").strip()
    if not body:
        body = result["text"].strip()
    return _wrap(result, {
        "body": body,
        "subject": str(parsed.get("subject") or "").strip(),
        "style": style,
        "raw": result["text"] if not parsed else "",
    })


# ---------------------------------------------------------------------------
# Operation: translate
# ---------------------------------------------------------------------------

_TRANSLATE_SYSTEM = (
    "You are a translator. Translate the user's text into the target "
    "language. Preserve formatting, line breaks, lists, and inline code. "
    "Reply with the translated text only — no preamble, no commentary, "
    "no notes about ambiguity."
)


def translate(*, text: str, target: str) -> dict:
    error = _text_error(required=("text", "target"), text=text, target=target)
    if error:
        return error
    text = _truncate(text, MAX_BODY_CHARS)

    prompt = (
        f"Target language: {target}\n\n"
        f"--- source ---\n{text}\n--- end ---"
    )

    result = _ai_call(prompt, system=_TRANSLATE_SYSTEM, max_units=4000)
    if "error" in result:
        return result

    return _wrap(result, {
        "translation": result["text"].strip(),
        "target": target,
    })


# ---------------------------------------------------------------------------
# Operation: triage
# ---------------------------------------------------------------------------

_TRIAGE_SYSTEM = (
    "You are an email triage assistant. Given the sender, subject and "
    "snippet of an incoming email, classify it. Return a single JSON "
    "object — no prose, no code fences — with exactly these keys:\n"
    "  category : one of " + ", ".join(CATEGORIES) + "\n"
    "  tags     : up to 4 short lowercase tag strings (e.g. \"invoice\", "
    "\"meeting\", \"github\")\n"
    "  priority : one of low | normal | high\n"
    "  reason   : one short sentence justifying the classification\n"
    "Be conservative — only mark high priority for things that genuinely "
    "need attention today (deadlines, alerts, personal messages from real "
    "people). Newsletters and marketing are never high."
)


def triage(
    *, subject: str = "", sender: str = "", snippet: str = "", has_attachments: bool = False,
) -> dict:
    error = _text_error(subject=subject, sender=sender, snippet=snippet)
    if error:
        return error
    if not isinstance(has_attachments, bool):
        return {"error": "has_attachments must be a boolean"}
    if not any(value.strip() for value in (subject, sender, snippet)):
        return {"error": "at least one of subject, sender, snippet must be non-empty"}

    prompt = (
        f"From:        {sender or '(unknown)'}\n"
        f"Subject:     {subject or '(none)'}\n"
        f"Attachments: {'yes' if has_attachments else 'no'}\n"
        f"Snippet:     {snippet[:1000] if snippet else '(empty)'}\n"
    )

    result = _ai_call(prompt, system=_TRIAGE_SYSTEM, max_units=1000)
    if "error" in result:
        return result

    parsed = _safe_loads(result["text"]) or {}
    category = str(parsed.get("category") or "other").lower()
    if category not in CATEGORIES:
        category = "other"
    priority = str(parsed.get("priority") or "normal").lower()
    if priority not in ("low", "normal", "high"):
        priority = "normal"
    payload = {
        "category": category,
        "tags": [str(x).lower().strip() for x in (parsed.get("tags") or [])][:4],
        "priority": priority,
        "reason": str(parsed.get("reason") or "").strip(),
        "raw": result["text"] if not parsed else "",
    }
    _remember_triage(subject, sender, payload)
    return _wrap(result, payload)


# ---------------------------------------------------------------------------
# Operation: chat (mailbox Q&A)
# ---------------------------------------------------------------------------

_CHAT_SYSTEM = (
    "You are a mailbox assistant. Answer the user's question grounded in "
    "the supplied email metadata. The emails are summaries — you only see "
    "what is listed below. If the answer is not in the supplied messages, "
    "say so plainly; never invent senders, dates or facts. Cite the "
    "matching emails by their integer index (1-based) in square brackets, "
    "e.g. [2]. Keep replies concise."
)


def chat(*, question: str, context_json: str = "[]", lang: str = "en") -> dict:
    error = _text_error(
        required=("question", "context_json", "lang"), question=question, context_json=context_json, lang=lang,
    )
    if error:
        return error
    try:
        context = json.loads(context_json)
    except (json.JSONDecodeError, RecursionError):
        return {"error": "context_json is not valid JSON"}
    if not isinstance(context, list):
        return {"error": "context_json must be a JSON array"}
    for message in context:
        if not isinstance(message, dict):
            return {"error": "context_json messages must be objects"}
        if message.keys() - {"sender", "subject", "date", "snippet"}:
            return {"error": "context_json message has unknown fields"}
        error = _text_error(**message)
        if error:
            return {"error": f"context_json: {error['error']}"}

    context = context[:MAX_CONTEXT_MESSAGES]

    lines = []
    for i, m in enumerate(context, start=1):
        lines.append(
            f"[{i}] from={m.get('sender', '?')} | date={m.get('date', '?')} | "
            f"subject={m.get('subject', '(none)')}\n"
            f"     {(m.get('snippet') or '')[:400]}"
        )
    ctx_block = "\n".join(lines) if lines else "(no context supplied)"

    prompt = (
        f"Reply language: {lang}\n"
        f"Question: {question}\n\n"
        f"--- mailbox context ({len(context)} messages) ---\n"
        f"{ctx_block}\n"
        f"--- end ---"
    )

    result = _ai_call(prompt, system=_CHAT_SYSTEM, max_units=3000)
    if "error" in result:
        return result

    citations = sorted({
        int(m.group(1))
        for m in re.finditer(r"\[(\d+)\]", result["text"])
        if 1 <= int(m.group(1)) <= len(context)
    })

    return _wrap(result, {
        "answer": result["text"].strip(),
        "citations": citations,
    })


# ---------------------------------------------------------------------------
# Shared operation registry (no transport or identity construction)
# ---------------------------------------------------------------------------

HANDLERS = {
    "summarize": summarize,
    "smart_reply": smart_reply,
    "smart_compose": smart_compose,
    "translate": translate,
    "triage": triage,
    "chat": chat,
}
