"""Static invariants for first-party app manifests.

These checks parse source without importing app entrypoints. They keep operation
dispatch and parser shape subordinate to app.json without running app code.
"""

from __future__ import annotations

import ast
import json
import re
from pathlib import Path


APPS_ROOT = Path(__file__).parent
FLAG = re.compile(r"^--([a-z][a-z0-9-]*)(?:=.*)?$")


def _assignments(tree: ast.Module, run: ast.FunctionDef) -> dict[str, ast.expr]:
    values: dict[str, ast.expr] = {}
    for statement in [*tree.body, *run.body]:
        if (
            isinstance(statement, ast.Assign)
            and len(statement.targets) == 1
            and isinstance(statement.targets[0], ast.Name)
        ):
            values[statement.targets[0].id] = statement.value
    return values


def _strings(
    node: ast.AST,
    assignments: dict[str, ast.expr],
    seen: frozenset[str] = frozenset(),
) -> set[str]:
    if isinstance(node, ast.Constant) and isinstance(node.value, str):
        return {node.value}
    if isinstance(node, (ast.Set, ast.List, ast.Tuple)):
        return set().union(*(_strings(item, assignments, seen) for item in node.elts))
    if isinstance(node, ast.Dict):
        return set().union(
            *(_strings(key, assignments, seen) for key in node.keys if key is not None)
        )
    if (
        isinstance(node, ast.Call)
        and isinstance(node.func, ast.Name)
        and node.func.id in {"set", "frozenset", "list", "tuple"}
        and node.args
    ):
        return _strings(node.args[0], assignments, seen)
    if isinstance(node, ast.BinOp) and isinstance(node.op, (ast.BitOr, ast.Add)):
        return _strings(node.left, assignments, seen) | _strings(
            node.right, assignments, seen
        )
    if (
        isinstance(node, ast.Name)
        and node.id in assignments
        and node.id not in seen
    ):
        return _strings(assignments[node.id], assignments, seen | {node.id})
    return set()


def _dispatch_operations(tree: ast.Module) -> set[str]:
    run = next(
        node
        for node in tree.body
        if isinstance(node, ast.FunctionDef) and node.name == "run"
    )
    assignments = _assignments(tree, run)
    operations: set[str] = set()
    for node in ast.walk(run):
        if (
            isinstance(node, ast.Compare)
            and isinstance(node.left, ast.Name)
            and node.left.id == "command"
        ):
            for comparator in node.comparators:
                operations.update(_strings(comparator, assignments))
        if (
            isinstance(node, ast.Call)
            and isinstance(node.func, ast.Attribute)
            and node.func.attr == "get"
            and node.args
            and isinstance(node.args[0], ast.Name)
            and node.args[0].id == "command"
        ):
            operations.update(_strings(node.func.value, assignments))
    return operations


def _parser_flags(tree: ast.Module) -> set[str]:
    flags: set[str] = set()
    for node in ast.walk(tree):
        candidates: list[ast.AST] = []
        if isinstance(node, ast.Compare):
            candidates = [node.left, *node.comparators]
        elif (
            isinstance(node, ast.Call)
            and isinstance(node.func, ast.Attribute)
            and node.func.attr in {"add_argument", "startswith"}
        ):
            candidates = list(node.args)
        elif isinstance(node, ast.Call) and any(
            isinstance(arg, ast.Name) and arg.id in {"args", "argv", "rest"}
            for arg in node.args
        ):
            candidates = list(node.args)
        if (
            isinstance(node, ast.Call)
            and isinstance(node.func, ast.Name)
            and node.func.id == "_parse_args"
            and len(node.args) > 1
            and isinstance(node.args[1], ast.Dict)
        ):
            flags.update(
                key.value
                for key in node.args[1].keys
                if isinstance(key, ast.Constant) and isinstance(key.value, str)
            )
        for candidate in candidates:
            for value in ast.walk(candidate):
                if isinstance(value, ast.Constant) and isinstance(value.value, str):
                    match = FLAG.match(value.value)
                    if match:
                        flags.add(match.group(1))
    return flags


def _function_map(tree: ast.Module) -> dict[str, ast.FunctionDef]:
    return {
        node.name: node
        for node in tree.body
        if isinstance(node, ast.FunctionDef)
    }


def _handler_map(tree: ast.Module) -> dict[str, str]:
    handlers: dict[str, str] = {}
    for node in ast.walk(tree):
        if not isinstance(node, ast.Dict):
            continue
        for key, value in zip(node.keys, node.values):
            if (
                isinstance(key, ast.Constant)
                and isinstance(key.value, str)
                and isinstance(value, ast.Name)
                and (value.id.startswith("cmd_") or value.id.startswith("_cmd_"))
            ):
                handlers[key.value] = value.id
    return handlers


def _literal(node: ast.AST, assignments: dict[str, ast.expr]):
    if isinstance(node, ast.Constant):
        return node.value
    if isinstance(node, ast.Name) and node.id in assignments:
        return _literal(assignments[node.id], assignments)
    return None


def _argparse_contract(
    operation: str,
    handler: ast.FunctionDef,
    functions: dict[str, ast.FunctionDef],
    assignments: dict[str, ast.expr],
) -> list[dict[str, object]]:
    parser_functions = [handler]
    for call in (node for node in ast.walk(handler) if isinstance(node, ast.Call)):
        if isinstance(call.func, ast.Name) and call.func.id in functions:
            candidate = functions[call.func.id]
            if any(
                isinstance(node, ast.Call)
                and isinstance(node.func, ast.Attribute)
                and node.func.attr == "add_argument"
                for node in ast.walk(candidate)
            ):
                parser_functions.append(candidate)

    arguments: list[dict[str, object]] = []
    for function in parser_functions:
        for call in (node for node in ast.walk(function) if isinstance(node, ast.Call)):
            if (
                not isinstance(call.func, ast.Attribute)
                or call.func.attr != "add_argument"
                or not call.args
                or not isinstance(call.args[0], ast.Constant)
                or not isinstance(call.args[0].value, str)
            ):
                continue
            raw_name = call.args[0].value
            binding = "flag" if raw_name.startswith("--") else "positional"
            name = raw_name.removeprefix("--")
            keywords = {keyword.arg: keyword.value for keyword in call.keywords if keyword.arg}
            required = binding == "positional" and _literal(
                keywords.get("nargs", ast.Constant(value=None)), assignments
            ) not in {"?", "*"}
            if "required" in keywords:
                required = bool(_literal(keywords["required"], assignments))
            kind = None
            if isinstance(keywords.get("type"), ast.Name) and keywords["type"].id == "int":
                kind = "integer"
            action = _literal(
                keywords.get("action", ast.Constant(value=None)), assignments
            )
            if action == "store_true":
                kind = "bool"
            default = _literal(
                keywords.get("default", ast.Constant(value=None)), assignments
            )
            if action == "store_true" and "default" not in keywords:
                default = False
            arguments.append(
                {
                    "operation": operation,
                    "name": name,
                    "binding": binding,
                    "required": required,
                    "kind": kind,
                    "default": default,
                }
            )
    return arguments


def _sources():
    for path in sorted(APPS_ROOT.rglob("main.py")):
        manifest_path = path.with_name("app.json")
        if manifest_path.is_file():
            yield path, ast.parse(path.read_text(encoding="utf-8")), json.loads(
                manifest_path.read_text(encoding="utf-8")
            )


def test_manifest_operations_match_dispatch() -> None:
    drift = {}
    for path, tree, manifest in _sources():
        dispatched = _dispatch_operations(tree)
        declared = set(manifest.get("operations", {}))
        if dispatched != declared:
            drift[str(path.relative_to(APPS_ROOT))] = {
                "dispatch_only": sorted(dispatched - declared),
                "manifest_only": sorted(declared - dispatched),
            }
    assert not drift, drift


def test_parser_flags_have_flag_bindings() -> None:
    drift = {}
    for path, tree, manifest in _sources():
        declared = {
            arg["name"].replace("_", "-")
            for operation in manifest.get("operations", {}).values()
            for arg in operation.get("args", [])
            if arg.get("binding", "positional") == "flag"
        }
        missing = _parser_flags(tree) - declared
        if missing:
            drift[str(path.relative_to(APPS_ROOT))] = sorted(missing)
    assert not drift, "\n".join(drift)


def test_argparse_contracts_match_manifests() -> None:
    drift: list[str] = []
    for path, tree, manifest in _sources():
        functions = _function_map(tree)
        handlers = _handler_map(tree)
        assignments = {
            node.targets[0].id: node.value
            for node in tree.body
            if isinstance(node, ast.Assign)
            and len(node.targets) == 1
            and isinstance(node.targets[0], ast.Name)
        }
        for operation, declaration in manifest.get("operations", {}).items():
            handler_name = handlers.get(operation)
            if handler_name is None:
                for candidate in (
                    f"cmd_{operation.replace('-', '_')}",
                    f"_cmd_{operation.replace('-', '_')}",
                ):
                    if candidate in functions:
                        handler_name = candidate
                        break
            if handler_name is None:
                continue
            manifest_args = {arg["name"]: arg for arg in declaration.get("args", [])}
            parsed_args = _argparse_contract(
                operation, functions[handler_name], functions, assignments
            )
            for parsed in parsed_args:
                arg = manifest_args.get(str(parsed["name"]))
                if arg is None:
                    drift.append(f"{path}:{operation} missing {parsed['name']}")
                    continue
                if parsed["binding"] != arg.get("binding", "positional") and any(
                    candidate["name"] == parsed["name"]
                    and candidate["binding"] == arg.get("binding", "positional")
                    for candidate in parsed_args
                ):
                    # argparse can retain a compatibility alias while app.json
                    # names the canonical binding (net download's output).
                    continue
                if arg.get("binding", "positional") != parsed["binding"]:
                    drift.append(f"{path}:{operation}.{parsed['name']} binding")
                if bool(arg.get("required", False)) != parsed["required"]:
                    drift.append(f"{path}:{operation}.{parsed['name']} required")
                if parsed["kind"] is not None and arg.get("kind") != parsed["kind"]:
                    drift.append(f"{path}:{operation}.{parsed['name']} kind")
                if parsed["default"] is not None and arg.get("default") != parsed["default"]:
                    drift.append(f"{path}:{operation}.{parsed['name']} default")
    assert not drift, "\n".join(drift)
