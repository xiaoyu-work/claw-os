import ast
import inspect
import json
import re
from collections.abc import Callable
from pathlib import Path
from unittest import mock

import pytest

from test_support import load_local_module


APP_DIR = Path(__file__).parent
MANIFEST_PATH = APP_DIR / "app.json"
SERVER_PATH = APP_DIR / "server.py"

main = load_local_module(APP_DIR / "main.py", "claw_test_log_main")


@pytest.fixture
def log_file(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> Path:
    path = tmp_path / "audit.jsonl"
    monkeypatch.setattr(main, "LOG_DIR", str(tmp_path))
    monkeypatch.setattr(main, "LOG_FILE", str(path))
    return path


def _store_entries(path: Path, entries: list[dict[str, object]]) -> None:
    path.write_text(
        "".join(f"{json.dumps(entry)}\n" for entry in entries),
        encoding="utf-8",
    )


def _handler_contracts() -> dict[str, tuple[list[str], dict[str, object]]]:
    contracts = {}
    for node in ast.parse(SERVER_PATH.read_text(encoding="utf-8")).body:
        if not isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)):
            continue
        tool_name = None
        for decorator in node.decorator_list:
            if (
                isinstance(decorator, ast.Call)
                and isinstance(decorator.func, ast.Attribute)
                and decorator.func.attr == "tool"
                and len(decorator.args) == 1
                and isinstance(decorator.args[0], ast.Constant)
            ):
                tool_name = decorator.args[0].value
        if tool_name is None:
            continue
        names = [argument.arg for argument in node.args.args]
        default_names = names[len(names) - len(node.args.defaults) :]
        contracts[tool_name] = (
            names,
            {
                name: ast.literal_eval(default)
                for name, default in zip(
                    default_names,
                    node.args.defaults,
                    strict=True,
                )
            },
        )
        assert all(argument.annotation is not None for argument in node.args.args)
    return contracts


def test_manifest_and_handlers_are_mcp_only_and_aligned() -> None:
    manifest = json.loads(MANIFEST_PATH.read_text(encoding="utf-8"))
    assert "operations" not in manifest

    tools = {tool["name"]: tool for tool in manifest["mcp"]["tools"]}
    assert list(tools) == ["log.read", "log.tail", "log.search", "log.write"]
    assert [need["verb"] for need in tools["log.read"]["needs"]] == [
        "data.log.read"
    ]
    assert [need["verb"] for need in tools["log.tail"]["needs"]] == [
        "data.log.read"
    ]
    assert [need["verb"] for need in tools["log.search"]["needs"]] == [
        "data.log.read"
    ]
    assert [need["verb"] for need in tools["log.write"]["needs"]] == [
        "data.log.write"
    ]
    assert all(
        need["scope"] == {"kind": "wild"}
        for tool in tools.values()
        for need in tool["needs"]
    )

    assert tools["log.read"]["args"] == [
        {
            "name": "limit",
            "kind": "integer",
            "required": False,
            "binding": "flag",
            "default": 20,
        },
        {
            "name": "app",
            "kind": "text",
            "required": False,
            "binding": "flag",
        },
        {
            "name": "status",
            "kind": "text",
            "required": False,
            "binding": "flag",
            "choices": ["ok", "error"],
        },
    ]
    assert tools["log.tail"]["args"] == [
        {
            "name": "n",
            "kind": "integer",
            "required": False,
            "binding": "positional",
            "default": 10,
        }
    ]
    assert tools["log.search"]["args"] == [
        {
            "name": "query",
            "kind": "text",
            "required": True,
            "binding": "positional",
        },
        {
            "name": "limit",
            "kind": "integer",
            "required": False,
            "binding": "flag",
            "default": 20,
        },
        {
            "name": "app",
            "kind": "text",
            "required": False,
            "binding": "flag",
        },
    ]
    assert tools["log.write"]["args"] == [
        {
            "name": "message",
            "kind": "text",
            "required": True,
            "binding": "positional",
        },
        {
            "name": "level",
            "kind": "text",
            "required": False,
            "binding": "flag",
            "choices": ["debug", "info", "warn", "error"],
            "default": "info",
        },
    ]

    server_source = SERVER_PATH.read_text(encoding="utf-8")
    assert "serve_manifest_operations" not in server_source
    assert server_source.count("App.from_manifest()") == 1
    assert _handler_contracts() == {
        "log.read": (
            ["limit", "app", "status"],
            {"limit": 20, "app": None, "status": None},
        ),
        "log.tail": (["n"], {"n": 10}),
        "log.search": (
            ["query", "limit", "app"],
            {"limit": 20, "app": None},
        ),
        "log.write": (["message", "level"], {"level": "info"}),
    }

    main_source = (APP_DIR / "main.py").read_text(encoding="utf-8")
    assert "canonical_argv" not in main_source
    assert not any(
        node.name == "run" or node.name.startswith("_cmd")
        for node in ast.parse(main_source).body
        if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef))
    )
    for function, parameters in (
        (main.read, ["limit", "app", "status"]),
        (main.tail, ["n"]),
        (main.search, ["query", "limit", "app"]),
        (main.write, ["message", "level"]),
    ):
        signature = inspect.signature(function)
        assert list(signature.parameters) == parameters
        assert all(
            parameter.annotation is not inspect.Parameter.empty
            for parameter in signature.parameters.values()
        )


def test_read_filters_and_returns_newest_first(
    log_file: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    entries = [
        {"app": "alpha", "status": "ok", "message": "Full scan started"},
        {"app": "beta", "status": "error", "message": "Full failure"},
        {"app": "alpha", "status": "error", "message": "Retry FULL"},
        {"app": "alpha", "status": "ok", "message": "Done"},
    ]
    _store_entries(log_file, entries)
    require = mock.Mock()
    monkeypatch.setattr(main.policy, "require", require)

    assert main.read(limit=1, app="alpha", status="ok") == {
        "entries": [entries[3]],
        "total": 2,
    }
    require.assert_called_once_with("data.log.read", wild=True)


def test_tail_is_chronological_and_search_preserves_stored_order(
    log_file: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    entries = [
        {"app": "alpha", "status": "ok", "message": "Full scan started"},
        {"app": "beta", "status": "error", "message": "Full failure"},
        {"app": "alpha", "status": "error", "message": "Retry FULL"},
        {"app": "alpha", "status": "ok", "message": "Done"},
    ]
    _store_entries(log_file, entries)
    require = mock.Mock()
    monkeypatch.setattr(main.policy, "require", require)

    assert main.tail(2) == {"entries": entries[-2:]}
    assert main.search("full", limit=1, app="alpha") == {
        "entries": [entries[0]],
        "total": 2,
    }
    assert require.call_args_list == [
        mock.call("data.log.read", wild=True),
        mock.call("data.log.read", wild=True),
    ]


def test_write_appends_utf8_jsonl_with_default_and_explicit_levels(
    log_file: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    require = mock.Mock()
    monkeypatch.setattr(main.policy, "require", require)

    first = main.write("Déploiement ✅")
    second = main.write("Failed", "error")

    assert first["source"] == second["source"] == "user"
    assert first["level"] == "info"
    assert second["level"] == "error"
    assert first["message"] == "Déploiement ✅"
    assert second["message"] == "Failed"
    assert re.fullmatch(
        r"\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}Z",
        first["timestamp"],
    )
    raw = log_file.read_bytes()
    assert raw.endswith(b"\n")
    assert [
        json.loads(line)
        for line in raw.decode("utf-8").splitlines()
    ] == [first, second]
    assert require.call_args_list == [
        mock.call("data.log.write", wild=True),
        mock.call("data.log.write", wild=True),
    ]


def test_missing_log_returns_empty_results(
    log_file: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    require = mock.Mock()
    monkeypatch.setattr(main.policy, "require", require)

    assert main.read() == {"entries": [], "total": 0}
    assert not log_file.exists()
    require.assert_called_once_with("data.log.read", wild=True)


@pytest.mark.parametrize(
    ("invoke", "message"),
    [
        (lambda: main.read(True), "limit"),
        (lambda: main.read(0), "limit"),
        (lambda: main.read(1001), "limit"),
        (lambda: main.read(app=""), "app"),
        (lambda: main.read(app="bad.app"), "app"),
        (lambda: main.read(app=7), "app"),
        (lambda: main.read(status="OK"), "status"),
        (lambda: main.read(status=1), "status"),
        (lambda: main.tail(False), "n"),
        (lambda: main.tail("10"), "n"),
        (lambda: main.tail(1001), "n"),
        (lambda: main.search(None), "query"),
        (lambda: main.search(" \t"), "query"),
        (lambda: main.search("term", limit=0), "limit"),
        (lambda: main.search("term", app=[]), "app"),
        (lambda: main.write(None), "message"),
        (lambda: main.write("\n"), "message"),
        (lambda: main.write("entry", "INFO"), "level"),
        (lambda: main.write("entry", None), "level"),
    ],
)
def test_validation_happens_before_policy_and_storage(
    monkeypatch: pytest.MonkeyPatch,
    invoke: Callable[[], object],
    message: str,
) -> None:
    require = mock.Mock()
    read_entries = mock.Mock()
    makedirs = mock.Mock()
    monkeypatch.setattr(main.policy, "require", require)
    monkeypatch.setattr(main, "_read_entries", read_entries)
    monkeypatch.setattr(main.os, "makedirs", makedirs)

    with pytest.raises(ValueError, match=message):
        invoke()

    require.assert_not_called()
    read_entries.assert_not_called()
    makedirs.assert_not_called()


@pytest.mark.parametrize(
    ("contents", "message"),
    [
        ('{"message": "valid"}\n{\n', r"line 2 contains invalid JSON"),
        ('{"message": "valid"}\n[]\n', r"line 2 must contain a JSON object"),
    ],
)
def test_malformed_log_fails_closed_with_line_context(
    log_file: Path,
    monkeypatch: pytest.MonkeyPatch,
    contents: str,
    message: str,
) -> None:
    log_file.write_text(contents, encoding="utf-8")
    require = mock.Mock()
    monkeypatch.setattr(main.policy, "require", require)

    with pytest.raises(ValueError, match=message):
        main.read()

    require.assert_called_once_with("data.log.read", wild=True)


@pytest.mark.parametrize(
    ("invoke", "error"),
    [
        (
            lambda: main.read(),
            main.policy.PermissionDenied({"summary": "log read denied"}),
        ),
        (
            lambda: main.write("entry"),
            main.policy.PolicyUnavailable("policy unavailable"),
        ),
    ],
)
def test_policy_errors_propagate_before_storage(
    monkeypatch: pytest.MonkeyPatch,
    invoke: Callable[[], object],
    error: Exception,
) -> None:
    read_entries = mock.Mock()
    makedirs = mock.Mock()
    monkeypatch.setattr(main.policy, "require", mock.Mock(side_effect=error))
    monkeypatch.setattr(main, "_read_entries", read_entries)
    monkeypatch.setattr(main.os, "makedirs", makedirs)

    with pytest.raises(type(error)) as raised:
        invoke()

    assert raised.value is error
    read_entries.assert_not_called()
    makedirs.assert_not_called()
