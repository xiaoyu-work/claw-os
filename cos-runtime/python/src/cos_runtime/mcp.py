"""Manifest-bound MCP service binding for bundled Python Apps."""

from __future__ import annotations

import json
import os
from collections.abc import Callable, Mapping
from pathlib import Path
from typing import Any

from claw_os_sdk.mcp import App, ManifestError, ToolResult


RunHandler = Callable[[str, list[str]], Any]


def serve_manifest_operations(
    run_handler: RunHandler,
    manifest_path: str | os.PathLike[str] | None = None,
) -> None:
    """Serve every manifest operation through its exact MCP tool."""

    path = Path(
        manifest_path or os.environ.get("COS_APP_MANIFEST") or "app.json"
    )
    manifest = _load_manifest(path)
    app = App.from_manifest(path)
    bindings = _operation_bindings(manifest)

    for tool_name, (operation_name, operation) in bindings.items():

        def invoke(
            _operation_name: str = operation_name,
            _operation: Mapping[str, Any] = operation,
            **arguments: Any,
        ) -> Any:
            result = run_handler(
                _operation_name,
                _operation_argv(_operation, arguments),
            )
            return _tool_result(result)

        app.tool(tool_name)(invoke)

    app.run()


def _load_manifest(path: Path) -> dict[str, Any]:
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except OSError as exc:
        raise ManifestError(f"cannot read App manifest {path}: {exc}") from exc
    except json.JSONDecodeError as exc:
        raise ManifestError(f"invalid App manifest JSON {path}: {exc}") from exc
    if not isinstance(value, dict):
        raise ManifestError("App manifest must be a JSON object")
    return value


def _operation_bindings(
    manifest: Mapping[str, Any],
) -> dict[str, tuple[str, Mapping[str, Any]]]:
    app_id = manifest.get("id")
    operations = manifest.get("operations")
    service = manifest.get("mcp")
    if not isinstance(app_id, str) or not app_id:
        raise ManifestError("App manifest requires a non-empty id")
    if not isinstance(operations, dict) or not operations:
        raise ManifestError("bundled App MCP service requires operations")
    if not isinstance(service, dict) or not isinstance(service.get("tools"), list):
        raise ManifestError("bundled App manifest requires mcp.tools")

    expected = {
        f"{app_id}.{operation_name}": (operation_name, operation)
        for operation_name, operation in operations.items()
    }
    tools = service["tools"]
    actual = {
        tool.get("name"): tool
        for tool in tools
        if isinstance(tool, dict) and isinstance(tool.get("name"), str)
    }
    if len(actual) != len(tools) or set(actual) != set(expected):
        raise ManifestError(
            "mcp.tools must map one-to-one to manifest operations"
        )

    for tool_name, (_, operation) in expected.items():
        if not isinstance(operation, dict):
            raise ManifestError(f"operation for `{tool_name}` must be an object")
        tool = actual[tool_name]
        operation_args = [
            {key: value for key, value in arg.items() if key != "binding"}
            for arg in operation.get("args", [])
        ]
        if tool.get("args", []) != operation_args:
            raise ManifestError(
                f"tool `{tool_name}` arguments diverge from its operation"
            )
        if tool.get("needs", []) != operation.get("needs", []):
            raise ManifestError(
                f"tool `{tool_name}` capabilities diverge from its operation"
            )
    return expected


def _operation_argv(
    operation: Mapping[str, Any],
    arguments: Mapping[str, Any],
) -> list[str]:
    positionals: list[str] = []
    flags: list[str] = []
    for declaration in operation.get("args", []):
        name = declaration["name"]
        if name not in arguments:
            continue
        values = (
            arguments[name]
            if declaration.get("repeatable", False)
            else [arguments[name]]
        )
        binding = declaration.get(
            "binding",
            "flag" if declaration["kind"] == "bool" else "positional",
        )
        if binding == "positional":
            positionals.extend(_scalar_text(value) for value in values)
            continue
        flag = f"--{name.replace('_', '-')}"
        for value in values:
            if declaration["kind"] == "bool":
                flags.append(flag if value else f"{flag}=false")
                continue
            text = _scalar_text(value)
            if text.startswith("--"):
                flags.append(f"{flag}={text}")
            else:
                flags.extend((flag, text))

    if any(value.startswith("--") for value in positionals):
        return [*flags, "--", *positionals]
    return [*positionals, *flags]


def _scalar_text(value: Any) -> str:
    if isinstance(value, str):
        return value
    if isinstance(value, bool):
        return "true" if value else "false"
    if isinstance(value, (int, float)):
        return str(value)
    raise TypeError(f"unsupported manifest argument value: {type(value).__name__}")


def _tool_result(value: Any) -> Any:
    if not isinstance(value, dict):
        return value
    if "error" not in value and value.get("ok") is not False:
        return value
    text = value.get("error")
    if not isinstance(text, str) or not text:
        text = json.dumps(value, ensure_ascii=False, separators=(",", ":"))
    return ToolResult(
        content=[{"type": "text", "text": text}],
        is_error=True,
        structured_content=value,
    )
