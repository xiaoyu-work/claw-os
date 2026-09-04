"""Static contract checks for command-line tool adapters."""

from __future__ import annotations

import ast
import json
from pathlib import Path

from jsonschema import Draft202012Validator


ADAPTERS_ROOT = Path(__file__).parent
REPO_ROOT = ADAPTERS_ROOT.parent


def _manifest_paths() -> list[Path]:
    return sorted(ADAPTERS_ROOT.glob("*/app.json"))


def _bound_tools(source: Path) -> set[str]:
    tree = ast.parse(source.read_text(encoding="utf-8"), filename=str(source))
    names: set[str] = set()
    for node in ast.walk(tree):
        if not isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)):
            continue
        for decorator in node.decorator_list:
            if (
                isinstance(decorator, ast.Call)
                and isinstance(decorator.func, ast.Attribute)
                and isinstance(decorator.func.value, ast.Name)
                and decorator.func.value.id == "app"
                and decorator.func.attr == "tool"
                and len(decorator.args) == 1
                and not decorator.keywords
                and isinstance(decorator.args[0], ast.Constant)
                and isinstance(decorator.args[0].value, str)
            ):
                names.add(decorator.args[0].value)
    return names


def test_adapters_use_the_published_app_manifest() -> None:
    schema = json.loads(
        (REPO_ROOT / "claw-os-sdk/wire/v1/manifest.schema.json").read_text(
            encoding="utf-8"
        )
    )
    validator = Draft202012Validator(schema)
    paths = _manifest_paths()
    assert paths
    for path in paths:
        validator.validate(json.loads(path.read_text(encoding="utf-8")))
        assert not path.with_name("manifest.json").exists()


def test_adapter_handlers_match_manifest_tools() -> None:
    for path in _manifest_paths():
        manifest = json.loads(path.read_text(encoding="utf-8"))
        declared = {tool["name"] for tool in manifest["mcp"]["tools"]}
        assert _bound_tools(path.with_name("main.py")) == declared
