"""Document App AI presentation over the shared Files parsing library."""

import sys

from claw_files.document import cmd_convert, cmd_info, cmd_read
from claw_os_sdk import ai
from cos_runtime import memory, policy


_SUMMARIZE_SYSTEM = "Summarize the document into exactly 5 short bullet lines."
_EXPLAIN_SYSTEM = (
    "You are a senior engineer. Explain the supplied content to a curious "
    "user. Keep it under 200 words. Use plain prose, no markdown headings."
)
_REWRITE_SYSTEM = (
    "Rewrite the supplied text following the user's instruction. Return "
    "ONLY the rewritten text — no preamble, no markdown fence, no "
    "commentary. Preserve the language of the input."
)
_MAX_INPUT_CHARS = 100_000


def _read_stdin_or_file(args):
    from canonical_argv import parse_canonical_argv
    try:
        rest, options = parse_canonical_argv(args, value_flags={"file", "instruction"})
    except ValueError as error:
        return None, None, {"error": str(error)}
    file_path = options.get("file")
    instruction = options.get("instruction")
    if file_path:
        result = cmd_read([file_path])
        if isinstance(result, dict) and "error" in result:
            return None, None, result
        text = result.get("content", "") if isinstance(result, dict) else ""
        return text, file_path, instruction
    if rest:
        return " ".join(rest), None, instruction
    if not sys.stdin.isatty():
        piped = sys.stdin.read()
        if piped:
            return piped, "<stdin>", instruction
    return None, None, {"error": "no input — supply --file PATH or pipe text on stdin"}


def _ai_call(*, text, source, system, max_units):
    if not text or not text.strip():
        return {"error": "document produced no extractable text", "source": source}
    if len(text) > _MAX_INPUT_CHARS:
        text = text[:_MAX_INPUT_CHARS]
    policy.require("ai.chat.untrusted", wild=True)
    try:
        response = ai.chat(
            prompt=text, origin="external-content", system=system, max_units=max_units,
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
        "text": response.text, "source": source, "source_chars": len(text),
        "model": response.model, "provider": response.provider,
        "usage": {
            "input_tokens": response.usage.input_tokens,
            "output_tokens": response.usage.output_tokens, "units": response.usage.units,
        },
        "budget": {
            "period": response.budget.period, "units_used": response.budget.units_used,
            "units_cap": response.budget.units_cap,
        },
        "review": {
            "safety": response.review.safety,
            "prompt_redacted": response.review.prompt_redacted,
        },
    }


def cmd_summarize(args):
    text, source, extra = _read_stdin_or_file(args)
    if text is None:
        return extra if isinstance(extra, dict) else {"error": "no input"}
    result = _ai_call(text=text, source=source, system=_SUMMARIZE_SYSTEM, max_units=6000)
    if "error" in result:
        return result
    out = dict(result)
    out["summary"] = out.pop("text")
    _remember_doc_summary(source, out.get("summary", ""))
    return out


def _remember_doc_summary(source, summary):
    try:
        if not summary or not source:
            return
        first = summary.strip().splitlines()
        head = first[0] if first else ""
        if len(head) > 200:
            head = head[:197] + "..."
        memory.remember(
            source="doc", text=f"Summarised document {source}: {head}",
            kind="note", entity_id=source, tags=["doc", "summary"],
            link=f"cos doc summarize --file {source}" if source != "<stdin>" else None,
        )
    except memory.MemoryError:
        pass


def cmd_explain(args):
    text, source, extra = _read_stdin_or_file(args)
    if text is None:
        return extra if isinstance(extra, dict) else {"error": "no input"}
    return _ai_call(text=text, source=source, system=_EXPLAIN_SYSTEM, max_units=4000)


def cmd_rewrite(args):
    text, source, instruction = _read_stdin_or_file(args)
    if text is None:
        return instruction if isinstance(instruction, dict) else {"error": "no input"}
    if not instruction:
        instruction = "Improve clarity, fix grammar, keep the original meaning."
    return _ai_call(
        text=text, source=source, system=_REWRITE_SYSTEM + "\n\nInstruction: " + instruction,
        max_units=8000,
    )


def run(command, args):
    from canonical_argv import normalize_canonical_argv
    if command not in {"summarize", "explain", "rewrite"}:
        args = normalize_canonical_argv(args)
    commands = {
        "read": cmd_read, "info": cmd_info, "convert": cmd_convert,
        "summarize": cmd_summarize, "explain": cmd_explain, "rewrite": cmd_rewrite,
    }
    handler = commands.get(command)
    if not handler:
        return {"error": f"unknown command: {command}"}
    try:
        return handler(args)
    except policy.PermissionDenied as denied:
        return {"error": str(denied), "denial": denied.denial}
    except policy.PolicyUnavailable as exc:
        return {"error": f"capability check failed: {exc}"}
