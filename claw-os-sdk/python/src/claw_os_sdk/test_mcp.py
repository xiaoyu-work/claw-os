"""Tests for the manifest-bound Claw App MCP runtime."""

from __future__ import annotations

import io
import json
import sys
import tempfile
import time
import unittest
from pathlib import Path
from typing import Any

from claw_os_sdk import mcp
from claw_os_sdk.generated import decode_wire_json


def _manifest(*tools: dict[str, Any]) -> dict[str, Any]:
    return {
        "schema_version": 2,
        "id": "mail",
        "version": "1.2.3",
        "name": {"en": "Mail"},
        "mcp": {
            "entry": "server.py",
            "tools": list(tools),
        },
    }


def _write_manifest(value: dict[str, Any]) -> tuple[tempfile.TemporaryDirectory, Path]:
    directory = tempfile.TemporaryDirectory()
    path = Path(directory.name, "app.json")
    path.write_text(json.dumps(value), encoding="utf-8")
    return directory, path


def _bound_app(
    test: unittest.TestCase,
    *tools: dict[str, Any],
) -> mcp.App:
    directory, path = _write_manifest(_manifest(*tools))
    test.addCleanup(directory.cleanup)
    return mcp.App.from_manifest(path)


def _context(
    *,
    call_id: str = "call-1",
    deadline_unix_ms: int | None = None,
) -> dict[str, Any]:
    value: dict[str, Any] = {
        "wire_version": 1,
        "call_id": call_id,
        "trace_id": "trace-1",
        "session_id": "session-1",
        "task_id": "task-1",
        "caller": {
            "kind": "system-agent",
            "id": "agent-session-1",
            "owner_uid": 1000,
        },
    }
    if deadline_unix_ms is not None:
        value["deadline_unix_ms"] = deadline_unix_ms
    return value


def _call(
    name: str,
    arguments: dict[str, Any],
    *,
    request_id: Any = 1,
    context: dict[str, Any] | None = None,
    progress_token: Any = None,
) -> dict[str, Any]:
    meta: dict[str, Any] = {}
    if context is not None:
        meta[mcp.CALL_CONTEXT_META_KEY] = context
    if progress_token is not None:
        meta["progressToken"] = progress_token
    params: dict[str, Any] = {
        "name": name,
        "arguments": arguments,
    }
    if meta:
        params["_meta"] = meta
    return {
        "jsonrpc": "2.0",
        "id": request_id,
        "method": "tools/call",
        "params": params,
    }


def _drive(app: mcp.App, *frames: dict[str, Any]) -> list[dict[str, Any]]:
    stdin_text = "\n".join(json.dumps(frame) for frame in frames) + "\n"
    real_stdin, real_stdout = sys.stdin, sys.stdout
    sys.stdin = io.StringIO(stdin_text)
    sys.stdout = io.StringIO()
    try:
        app.serve()
        sys.stdout.seek(0)
        output = sys.stdout.read()
    finally:
        sys.stdin, sys.stdout = real_stdin, real_stdout
    return [
        decode_wire_json(line)
        for line in output.splitlines()
        if line.strip()
    ]


class ManifestBindingTests(unittest.TestCase):
    def test_manifest_contract_is_closed_and_requires_tools(self) -> None:
        tool = {
            "name": "mail.status",
            "summary": {"en": "Show status."},
        }
        invalid_manifests = [
            {
                **_manifest(tool),
                "mcp": {**_manifest(tool)["mcp"], "access": {"apps": []}},
            },
            {**_manifest(tool), "unknown": True},
            {
                **_manifest(tool),
                "mcp": {**_manifest(tool)["mcp"], "unknown": True},
            },
            _manifest({**tool, "unknown": True}),
            _manifest(
                {
                    **tool,
                    "args": [
                        {
                            "name": "value",
                            "kind": "text",
                            "binding": "sideways",
                        }
                    ],
                }
            ),
            _manifest(),
        ]
        for index, manifest in enumerate(invalid_manifests):
            with self.subTest(index=index):
                directory, path = _write_manifest(manifest)
                self.addCleanup(directory.cleanup)
                with self.assertRaises(mcp.ManifestError):
                    mcp.App.from_manifest(path)

    def test_mcp_tool_arg_binding_is_cli_metadata(self) -> None:
        directory, path = _write_manifest(
            _manifest(
                {
                    "name": "mail.compose",
                    "summary": {"en": "Compose."},
                    "args": [
                        {"name": "message", "kind": "text", "binding": "positional"},
                        {"name": "loud", "kind": "bool", "binding": "flag"},
                    ],
                }
            )
        )
        self.addCleanup(directory.cleanup)
        app = mcp.App.from_manifest(path)

        @app.tool("mail.compose")
        def compose(message: str, loud: bool = False) -> dict[str, Any]:
            return {"message": message, "loud": loud}

        frames = _drive(
            app,
            {"jsonrpc": "2.0", "id": 1, "method": "tools/list"},
        )
        tool = frames[0]["result"]["tools"][0]
        # `binding` is one-shot CLI metadata only; it must not surface in the
        # model-facing MCP inputSchema.
        self.assertNotIn("binding", json.dumps(tool["inputSchema"]))
        self.assertIn("message", tool["inputSchema"]["properties"])
        self.assertIn("loud", tool["inputSchema"]["properties"])

    def test_manifest_is_the_only_tool_descriptor_source(self) -> None:
        directory, path = _write_manifest(
            _manifest(
                {
                    "name": "mail.search",
                    "summary": {"en": "Search synchronized mail."},
                    "args": [
                        {
                            "name": "query",
                            "kind": "text",
                            "required": True,
                            "label": {"en": "Search query"},
                        },
                        {
                            "name": "folders",
                            "kind": "name",
                            "repeatable": True,
                            "choices": ["inbox", "archive"],
                        },
                        {
                            "name": "limit",
                            "kind": "integer",
                            "default": 20,
                        },
                    ],
                }
            )
        )
        self.addCleanup(directory.cleanup)
        app = mcp.App.from_manifest(path)

        @app.tool("mail.search")
        def search(
            query: str,
            limit: int,
            folders: list[str] | None = None,
        ) -> dict[str, Any]:
            return {
                "query": query,
                "folders": folders or [],
                "limit": limit,
            }

        with self.assertRaisesRegex(TypeError, "unexpected keyword argument"):
            app.tool("mail.search", summary="drift")

        with self.assertRaisesRegex(ValueError, "not declared"):

            @app.tool("mail.unknown")
            def unknown() -> None:
                return None

        frames = _drive(
            app,
            {"jsonrpc": "2.0", "id": 1, "method": "tools/list"},
        )
        tool = frames[0]["result"]["tools"][0]
        self.assertEqual(app.name, "mail")
        self.assertEqual(app.version, "1.2.3")
        self.assertEqual(tool["name"], "mail.search")
        self.assertEqual(tool["description"], "Search synchronized mail.")
        self.assertEqual(tool["inputSchema"]["required"], ["query"])
        self.assertEqual(
            tool["inputSchema"]["properties"]["folders"],
            {
                "type": "array",
                "items": {
                    "type": "string",
                    "enum": ["inbox", "archive"],
                },
            },
        )
        self.assertFalse(tool["inputSchema"]["additionalProperties"])

        call = _drive(
            app,
            _call(
                "mail.search",
                {"query": "customer"},
                context=_context(),
            ),
        )
        self.assertEqual(call[0]["result"]["structuredContent"]["limit"], 20)

        invalid = _drive(
            app,
            _call(
                "mail.search",
                {"query": "customer", "folders": ["spam"]},
                context=_context(call_id="call-2"),
            ),
        )
        self.assertTrue(invalid[0]["result"]["isError"])
        self.assertIn("allowed values", invalid[0]["result"]["content"][0]["text"])

    def test_unbound_manifest_tool_fails_before_serving(self) -> None:
        directory, path = _write_manifest(
            _manifest(
                {
                    "name": "mail.search",
                    "summary": {"en": "Search mail."},
                }
            )
        )
        self.addCleanup(directory.cleanup)
        app = mcp.App.from_manifest(path)
        with self.assertRaisesRegex(mcp.ManifestError, "missing handlers"):
            _drive(app, {"jsonrpc": "2.0", "id": 1, "method": "ping"})

    def test_direct_construction_is_rejected(self) -> None:
        with self.assertRaisesRegex(TypeError, "App.from_manifest"):
            mcp.App(name="code-authored")


class AuthenticatedContextTests(unittest.TestCase):
    def test_removed_app_identity_and_cross_call_metadata_are_rejected(self) -> None:
        for field, value in (
            ("kind", "app"),
            ("kind", "app-agent"),
            ("app_id", "caller"),
        ):
            with self.subTest(field=field, value=value):
                directory, app = self._app()
                self.addCleanup(directory.cleanup)
                context = _context()
                context["caller"][field] = value
                frames = _drive(app, _call("mail.context", {"caller": "x"}, context=context))
                self.assertEqual(frames[0]["error"]["code"], mcp.ERR_INVALID_PARAMS)
                expected = "WIRE_ENUM" if field == "kind" else "WIRE_UNKNOWN_FIELD"
                self.assertIn(expected, frames[0]["error"]["message"])
        for field in ("parent_call_id", "depth"):
            directory, app = self._app()
            self.addCleanup(directory.cleanup)
            context = _context()
            context[field] = None
            frames = _drive(app, _call("mail.context", {"caller": "x"}, context=context))
            self.assertEqual(frames[0]["error"]["code"], mcp.ERR_INVALID_PARAMS)
            self.assertIn("WIRE_UNKNOWN_FIELD", frames[0]["error"]["message"])

    def _app(self) -> tuple[tempfile.TemporaryDirectory, mcp.App]:
        directory, path = _write_manifest(
            _manifest(
                {
                    "name": "mail.context",
                    "summary": {"en": "Return authenticated call context."},
                    "args": [
                        {
                            "name": "caller",
                            "kind": "text",
                            "required": True,
                        }
                    ],
                }
            )
        )
        app = mcp.App.from_manifest(path)

        @app.tool("mail.context")
        def context_tool(caller: str) -> dict[str, Any]:
            context = mcp.current_context()
            return {
                "argument": caller,
                "authenticated_kind": context.caller.kind,
                "authenticated_id": context.caller.id,
                "call_id": context.call_id,
            }

        return directory, app

    def test_manifest_bound_call_requires_gateway_context(self) -> None:
        directory, app = self._app()
        self.addCleanup(directory.cleanup)
        frames = _drive(app, _call("mail.context", {"caller": "forged"}))
        self.assertEqual(frames[0]["error"]["code"], mcp.ERR_INVALID_PARAMS)
        self.assertIn("missing authenticated", frames[0]["error"]["message"])

    def test_context_is_separate_from_caller_arguments(self) -> None:
        directory, app = self._app()
        self.addCleanup(directory.cleanup)
        frames = _drive(
            app,
            _call(
                "mail.context",
                {"caller": "forged"},
                context=_context(),
            ),
        )
        result = frames[0]["result"]
        self.assertFalse(result["isError"])
        self.assertEqual(
            result["structuredContent"],
            {
                "argument": "forged",
                "authenticated_kind": "system-agent",
                "authenticated_id": "agent-session-1",
                "call_id": "call-1",
            },
        )
        with self.assertRaisesRegex(RuntimeError, "no MCP tool call"):
            mcp.current_context()

    def test_unknown_call_context_field_is_rejected(self) -> None:
        directory, app = self._app()
        self.addCleanup(directory.cleanup)
        context = _context()
        context["caller_supplied"] = True
        frames = _drive(
            app,
            _call("mail.context", {"caller": "x"}, context=context),
        )
        self.assertEqual(frames[0]["error"]["code"], mcp.ERR_INVALID_PARAMS)
        self.assertIn("WIRE_UNKNOWN_FIELD", frames[0]["error"]["message"])

class ProgressAndCancellationTests(unittest.TestCase):
    def test_handler_can_emit_progress(self) -> None:
        app = _bound_app(
            self,
            {
                "name": "work.run",
                "summary": {"en": "Run work."},
            },
        )

        @app.tool("work.run")
        def run() -> mcp.ToolResult:
            context = mcp.current_context()
            context.report_progress(1, total=2, message="halfway")
            return mcp.ToolResult.structured({"done": True}, text="done")

        frames = _drive(
            app,
            _call(
                "work.run",
                {},
                context=_context(),
                progress_token="progress-1",
            ),
        )
        self.assertEqual(
            frames[0],
            {
                "jsonrpc": "2.0",
                "method": "notifications/progress",
                "params": {
                    "progressToken": "progress-1",
                    "progress": 1,
                    "total": 2,
                    "message": "halfway",
                },
            },
        )
        self.assertEqual(frames[1]["result"]["structuredContent"], {"done": True})

        without_token = _drive(
            app,
            _call(
                "work.run",
                {},
                context=_context(call_id="call-2"),
            ),
        )
        self.assertFalse(without_token[0]["result"]["isError"])

    def test_invalid_progress_token_is_rejected(self) -> None:
        app = _bound_app(
            self,
            {
                "name": "work.run",
                "summary": {"en": "Run work."},
            },
        )

        @app.tool("work.run")
        def run() -> str:
            return "done"

        frames = _drive(
            app,
            _call(
                "work.run",
                {},
                context=_context(),
                progress_token={"forged": True},
            ),
        )
        self.assertEqual(frames[0]["error"]["code"], mcp.ERR_INVALID_PARAMS)
        self.assertIn("progressToken", frames[0]["error"]["message"])

    def test_cancellation_reaches_running_handler(self) -> None:
        app = _bound_app(
            self,
            {
                "name": "work.wait",
                "summary": {"en": "Wait for cancellation."},
            },
        )

        @app.tool("work.wait")
        def wait() -> str:
            context = mcp.current_context()
            while not context.cancelled:
                time.sleep(0.001)
            context.raise_if_cancelled()
            return "unreachable"

        frames = _drive(
            app,
            _call(
                "work.wait",
                {},
                request_id="request-1",
                context=_context(call_id="call-wait"),
            ),
            {
                "jsonrpc": "2.0",
                "method": "notifications/cancelled",
                "params": {"requestId": "request-1", "reason": "no longer needed"},
            },
        )
        self.assertEqual(frames, [])

    def test_eof_cancels_a_cooperative_in_flight_call(self) -> None:
        app = _bound_app(
            self,
            {
                "name": "work.wait",
                "summary": {"en": "Wait for cancellation."},
            },
        )

        @app.tool("work.wait")
        def wait() -> str:
            context = mcp.current_context()
            while not context.cancelled:
                time.sleep(0.001)
            context.raise_if_cancelled()
            return "unreachable"

        self.assertEqual(
            _drive(
                app,
                _call("work.wait", {}, context=_context()),
            ),
            [],
        )

    def test_expired_deadline_stops_call_before_handler(self) -> None:
        directory, path = _write_manifest(
            _manifest(
                {
                    "name": "work.run",
                    "summary": {"en": "Run work."},
                }
            )
        )
        self.addCleanup(directory.cleanup)
        app = mcp.App.from_manifest(path)
        called = False

        @app.tool("work.run")
        def run() -> str:
            nonlocal called
            called = True
            return "done"

        frames = _drive(
            app,
            _call(
                "work.run",
                {},
                context=_context(deadline_unix_ms=1),
            ),
        )
        self.assertFalse(called)
        self.assertTrue(frames[0]["result"]["isError"])
        self.assertIn("cancelled", frames[0]["result"]["content"][0]["text"])


if __name__ == "__main__":
    unittest.main()
