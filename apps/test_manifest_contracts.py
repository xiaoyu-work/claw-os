"""Static invariants for first-party app manifests.

These checks parse source without importing app entrypoints. They keep operation
dispatch and parser shape subordinate to app.json without running app code.
"""

from __future__ import annotations

import ast
import json
import os
import re
import subprocess
import sys
from pathlib import Path


APPS_ROOT = Path(__file__).parent
FLAG = re.compile(r"^--([a-z][a-z0-9-]*)(?:=.*)?$")


def _binding(arg: dict[str, object]) -> str:
    return str(arg.get("binding") or ("flag" if arg.get("kind") == "bool" else "positional"))


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
            and isinstance(node.func, ast.Attribute)
            and isinstance(node.func.value, ast.Name)
            and node.func.value.id == "gateway_args"
            and node.func.attr == "parse"
        ):
            for keyword in node.keywords:
                if keyword.arg in {"value_flags", "bool_flags"}:
                    flags.update(_strings(keyword.value, {}))
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


def _gateway_list_contract(tree: ast.Module):
    positionals: list[str] = []
    positional_aliases: list[str] = []
    flags: set[str] = set()
    for node in ast.walk(tree):
        if not (
            isinstance(node, ast.Call)
            and isinstance(node.func, ast.Attribute)
            and isinstance(node.func.value, ast.Name)
            and node.func.value.id == "gateway_args"
            and node.func.attr == "parse"
        ):
            continue
        for keyword in node.keywords:
            if keyword.arg == "positional":
                if isinstance(keyword.value, (ast.Tuple, ast.List)):
                    positionals.extend(
                        item.value
                        for item in keyword.value.elts
                        if isinstance(item, ast.Constant) and isinstance(item.value, str)
                    )
            elif keyword.arg == "positional_aliases":
                if isinstance(keyword.value, (ast.Tuple, ast.List)):
                    positional_aliases.extend(
                        item.value
                        for item in keyword.value.elts
                        if isinstance(item, ast.Constant) and isinstance(item.value, str)
                    )
            elif keyword.arg in {"value_flags", "bool_flags"}:
                values = _strings(keyword.value, {})
                flags.update(value.replace("-", "_") for value in values)
    return positionals, positional_aliases, flags


def _normalized_bool_flags(tree: ast.Module) -> set[str]:
    flags = set()
    for node in ast.walk(tree):
        if not (
            isinstance(node, ast.Call)
            and isinstance(node.func, ast.Name)
            and node.func.id in {
                "normalize_canonical_argv",
                "normalize_argparse_booleans",
                "parse_canonical_argv",
            }
        ):
            continue
        for keyword in node.keywords:
            if keyword.arg == "bool_flags":
                flags.update(value.replace("-", "_") for value in _strings(keyword.value, {}))
    return flags


def _parser_aliases(tree: ast.Module) -> set[str]:
    aliases = set()
    for node in ast.walk(tree):
        if not isinstance(node, ast.Call):
            continue
        if isinstance(node.func, ast.Name) and node.func.id == "parse_canonical_argv":
            for keyword in node.keywords:
                if keyword.arg == "aliases" and isinstance(keyword.value, ast.Dict):
                    aliases.update(
                        key.value
                        for key in keyword.value.keys
                        if isinstance(key, ast.Constant) and isinstance(key.value, str)
                    )
        if isinstance(node.func, ast.Attribute) and node.func.attr == "add_argument":
            option_args = [
                arg.value
                for arg in node.args
                if isinstance(arg, ast.Constant)
                and isinstance(arg.value, str)
                and arg.value.startswith("-")
            ]
            if option_args:
                aliases.update(option_args)
    return aliases


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


def _capability_verbs(
    function: ast.FunctionDef,
    functions: dict[str, ast.FunctionDef],
    seen: frozenset[str] = frozenset(),
) -> set[str]:
    if function.name in seen:
        return set()
    verbs: set[str] = set()
    seen = seen | {function.name}
    for call in (node for node in ast.walk(function) if isinstance(node, ast.Call)):
        if (
            isinstance(call.func, ast.Attribute)
            and isinstance(call.func.value, ast.Name)
            and call.func.value.id == "policy"
            and call.func.attr in {"require", "check"}
            and call.args
            and isinstance(call.args[0], ast.Constant)
            and isinstance(call.args[0].value, str)
        ):
            verbs.add(call.args[0].value)
        elif isinstance(call.func, ast.Name) and call.func.id in functions:
            verbs.update(_capability_verbs(functions[call.func.id], functions, seen))
    return verbs


def _uses_variadic_join(
    function: ast.FunctionDef,
    functions: dict[str, ast.FunctionDef],
    seen: frozenset[str] = frozenset(),
    check_direct_loops: bool = True,
) -> bool:
    if function.name in seen:
        return False
    seen = seen | {function.name}
    for loop in (
        node
        for node in ast.walk(function)
        if check_direct_loops and isinstance(node, ast.For)
    ):
        if not (
            isinstance(loop.iter, ast.Name)
            and loop.iter.id in {"args", "argv", "positionals"}
            and isinstance(loop.target, ast.Name)
        ):
            continue
        target = loop.target.id
        for call in (
            node
            for statement in loop.body
            for node in ast.walk(statement)
            if isinstance(node, ast.Call)
        ):
            if any(isinstance(node, ast.Name) and node.id == target for node in ast.walk(call)):
                if (
                    isinstance(call.func, ast.Attribute)
                    and call.func.attr in {"append", "extend", "require"}
                ):
                    return True
    for call in (node for node in ast.walk(function) if isinstance(node, ast.Call)):
        if (
            isinstance(call.func, ast.Attribute)
            and call.func.attr == "join"
            and call.args
            and any(
                isinstance(node, ast.Name)
                and node.id
                in {"args", "argv", "rest", "remaining", "positionals", "query_parts"}
                for node in ast.walk(call.args[0])
            )
        ):
            return True
        if (
            isinstance(call.func, ast.Name)
            and call.func.id in functions
            and _uses_variadic_join(
                functions[call.func.id], functions, seen, check_direct_loops=False
            )
        ):
            return True
    return False


def _reads_stdin(
    function: ast.FunctionDef,
    functions: dict[str, ast.FunctionDef],
    seen: frozenset[str] = frozenset(),
) -> bool:
    if function.name in seen:
        return False
    seen = seen | {function.name}
    for call in (node for node in ast.walk(function) if isinstance(node, ast.Call)):
        if (
            isinstance(call.func, ast.Attribute)
            and call.func.attr == "read"
            and isinstance(call.func.value, ast.Attribute)
            and call.func.value.attr == "stdin"
            and isinstance(call.func.value.value, ast.Name)
            and call.func.value.value.id == "sys"
        ):
            return True
        if (
            isinstance(call.func, ast.Name)
            and call.func.id in functions
            and _reads_stdin(functions[call.func.id], functions, seen)
        ):
            return True
    return False


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
                    "option": raw_name if binding == "flag" else None,
                    "binding": binding,
                    "required": required,
                    "kind": kind,
                    "default": default,
                    "choices": _strings(
                        keywords.get("choices", ast.Tuple(elts=[])), assignments
                    ),
                    "repeatable": action == "append"
                    or _literal(
                        keywords.get("nargs", ast.Constant(value=None)), assignments
                    )
                    in {"*", "+"},
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
            if _binding(arg) == "flag"
        }
        declared.update(
            alias.removeprefix("--")
            for operation in manifest.get("operations", {}).values()
            for arg in operation.get("args", [])
            for alias in arg.get("aliases", [])
            if alias.startswith("--")
        )
        missing = _parser_flags(tree) - declared
        if missing:
            drift[str(path.relative_to(APPS_ROOT))] = sorted(missing)
    assert not drift, "\n".join(drift)


def test_handler_option_aliases_match_manifests() -> None:
    drift = {}
    for path, tree, manifest in _sources():
        declared = {
            alias
            for operation in manifest.get("operations", {}).values()
            for arg in operation.get("args", [])
            for alias in arg.get("aliases", [])
        }
        canonical = {
            f"--{arg['name'].replace('_', '-')}"
            for operation in manifest.get("operations", {}).values()
            for arg in operation.get("args", [])
            if _binding(arg) == "flag"
        }
        parsed = _parser_aliases(tree) - canonical
        if declared != parsed:
            drift[str(path.relative_to(APPS_ROOT))] = {
                "manifest_only": sorted(declared - parsed),
                "parser_only": sorted(parsed - declared),
            }
    assert not drift, drift


def test_every_direct_list_handler_consumes_canonical_argv() -> None:
    drift = []
    for path, tree, manifest in _sources():
        source = path.read_text(encoding="utf-8")
        uses_argparse = any(
            isinstance(node, (ast.Import, ast.ImportFrom))
            and any(alias.name == "argparse" for alias in node.names)
            for node in tree.body
        )
        uses_gateway_parser = "gateway_args.parse" in source
        if (
            not uses_argparse
            and not uses_gateway_parser
            and "normalize_canonical_argv" not in source
            and "parse_canonical_argv" not in source
        ):
            drift.append(f"{path.relative_to(APPS_ROOT)} missing canonical parser")
        declared_bools = {
            arg["name"].replace("-", "_")
            for operation in manifest.get("operations", {}).values()
            for arg in operation.get("args", [])
            if arg.get("kind") == "bool" and _binding(arg) == "flag"
        }
        if not uses_gateway_parser:
            missing_bools = declared_bools - _normalized_bool_flags(tree)
            if missing_bools:
                drift.append(
                    f"{path.relative_to(APPS_ROOT)} missing bool normalization "
                    f"{sorted(missing_bools)}"
                )
    assert not drift, "\n".join(drift)


def test_canonical_positionals_are_not_reparsed_as_options() -> None:
    drift = []
    for path, tree, _manifest in _sources():
        run = next(
            node
            for node in tree.body
            if isinstance(node, ast.FunctionDef) and node.name == "run"
        )
        if not any(
            isinstance(node, ast.Call)
            and isinstance(node.func, ast.Name)
            and node.func.id == "parse_canonical_argv"
            for node in ast.walk(run)
        ):
            continue
        for function in (
            node
            for node in tree.body
            if isinstance(node, ast.FunctionDef) and node is not run
        ):
            if any(
                isinstance(node, ast.Call)
                and isinstance(node.func, ast.Attribute)
                and node.func.attr == "startswith"
                and any(
                    isinstance(arg, ast.Constant) and arg.value == "--"
                    for arg in node.args
                )
                for node in ast.walk(function)
            ):
                drift.append(f"{path}:{function.name} reparses canonical options")
    assert not drift, "\n".join(drift)


def test_gateway_list_bindings_match_manifests() -> None:
    drift = {}
    for path, tree, manifest in _sources():
        positionals, positional_aliases, flags = _gateway_list_contract(tree)
        if not positionals and not positional_aliases and not flags:
            continue
        declaration = manifest["operations"]["send"]
        manifest_positionals = [
            arg["name"]
            for arg in declaration.get("args", [])
            if _binding(arg) == "positional"
        ]
        manifest_flags = {
            arg["name"].replace("-", "_")
            for arg in declaration.get("args", [])
            if _binding(arg) == "flag"
        }
        manifest_positional_aliases = [
            arg["name"]
            for arg in declaration.get("args", [])
            if arg.get("positional_alias", False)
        ]
        if (
            positionals != manifest_positionals
            or positional_aliases != manifest_positional_aliases
            or flags != manifest_flags
        ):
            drift[str(path.relative_to(APPS_ROOT))] = {
                "parser_positionals": positionals,
                "manifest_positionals": manifest_positionals,
                "parser_positional_aliases": positional_aliases,
                "manifest_positional_aliases": manifest_positional_aliases,
                "parser_flags": sorted(flags),
                "manifest_flags": sorted(manifest_flags),
            }
    assert not drift, drift


def test_positional_order_and_fixed_path_scopes_are_unambiguous() -> None:
    drift: list[str] = []
    for path, _tree, manifest in _sources():
        for surface, declaration in [
            *manifest.get("operations", {}).items(),
            *(
                (tool["name"], tool)
                for tool in manifest.get("session", {}).get("tools", [])
            ),
        ]:
            optional_seen = False
            optional_gap_seen = False
            positional_args = [
                arg
                for arg in declaration.get("args", [])
                if _binding(arg) == "positional"
            ]
            if any(
                not arg.get("required", False)
                and "default" not in arg
                and "default_from" not in arg
                for arg in positional_args
            ) and any(
                "default" in arg or "default_from" in arg
                for arg in positional_args
            ):
                drift.append(f"{path}:{surface} mixes positional defaults and gaps")
            for index, arg in enumerate(positional_args):
                if not arg.get("required", False):
                    optional_seen = True
                    if "default" not in arg and "default_from" not in arg:
                        optional_gap_seen = True
                elif optional_seen:
                    drift.append(f"{path}:{surface} optional positional before {arg['name']}")
                if optional_gap_seen and (
                    "default" in arg or "default_from" in arg
                ):
                    drift.append(f"{path}:{surface} default follows positional gap")
                if arg.get("repeatable") and index != len(positional_args) - 1:
                    drift.append(f"{path}:{surface} repeatable positional before {arg['name']}")
            for need in declaration.get("needs", []):
                scope = need.get("scope", {})
                fixed = scope.get("scope", {}) if scope.get("kind") == "fixed" else {}
                value = fixed.get("value")
                if (
                    fixed.get("kind") in {"path", "host", "name"}
                    and value in {"*", "**", "/**", "/"}
                ):
                    drift.append(f"{path}:{surface} typed wildcard scope {value}")
                if fixed.get("kind") == "path" and isinstance(value, str) and "$" in value:
                    drift.append(f"{path}:{surface} unsupported path placeholder {value}")
    assert not drift, "\n".join(drift)


def test_handler_capability_use_is_declared() -> None:
    drift: list[str] = []
    for path, tree, manifest in _sources():
        functions = _function_map(tree)
        handlers = _handler_map(tree)
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
            used = _capability_verbs(functions[handler_name], functions)
            declared = {need["verb"] for need in declaration.get("needs", [])}
            for verb in sorted(used - declared):
                drift.append(f"{path}:{operation} uses undeclared capability {verb}")
    assert not drift, "\n".join(drift)


def test_variadic_join_handlers_declare_repeatable_positionals() -> None:
    drift = []
    for path, tree, manifest in _sources():
        if any(
            isinstance(node, (ast.Import, ast.ImportFrom))
            and any(alias.name == "argparse" for alias in node.names)
            for node in tree.body
        ):
            continue
        functions = _function_map(tree)
        handlers = _handler_map(tree)
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
            if (
                handler_name is not None
                and _uses_variadic_join(functions[handler_name], functions)
                and not any(
                    arg.get("repeatable") and _binding(arg) == "positional"
                    for arg in declaration.get("args", [])
                )
            ):
                drift.append(f"{path}:{operation} variadic join is not repeatable")
    assert not drift, "\n".join(drift)


def test_stdin_readers_require_manifest_opt_in() -> None:
    drift = []
    for path, tree, manifest in _sources():
        functions = _function_map(tree)
        handlers = _handler_map(tree)
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
            if (
                handler_name is not None
                and _reads_stdin(functions[handler_name], functions)
                and not declaration.get("stdin", False)
            ):
                drift.append(f"{path}:{operation} reads undeclared stdin")
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
                alias_binding = parsed["option"] in arg.get("aliases", [])
                if _binding(arg) != parsed["binding"] and not alias_binding:
                    drift.append(f"{path}:{operation}.{parsed['name']} binding")
                handler_required = bool(arg.get("required", False)) or arg.get(
                    "trusted_resolver"
                ) in {"email-provider", "calendar-provider"}
                if handler_required != parsed["required"]:
                    drift.append(f"{path}:{operation}.{parsed['name']} required")
                if parsed["kind"] is not None and arg.get("kind") != parsed["kind"]:
                    drift.append(f"{path}:{operation}.{parsed['name']} kind")
                if bool(arg.get("repeatable", False)) != parsed["repeatable"]:
                    drift.append(f"{path}:{operation}.{parsed['name']} repeatable")
                if parsed["choices"] and set(arg.get("choices", [])) != parsed["choices"]:
                    drift.append(f"{path}:{operation}.{parsed['name']} choices")
                if parsed["default"] is not None and arg.get("default") != parsed["default"]:
                    drift.append(f"{path}:{operation}.{parsed['name']} default")
    assert not drift, "\n".join(drift)


def test_every_manifest_matches_published_schema_contract() -> None:
    schema = json.loads(
        (APPS_ROOT.parent / "claw-os-sdk/wire/v1/manifest.schema.json").read_text(
            encoding="utf-8"
        )
    )
    defs = schema["$defs"]
    kinds = set(defs["arg"]["properties"]["kind"]["enum"])
    bindings = set(defs["arg"]["properties"]["binding"]["enum"])
    verbs = set(defs["need"]["properties"]["verb"]["enum"])
    scope_kinds = set(defs["scopeBinding"]["properties"]["kind"]["enum"])
    payloads = {
        "from-arg": ({"arg"}, {"scope", "values", "wild_when"}),
        "from-arg-map": ({"arg", "values"}, {"scope", "wild_when", "transform"}),
        "from-arg-or-wild": ({"arg", "wild_when"}, {"scope", "values", "transform"}),
        "fixed": ({"scope"}, {"arg", "values", "wild_when", "transform"}),
        "wild": (set(), {"arg", "scope", "values", "wild_when", "transform"}),
    }
    drift: list[str] = []

    def check_args(path, app_id, surface, args, *, session=False):
        for arg in args:
            if arg.get("kind") not in kinds:
                drift.append(f"{path}:{surface}.{arg.get('name')} unknown kind")
            binding = arg.get("binding")
            if binding is not None and binding not in bindings:
                drift.append(f"{path}:{surface}.{arg.get('name')} unknown binding")
            if arg.get("default") is None and "default" in arg:
                drift.append(f"{path}:{surface}.{arg.get('name')} null default")
            if (
                arg.get("kind") == "bool"
                and arg.get("required", False)
                and arg.get("choices") != [True]
            ):
                drift.append(
                    f"{path}:{surface}.{arg.get('name')} required bool must be true-only"
                )
            if session and (
                "default_from" in arg
                or "trusted_resolver" in arg
                or "aliases" in arg
                or "positional_alias" in arg
            ):
                drift.append(f"{path}:{surface}.{arg.get('name')} session resolver")
            if arg.get("positional_alias") and (
                _binding(arg) != "flag"
                or arg.get("required", False)
                or arg.get("repeatable", False)
                or arg.get("kind") == "bool"
            ):
                drift.append(f"{path}:{surface}.{arg.get('name')} positional alias shape")
            aliases = arg.get("aliases", [])
            if len(aliases) != len(set(aliases)) or any(
                re.fullmatch(r"(?:-[A-Za-z0-9]|--[a-z][a-z0-9-]*)", alias) is None
                for alias in aliases
            ):
                drift.append(f"{path}:{surface}.{arg.get('name')} invalid aliases")
            resolver_app = {
                "email-provider": "email",
                "email-host": "email",
                "calendar-provider": "calendar",
                "ntfy-server": "gateway-ntfy",
            }.get(arg.get("trusted_resolver"))
            expected_resolver_shape = (
                ("host", "host")
                if arg.get("trusted_resolver") == "email-host"
                else (
                    ("server", "text")
                    if arg.get("trusted_resolver") == "ntfy-server"
                    else ("provider", "name")
                )
            )
            if arg.get("trusted_resolver") and (
                resolver_app != app_id
                or (arg.get("name"), arg.get("kind")) != expected_resolver_shape
                or _binding(arg) != "flag"
                or arg.get("required", False)
                or "default" in arg
                or "default_from" in arg
            ):
                drift.append(f"{path}:{surface}.{arg.get('name')} invalid trusted resolver")
            if arg.get("repeatable") and (
                arg.get("kind") == "bool"
                or "default_from" in arg
                or "trusted_resolver" in arg
            ):
                drift.append(f"{path}:{surface}.{arg.get('name')} repeatable shape")
            choices = arg.get("choices", [])
            if arg.get("name") == "provider" and not choices:
                drift.append(f"{path}:{surface}.provider missing choices")
            if len(choices) != len({json.dumps(value, sort_keys=True) for value in choices}):
                drift.append(f"{path}:{surface}.{arg.get('name')} duplicate choices")
            if "default" in arg:
                value = arg["default"]
                kind = arg.get("kind")
                values = value if arg.get("repeatable") and isinstance(value, list) else [value]
                valid = (
                    (not arg.get("repeatable") or isinstance(value, list))
                    and all(
                        (
                            kind in {"path", "host", "name", "text"}
                            and isinstance(item, str)
                        )
                        or (kind == "bool" and isinstance(item, bool))
                        or (
                            kind == "integer"
                            and isinstance(item, int)
                            and not isinstance(item, bool)
                        )
                        or (
                            kind == "number"
                            and isinstance(item, (int, float))
                            and not isinstance(item, bool)
                        )
                        for item in values
                    )
                    and all(not choices or item in choices for item in values)
                )
                if not valid:
                    drift.append(f"{path}:{surface}.{arg.get('name')} default type")
        if any(arg.get("positional_alias") for arg in args) and any(
            _binding(arg) == "positional" and not arg.get("required", False)
            for arg in args
        ):
            drift.append(f"{path}:{surface} positional alias with optional positional")

    condition_kinds = set(defs["needCondition"]["properties"]["kind"]["enum"])

    def check_needs(path, surface, needs, args):
        by_name = {arg["name"]: arg for arg in args}
        for need in needs:
            if need.get("verb") not in verbs:
                drift.append(f"{path}:{surface} unknown verb {need.get('verb')}")
            scope = need.get("scope", {})
            kind = scope.get("kind")
            if kind not in scope_kinds:
                drift.append(f"{path}:{surface} unknown scope binding {kind}")
                continue
            if (
                need.get("verb") == "net.dial"
                and kind == "wild"
                and by_name.keys() & {"provider", "url", "urls", "server", "host"}
            ):
                drift.append(f"{path}:{surface} wildcard dynamic network scope")
            required, forbidden = payloads[kind]
            fields = set(scope)
            unknown = fields - {"kind", "arg", "scope", "values", "wild_when", "transform"}
            if not required <= fields or forbidden & fields or unknown:
                drift.append(f"{path}:{surface} invalid {kind} payload")
            condition = need.get("when")
            if condition is not None:
                condition_kind = condition.get("kind")
                condition_arg = condition.get("arg")
                if condition_kind not in condition_kinds or condition_arg not in by_name:
                    drift.append(f"{path}:{surface} invalid need condition")
                expected_fields = (
                    {"kind", "arg"}
                    if condition_kind == "arg-present"
                    else {"kind", "arg", "value"}
                )
                if set(condition) != expected_fields:
                    drift.append(f"{path}:{surface} invalid condition payload")
                if condition_kind == "arg-present" and "value" in condition:
                    drift.append(f"{path}:{surface} arg-present has value")
                if condition_kind in {"arg-equals", "arg-not-equals"} and "value" not in condition:
                    drift.append(f"{path}:{surface} comparison condition missing value")
                if (
                    condition_kind in {"arg-equals", "arg-not-equals"}
                    and by_name.get(condition_arg, {}).get("repeatable")
                ):
                    drift.append(f"{path}:{surface} comparison targets repeatable arg")
            bound_arg = scope.get("arg")
            if bound_arg in by_name:
                declaration = by_name[bound_arg]
                if scope.get("transform") == "parent" and declaration.get("kind") != "path":
                    drift.append(f"{path}:{surface} parent transform requires path")
                if scope.get("transform") == "url-host" and declaration.get("kind") != "text":
                    drift.append(f"{path}:{surface} url-host transform requires text")
                guaranteed = (
                    declaration.get("required", False)
                    or "default" in declaration
                    or "default_from" in declaration
                    or "trusted_resolver" in declaration
                    or declaration.get("kind") == "bool"
                )
                guarded = (
                    condition is not None and condition.get("arg") == bound_arg
                )
                if not guaranteed and not guarded:
                    drift.append(
                        f"{path}:{surface} unconditional optional binding {bound_arg}"
                    )
                if (
                    kind == "from-arg-map"
                    and condition is not None
                    and condition.get("kind") == "arg-equals"
                    and condition.get("value") not in scope.get("values", {})
                ):
                    drift.append(
                        f"{path}:{surface} active condition is unmapped"
                    )

    for manifest_path in sorted(APPS_ROOT.rglob("app.json")):
        manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
        for name, operation in manifest.get("operations", {}).items():
            check_args(
                manifest_path, manifest["id"], name, operation.get("args", [])
            )
            check_needs(
                manifest_path,
                name,
                operation.get("needs", []),
                operation.get("args", []),
            )
        for tool in manifest.get("session", {}).get("tools", []):
            check_args(
                manifest_path,
                manifest["id"],
                tool["name"],
                tool.get("args", []),
                session=True,
            )
            check_needs(
                manifest_path,
                tool["name"],
                tool.get("needs", []),
                tool.get("args", []),
            )
    assert not drift, "\n".join(drift)


def test_published_schema_validates_all_manifests_and_rejects_alias_ambiguity() -> None:
    from jsonschema import Draft202012Validator

    schema = json.loads(
        (APPS_ROOT.parent / "claw-os-sdk/wire/v1/manifest.schema.json").read_text(
            encoding="utf-8"
        )
    )
    validator = Draft202012Validator(schema)
    for path in APPS_ROOT.rglob("app.json"):
        validator.validate(json.loads(path.read_text(encoding="utf-8")))

    ambiguous = {
        "id": "ambiguous",
        "version": "1",
        "name": {"en": "Ambiguous"},
        "operations": {
            "send": {
                "label": {"en": "Send"},
                "args": [
                    {"name": "text", "kind": "text", "required": False},
                    {
                        "name": "target",
                        "kind": "name",
                        "binding": "flag",
                        "positional_alias": True,
                    },
                ],
            }
        },
    }
    assert list(validator.iter_errors(ambiguous))


def test_wire_capability_catalog_matches_kernel_and_manifests() -> None:
    schema = json.loads(
        (APPS_ROOT.parent / "claw-os-sdk/wire/v1/manifest.schema.json").read_text(
            encoding="utf-8"
        )
    )
    wire = set(schema["$defs"]["need"]["properties"]["verb"]["enum"])
    source = (APPS_ROOT.parent / "core/src/caps/verb.rs").read_text(encoding="utf-8")
    kernel = set(re.findall(r'Verb::new\("([^"]+)"\)', source))
    declared = set()
    for manifest_path in APPS_ROOT.rglob("app.json"):
        manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
        for operation in manifest.get("operations", {}).values():
            declared.update(need["verb"] for need in operation.get("needs", []))
        for tool in manifest.get("session", {}).get("tools", []):
            declared.update(need["verb"] for need in tool.get("needs", []))
    assert wire == kernel
    assert declared <= wire


def _direct_numeric_positions(node: ast.AST, caster: str) -> set[int]:
    positions = set()
    for call in (candidate for candidate in ast.walk(node) if isinstance(candidate, ast.Call)):
        if (
            isinstance(call.func, ast.Name)
            and call.func.id == caster
            and call.args
            and isinstance(call.args[0], ast.Subscript)
            and isinstance(call.args[0].value, ast.Name)
            and call.args[0].value.id == "args"
            and isinstance(call.args[0].slice, ast.Constant)
            and isinstance(call.args[0].slice.value, int)
        ):
            positions.add(call.args[0].slice.value)
    return positions


def test_direct_numeric_parsers_match_manifest_kinds() -> None:
    drift: list[str] = []
    for path, tree, manifest in _sources():
        run = next(
            node
            for node in tree.body
            if isinstance(node, ast.FunctionDef) and node.name == "run"
        )
        assignments = _assignments(tree, run)
        functions = _function_map(tree)
        handlers = _handler_map(tree)
        checks = []
        for statement in run.body:
            if (
                isinstance(statement, ast.If)
                and isinstance(statement.test, ast.Compare)
                and isinstance(statement.test.left, ast.Name)
                and statement.test.left.id == "command"
            ):
                operations = set().union(
                    *(
                        _strings(comparator, assignments)
                        for comparator in statement.test.comparators
                    )
                )
                body = ast.Module(body=statement.body, type_ignores=[])
                checks.append((operations, body))
        for operation, handler in handlers.items():
            if handler in functions:
                checks.append(({operation}, functions[handler]))

        for operations, node in checks:
            for operation in operations & set(manifest.get("operations", {})):
                positionals = [
                    arg
                    for arg in manifest["operations"][operation].get("args", [])
                    if _binding(arg) == "positional"
                ]
                for caster, expected in (("int", "integer"), ("float", "number")):
                    for index in _direct_numeric_positions(node, caster):
                        if index < len(positionals) and positionals[index]["kind"] != expected:
                            drift.append(
                                f"{path}:{operation}.{positionals[index]['name']} "
                                f"uses {caster} but declares {positionals[index]['kind']}"
                            )
    assert not drift, "\n".join(drift)


def test_wire_generation_is_fresh() -> None:
    sdk_root = APPS_ROOT.parent / "claw-os-sdk"
    environment = os.environ.copy()
    environment["PYTHONUTF8"] = "0"
    result = subprocess.run(
        [sys.executable, "wire/codegen.py", "--check"],
        cwd=sdk_root,
        env=environment,
        text=True,
        capture_output=True,
    )
    assert result.returncode == 0, result.stdout + result.stderr
