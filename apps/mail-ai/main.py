"""mail-ai — AI helpers for the Thunderbird ``claw-mail-ai`` MailExtension.

This is the **agent-side** verb surface that the MailExtension talks to
over a Native Messaging port (see ``native_host.py``). Like every other
``apps/`` Python app, it routes every model call through the kernel's
AI gate via ``claw_os_sdk.ai`` — keys, budgets, safety, audit all happen there.

The MailExtension is the **user-driven** surface: the user clicks a
button, the extension hands us the email body, we hand back a summary
or a draft. Nothing in this app makes outbound network calls of its
own; the only privileged thing it does is invoke ``cos ai chat``.

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

import argparse
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
    policy.require("ai.chat.untrusted", wild=True)
    try:
        response = ai.chat(
            prompt=prompt,
            origin="external-content",
            system=system,
            max_units=max_units,
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
# Parsers
# ---------------------------------------------------------------------------

def _build_summarize_parser():
    p = argparse.ArgumentParser(prog="cos mail-ai summarize", add_help=False)
    p.add_argument("--subject", default="")
    p.add_argument("--from", dest="sender", default="")
    p.add_argument("--body", required=True)
    p.add_argument("--lang", default="en", help="output language, e.g. en, zh_CN")
    return p


def _build_smart_reply_parser():
    p = argparse.ArgumentParser(prog="cos mail-ai smart_reply", add_help=False)
    p.add_argument("--subject", default="")
    p.add_argument("--from", dest="sender", default="")
    p.add_argument("--thread", required=True, help="full thread text, oldest-first")
    p.add_argument("--my-intent", dest="intent", default="",
                   help="optional hint, e.g. 'decline politely'")
    p.add_argument("--lang", default="en")
    return p


def _build_smart_compose_parser():
    p = argparse.ArgumentParser(prog="cos mail-ai smart_compose", add_help=False)
    p.add_argument("--subject", default="")
    p.add_argument("--to", dest="recipient", default="")
    p.add_argument("--draft", default="", help="current draft text (may be empty)")
    p.add_argument("--intent", required=True, help="what the user wants to say")
    p.add_argument("--style", default="formal", choices=["formal", "casual", "short"])
    p.add_argument("--lang", default="en")
    return p


def _build_translate_parser():
    p = argparse.ArgumentParser(prog="cos mail-ai translate", add_help=False)
    p.add_argument("--text", required=True)
    p.add_argument("--target", required=True, help="target language code or name")
    return p


def _build_triage_parser():
    p = argparse.ArgumentParser(prog="cos mail-ai triage", add_help=False)
    p.add_argument("--subject", default="")
    p.add_argument("--from", dest="sender", default="")
    p.add_argument("--snippet", default="", help="short body preview")
    p.add_argument("--has-attachments", dest="has_attachments",
                   action="store_true")
    return p


def _build_chat_parser():
    p = argparse.ArgumentParser(prog="cos mail-ai chat", add_help=False)
    p.add_argument("--question", required=True)
    p.add_argument("--context-json", dest="context_json", default="[]",
                   help="JSON list of {from,subject,date,snippet} objects")
    p.add_argument("--lang", default="en")
    return p


def _remember_summary(opts, payload):
    """Push the summary of an email into the agent's memory."""
    try:
        subject = opts.subject or "(no subject)"
        sender = opts.sender or "(unknown)"
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


def _remember_triage(opts, payload):
    """Push a triage decision into the agent's memory (only when notable)."""
    try:
        priority = payload.get("priority")
        category = payload.get("category")
        # Skip low-signal triage to keep memory clean.
        if priority not in ("high",) and category in ("other", "newsletter", "marketing"):
            return
        subject = opts.subject or "(no subject)"
        sender = opts.sender or "(unknown)"
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


def cmd_summarize(args):
    parser = _build_summarize_parser()
    try:
        opts = parser.parse_args(args)
    except SystemExit:
        return {"error": "usage: summarize --body <text> [--subject S] [--from F] [--lang L]"}

    body = _truncate(_strip_quoted(opts.body), MAX_BODY_CHARS)
    if not body.strip():
        return {"error": "--body must be non-empty after quote-stripping"}

    prompt = (
        f"Email metadata:\n"
        f"  From:    {opts.sender or '(unknown)'}\n"
        f"  Subject: {opts.subject or '(no subject)'}\n"
        f"Reply language: {opts.lang}\n\n"
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
    _remember_summary(opts, payload)
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


def cmd_smart_reply(args):
    parser = _build_smart_reply_parser()
    try:
        opts = parser.parse_args(args)
    except SystemExit:
        return {"error": "usage: smart_reply --thread <text> [--subject S] [--from F] [--my-intent I] [--lang L]"}

    thread = _truncate(opts.thread, MAX_THREAD_CHARS)
    if not thread.strip():
        return {"error": "--thread must be non-empty"}

    intent_hint = (
        f"User wants the reply to: {opts.intent}\n" if opts.intent.strip() else ""
    )
    prompt = (
        f"Conversation metadata:\n"
        f"  Last sender: {opts.sender or '(unknown)'}\n"
        f"  Subject:     {opts.subject or '(no subject)'}\n"
        f"  Reply language: {opts.lang}\n"
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


def cmd_smart_compose(args):
    parser = _build_smart_compose_parser()
    try:
        opts = parser.parse_args(args)
    except SystemExit:
        return {"error": "usage: smart_compose --intent <text> [--draft D] [--subject S] [--to T] [--style formal|casual|short] [--lang L]"}

    if not opts.intent.strip():
        return {"error": "--intent must be non-empty"}

    draft = _truncate(opts.draft, MAX_DRAFT_CHARS)
    style_hint = _SMART_COMPOSE_STYLES.get(opts.style, _SMART_COMPOSE_STYLES["formal"])

    system = (
        "You are an email assistant. Produce a complete email body the user "
        "can send as-is. Return JSON — no prose, no code fences — with keys:\n"
        "  body    : the email body only (no subject, no salutation labels)\n"
        "  subject : a one-line subject suggestion (may be empty if already set)\n"
        f"Style guidance: {style_hint}"
    )

    prompt = (
        f"Recipient: {opts.recipient or '(unknown)'}\n"
        f"Subject:   {opts.subject or '(no subject yet)'}\n"
        f"Language:  {opts.lang}\n"
        f"User intent: {opts.intent}\n\n"
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
        "style": opts.style,
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


def cmd_translate(args):
    parser = _build_translate_parser()
    try:
        opts = parser.parse_args(args)
    except SystemExit:
        return {"error": "usage: translate --text <text> --target <lang>"}

    text = _truncate(opts.text, MAX_BODY_CHARS)
    if not text.strip():
        return {"error": "--text must be non-empty"}
    if not opts.target.strip():
        return {"error": "--target must be non-empty"}

    prompt = (
        f"Target language: {opts.target}\n\n"
        f"--- source ---\n{text}\n--- end ---"
    )

    result = _ai_call(prompt, system=_TRANSLATE_SYSTEM, max_units=4000)
    if "error" in result:
        return result

    return _wrap(result, {
        "translation": result["text"].strip(),
        "target": opts.target,
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


def cmd_triage(args):
    parser = _build_triage_parser()
    try:
        opts = parser.parse_args(args)
    except SystemExit:
        return {"error": "usage: triage [--subject S] [--from F] [--snippet T] [--has-attachments]"}

    if not (opts.subject or opts.sender or opts.snippet):
        return {"error": "at least one of --subject / --from / --snippet must be supplied"}

    prompt = (
        f"From:        {opts.sender or '(unknown)'}\n"
        f"Subject:     {opts.subject or '(none)'}\n"
        f"Attachments: {'yes' if opts.has_attachments else 'no'}\n"
        f"Snippet:     {opts.snippet[:1000] if opts.snippet else '(empty)'}\n"
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
    _remember_triage(opts, payload)
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


def cmd_chat(args):
    parser = _build_chat_parser()
    try:
        opts = parser.parse_args(args)
    except SystemExit:
        return {"error": "usage: chat --question <text> [--context-json <json>] [--lang L]"}

    if not opts.question.strip():
        return {"error": "--question must be non-empty"}

    try:
        context = json.loads(opts.context_json)
        if not isinstance(context, list):
            return {"error": "--context-json must be a JSON array"}
    except json.JSONDecodeError as exc:
        return {"error": f"--context-json is not valid JSON: {exc}"}

    context = context[:MAX_CONTEXT_MESSAGES]

    lines = []
    for i, m in enumerate(context, start=1):
        if not isinstance(m, dict):
            continue
        lines.append(
            f"[{i}] from={m.get('from', '?')} | date={m.get('date', '?')} | "
            f"subject={m.get('subject', '(none)')}\n"
            f"     {(m.get('snippet') or '')[:400]}"
        )
    ctx_block = "\n".join(lines) if lines else "(no context supplied)"

    prompt = (
        f"Reply language: {opts.lang}\n"
        f"Question: {opts.question}\n\n"
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
# Schema (for `cos app mail-ai __schema__`)
# ---------------------------------------------------------------------------

def _schema():
    return {
        "summarize": {
            "description": "Summarize an email body into a one-line summary, key points, and action items.",
            "parameters": [
                {"name": "--subject", "type": "string", "required": False, "description": "Email subject line for context.", "kind": "flag"},
                {"name": "--from", "type": "string", "required": False, "description": "Sender address for context.", "kind": "flag"},
                {"name": "--body", "type": "string", "required": True, "description": "Email body (HTML or plain text).", "kind": "flag"},
                {"name": "--lang", "type": "string", "required": False, "description": "Output language (default: en).", "kind": "flag", "default": "en"},
            ],
            "example": "cos app mail-ai summarize --body 'Long email here...' --subject 'Q3 plan'",
        },
        "smart_reply": {
            "description": "Generate three reply suggestions (formal / casual / short) for an email thread.",
            "parameters": [
                {"name": "--subject", "type": "string", "required": False, "description": "Thread subject.", "kind": "flag"},
                {"name": "--from", "type": "string", "required": False, "description": "Most-recent sender.", "kind": "flag"},
                {"name": "--thread", "type": "string", "required": True, "description": "Full thread text, oldest first.", "kind": "flag"},
                {"name": "--my-intent", "type": "string", "required": False, "description": "Hint on what the user wants to say.", "kind": "flag"},
                {"name": "--lang", "type": "string", "required": False, "description": "Output language.", "kind": "flag", "default": "en"},
            ],
            "example": "cos app mail-ai smart_reply --thread '...' --my-intent 'decline politely'",
        },
        "smart_compose": {
            "description": "Continue or complete a draft from a brief intent.",
            "parameters": [
                {"name": "--subject", "type": "string", "required": False, "description": "Subject line.", "kind": "flag"},
                {"name": "--to", "type": "string", "required": False, "description": "Recipient address.", "kind": "flag"},
                {"name": "--draft", "type": "string", "required": False, "description": "Current draft text (may be empty).", "kind": "flag"},
                {"name": "--intent", "type": "string", "required": True, "description": "What the user wants to say.", "kind": "flag"},
                {"name": "--style", "type": "string", "required": False, "description": "Tone: formal, casual, or short.", "kind": "flag", "default": "formal"},
                {"name": "--lang", "type": "string", "required": False, "description": "Output language.", "kind": "flag", "default": "en"},
            ],
            "example": "cos app mail-ai smart_compose --to alex@example.com --intent 'ask for the report deadline'",
        },
        "translate": {
            "description": "Translate email text into a target language.",
            "parameters": [
                {"name": "--text", "type": "string", "required": True, "description": "Source text.", "kind": "flag"},
                {"name": "--target", "type": "string", "required": True, "description": "Target language code or name.", "kind": "flag"},
            ],
            "example": "cos app mail-ai translate --text 'Bonjour' --target 'English'",
        },
        "triage": {
            "description": "Classify an incoming email into a category, suggest tags, and assign a priority.",
            "parameters": [
                {"name": "--subject", "type": "string", "required": False, "description": "Subject line.", "kind": "flag"},
                {"name": "--from", "type": "string", "required": False, "description": "Sender address.", "kind": "flag"},
                {"name": "--snippet", "type": "string", "required": False, "description": "Short body preview (first 1000 chars).", "kind": "flag"},
                {"name": "--has-attachments", "type": "boolean", "required": False, "description": "Whether the message has attachments.", "kind": "flag", "default": False},
            ],
            "example": "cos app mail-ai triage --from 'noreply@stripe.com' --subject 'Your receipt'",
        },
        "chat": {
            "description": "Answer a question grounded in supplied email metadata (chat with your mailbox).",
            "parameters": [
                {"name": "--question", "type": "string", "required": True, "description": "User question.", "kind": "flag"},
                {"name": "--context-json", "type": "string", "required": False, "description": "JSON array of {from,subject,date,snippet} objects.", "kind": "flag", "default": "[]"},
                {"name": "--lang", "type": "string", "required": False, "description": "Output language.", "kind": "flag", "default": "en"},
            ],
            "example": "cos app mail-ai chat --question 'When is the next standup?' --context-json '[{\"from\":\"alex\",\"subject\":\"Standup\",\"snippet\":\"Mondays 10am\"}]'",
        },
    }


# ---------------------------------------------------------------------------
# Entry point
# ---------------------------------------------------------------------------

HANDLERS = {
    "summarize": cmd_summarize,
    "smart_reply": cmd_smart_reply,
    "smart_compose": cmd_smart_compose,
    "translate": cmd_translate,
    "triage": cmd_triage,
    "chat": cmd_chat,
}


def run(command, args):
    """Entry point called by cos."""
    if command == "__schema__":
        return _schema()
    handler = HANDLERS.get(command)
    if handler is None:
        return {"error": f"unknown command: {command}"}
    try:
        return handler(args)
    except policy.PermissionDenied as denied:
        return {"error": str(denied), "denial": denied.denial}
    except policy.PolicyUnavailable as exc:
        return {"error": f"capability check failed: {exc}"}
