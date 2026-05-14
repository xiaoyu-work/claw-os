"""MCP server scaffold for Claw OS apps that expose long-lived tools.

This is the kernel-blessed way for an app to participate in an
**agent session** — the OS Agent attaches to the app's server,
discovers its tools, and calls them as part of a larger task. It's the
symmetric counterpart to `_lib.ai`:

  * `_lib.ai`     — the *app calls AI*; kernel mediates LLM access.
  * `_lib.serve`  — *AI calls the app*; kernel mediates tool calls.

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

    from _lib.serve import App

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

import json
import os
import sys
import traceback
from dataclasses import dataclass, field
from typing import Any, Callable, Dict, List, Optional


PROTOCOL_VERSION = "2025-06-18"
JSONRPC_VERSION = "2.0"

ERR_PARSE = -32700
ERR_INVALID_REQUEST = -32600
ERR_METHOD_NOT_FOUND = -32601
ERR_INVALID_PARAMS = -32602
ERR_INTERNAL = -32603


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
        """
        for line in sys.stdin:
            line = line.strip()
            if not line:
                continue
            self._handle_line(line)

    # ---- per-frame dispatch -----------------------------------------

    def _handle_line(self, line: str) -> None:
        try:
            msg = json.loads(line)
        except json.JSONDecodeError as e:
            # Parse errors get a null-id response per JSON-RPC.
            self._send_error(None, ERR_PARSE, f"parse error: {e}")
            return
        if not isinstance(msg, dict) or msg.get("jsonrpc") != JSONRPC_VERSION:
            self._send_error(msg.get("id"), ERR_INVALID_REQUEST, "missing jsonrpc 2.0 envelope")
            return

        method = msg.get("method")
        params = msg.get("params")
        msg_id = msg.get("id")

        if msg_id is None:
            self._handle_notification(method, params)
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
            return self._on_initialize(params or {})
        if method == "ping":
            return {}
        if method == "tools/list":
            return self._on_list_tools()
        if method == "tools/call":
            return self._on_call_tool(params or {})
        raise _RpcError(ERR_METHOD_NOT_FOUND, f"unknown method `{method}`")

    # ---- method handlers --------------------------------------------

    def _on_initialize(self, _params: Dict[str, Any]) -> Dict[str, Any]:
        # We accept any client protocol version and report ours.
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
        arguments = params.get("arguments") or {}
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
