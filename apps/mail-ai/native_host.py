#!/usr/bin/env python3
"""Native Messaging transport for the shared Mail AI business functions.

The root-owned claw-mail-ai-host launcher verifies Thunderbird and registers
the App session before launching this script with Python's isolated mode.
This transport neither creates an MCP caller nor invokes another App.
"""

from __future__ import annotations

import inspect
import json
from pathlib import Path
import struct
import sys


MAX_FRAME = 8 * 1024 * 1024
EOF = object()
_HERE = Path(__file__).resolve().parent


def _bootstrap_sdk_path() -> None:
    # -I ignores PYTHONPATH and the script directory. Installed execution uses
    # only the canonical vendor package and explicit shared SDK/runtime tree.
    if _HERE == Path("/usr/lib/cos/apps/mail-ai"):
        paths = [Path("/usr/lib/cos/python")]
    else:
        root = _HERE.parent.parent
        paths = [
            root / "claw-os-sdk" / "python" / "src",
            root / "cos-runtime" / "python" / "src",
        ]
        if not all(path.is_dir() for path in paths):
            raise ImportError("Mail AI requires the canonical installation or a source checkout")
    sys.path[:0] = [str(_HERE), *(str(path) for path in paths)]


_bootstrap_sdk_path()

import main as mail_ai  # noqa: E402


class FrameError(ValueError):
    """A malformed frame cannot safely be resumed."""


def _reject_constant(value: str) -> None:
    raise ValueError("non-finite JSON number")


def _read_frame(stream):
    header = stream.read(4)
    if header == b"":
        return EOF
    if len(header) != 4:
        raise FrameError("truncated native frame header (expected 4 bytes)")
    (length,) = struct.unpack("<I", header)
    if not 0 < length <= MAX_FRAME:
        raise FrameError(f"native frame length must be between 1 and {MAX_FRAME} bytes")
    body = stream.read(length)
    if len(body) != length:
        raise FrameError("truncated native frame body")
    try:
        text = body.decode("utf-8")
    except UnicodeDecodeError as exc:
        raise FrameError("native frame body is not valid UTF-8") from exc
    try:
        return json.loads(text, parse_constant=_reject_constant)
    except (ValueError, RecursionError) as exc:
        raise FrameError("native frame body is not valid JSON") from exc


def _write_frame(stream, payload: dict) -> None:
    body = json.dumps(payload, ensure_ascii=False, allow_nan=False).encode("utf-8")
    if len(body) > MAX_FRAME:
        raise FrameError(f"native response exceeds {MAX_FRAME} bytes")
    stream.write(struct.pack("<I", len(body)))
    stream.write(body)
    stream.flush()


def _dispatch(request) -> dict:
    if not isinstance(request, dict):
        return {"error": "request must be an object"}
    if request.keys() != {"id", "verb", "args"}:
        return {"error": "request must contain exactly id, verb, args"}
    if not isinstance(request["id"], str) or not request["id"].strip():
        return {"error": "id must be a non-empty string"}
    verb = request["verb"]
    if not isinstance(verb, str) or not verb.strip():
        return {"error": "verb must be a non-empty string"}
    args = request["args"]
    if not isinstance(args, dict):
        return {"error": "args must be an object"}
    handler = mail_ai.HANDLERS.get(verb)
    if handler is None:
        return {"error": "unknown Mail AI verb"}
    try:
        inspect.signature(handler).bind(**args)
    except TypeError:
        return {"error": f"invalid arguments for {verb}: check required and unknown fields"}
    return handler(**args)


def _reply(request) -> dict:
    rid = request.get("id", "") if isinstance(request, dict) else ""
    if not isinstance(rid, str):
        rid = ""
    result = _dispatch(request)
    if "error" in result:
        return {"id": rid, "ok": False, "error": result["error"], "detail": result}
    return {"id": rid, "ok": True, "result": result}


def main() -> int:
    while True:
        try:
            request = _read_frame(sys.stdin.buffer)
        except FrameError as exc:
            print(f"Mail AI native protocol error: {exc}", file=sys.stderr)
            return 1
        if request is EOF:
            return 0
        try:
            reply = _reply(request)
        except Exception:  # Last-resort request boundary; never expose email bodies or tracebacks.
            rid = request.get("id", "") if isinstance(request, dict) else ""
            reply = {
                "id": rid if isinstance(rid, str) else "",
                "ok": False,
                "error": "Mail AI request failed unexpectedly; retry or check host diagnostics",
            }
            print("Mail AI native request failed unexpectedly", file=sys.stderr)
        try:
            _write_frame(sys.stdout.buffer, reply)
        except (FrameError, OSError) as exc:
            print(f"Mail AI native output error: {type(exc).__name__}", file=sys.stderr)
            return 1


if __name__ == "__main__":
    if sys.argv[1:] == ["--probe"]:
        print(json.dumps({"ok": True, "verbs": sorted(mail_ai.HANDLERS)}))
        sys.exit(0)
    if sys.argv[1:]:
        print("usage: native_host.py [--probe]", file=sys.stderr)
        sys.exit(2)
    sys.exit(main())
