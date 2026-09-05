"""Manifest-bound MCP server runtime for Claw OS Apps.

This is the public way for an App to expose a private Claw service.
The Gateway authenticates the non-App caller, derives exact
capabilities from the verified manifest, and then forwards the call to
this runtime over the App Host's private stdio transport.

``App.from_manifest()`` makes ``app.json.mcp.tools`` authoritative.
Decorators bind implementation functions by name; they do not repeat
tool descriptions or schemas in code:

    from claw_os_sdk.mcp import App, current_context

    app = App.from_manifest()

    @app.tool("kv.get")
    def get(key: str) -> str:
        current_context().raise_if_cancelled()
        return state.get(key, "")

    app.serve()

The runtime supports MCP initialize, tools/list, tools/call, ping,
progress, and cooperative cancellation. Tool exceptions remain MCP
tool errors rather than transport errors. Stdout is reserved for
JSON-RPC frames; diagnostics go to stderr.
"""

from __future__ import annotations

import contextvars
import copy
import io
import inspect
import json
import math
import os
import pathlib
import re
import sys
import threading
import time
import traceback
from concurrent.futures import ThreadPoolExecutor
from dataclasses import dataclass, field
from decimal import Decimal
from typing import Any, Callable, Dict, List, Optional

from .generated import (
    JSONRPC_ERROR_INTERNAL as ERR_INTERNAL,
    JSONRPC_ERROR_INVALID_PARAMS as ERR_INVALID_PARAMS,
    JSONRPC_ERROR_INVALID_REQUEST as ERR_INVALID_REQUEST,
    JSONRPC_ERROR_METHOD_NOT_FOUND as ERR_METHOD_NOT_FOUND,
    JSONRPC_ERROR_PARSE as ERR_PARSE,
    WireDecimal,
    WireDecodeError,
    decode_wire_json,
    encode_wire_json,
    validate_mcp_call_context,
    wire_integer_to_int,
)

__all__ = [
    "App",
    "CALL_CONTEXT_META_KEY",
    "CallCancelled",
    "CallContext",
    "ERR_INTERNAL",
    "ERR_INVALID_PARAMS",
    "ERR_INVALID_REQUEST",
    "ERR_METHOD_NOT_FOUND",
    "ERR_PARSE",
    "ERR_SERVER_BUSY",
    "JSONRPC_VERSION",
    "MAX_LINE_BYTES",
    "ManifestError",
    "McpPrincipal",
    "PROTOCOL_VERSION",
    "ToolResult",
    "current_context",
]

PROTOCOL_VERSION = "2025-06-18"
JSONRPC_VERSION = "2.0"
CALL_CONTEXT_META_KEY = "claw-os.dev/call-context"

# Wire-version reported by the kernel; must match wire/v1/envelope.schema.json.
EXPECTED_WIRE_VERSION = 1

# Cap any single inbound JSON-RPC frame at 16 MiB. A peer (buggy debug
# client, fuzz harness, malicious caller) that sends a single 4 GB line
# without a newline would otherwise allocate the entire frame in RAM
# before json.loads even sees it. 16 MiB is comfortably above MCP's
# realistic per-frame ceiling (largest tools/list payloads are < 1 MiB).
MAX_LINE_BYTES = 16 * 1024 * 1024
MAX_MANIFEST_BYTES = 1024 * 1024
MAX_CONCURRENT_CALLS = 1
MAX_PENDING_CALLS = 64
EOF_CANCELLATION_GRACE_SECONDS = 0.05
ERR_SERVER_BUSY = -32000
_APP_ID_PATTERN = re.compile(r"^[a-z][a-z0-9_-]*$")
_TOOL_NAME_PATTERN = re.compile(r"^[a-z][a-z0-9._-]*$")


class ManifestError(ValueError):
    """The verified App manifest cannot construct an MCP runtime."""


class CallCancelled(RuntimeError):
    """Raised by :meth:`CallContext.raise_if_cancelled`."""


class _ToolArgumentError(ValueError):
    pass


@dataclass(frozen=True)
class McpPrincipal:
    """Authenticated workload identity supplied by the Claw Gateway."""

    kind: str
    id: str
    owner_uid: int


@dataclass
class _CallState:
    cancelled: threading.Event = field(default_factory=threading.Event)


@dataclass(frozen=True)
class CallContext:
    """Gateway-authenticated identity and lineage for one tool call.

    Apps may use this for state partitioning and diagnostics. It is not
    an authority token: privileged work still goes through ``clawd``
    using the transient target grant held by the App Host.
    """

    call_id: str
    trace_id: str
    caller: McpPrincipal
    deadline_unix_ms: Optional[int] = None
    session_id: Optional[str] = None
    task_id: Optional[str] = None
    _cancelled: threading.Event = field(
        default_factory=threading.Event,
        repr=False,
        compare=False,
    )
    _progress_token: Any = field(default=None, repr=False, compare=False)
    _emit_notification: Optional[Callable[[str, Dict[str, Any]], None]] = field(
        default=None,
        repr=False,
        compare=False,
    )

    @property
    def progress_requested(self) -> bool:
        return self._progress_token is not None

    @property
    def cancelled(self) -> bool:
        if self._cancelled.is_set():
            return True
        return (
            self.deadline_unix_ms is not None
            and int(time.time() * 1000) >= self.deadline_unix_ms
        )

    def raise_if_cancelled(self) -> None:
        if self.cancelled:
            raise CallCancelled(f"call `{self.call_id}` was cancelled")

    def report_progress(
        self,
        progress: int | float,
        *,
        total: Optional[int | float] = None,
        message: Optional[str] = None,
    ) -> None:
        """Emit an MCP progress notification for this call."""

        self.raise_if_cancelled()
        if self._progress_token is None or self._emit_notification is None:
            return
        _validate_progress_number(progress, "progress")
        params: Dict[str, Any] = {
            "progressToken": self._progress_token,
            "progress": progress,
        }
        if total is not None:
            _validate_progress_number(total, "total")
            params["total"] = total
        if message is not None:
            if not isinstance(message, str):
                raise TypeError("progress message must be a string")
            params["message"] = message
        self._emit_notification("notifications/progress", params)


@dataclass(frozen=True)
class ToolResult:
    """Explicit MCP tool result with optional structured content."""

    content: List[Dict[str, Any]]
    is_error: bool = False
    structured_content: Any = None

    @classmethod
    def text(cls, text: str) -> "ToolResult":
        return cls(content=[{"type": "text", "text": text}])

    @classmethod
    def error(cls, message: str) -> "ToolResult":
        return cls(
            content=[{"type": "text", "text": message}],
            is_error=True,
        )

    @classmethod
    def structured(
        cls,
        value: Dict[str, Any],
        *,
        text: Optional[str] = None,
    ) -> "ToolResult":
        if not isinstance(value, dict):
            raise TypeError("structured MCP content must be an object")
        rendered = _stringify(value) if text is None else text
        return cls(
            content=[{"type": "text", "text": rendered}],
            structured_content=value,
        )


_CURRENT_CONTEXT: contextvars.ContextVar[Optional[CallContext]] = (
    contextvars.ContextVar("claw_os_mcp_call_context", default=None)
)


def current_context() -> CallContext:
    """Return the context for the currently executing tool handler."""

    context = _CURRENT_CONTEXT.get()
    if context is None:
        raise RuntimeError("no MCP tool call is active")
    return context


def _has_unpaired_surrogate(value: str) -> bool:
    index = 0
    while index < len(value):
        code = ord(value[index])
        if 0xD800 <= code <= 0xDBFF:
            if index + 1 >= len(value) or not (
                0xDC00 <= ord(value[index + 1]) <= 0xDFFF
            ):
                return True
            index += 2
            continue
        if 0xDC00 <= code <= 0xDFFF:
            return True
        index += 1
    return False


@dataclass
class _Tool:
    """Internal record for one registered tool."""

    name: str
    summary: str
    input_schema: Dict[str, Any]
    manifest_args: List[Dict[str, Any]]
    handler: Optional[Callable[..., Any]] = None


class App:
    """A manifest-bound App MCP server.

    Construct with :meth:`from_manifest`, bind each declared tool with
    ``@app.tool(name)``, then call :meth:`serve`. Direct construction is
    rejected because code-authored tool schemas and unauthenticated calls
    are not part of the Claw App service contract.
    """

    name: str
    version: str
    _tools: Dict[str, _Tool]
    _initialized: bool
    _write_lock: threading.Lock
    _active_lock: threading.Lock
    _active_calls: Dict[str, _CallState]
    _pending_slots: threading.BoundedSemaphore
    _executor: Optional[ThreadPoolExecutor]

    def __init__(self, *args: Any, **kwargs: Any) -> None:
        del args, kwargs
        raise TypeError("App must be created with App.from_manifest()")

    @classmethod
    def from_manifest(
        cls,
        path: Optional[str | os.PathLike[str]] = None,
    ) -> "App":
        """Build from the verified ``app.json.mcp`` declaration.

        The App Host sets ``COS_APP_MANIFEST`` to its immutable package
        snapshot. A direct development run may pass a path explicitly
        or use ``./app.json``.
        """

        manifest_path = pathlib.Path(
            path or os.environ.get("COS_APP_MANIFEST") or "app.json"
        )
        name, version, tools = _load_manifest_service(manifest_path)
        app = object.__new__(cls)
        app.name = name
        app.version = version
        app._tools = {tool.name: tool for tool in tools}
        app._initialized = False
        app._write_lock = threading.Lock()
        app._active_lock = threading.Lock()
        app._active_calls = {}
        app._pending_slots = threading.BoundedSemaphore(MAX_PENDING_CALLS)
        app._executor = None
        return app

    def tool(
        self,
        name: str,
    ) -> Callable[[Callable[..., Any]], Callable[..., Any]]:
        """Bind a handler to one manifest-declared tool name."""

        def decorator(fn: Callable[..., Any]) -> Callable[..., Any]:
            tool = self._tools.get(name)
            if tool is None:
                raise ValueError(f"tool `{name}` is not declared in app.json.mcp")
            if tool.handler is not None:
                raise ValueError(f"tool `{name}` registered twice")
            tool.handler = fn
            return fn

        return decorator

    def _validate_bindings(self) -> None:
        missing = [name for name, tool in self._tools.items() if tool.handler is None]
        if missing:
            raise ManifestError(
                "missing handlers for manifest tools: " + ", ".join(missing)
            )

    # ---- lifecycle --------------------------------------------------

    def serve(self) -> None:
        """Block reading newline-delimited JSON-RPC frames from stdin
        and writing replies to stdout. Returns when stdin reaches EOF.
        Notifications produce no reply. Requests get one response unless
        the caller cancels them, in which case the obsolete response is
        suppressed.

        Lines are capped at :data:`MAX_LINE_BYTES` (16 MiB). A peer
        emitting an over-sized frame gets a single parse-error response
        and we drain its bytes up to the next newline before continuing,
        so one bad frame can't poison the rest of the session.
        """
        self._validate_bindings()
        reader = _wrap_stdin_for_bounded_lines(sys.stdin)
        executor = ThreadPoolExecutor(
            max_workers=MAX_CONCURRENT_CALLS,
            thread_name_prefix=f"claw-mcp-{self.name}",
        )
        self._executor = executor
        try:
            while True:
                try:
                    line, overflowed = _read_bounded_line(
                        reader,
                        MAX_LINE_BYTES,
                    )
                except UnicodeDecodeError as error:
                    self._send_error(
                        None,
                        ERR_PARSE,
                        f"frame is not valid UTF-8: {error}",
                    )
                    continue
                if not line and not overflowed:
                    break
                if overflowed:
                    self._send_error(
                        None,
                        ERR_PARSE,
                        f"frame exceeds {MAX_LINE_BYTES} bytes; rejected",
                    )
                    continue
                stripped = line.strip()
                if stripped:
                    self._handle_line(stripped)
        finally:
            self._cancel_calls_after_eof_grace()
            executor.shutdown(wait=True)
            self._executor = None

    def _cancel_calls_after_eof_grace(self) -> None:
        deadline = time.monotonic() + EOF_CANCELLATION_GRACE_SECONDS
        while time.monotonic() < deadline:
            with self._active_lock:
                if not self._active_calls:
                    return
            time.sleep(0.001)
        with self._active_lock:
            states = list(self._active_calls.values())
        for state in states:
            state.cancelled.set()

    # ---- per-frame dispatch -----------------------------------------

    def _handle_line(self, line: str) -> None:
        try:
            msg = decode_wire_json(line)
        except (json.JSONDecodeError, ValueError, RecursionError) as e:
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

        has_id = "id" in msg
        raw_id = msg.get("id")
        if isinstance(raw_id, str) and _has_unpaired_surrogate(raw_id):
            self._send_error(
                None,
                ERR_PARSE,
                "request id contains an unpaired Unicode surrogate",
            )
            return
        valid_id = (
            raw_id is None
            or isinstance(raw_id, str)
            or (
                isinstance(raw_id, (int, float, Decimal, WireDecimal))
                and not isinstance(raw_id, bool)
                and (
                    (not isinstance(raw_id, float) or math.isfinite(raw_id))
                    and (not isinstance(raw_id, Decimal) or raw_id.is_finite())
                )
            )
        )
        msg_id = raw_id if valid_id else None
        if has_id and not valid_id:
            self._send_error(
                None,
                ERR_INVALID_REQUEST,
                "request id must be a string, integer, or null",
            )
            return

        if msg.get("jsonrpc") != JSONRPC_VERSION:
            self._send_error(msg_id, ERR_INVALID_REQUEST, "missing jsonrpc 2.0 envelope")
            return

        method = msg.get("method")
        params = msg.get("params")
        if not isinstance(method, str):
            self._send_error(
                msg_id,
                ERR_INVALID_REQUEST,
                "request method must be a string",
            )
            return
        if (
            not has_id
            and "params" in msg
            and not isinstance(params, (dict, list))
        ):
            self._send_error(
                msg_id,
                ERR_INVALID_REQUEST,
                "request params must be an object or array",
            )
            return
        if not has_id:
            self._handle_notification(method, params)
            return

        if method == "tools/call" and self._executor is not None:
            self._dispatch_tool_call(msg_id, params, "params" in msg)
            return
        self._handle_request_and_send(
            msg_id,
            method,
            params,
            "params" in msg,
        )

    def _handle_request_and_send(
        self,
        msg_id: Any,
        method: str,
        params: Any,
        params_present: bool,
        state: Optional[_CallState] = None,
    ) -> None:
        try:
            result = self._handle_request(
                method,
                params,
                params_present,
                msg_id,
                state,
            )
            if state is not None and state.cancelled.is_set():
                return
            self._send_result(msg_id, result)
        except _RpcError as e:
            if state is not None and state.cancelled.is_set():
                return
            self._send_error(msg_id, e.code, e.message, data=e.data)
            return
        except Exception as e:  # noqa: BLE001 — last-resort safety net
            if state is not None and state.cancelled.is_set():
                return
            self._log_stderr("internal", repr(e), traceback.format_exc())
            self._send_error(msg_id, ERR_INTERNAL, f"internal error: {e}")
            return

    def _dispatch_tool_call(
        self,
        msg_id: Any,
        params: Any,
        params_present: bool,
    ) -> None:
        if not self._pending_slots.acquire(blocking=False):
            self._send_error(
                msg_id,
                ERR_SERVER_BUSY,
                "too many pending MCP tool calls",
            )
            return

        key = _request_key(msg_id)
        state = _CallState()
        with self._active_lock:
            if key in self._active_calls:
                self._pending_slots.release()
                self._send_error(
                    msg_id,
                    ERR_INVALID_REQUEST,
                    "duplicate active request id",
                )
                return
            self._active_calls[key] = state

        executor = self._executor
        if executor is None:
            with self._active_lock:
                self._active_calls.pop(key, None)
            self._pending_slots.release()
            self._send_error(msg_id, ERR_INTERNAL, "MCP call executor is unavailable")
            return

        def run() -> None:
            try:
                self._handle_request_and_send(
                    msg_id,
                    "tools/call",
                    params,
                    params_present,
                    state,
                )
            finally:
                with self._active_lock:
                    self._active_calls.pop(key, None)
                self._pending_slots.release()

        try:
            executor.submit(run)
        except RuntimeError as error:
            with self._active_lock:
                self._active_calls.pop(key, None)
            self._pending_slots.release()
            self._send_error(
                msg_id,
                ERR_INTERNAL,
                f"cannot schedule MCP tool call: {error}",
            )

    def _handle_notification(self, method: Optional[str], params: Any) -> None:
        if method == "notifications/initialized":
            self._initialized = True
            return
        if method == "notifications/cancelled":
            if not isinstance(params, dict) or "requestId" not in params:
                self._log_stderr("protocol", "invalid cancellation notification")
                return
            try:
                key = _request_key(params["requestId"])
            except (TypeError, ValueError):
                self._log_stderr("protocol", "invalid cancellation request id")
                return
            with self._active_lock:
                state = self._active_calls.get(key)
            if state is not None:
                state.cancelled.set()

    def _handle_request(
        self,
        method: Optional[str],
        params: Any,
        params_present: bool,
        msg_id: Any = None,
        state: Optional[_CallState] = None,
    ) -> Any:
        if method == "initialize":
            if not isinstance(params, dict):
                raise _RpcError(ERR_INVALID_PARAMS, "initialize params must be an object")
            return self._on_initialize(params)
        if method == "ping":
            if params_present and not isinstance(params, dict):
                raise _RpcError(ERR_INVALID_PARAMS, "ping params must be an object")
            return {}
        if method == "tools/list":
            if params_present:
                if not isinstance(params, dict):
                    raise _RpcError(ERR_INVALID_PARAMS, "tools/list params must be an object")
                if "cursor" in params and not isinstance(params["cursor"], str):
                    raise _RpcError(
                        ERR_INVALID_PARAMS,
                        "tools/list cursor must be a string",
                    )
            return self._on_list_tools()
        if method == "tools/call":
            if not isinstance(params, dict):
                raise _RpcError(ERR_INVALID_PARAMS, "tools/call params must be an object")
            return self._on_call_tool(
                params,
                msg_id,
                state or _CallState(),
            )
        raise _RpcError(ERR_METHOD_NOT_FOUND, f"unknown method `{method}`")

    # ---- method handlers --------------------------------------------

    def _on_initialize(self, params: Dict[str, Any]) -> Dict[str, Any]:
        if not isinstance(params.get("protocolVersion"), str):
            raise _RpcError(ERR_INVALID_PARAMS, "missing `protocolVersion`")
        capabilities = params.get("capabilities")
        if not isinstance(capabilities, dict):
            raise _RpcError(ERR_INVALID_PARAMS, "missing `capabilities`")
        for name in ("experimental", "sampling", "elicitation"):
            if name in capabilities and not isinstance(capabilities[name], dict):
                raise _RpcError(ERR_INVALID_PARAMS, f"`capabilities.{name}` must be an object")
        if "roots" in capabilities:
            roots = capabilities["roots"]
            if not isinstance(roots, dict):
                raise _RpcError(ERR_INVALID_PARAMS, "`capabilities.roots` must be an object")
            if (
                "listChanged" in roots
                and not isinstance(roots["listChanged"], bool)
            ):
                raise _RpcError(
                    ERR_INVALID_PARAMS,
                    "`capabilities.roots.listChanged` must be a boolean",
                )
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

    def _on_call_tool(
        self,
        params: Dict[str, Any],
        msg_id: Any,
        state: _CallState,
    ) -> Dict[str, Any]:
        name = params.get("name")
        if not isinstance(name, str):
            raise _RpcError(ERR_INVALID_PARAMS, "missing `name`")
        if "arguments" not in params:
            arguments = {}
        else:
            arguments = params["arguments"]
        if not isinstance(arguments, dict):
            raise _RpcError(ERR_INVALID_PARAMS, "`arguments` must be an object")
        tool = self._tools.get(name)
        if tool is None:
            raise _RpcError(ERR_INVALID_PARAMS, f"unknown tool `{name}`")
        handler = tool.handler
        if handler is None:
            raise _RpcError(ERR_INTERNAL, f"tool `{name}` has no handler")
        context = self._call_context(params, state)
        try:
            arguments = _resolve_manifest_arguments(tool, arguments)
        except _ToolArgumentError as error:
            return _text_result(
                f"bad arguments for `{name}`: {error}",
                is_error=True,
            )
        try:
            inspect.signature(handler).bind(**arguments)
        except TypeError as e:
            return _text_result(f"bad arguments for `{name}`: {e}", is_error=True)
        token = _CURRENT_CONTEXT.set(context)
        try:
            context.raise_if_cancelled()
            value = handler(**arguments)
            context.raise_if_cancelled()
        except CallCancelled as error:
            return _text_result(str(error), is_error=True)
        except Exception as e:  # noqa: BLE001 — convert to tool error
            self._log_stderr("tool", name, repr(e), traceback.format_exc())
            return _text_result(f"{type(e).__name__}: {e}", is_error=True)
        finally:
            _CURRENT_CONTEXT.reset(token)
        return _tool_result_payload(_coerce_tool_result(value))

    def _call_context(
        self,
        params: Dict[str, Any],
        state: _CallState,
    ) -> CallContext:
        raw_meta = params.get("_meta", {})
        if not isinstance(raw_meta, dict):
            raise _RpcError(ERR_INVALID_PARAMS, "`_meta` must be an object")
        progress_token = raw_meta.get("progressToken")
        if progress_token is not None and (
            isinstance(progress_token, bool)
            or not isinstance(progress_token, (str, int))
        ):
            raise _RpcError(
                ERR_INVALID_PARAMS,
                "`_meta.progressToken` must be a string or integer",
            )
        raw_context = raw_meta.get(CALL_CONTEXT_META_KEY)
        if raw_context is None:
            raise _RpcError(
                ERR_INVALID_PARAMS,
                f"missing authenticated `{CALL_CONTEXT_META_KEY}`",
            )
        try:
            validate_mcp_call_context(raw_context)
        except WireDecodeError as error:
            raise _RpcError(
                ERR_INVALID_PARAMS,
                f"invalid authenticated call context: {error}",
            ) from error
        caller = raw_context["caller"]
        return CallContext(
            call_id=raw_context["call_id"],
            trace_id=raw_context["trace_id"],
            deadline_unix_ms=raw_context.get("deadline_unix_ms"),
            session_id=raw_context.get("session_id"),
            task_id=raw_context.get("task_id"),
            caller=McpPrincipal(
                kind=caller["kind"],
                id=caller["id"],
                owner_uid=caller["owner_uid"],
            ),
            _cancelled=state.cancelled,
            _progress_token=progress_token,
            _emit_notification=self._send_notification,
        )

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

    def _send_notification(self, method: str, params: Dict[str, Any]) -> None:
        self._write(
            {
                "jsonrpc": JSONRPC_VERSION,
                "method": method,
                "params": params,
            }
        )

    def _write(self, frame: Dict[str, Any]) -> None:
        encoded = encode_wire_json(frame)
        with self._write_lock:
            sys.stdout.write(encoded)
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


def _load_manifest_service(path: pathlib.Path) -> tuple[str, str, List[_Tool]]:
    try:
        with path.open("rb") as stream:
            raw = stream.read(MAX_MANIFEST_BYTES + 1)
    except OSError as error:
        raise ManifestError(f"cannot read App manifest `{path}`: {error}") from error
    if len(raw) > MAX_MANIFEST_BYTES:
        raise ManifestError(
            f"App manifest `{path}` exceeds {MAX_MANIFEST_BYTES} bytes"
        )
    try:
        manifest = decode_wire_json(raw.decode("utf-8"))
    except (UnicodeDecodeError, json.JSONDecodeError, ValueError) as error:
        raise ManifestError(f"invalid App manifest `{path}`: {error}") from error
    if not isinstance(manifest, dict):
        raise ManifestError("App manifest must be a JSON object")
    _reject_unknown_fields(
        manifest,
        {
            "id",
            "version",
            "schema_version",
            "name",
            "summary",
            "icon",
            "runtime",
            "entry",
            "operations",
            "ai",
            "mcp",
            "desktop",
            "dependencies",
        },
        "App manifest",
    )
    if manifest.get("schema_version") != 2:
        raise ManifestError("MCP Apps require `schema_version: 2`")
    app_id = manifest.get("id")
    version = manifest.get("version")
    service = manifest.get("mcp")
    if not isinstance(app_id, str) or _APP_ID_PATTERN.fullmatch(app_id) is None:
        raise ManifestError("App manifest has no valid `id`")
    if not isinstance(version, str) or not version:
        raise ManifestError("App manifest has no valid `version`")
    if not isinstance(service, dict):
        raise ManifestError("App manifest has no `mcp` service")
    _localized_english(manifest.get("name"), "name")
    _reject_unknown_fields(
        service,
        {"entry", "transport", "lifecycle", "access", "tools"},
        "`mcp`",
    )
    if service.get("transport", "stdio") != "stdio":
        raise ManifestError("`mcp.transport` must be `stdio`")
    if service.get("lifecycle", "lazy") not in {
        "lazy",
        "always-on",
        "while-app-running",
    }:
        raise ManifestError("`mcp.lifecycle` is invalid")
    access = service.get("access")
    if access is not None:
        if not isinstance(access, dict):
            raise ManifestError("`mcp.access` must be an object")
        _reject_unknown_fields(
            access,
            {"system_agent", "external_agents"},
            "`mcp.access`",
        )
    raw_tools = service.get("tools")
    if not isinstance(raw_tools, list):
        raise ManifestError("`mcp.tools` must be an array")
    if not raw_tools:
        raise ManifestError("`mcp.tools` must contain at least one tool")

    tools: List[_Tool] = []
    names: set[str] = set()
    for index, raw_tool in enumerate(raw_tools):
        if not isinstance(raw_tool, dict):
            raise ManifestError(f"`mcp.tools[{index}]` must be an object")
        _reject_unknown_fields(
            raw_tool,
            {"name", "summary", "args", "needs"},
            f"`mcp.tools[{index}]`",
        )
        name = raw_tool.get("name")
        if (
            not isinstance(name, str)
            or _TOOL_NAME_PATTERN.fullmatch(name) is None
        ):
            raise ManifestError(f"`mcp.tools[{index}].name` must be a string")
        if name in names:
            raise ManifestError(f"tool `{name}` is declared twice")
        names.add(name)
        summary = _localized_english(
            raw_tool.get("summary"),
            f"mcp.tools[{index}].summary",
        )
        args = raw_tool.get("args", [])
        if not isinstance(args, list):
            raise ManifestError(f"`mcp.tools[{index}].args` must be an array")
        tools.append(
            _Tool(
                name=name,
                summary=summary,
                input_schema=_manifest_input_schema(args, name),
                manifest_args=[dict(arg) for arg in args],
            )
        )
    return app_id, version, tools


def _localized_english(value: Any, field_name: str) -> str:
    if isinstance(value, str) and value:
        return value
    if isinstance(value, dict):
        english = value.get("en")
        if isinstance(english, str) and english:
            return english
    raise ManifestError(f"`{field_name}` requires non-empty English text")


def _reject_unknown_fields(
    value: Dict[str, Any],
    allowed: set[str],
    field_name: str,
) -> None:
    unknown = sorted(set(value) - allowed)
    if unknown:
        raise ManifestError(
            f"{field_name} contains unknown field `{unknown[0]}`"
        )


def _manifest_input_schema(
    args: List[Any],
    tool_name: str,
) -> Dict[str, Any]:
    json_types = {
        "path": "string",
        "host": "string",
        "name": "string",
        "text": "string",
        "number": "number",
        "integer": "integer",
        "bool": "boolean",
    }
    properties: Dict[str, Any] = {}
    required: List[str] = []
    conditional: List[Dict[str, Any]] = []
    for index, raw_arg in enumerate(args):
        if not isinstance(raw_arg, dict):
            raise ManifestError(
                f"tool `{tool_name}` arg {index} must be an object"
            )
        _reject_unknown_fields(
            raw_arg,
            {
                "name",
                "kind",
                "binding",
                "required",
                "required_when",
                "repeatable",
                "choices",
                "default",
                "label",
            },
            f"tool `{tool_name}` arg {index}",
        )
        name = raw_arg.get("name")
        kind = raw_arg.get("kind")
        if not isinstance(name, str) or not name:
            raise ManifestError(
                f"tool `{tool_name}` arg {index} has no valid name"
            )
        if name in properties:
            raise ManifestError(f"tool `{tool_name}` arg `{name}` is duplicated")
        json_type = json_types.get(kind)
        if json_type is None:
            raise ManifestError(
                f"tool `{tool_name}` arg `{name}` has invalid kind `{kind}`"
            )
        # `binding` is one-shot CLI metadata only: validated for shape here and
        # deliberately never copied into the model-facing input schema.
        binding = raw_arg.get("binding")
        if binding is not None and binding not in ("positional", "flag"):
            raise ManifestError(
                f"tool `{tool_name}` arg `{name}` has invalid binding `{binding}`"
            )
        choices = raw_arg.get("choices", [])
        if not isinstance(choices, list):
            raise ManifestError(
                f"tool `{tool_name}` arg `{name}` choices must be an array"
            )
        if raw_arg.get("repeatable", False):
            item_schema: Dict[str, Any] = {"type": json_type}
            if choices:
                item_schema["enum"] = choices
            prop: Dict[str, Any] = {
                "type": "array",
                "items": item_schema,
            }
        else:
            prop = {"type": json_type}
            if choices:
                prop["enum"] = choices
        label = raw_arg.get("label")
        if label is not None:
            prop["description"] = _localized_english(
                label,
                f"tool `{tool_name}` arg `{name}` label",
            )
        if "default" in raw_arg:
            prop["default"] = raw_arg["default"]
        properties[name] = prop
        if raw_arg.get("required", False):
            required.append(name)
        required_when = raw_arg.get("required_when")
        if required_when is not None:
            conditional.append(
                {
                    "if": _condition_schema(
                        required_when,
                        tool_name,
                        name,
                    ),
                    "then": {"required": [name]},
                    "else": {"not": {"required": [name]}},
                }
            )

    schema: Dict[str, Any] = {
        "type": "object",
        "properties": properties,
        "additionalProperties": False,
    }
    if required:
        schema["required"] = required
    if conditional:
        schema["allOf"] = conditional
    return schema


def _condition_schema(
    condition: Any,
    tool_name: str,
    arg_name: str,
) -> Dict[str, Any]:
    if not isinstance(condition, dict):
        raise ManifestError(
            f"tool `{tool_name}` arg `{arg_name}` required_when must be an object"
        )
    kind = condition.get("kind")
    source = condition.get("arg")
    if not isinstance(source, str) or not source:
        raise ManifestError(
            f"tool `{tool_name}` arg `{arg_name}` required_when needs an arg"
        )
    if kind == "arg-present":
        return {"required": [source]}
    if kind == "arg-equals" and "value" in condition:
        return {
            "properties": {source: {"const": condition["value"]}},
            "required": [source],
        }
    if kind == "arg-not-equals" and "value" in condition:
        return {
            "required": [source],
            "not": {
                "properties": {source: {"const": condition["value"]}},
                "required": [source],
            },
        }
    raise ManifestError(
        f"tool `{tool_name}` arg `{arg_name}` has invalid required_when"
    )


def _resolve_manifest_arguments(
    tool: _Tool,
    supplied: Dict[str, Any],
) -> Dict[str, Any]:
    declared = {arg["name"]: arg for arg in tool.manifest_args}
    extras = sorted(name for name in supplied if name not in declared)
    if extras:
        raise _ToolArgumentError(f"unknown argument `{extras[0]}`")

    resolved = dict(supplied)
    for arg in tool.manifest_args:
        name = arg["name"]
        condition = arg.get("required_when")
        condition_active = (
            condition is None or _condition_matches(condition, resolved)
        )
        if condition is not None and not condition_active:
            if name in resolved:
                raise _ToolArgumentError(
                    f"`{name}` is not accepted when its condition is false"
                )
            continue
        if name not in resolved:
            if "default" in arg:
                resolved[name] = copy.deepcopy(arg["default"])
            elif arg.get("required", False) or condition is not None:
                raise _ToolArgumentError(f"missing required argument `{name}`")
            else:
                continue
        resolved[name] = _validate_manifest_argument(
            name,
            arg,
            resolved[name],
        )
    return resolved


def _condition_matches(
    condition: Dict[str, Any],
    values: Dict[str, Any],
) -> bool:
    source = condition["arg"]
    kind = condition["kind"]
    if kind == "arg-present":
        return source in values
    if source not in values:
        return False
    if kind == "arg-equals":
        return values[source] == condition["value"]
    if kind == "arg-not-equals":
        return values[source] != condition["value"]
    raise _ToolArgumentError(f"unsupported argument condition `{kind}`")


def _validate_manifest_argument(
    name: str,
    declaration: Dict[str, Any],
    value: Any,
) -> Any:
    if declaration.get("repeatable", False):
        if not isinstance(value, list):
            raise _ToolArgumentError(f"`{name}` must be an array")
        return [
            _validate_manifest_scalar(name, declaration, item)
            for item in value
        ]
    return _validate_manifest_scalar(name, declaration, value)


def _validate_manifest_scalar(
    name: str,
    declaration: Dict[str, Any],
    value: Any,
) -> Any:
    kind = declaration["kind"]
    if kind in {"path", "host", "name", "text"}:
        if not isinstance(value, str):
            raise _ToolArgumentError(f"`{name}` must be a string")
        normalized = value
    elif kind == "bool":
        if not isinstance(value, bool):
            raise _ToolArgumentError(f"`{name}` must be a boolean")
        normalized = value
    elif kind == "integer":
        try:
            normalized = wire_integer_to_int(value)
        except ValueError as error:
            raise _ToolArgumentError(
                f"`{name}` must be an integer"
            ) from error
    elif kind == "number":
        if isinstance(value, bool) or not isinstance(
            value,
            (int, float, Decimal, WireDecimal),
        ):
            raise _ToolArgumentError(f"`{name}` must be a number")
        if isinstance(value, float) and not math.isfinite(value):
            raise _ToolArgumentError(f"`{name}` must be a finite number")
        if isinstance(value, Decimal) and not value.is_finite():
            raise _ToolArgumentError(f"`{name}` must be a finite number")
        normalized = value
    else:
        raise _ToolArgumentError(f"`{name}` has unsupported kind `{kind}`")

    choices = declaration.get("choices", [])
    if choices and normalized not in choices:
        raise _ToolArgumentError(f"`{name}` is not one of its allowed values")
    return normalized


def _request_key(value: Any) -> str:
    valid = (
        value is None
        or isinstance(value, str)
        or (
            isinstance(value, (int, float, Decimal, WireDecimal))
            and not isinstance(value, bool)
            and (not isinstance(value, float) or math.isfinite(value))
            and (not isinstance(value, Decimal) or value.is_finite())
        )
    )
    if not valid:
        raise TypeError("request id must be a string, number, or null")
    return encode_wire_json(value)


def _validate_progress_number(value: Any, field_name: str) -> None:
    if isinstance(value, bool) or not isinstance(value, (int, float, Decimal)):
        raise TypeError(f"{field_name} must be a finite non-negative number")
    if isinstance(value, float) and not math.isfinite(value):
        raise ValueError(f"{field_name} must be a finite non-negative number")
    if isinstance(value, Decimal) and not value.is_finite():
        raise ValueError(f"{field_name} must be a finite non-negative number")
    if value < 0:
        raise ValueError(f"{field_name} must be a finite non-negative number")


def _coerce_tool_result(value: Any) -> ToolResult:
    if isinstance(value, ToolResult):
        return value
    if isinstance(value, dict):
        return ToolResult.structured(value)
    return ToolResult.text(_stringify(value))


def _tool_result_payload(result: ToolResult) -> Dict[str, Any]:
    payload: Dict[str, Any] = {
        "content": result.content,
        "isError": result.is_error,
    }
    if result.structured_content is not None:
        payload["structuredContent"] = result.structured_content
    return payload


def _stringify(value: Any) -> str:
    if value is None:
        return ""
    if isinstance(value, str):
        return value
    try:
        return encode_wire_json(value)
    except (TypeError, ValueError):
        return repr(value)


def _text_result(text: str, *, is_error: bool) -> Dict[str, Any]:
    result = ToolResult.error(text) if is_error else ToolResult.text(text)
    return _tool_result_payload(result)


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
        drain through the actual next newline without retaining the
        discarded bytes.

    Works for both binary buffered streams (production) and text
    streams (tests inject :class:`io.StringIO`).
    """
    binary = hasattr(reader, "read1") or isinstance(
        reader,
        (io.RawIOBase, io.BufferedIOBase),
    )
    newline = b"\n" if binary else "\n"
    chunk = reader.readline(limit + 2)
    if not chunk:
        return "", False

    terminated = chunk.endswith(newline)
    content = chunk[:-1] if terminated else chunk
    byte_length = len(content) if binary else len(
        content.encode("utf-8", errors="replace")
    )
    if byte_length <= limit and (terminated or len(chunk) < limit + 2):
        if binary:
            return content.decode("utf-8"), False
        return content, False

    # The retained allocation is bounded by limit + 2. Discard the
    # remainder in fixed-size chunks until the real frame boundary.
    while not terminated:
        discarded = reader.readline(64 * 1024)
        if not discarded:
            break
        terminated = discarded.endswith(newline)
    return "", True
