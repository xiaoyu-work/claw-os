"""Static invariants for first-party app manifests.

These checks parse source without importing app entrypoints. They keep operation
dispatch and parser shape subordinate to app.json without running app code.
"""

from __future__ import annotations

import ast
import json
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
            elif keyword.arg in {"value_flags", "bool_flags"}:
                values = _strings(keyword.value, {})
                flags.update(value.replace("-", "_") for value in values)
    return positionals, flags


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
            if _binding(arg) == "flag"
        }
        missing = _parser_flags(tree) - declared
        if missing:
            drift[str(path.relative_to(APPS_ROOT))] = sorted(missing)
    assert not drift, "\n".join(drift)


def test_gateway_list_bindings_match_manifests() -> None:
    drift = {}
    for path, tree, manifest in _sources():
        positionals, flags = _gateway_list_contract(tree)
        if not positionals and not flags:
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
        if positionals != manifest_positionals or flags != manifest_flags:
            drift[str(path.relative_to(APPS_ROOT))] = {
                "parser_positionals": positionals,
                "manifest_positionals": manifest_positionals,
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
            for arg in declaration.get("args", []):
                if _binding(arg) != "positional":
                    continue
                if not arg.get("required", False):
                    optional_seen = True
                elif optional_seen:
                    drift.append(f"{path}:{surface} optional positional before {arg['name']}")
            for need in declaration.get("needs", []):
                scope = need.get("scope", {})
                fixed = scope.get("scope", {}) if scope.get("kind") == "fixed" else {}
                value = fixed.get("value")
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
                if parsed["binding"] != _binding(arg) and any(
                    candidate["name"] == parsed["name"]
                    and candidate["binding"] == _binding(arg)
                    for candidate in parsed_args
                ):
                    # argparse can retain a compatibility alias while app.json
                    # names the canonical binding (net download's output).
                    continue
                if _binding(arg) != parsed["binding"]:
                    drift.append(f"{path}:{operation}.{parsed['name']} binding")
                handler_required = bool(arg.get("required", False)) or bool(
                    arg.get("trusted_resolver")
                )
                if handler_required != parsed["required"]:
                    drift.append(f"{path}:{operation}.{parsed['name']} required")
                if parsed["kind"] is not None and arg.get("kind") != parsed["kind"]:
                    drift.append(f"{path}:{operation}.{parsed['name']} kind")
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
        "from-arg-map": ({"arg", "values"}, {"scope", "wild_when"}),
        "from-arg-or-wild": ({"arg", "wild_when"}, {"scope", "values"}),
        "fixed": ({"scope"}, {"arg", "values", "wild_when"}),
        "wild": (set(), {"arg", "scope", "values", "wild_when"}),
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
            if session and (
                "default_from" in arg or "trusted_resolver" in arg
            ):
                drift.append(f"{path}:{surface}.{arg.get('name')} session resolver")
            if arg.get("trusted_resolver") and (
                app_id != "email"
                or arg.get("name") != "provider"
                or arg.get("kind") != "name"
                or _binding(arg) != "flag"
                or arg.get("required", False)
            ):
                drift.append(f"{path}:{surface}.{arg.get('name')} trusted resolver")
            if "default" in arg:
                value = arg["default"]
                kind = arg.get("kind")
                valid = (
                    (kind in {"path", "host", "name", "text"} and isinstance(value, str))
                    or (kind == "bool" and isinstance(value, bool))
                    or (
                        kind == "integer"
                        and isinstance(value, int)
                        and not isinstance(value, bool)
                    )
                    or (
                        kind == "number"
                        and isinstance(value, (int, float))
                        and not isinstance(value, bool)
                    )
                )
                if not valid:
                    drift.append(f"{path}:{surface}.{arg.get('name')} default type")

    def check_needs(path, surface, needs):
        for need in needs:
            if need.get("verb") not in verbs:
                drift.append(f"{path}:{surface} unknown verb {need.get('verb')}")
            scope = need.get("scope", {})
            kind = scope.get("kind")
            if kind not in scope_kinds:
                drift.append(f"{path}:{surface} unknown scope binding {kind}")
                continue
            required, forbidden = payloads[kind]
            fields = set(scope)
            if not required <= fields or forbidden & fields:
                drift.append(f"{path}:{surface} invalid {kind} payload")

    for manifest_path in sorted(APPS_ROOT.rglob("app.json")):
        manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
        for name, operation in manifest.get("operations", {}).items():
            check_args(
                manifest_path, manifest["id"], name, operation.get("args", [])
            )
            check_needs(manifest_path, name, operation.get("needs", []))
        for tool in manifest.get("session", {}).get("tools", []):
            check_args(
                manifest_path,
                manifest["id"],
                tool["name"],
                tool.get("args", []),
                session=True,
            )
            check_needs(manifest_path, tool["name"], tool.get("needs", []))
    assert not drift, "\n".join(drift)


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
    result = subprocess.run(
        [sys.executable, "wire/codegen.py", "--check"],
        cwd=sdk_root,
        text=True,
        capture_output=True,
    )
    assert result.returncode == 0, result.stdout + result.stderr
