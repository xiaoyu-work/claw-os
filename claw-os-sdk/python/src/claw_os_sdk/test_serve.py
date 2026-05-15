"""Unit tests for the MCP server SDK in `claw_os_sdk.serve`."""

import io
import json
import os
import sys
import unittest

_THIS_DIR = os.path.dirname(__file__)
sys.path.insert(0, os.path.dirname(_THIS_DIR))  # so `from claw_os_sdk import serve` works

from claw_os_sdk import serve  # noqa: E402


def _drive(app: serve.App, *frames: dict) -> list[dict]:
    """Feed `frames` into `app.serve()` via captured stdio and return
    every JSON-RPC envelope written to stdout (skipping notifications,
    which produce no output)."""
    stdin_text = "\n".join(json.dumps(f) for f in frames) + "\n"
    real_stdin, real_stdout = sys.stdin, sys.stdout
    sys.stdin = io.StringIO(stdin_text)
    sys.stdout = io.StringIO()
    try:
        app.serve()
        sys.stdout.seek(0)
        out = sys.stdout.read()
    finally:
        sys.stdin, sys.stdout = real_stdin, real_stdout
    return [json.loads(line) for line in out.splitlines() if line.strip()]


class HandshakeTests(unittest.TestCase):
    def test_initialize_reports_protocol_and_server_info(self) -> None:
        app = serve.App(name="kv", version="9.9.9")

        out = _drive(
            app,
            {"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}},
        )

        self.assertEqual(len(out), 1)
        result = out[0]["result"]
        self.assertEqual(result["protocolVersion"], serve.PROTOCOL_VERSION)
        self.assertEqual(result["serverInfo"]["name"], "kv")
        self.assertEqual(result["serverInfo"]["version"], "9.9.9")
        self.assertIn("tools", result["capabilities"])

    def test_initialized_notification_produces_no_response(self) -> None:
        app = serve.App(name="kv")

        out = _drive(
            app,
            {"jsonrpc": "2.0", "method": "notifications/initialized"},
        )

        self.assertEqual(out, [])

    def test_name_defaults_to_cos_app_id(self) -> None:
        prev = os.environ.get("COS_APP_ID")
        os.environ["COS_APP_ID"] = "calendar"
        try:
            self.assertEqual(serve.App().name, "calendar")
        finally:
            if prev is None:
                os.environ.pop("COS_APP_ID", None)
            else:
                os.environ["COS_APP_ID"] = prev


class ToolListTests(unittest.TestCase):
    def test_list_tools_returns_registered_tools(self) -> None:
        app = serve.App(name="kv")

        @app.tool(
            "kv.get",
            summary="Look up a key.",
            args={"key": {"type": "string"}},
            required=["key"],
        )
        def _get(key: str) -> str:
            return "ok"

        out = _drive(
            app,
            {"jsonrpc": "2.0", "id": 1, "method": "tools/list"},
        )

        tools = out[0]["result"]["tools"]
        self.assertEqual(len(tools), 1)
        self.assertEqual(tools[0]["name"], "kv.get")
        self.assertEqual(tools[0]["description"], "Look up a key.")
        self.assertEqual(tools[0]["inputSchema"]["required"], ["key"])

    def test_duplicate_tool_name_raises(self) -> None:
        app = serve.App(name="kv")

        @app.tool("kv.x", summary="first")
        def _a() -> str:
            return "a"

        with self.assertRaises(ValueError):

            @app.tool("kv.x", summary="second")
            def _b() -> str:
                return "b"


class ToolsCallTests(unittest.TestCase):
    def _kv_app(self) -> serve.App:
        app = serve.App(name="kv")
        store: dict[str, str] = {}

        @app.tool("kv.set", args={"key": {"type": "string"}, "value": {"type": "string"}})
        def _set(key: str, value: str) -> str:
            store[key] = value
            return "ok"

        @app.tool("kv.get", args={"key": {"type": "string"}})
        def _get(key: str) -> str:
            return store.get(key, "")

        @app.tool("kv.boom")
        def _boom() -> str:
            raise RuntimeError("on fire")

        return app

    def test_tool_call_returns_text_content(self) -> None:
        app = self._kv_app()

        out = _drive(
            app,
            {
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/call",
                "params": {"name": "kv.set", "arguments": {"key": "a", "value": "1"}},
            },
            {
                "jsonrpc": "2.0",
                "id": 2,
                "method": "tools/call",
                "params": {"name": "kv.get", "arguments": {"key": "a"}},
            },
        )

        self.assertEqual(out[0]["result"]["content"][0]["text"], "ok")
        self.assertEqual(out[0]["result"]["isError"], False)
        self.assertEqual(out[1]["result"]["content"][0]["text"], "1")

    def test_tool_exception_becomes_is_error(self) -> None:
        app = self._kv_app()

        out = _drive(
            app,
            {
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/call",
                "params": {"name": "kv.boom", "arguments": {}},
            },
        )

        result = out[0]["result"]
        self.assertEqual(result["isError"], True)
        self.assertIn("on fire", result["content"][0]["text"])

    def test_tool_wrong_kwargs_become_is_error_not_transport_error(self) -> None:
        app = self._kv_app()

        out = _drive(
            app,
            {
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/call",
                "params": {"name": "kv.get", "arguments": {"wrong": "arg"}},
            },
        )

        # Per MCP, bad args inside a known tool are reported as a tool
        # error (still `result`, with `isError: true`) so the model
        # can self-correct.
        self.assertIn("result", out[0])
        self.assertEqual(out[0]["result"]["isError"], True)

    def test_unknown_tool_returns_invalid_params(self) -> None:
        app = self._kv_app()

        out = _drive(
            app,
            {
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/call",
                "params": {"name": "kv.does-not-exist", "arguments": {}},
            },
        )

        self.assertIn("error", out[0])
        self.assertEqual(out[0]["error"]["code"], serve.ERR_INVALID_PARAMS)

    def test_dict_return_value_is_serialised_as_json_text(self) -> None:
        app = serve.App(name="x")

        @app.tool("x.echo")
        def _echo() -> dict:
            return {"a": 1, "b": [True, None]}

        out = _drive(
            app,
            {"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": "x.echo"}},
        )

        text = out[0]["result"]["content"][0]["text"]
        # Deserialising must round-trip; we deliberately don't pin the
        # key order beyond what json.dumps emits.
        self.assertEqual(json.loads(text), {"a": 1, "b": [True, None]})


class TransportTests(unittest.TestCase):
    def test_unknown_method_returns_method_not_found(self) -> None:
        app = serve.App(name="kv")

        out = _drive(
            app,
            {"jsonrpc": "2.0", "id": 1, "method": "ghost", "params": {}},
        )

        self.assertEqual(out[0]["error"]["code"], serve.ERR_METHOD_NOT_FOUND)

    def test_missing_jsonrpc_field_returns_invalid_request(self) -> None:
        app = serve.App(name="kv")
        sys.stdin_orig, sys.stdout_orig = sys.stdin, sys.stdout
        sys.stdin = io.StringIO('{"id":1,"method":"ping"}\n')
        sys.stdout = io.StringIO()
        try:
            app.serve()
            sys.stdout.seek(0)
            out = sys.stdout.read()
        finally:
            sys.stdin, sys.stdout = sys.stdin_orig, sys.stdout_orig
        frame = json.loads(out.strip())
        self.assertEqual(frame["error"]["code"], serve.ERR_INVALID_REQUEST)

    def test_garbage_line_returns_parse_error_with_null_id(self) -> None:
        app = serve.App(name="kv")
        sys.stdin_orig, sys.stdout_orig = sys.stdin, sys.stdout
        sys.stdin = io.StringIO("not json at all\n")
        sys.stdout = io.StringIO()
        try:
            app.serve()
            sys.stdout.seek(0)
            out = sys.stdout.read()
        finally:
            sys.stdin, sys.stdout = sys.stdin_orig, sys.stdout_orig
        frame = json.loads(out.strip())
        self.assertEqual(frame["error"]["code"], serve.ERR_PARSE)
        self.assertIsNone(frame["id"])

    def test_ping_returns_empty_object(self) -> None:
        app = serve.App(name="kv")

        out = _drive(
            app,
            {"jsonrpc": "2.0", "id": 1, "method": "ping"},
        )

        self.assertEqual(out[0]["result"], {})


if __name__ == "__main__":
    unittest.main()
