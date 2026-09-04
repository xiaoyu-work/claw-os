import ast
import json
import pathlib
import sqlite3
from unittest import mock

import pytest

from test_support import load_local_module


APP_DIR = pathlib.Path(__file__).parent
MANIFEST_PATH = APP_DIR / "app.json"
SERVER_PATH = APP_DIR / "server.py"
TOOL_NAMES = [
    "db.query",
    "db.exec",
    "db.tables",
    "db.schema",
    "db.databases",
]

main = load_local_module(
    APP_DIR / "main.py",
    "claw_test_db_main",
    clear_modules=("_shared",),
)


def _server_bindings() -> dict[str, ast.FunctionDef]:
    bindings: dict[str, ast.FunctionDef] = {}
    for node in ast.parse(SERVER_PATH.read_text(encoding="utf-8")).body:
        if not isinstance(node, ast.FunctionDef):
            continue
        for decorator in node.decorator_list:
            if (
                isinstance(decorator, ast.Call)
                and isinstance(decorator.func, ast.Attribute)
                and isinstance(decorator.func.value, ast.Name)
                and decorator.func.value.id == "app"
                and decorator.func.attr == "tool"
                and len(decorator.args) == 1
                and isinstance(decorator.args[0], ast.Constant)
                and isinstance(decorator.args[0].value, str)
            ):
                bindings[decorator.args[0].value] = node
    return bindings


def _argument_contract(function: ast.FunctionDef) -> tuple[list[str], list[str]]:
    return (
        [argument.arg for argument in function.args.args],
        [
            ast.unparse(argument.annotation)
            for argument in function.args.args
            if argument.annotation is not None
        ],
    )


def _from_database(verb: str) -> list[dict[str, object]]:
    return [
        {
            "verb": verb,
            "scope": {"kind": "from-arg", "arg": "database"},
            "why": {
                "en": {
                    "data.db.read": "Read rows from the database you asked to query.",
                    "data.db.write": "Modify the database you asked to execute SQL on.",
                }[verb]
            },
        }
    ]


def test_manifest_and_handlers_are_mcp_only_and_aligned() -> None:
    manifest = json.loads(MANIFEST_PATH.read_text(encoding="utf-8"))
    assert "operations" not in manifest

    tools = manifest["mcp"]["tools"]
    assert [tool["name"] for tool in tools] == TOOL_NAMES
    tool_map = {tool["name"]: tool for tool in tools}
    database_arg = {
        "name": "database",
        "kind": "name",
        "required": True,
        "binding": "positional",
    }
    sql_arg = {
        "name": "sql",
        "kind": "text",
        "required": True,
        "binding": "positional",
    }
    assert tool_map["db.query"]["args"] == [database_arg, sql_arg]
    assert tool_map["db.exec"]["args"] == [database_arg, sql_arg]
    assert tool_map["db.tables"]["args"] == [database_arg]
    assert tool_map["db.schema"]["args"] == [
        database_arg,
        {
            "name": "table",
            "kind": "text",
            "required": True,
            "binding": "positional",
        },
    ]
    assert tool_map["db.databases"].get("args", []) == []
    assert tool_map["db.query"]["needs"] == _from_database("data.db.read")
    assert tool_map["db.exec"]["needs"] == _from_database("data.db.write")
    assert tool_map["db.databases"]["needs"][0]["scope"] == {"kind": "wild"}

    server_source = SERVER_PATH.read_text(encoding="utf-8")
    assert "from claw_os_sdk.mcp import App" in server_source
    assert "serve_manifest_operations" not in server_source
    bindings = _server_bindings()
    assert list(bindings) == TOOL_NAMES
    assert _argument_contract(bindings["db.query"]) == (
        ["database", "sql"],
        ["str", "str"],
    )
    assert _argument_contract(bindings["db.exec"]) == (
        ["database", "sql"],
        ["str", "str"],
    )
    assert _argument_contract(bindings["db.tables"]) == (["database"], ["str"])
    assert _argument_contract(bindings["db.schema"]) == (
        ["database", "table"],
        ["str", "str"],
    )
    assert _argument_contract(bindings["db.databases"]) == ([], [])

    main_tree = ast.parse((APP_DIR / "main.py").read_text(encoding="utf-8"))
    assert not any(
        isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef))
        and node.name == "run"
        for node in main_tree.body
    )


@pytest.fixture(autouse=True)
def isolated_database(tmp_path, monkeypatch):
    monkeypatch.setattr(main, "DB_DIR", str(tmp_path / "db"))
    with mock.patch.object(main.policy, "require") as require:
        yield require


def _create_database(name: str = "testdb", rows: int = 0) -> pathlib.Path:
    path = pathlib.Path(main._db_path(name))
    with sqlite3.connect(path) as connection:
        connection.execute(
            "CREATE TABLE items (id INTEGER PRIMARY KEY, val TEXT)"
        )
        connection.executemany(
            "INSERT INTO items (id, val) VALUES (?, ?)",
            ((index, f"row_{index}") for index in range(rows)),
        )
    return path


@pytest.mark.parametrize("row_count", [10, main.MAX_ROWS])
def test_query_returns_rows_without_truncation(row_count, isolated_database):
    _create_database(rows=row_count)

    result = main.query("testdb", "SELECT * FROM items ORDER BY id")

    assert result["count"] == row_count
    assert len(result["rows"]) == row_count
    assert "truncated" not in result
    assert "total_rows" not in result
    isolated_database.assert_called_once_with("data.db.read", name="testdb")


def test_query_limits_returned_rows_and_counts_the_remainder(isolated_database):
    total = main.MAX_ROWS + 100
    _create_database(rows=total)

    result = main.query("testdb", "SELECT * FROM items ORDER BY id")

    assert result["count"] == main.MAX_ROWS
    assert len(result["rows"]) == main.MAX_ROWS
    assert result["truncated"] is True
    assert result["total_rows"] == total
    isolated_database.assert_called_once_with("data.db.read", name="testdb")


def test_query_is_read_only(isolated_database):
    path = _create_database(rows=2)

    with pytest.raises(RuntimeError, match="database query failed"):
        main.query("testdb", "DELETE FROM items")

    with sqlite3.connect(path) as connection:
        assert connection.execute("SELECT COUNT(*) FROM items").fetchone()[0] == 2
    isolated_database.assert_called_once_with("data.db.read", name="testdb")


def test_query_cannot_attach_another_database(isolated_database):
    _create_database("testdb")
    other_path = _create_database("other")

    with pytest.raises(RuntimeError, match="database query failed"):
        main.query("testdb", f"ATTACH DATABASE '{other_path}' AS other")

    isolated_database.assert_called_once_with("data.db.read", name="testdb")


def test_query_does_not_create_a_missing_database(isolated_database):
    path = pathlib.Path(main._db_path("missing"))

    with pytest.raises(RuntimeError, match="database query failed"):
        main.query("missing", "SELECT 1")

    assert not path.exists()
    isolated_database.assert_called_once_with("data.db.read", name="missing")


def test_execute_and_schema_tools_use_exact_scopes(isolated_database):
    created = main.execute(
        "inventory",
        "CREATE TABLE products (id INTEGER PRIMARY KEY, name TEXT)",
    )
    inserted = main.execute(
        "inventory",
        "INSERT INTO products (name) VALUES ('camera')",
    )
    table_result = main.tables("inventory")
    schema_result = main.schema("inventory", "products")
    database_result = main.databases()

    assert created["database"] == "inventory"
    assert inserted["rows_affected"] == 1
    assert table_result == {"database": "inventory", "tables": ["products"]}
    assert schema_result["database"] == "inventory"
    assert schema_result["table"] == "products"
    assert schema_result["schema"].startswith("CREATE TABLE products")
    assert database_result["databases"][0]["name"] == "inventory"
    assert database_result["databases"][0]["tables"] == 1
    assert isolated_database.call_args_list == [
        mock.call("data.db.write", name="inventory"),
        mock.call("data.db.write", name="inventory"),
        mock.call("data.db.read", name="inventory"),
        mock.call("data.db.read", name="inventory"),
        mock.call("data.db.read", wild=True),
    ]


def test_execute_cannot_attach_another_database(isolated_database):
    _create_database("testdb")
    other_path = _create_database("other")

    with pytest.raises(RuntimeError, match="database execution failed"):
        main.execute("testdb", f"ATTACH DATABASE '{other_path}' AS other")

    isolated_database.assert_called_once_with("data.db.write", name="testdb")


def test_missing_table_is_an_error(isolated_database):
    _create_database()

    with pytest.raises(ValueError, match="table not found: missing"):
        main.schema("testdb", "missing")

    isolated_database.assert_called_once_with("data.db.read", name="testdb")


def test_corrupt_database_is_not_reported_as_empty(isolated_database):
    path = pathlib.Path(main._db_path("corrupt"))
    path.write_bytes(b"not a sqlite database")

    with pytest.raises(RuntimeError, match="database listing failed for corrupt"):
        main.databases()

    isolated_database.assert_called_once_with("data.db.read", wild=True)


@pytest.mark.parametrize(
    "database",
    [
        "../etc/passwd",
        "../../etc/passwd",
        "/etc/passwd",
        "foo/bar",
        "foo\\bar",
        ".hidden",
        "..parent",
        ".",
        "foo\x00bar",
        "name\n",
        "",
    ],
)
def test_invalid_database_names_are_rejected_before_policy(
    database,
    isolated_database,
):
    with pytest.raises(main._InvalidName):
        main.query(database, "SELECT 1")

    isolated_database.assert_not_called()


@pytest.mark.parametrize("sql", ["", "   ", None, 7])
def test_invalid_sql_is_rejected_before_policy(sql, isolated_database):
    with pytest.raises(ValueError, match="sql must be a non-empty string"):
        main.query("testdb", sql)

    isolated_database.assert_not_called()


def test_valid_database_name_resolves_under_database_directory():
    path = pathlib.Path(main._db_path("safe_name-1.test"))

    assert path.parent == pathlib.Path(main.DB_DIR).resolve()
