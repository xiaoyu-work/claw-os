"""MCP server scaffold for Claw OS apps that expose long-lived tools.

This is the kernel-blessed way for an app to participate in an
**agent session** — the OS Agent attaches to the app's server,
discovers its tools, and calls them as part of a larger task. It's the
symmetric counterpart to `claw_os_sdk.ai`:

  * `claw_os_sdk.ai`     — the *app calls AI*; kernel mediates LLM access.
  * `claw_os_sdk.serve`  — *AI calls the app*; kernel mediates tool calls.

Both surfaces share the same caps gate (declared in `app.json`), the
same audit log (`ai.jsonl`), and the same app identity (`COS_APP_ID`).

## Wire protocol

Newline-delimited JSON-RPC 2.0 over stdin/stdout — the Model Context
Protocol stdio transport. We hand-roll the three methods the kernel
client actually invokes:

  * `initialize`              — handshake
  * `notifications/initialized` — post-handshake notification (no-op)
  * `tools/list`              — return the registered tool descriptors
  * `tools/call`              — invoke one tool, return its result
  * `ping`                    — liveness probe (optional)

Everything else returns `-32601 method not found`. Logs go to stderr;
stdout is *only* JSON-RPC frames.

## Author surface

    from claw_os_sdk.serve import App

    app = App()  # name auto-detected from $COS_APP_ID

    state = {}   # persists across calls inside this session

    @app.tool(
        "kv.get",
        summary="Get a value by key",
        args={"key": {"type": "string", "description": "Key to look up"}},
        required=["key"],
    )
    def get(key: str) -> str:
        return state.get(key, "")

    if __name__ == "__main__":
        app.serve()

Each `@app.tool` function may return:

  * a `str` — wrapped as a single MCP text content item.
  * a `dict` / `list` — serialised as compact JSON text.
  * `None` — treated as empty text.

Any uncaught exception becomes a tool error: the JSON-RPC response is
still `result` (per MCP), but with `isError: true` and the exception
message as the content. Tool functions are run synchronously; if you
need background work, keep an internal task table and return task ids.
"""

from __future__ import annotations

import io
import json
import os
import sys
import traceback
from dataclasses import dataclass, field
from typing import Any, Callable, Dict, List, Optional

from .generated import (
    JSONRPC_ERROR_INTERNAL as ERR_INTERNAL,
    JSONRPC_ERROR_INVALID_PARAMS as ERR_INVALID_PARAMS,
    JSONRPC_ERROR_INVALID_REQUEST as ERR_INVALID_REQUEST,
    JSONRPC_ERROR_METHOD_NOT_FOUND as ERR_METHOD_NOT_FOUND,
    JSONRPC_ERROR_PARSE as ERR_PARSE,
)


PROTOCOL_VERSION = "2025-06-18"
JSONRPC_VERSION = "2.0"

# Wire-version reported by the kernel; must match wire/v1/envelope.schema.json.
EXPECTED_WIRE_VERSION = 1

# Cap any single inbound JSON-RPC frame at 16 MiB. A peer (buggy debug
# client, fuzz harness, malicious caller) that sends a single 4 GB line
# without a newline would otherwise allocate the entire frame in RAM
# before json.loads even sees it. 16 MiB is comfortably above MCP's
# realistic per-frame ceiling (largest tools/list payloads are < 1 MiB).
MAX_LINE_BYTES = 16 * 1024 * 1024


@dataclass
class _Tool:
    """Internal record for one registered tool."""

    name: str
    summary: str
    input_schema: Dict[str, Any]
    handler: Callable[..., Any]


@dataclass
class App:
    """One MCP server instance. Build with [`App()`], decorate handlers
    with `@app.tool(...)`, then call `app.serve()` to enter the stdio
    loop.

    `name` defaults to `$COS_APP_ID` (set by the kernel when it spawns
    the server). `version` defaults to `"0.0.0"`; the kernel doesn't
    rely on it but it shows up in the MCP handshake.
    """

    name: Optional[str] = None
    version: str = "0.0.0"
    _tools: Dict[str, _Tool] = field(default_factory=dict)
    _initialized: bool = False

    def __post_init__(self) -> None:
        if self.name is None:
            self.name = os.environ.get("COS_APP_ID") or "unknown"

    def tool(
        self,
        name: str,
        *,
        summary: str = "",
        args: Optional[Dict[str, Dict[str, Any]]] = None,
        required: Optional[List[str]] = None,
    ) -> Callable[[Callable[..., Any]], Callable[..., Any]]:
        """Register `fn` as the handler for the tool `name`.

        `args` is a JSON-Schema-style property map keyed by argument
        name; each value is a property descriptor (`type`,
        `description`, etc.). `required` lists which keys are
        mandatory. The kernel still enforces the manifest's
        `needs[]`; this schema is purely advisory to the model.

        Tool names must be unique within one App instance.
        """

        def decorator(fn: Callable[..., Any]) -> Callable[..., Any]:
            if name in self._tools:
                raise ValueError(f"tool `{name}` registered twice")
            schema: Dict[str, Any] = {
                "type": "object",
                "properties": dict(args or {}),
            }
            if required:
                schema["required"] = list(required)
            self._tools[name] = _Tool(
                name=name,
                summary=summary or fn.__doc__ or "",
                input_schema=schema,
                handler=fn,
            )
            return fn

        return decorator

    # ---- lifecycle --------------------------------------------------

    def serve(self) -> None:
        """Block reading newline-delimited JSON-RPC frames from stdin
        and writing replies to stdout. Returns when stdin reaches EOF.
        Notifications produce no reply; everything else gets exactly
        one envelope per inbound request.

        Lines are capped at :data:`MAX_LINE_BYTES` (16 MiB). A peer
        emitting an over-sized frame gets a single parse-error response
        and we drain its bytes up to the next newline before continuing,
        so one bad frame can't poison the rest of the session.
        """
        reader = _wrap_stdin_for_bounded_lines(sys.stdin)
        while True:
            line, overflowed = _read_bounded_line(reader, MAX_LINE_BYTES)
            if not line and not overflowed:
                # EOF — stop the serve loop.
                return
            if overflowed:
                self._send_error(
                    None,
                    ERR_PARSE,
                    f"frame exceeds {MAX_LINE_BYTES} bytes; rejected",
                )
                continue
            stripped = line.strip()
            if not stripped:
                continue
            self._handle_line(stripped)

    # ---- per-frame dispatch -----------------------------------------

    def _handle_line(self, line: str) -> None:
        try:
            msg = json.loads(line)
        except json.JSONDecodeError as e:
            # Parse errors get a null-id response per JSON-RPC.
            self._send_error(None, ERR_PARSE, f"parse error: {e}")
            return
        # JSON allows scalars / arrays at the top level; the JSON-RPC
        # framing requires an object. Reject anything else before we
        # try to look up fields on it — otherwise `msg.get("id")` on a
        # list / int / string raises AttributeError and crashes the
        # whole serve() loop.
        if not isinstance(msg, dict):
            self._send_error(None, ERR_INVALID_REQUEST, "request not an object")
            return
        if msg.get("jsonrpc") != JSONRPC_VERSION:
            self._send_error(msg.get("id"), ERR_INVALID_REQUEST, "missing jsonrpc 2.0 envelope")
            return

        method = msg.get("method")
        params = msg.get("params")
        if "id" not in msg:
            self._handle_notification(method, params)
            return
        if not isinstance(method, str):
            self._send_error(
                msg.get("id"),
                ERR_INVALID_REQUEST,
                "request method must be a string",
            )
            return

        msg_id = msg["id"]
        valid_id = (
            msg_id is None
            or isinstance(msg_id, str)
            or (
                isinstance(msg_id, int)
                and not isinstance(msg_id, bool)
                and -(2**63) <= msg_id < 2**63
            )
        )
        if not valid_id:
            self._send_error(
                None,
                ERR_INVALID_REQUEST,
                "request id must be a string, number, or null",
            )
            return

        try:
            result = self._handle_request(method, params)
        except _RpcError as e:
            self._send_error(msg_id, e.code, e.message, data=e.data)
            return
        except Exception as e:  # noqa: BLE001 — last-resort safety net
            self._log_stderr("internal", repr(e), traceback.format_exc())
            self._send_error(msg_id, ERR_INTERNAL, f"internal error: {e}")
            return
        self._send_result(msg_id, result)

    def _handle_notification(self, method: Optional[str], params: Any) -> None:
        if method == "notifications/initialized":
            self._initialized = True
        # Silently ignore anything else — notifications must never
        # produce a response and we don't want to crash on unknown
        # ones.

    def _handle_request(self, method: Optional[str], params: Any) -> Any:
        if method == "initialize":
            if not isinstance(params, dict):
                raise _RpcError(ERR_INVALID_PARAMS, "initialize params must be an object")
            return self._on_initialize(params)
        if method == "ping":
            return {}
        if method == "tools/list":
            return self._on_list_tools()
        if method == "tools/call":
            if not isinstance(params, dict):
                raise _RpcError(ERR_INVALID_PARAMS, "tools/call params must be an object")
            return self._on_call_tool(params)
        raise _RpcError(ERR_METHOD_NOT_FOUND, f"unknown method `{method}`")

    # ---- method handlers --------------------------------------------

    def _on_initialize(self, params: Dict[str, Any]) -> Dict[str, Any]:
        if not isinstance(params.get("protocolVersion"), str):
            raise _RpcError(ERR_INVALID_PARAMS, "missing `protocolVersion`")
        if not isinstance(params.get("capabilities"), dict):
            raise _RpcError(ERR_INVALID_PARAMS, "missing `capabilities`")
        client_info = params.get("clientInfo")
        if (
            not isinstance(client_info, dict)
            or not isinstance(client_info.get("name"), str)
            or not isinstance(client_info.get("version"), str)
        ):
            raise _RpcError(ERR_INVALID_PARAMS, "missing or invalid `clientInfo`")
        return {
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": {"tools": {"listChanged": False}},
            "serverInfo": {"name": self.name, "version": self.version},
        }

    def _on_list_tools(self) -> Dict[str, Any]:
        return {
            "tools": [
                {
                    "name": t.name,
                    "description": t.summary,
                    "inputSchema": t.input_schema,
                }
                for t in self._tools.values()
            ]
        }

    def _on_call_tool(self, params: Dict[str, Any]) -> Dict[str, Any]:
        name = params.get("name")
        if not isinstance(name, str):
            raise _RpcError(ERR_INVALID_PARAMS, "missing `name`")
        arguments = params.get("arguments")
        if arguments is None:
            arguments = {}
        if not isinstance(arguments, dict):
            raise _RpcError(ERR_INVALID_PARAMS, "`arguments` must be an object")
        tool = self._tools.get(name)
        if tool is None:
            raise _RpcError(ERR_INVALID_PARAMS, f"unknown tool `{name}`")
        try:
            value = tool.handler(**arguments)
        except TypeError as e:
            # Wrong kwargs: report as a tool error rather than transport
            # error so the agent can see what shape was expected.
            return _text_result(f"bad arguments for `{name}`: {e}", is_error=True)
        except Exception as e:  # noqa: BLE001 — convert to tool error
            self._log_stderr("tool", name, repr(e), traceback.format_exc())
            return _text_result(f"{type(e).__name__}: {e}", is_error=True)
        return _text_result(_stringify(value), is_error=False)

    # ---- wire helpers -----------------------------------------------

    def _send_result(self, msg_id: Any, result: Any) -> None:
        self._write({"jsonrpc": JSONRPC_VERSION, "id": msg_id, "result": result})

    def _send_error(
        self,
        msg_id: Any,
        code: int,
        message: str,
        data: Optional[Any] = None,
    ) -> None:
        err: Dict[str, Any] = {"code": code, "message": message}
        if data is not None:
            err["data"] = data
        self._write({"jsonrpc": JSONRPC_VERSION, "id": msg_id, "error": err})

    @staticmethod
    def _write(frame: Dict[str, Any]) -> None:
        sys.stdout.write(json.dumps(frame, separators=(",", ":"), ensure_ascii=False))
        sys.stdout.write("\n")
        sys.stdout.flush()

    @staticmethod
    def _log_stderr(*parts: str) -> None:
        sys.stderr.write(" ".join(parts) + "\n")


class _RpcError(Exception):
    """Internal: signals a JSON-RPC-level failure (vs. a tool error,
    which travels inside a successful response with `isError: true`)."""

    def __init__(self, code: int, message: str, data: Optional[Any] = None) -> None:
        super().__init__(message)
        self.code = code
        self.message = message
        self.data = data


def _stringify(value: Any) -> str:
    if value is None:
        return ""
    if isinstance(value, str):
        return value
    try:
        return json.dumps(value, ensure_ascii=False, separators=(",", ":"))
    except (TypeError, ValueError):
        return repr(value)


def _text_result(text: str, *, is_error: bool) -> Dict[str, Any]:
    return {
        "content": [{"type": "text", "text": text}],
        "isError": is_error,
    }


def _wrap_stdin_for_bounded_lines(stream: Any) -> Any:
    """Return an object that supports ``readline()``. We prefer the raw
    buffered binary stream behind :data:`sys.stdin` so we can enforce a
    byte-level cap (a 4 GB UTF-8 sequence is still 4 GB whether or not
    the text layer would have decoded it).

    Tests that swap in an :class:`io.StringIO` are also supported — we
    fall through to the original stream in that case.
    """
    buffered = getattr(stream, "buffer", None)
    if buffered is not None:
        return buffered
    return stream


def _read_bounded_line(reader: Any, limit: int) -> tuple[str, bool]:
    """Read one newline-terminated line from ``reader``, capped at
    ``limit`` bytes.

    Returns ``(text, overflowed)``:

      * ``("", False)`` at EOF.
      * ``(text, False)`` for any line within the cap (newline stripped).
      * ``("", True)`` if the line exceeds ``limit``; in that case we
        drain bytes up to the next newline (still bounded) so a single
        oversize frame doesn't poison the rest of the stream.

    Works for both binary buffered streams (production) and text
    streams (tests inject :class:`io.StringIO`).
    """
    # Binary path — preferred in production. The buffer attribute on
    # sys.stdin is typically a BufferedReader.
    if hasattr(reader, "read1") or isinstance(reader, (io.RawIOBase, io.BufferedIOBase)):
        chunks: list[bytes] = []
        total = 0
        while True:
            byte = reader.read(1)
            if not byte:
                if chunks:
                    return b"".join(chunks).decode("utf-8", errors="replace"), False
                return "", False
            if byte == b"\n":
                return b"".join(chunks).decode("utf-8", errors="replace"), False
            total += 1
            if total > limit:
                # Drain the rest of this line, still bounded, so we
                # land cleanly at the next frame boundary.
                drained = 0
                while drained < limit:
                    b = reader.read(1)
                    if not b or b == b"\n":
                        break
                    drained += 1
                return "", True
            chunks.append(byte)
    # Text-stream fallback — used by tests.
    text_chunks: list[str] = []
    total = 0
    while True:
        ch = reader.read(1)
        if not ch:
            if text_chunks:
                return "".join(text_chunks), False
            return "", False
        if ch == "\n":
            return "".join(text_chunks), False
        total += len(ch.encode("utf-8", errors="replace"))
        if total > limit:
            drained = 0
            while drained < limit:
                c = reader.read(1)
                if not c or c == "\n":
                    break
                drained += len(c.encode("utf-8", errors="replace"))
            return "", True
        text_chunks.append(ch)
