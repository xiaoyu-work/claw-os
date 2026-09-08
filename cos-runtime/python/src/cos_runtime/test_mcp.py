import json
from unittest import mock

import pytest

from claw_os_sdk.mcp import ManifestError, ToolResult

from .mcp import _operation_argv, _operation_bindings, _tool_result, serve_manifest_operations


def _manifest():
    operation = {
        "label": {"en": "Run"},
        "args": [
            {"name": "path", "kind": "path", "required": True},
            {
                "name": "tag",
                "kind": "name",
                "binding": "flag",
                "repeatable": True,
            },
            {
                "name": "enabled",
                "kind": "bool",
                "binding": "flag",
                "default": False,
            },
        ],
        "needs": [
            {
                "verb": "fs.read",
                "scope": {"kind": "from-arg", "arg": "path"},
                "why": {"en": "Read the requested path."},
            }
        ],
    }
    return {
        "id": "sample",
        "version": "0.1.0",
        "schema_version": 2,
        "name": {"en": "Sample"},
        "operations": {"run": operation},
        "mcp": {
            "tools": [
                {
                    "name": "sample.run",
                    "summary": {"en": "Run"},
                    "args": [
                        {key: value for key, value in arg.items() if key != "binding"}
                        for arg in operation["args"]
                    ],
                    "needs": operation["needs"],
                }
            ]
        },
    }


def test_operation_bindings_require_exact_tool_alignment():
    manifest = _manifest()
    bindings = _operation_bindings(manifest)
    assert bindings["sample.run"][0] == "run"

    manifest["mcp"]["tools"][0]["args"][0]["required"] = False
    with pytest.raises(ManifestError, match="arguments diverge"):
        _operation_bindings(manifest)


def test_operation_argv_uses_canonical_manifest_bindings():
    operation = _manifest()["operations"]["run"]
    assert _operation_argv(
        operation,
        {
            "path": "--literal-path",
            "tag": ["one", "--two"],
            "enabled": False,
        },
    ) == [
        "--tag",
        "one",
        "--tag=--two",
        "--enabled=false",
        "--",
        "--literal-path",
    ]


def test_operation_error_becomes_structured_mcp_error():
    value = {"ok": False, "error": "denied", "code": "PERMISSION_DENIED"}
    result = _tool_result(value)
    assert isinstance(result, ToolResult)
    assert result.is_error is True
    assert result.structured_content == value


def test_operation_success_stays_structured_value():
    value = {"ok": True, "value": 42}
    assert _tool_result(value) is value


def test_operation_service_starts_through_the_sdk_serve_entrypoint(tmp_path):
    path = tmp_path / "app.json"
    path.write_text(json.dumps(_manifest()))
    with mock.patch("claw_os_sdk.mcp.App.serve", autospec=True) as serve:
        serve_manifest_operations(lambda command, args: {"command": command}, path)
    serve.assert_called_once()
    app = serve.call_args.args[0]
    listing = app._handle_request("tools/list", {}, True)
    assert [tool["name"] for tool in listing["tools"]] == ["sample.run"]
