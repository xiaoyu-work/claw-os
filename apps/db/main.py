"""SQLite database — create, query, and manage databases."""

from contextlib import closing
import os
from pathlib import Path
import re
import sqlite3

from cos_runtime import policy

DATA_DIR = os.environ.get("COS_DATA_DIR", "/var/lib/cos")
DB_DIR = os.path.join(DATA_DIR, "db")
MAX_ROWS = 1000  # Maximum rows returned from a single query

# Database names must look like ordinary filenames — letters, digits,
# underscore, dash, dot. Anything else (slashes, NULs, `..`, leading
# dot, control chars) is rejected at the gate. This is what stops the
# old `_db_path(name)` from being turned into a path-traversal
# primitive by a caller passing ``../../../tmp/pwned``.
_VALID_DB_NAME = re.compile(r"^[A-Za-z0-9_][A-Za-z0-9_\-.]*$")
_READ_ONLY_ACTIONS = frozenset(
    {
        sqlite3.SQLITE_FUNCTION,
        sqlite3.SQLITE_READ,
        sqlite3.SQLITE_RECURSIVE,
        sqlite3.SQLITE_SELECT,
    }
)


class _InvalidName(ValueError):
    """Sentinel raised by ``_db_path`` for a bad database name."""


def _validate_name(name):
    """Return ``name`` if it is a safe single path component, else raise.

    Refuses:

    * empty / non-string input
    * anything containing ``/``, ``\\``, ``\\0``, or ``..``
    * leading ``.`` (hidden / dotfile abuse)
    """
    if not isinstance(name, str) or not name:
        raise _InvalidName("database name must be a non-empty string")
    if (
        "/" in name
        or "\\" in name
        or "\x00" in name
        or ".." in name
        or name.startswith(".")
    ):
        raise _InvalidName(
            f"invalid database name {name!r}: must be a single path component"
        )
    if _VALID_DB_NAME.fullmatch(name) is None:
        raise _InvalidName(
            f"invalid database name {name!r}: only [A-Za-z0-9_.-] allowed"
        )
    return name


def _db_path(name):
    """Return the full path for a database name, creating the directory if needed.

    SECURITY: ``name`` is validated against a strict whitelist so a
    caller passing ``../../../tmp/pwned`` can no longer escape
    ``DB_DIR`` to read or write arbitrary files.
    """
    safe = _validate_name(name)
    os.makedirs(DB_DIR, exist_ok=True)
    full = os.path.join(DB_DIR, f"{safe}.db")
    # Defence in depth: after joining, confirm the result still sits
    # directly inside DB_DIR with no symlink escape.
    real_dir = os.path.realpath(DB_DIR)
    real_full = os.path.realpath(full)
    if os.path.dirname(real_full) != real_dir:
        raise _InvalidName(f"resolved path {real_full!r} escapes db dir {real_dir!r}")
    return real_full


def _validate_sql(sql: object) -> str:
    if not isinstance(sql, str) or not sql.strip():
        raise ValueError("sql must be a non-empty string")
    return sql


def _validate_table(table: object) -> str:
    if not isinstance(table, str) or not table:
        raise ValueError("table must be a non-empty string")
    return table


def _open_read_only(path: str) -> sqlite3.Connection:
    connection = sqlite3.connect(f"{Path(path).as_uri()}?mode=ro", uri=True)
    connection.execute("PRAGMA query_only = ON")
    return connection


def _read_authorizer(
    action: int,
    _arg1: str | None,
    _arg2: str | None,
    _database: str | None,
    _trigger: str | None,
) -> int:
    if action in _READ_ONLY_ACTIONS:
        return sqlite3.SQLITE_OK
    return sqlite3.SQLITE_DENY


def _same_database_authorizer(
    action: int,
    _arg1: str | None,
    _arg2: str | None,
    _database: str | None,
    _trigger: str | None,
) -> int:
    if action in {sqlite3.SQLITE_ATTACH, sqlite3.SQLITE_DETACH}:
        return sqlite3.SQLITE_DENY
    return sqlite3.SQLITE_OK


def _bounded_rows(cursor: sqlite3.Cursor) -> tuple[list[list[object]], int | None]:
    rows = cursor.fetchmany(MAX_ROWS + 1)
    if len(rows) <= MAX_ROWS:
        return [list(row) for row in rows], None

    total_rows = len(rows)
    while batch := cursor.fetchmany(MAX_ROWS):
        total_rows += len(batch)
    return [list(row) for row in rows[:MAX_ROWS]], total_rows


def query(database: str, sql: str) -> dict[str, object]:
    """Run a SELECT query on a database."""
    database = _validate_name(database)
    sql = _validate_sql(sql)
    policy.require("data.db.read", name=database)
    path = _db_path(database)
    try:
        with closing(_open_read_only(path)) as connection:
            connection.set_authorizer(_read_authorizer)
            cursor = connection.execute(sql)
            columns = (
                [description[0] for description in cursor.description]
                if cursor.description
                else []
            )
            rows, total_rows = _bounded_rows(cursor)
            result: dict[str, object] = {
                "database": database,
                "columns": columns,
                "rows": rows,
                "count": len(rows),
            }
            if total_rows is not None:
                result["truncated"] = True
                result["total_rows"] = total_rows
            return result
    except sqlite3.Error as exc:
        raise RuntimeError(f"database query failed: {exc}") from exc


def execute(database: str, sql: str) -> dict[str, object]:
    """Execute one SQL statement (CREATE, INSERT, UPDATE, DELETE)."""
    database = _validate_name(database)
    sql = _validate_sql(sql)
    policy.require("data.db.write", name=database)
    path = _db_path(database)
    try:
        with closing(sqlite3.connect(path)) as connection:
            connection.set_authorizer(_same_database_authorizer)
            cursor = connection.execute(sql)
            connection.commit()
            return {
                "database": database,
                "statement": sql,
                "rows_affected": cursor.rowcount,
            }
    except sqlite3.Error as exc:
        raise RuntimeError(f"database execution failed: {exc}") from exc


def tables(database: str) -> dict[str, object]:
    """List tables in a database."""
    database = _validate_name(database)
    policy.require("data.db.read", name=database)
    path = _db_path(database)
    try:
        with closing(_open_read_only(path)) as connection:
            cursor = connection.execute(
                "SELECT name FROM sqlite_master WHERE type='table' ORDER BY name"
            )
            names = [row[0] for row in cursor.fetchall()]
            return {"database": database, "tables": names}
    except sqlite3.Error as exc:
        raise RuntimeError(f"database table listing failed: {exc}") from exc


def schema(database: str, table: str) -> dict[str, object]:
    """Show the CREATE TABLE statement for a table."""
    database = _validate_name(database)
    table = _validate_table(table)
    policy.require("data.db.read", name=database)
    path = _db_path(database)
    try:
        with closing(_open_read_only(path)) as connection:
            cursor = connection.execute(
                "SELECT sql FROM sqlite_master WHERE type='table' AND name=?",
                (table,),
            )
            row = cursor.fetchone()
            if row is None:
                raise ValueError(f"table not found: {table}")
            return {"database": database, "table": table, "schema": row[0]}
    except sqlite3.Error as exc:
        raise RuntimeError(f"database schema lookup failed: {exc}") from exc


def databases() -> dict[str, object]:
    """List all databases in the data directory.

    Filters to entries with a ``.db`` suffix that pass the same
    name-validation as ``_db_path`` — so a malicious file someone
    dropped into ``DB_DIR`` (e.g. by writing through a different
    code path) isn't surfaced as a usable database name.
    """
    policy.require("data.db.read", wild=True)
    os.makedirs(DB_DIR, exist_ok=True)
    databases = []
    for entry in sorted(os.listdir(DB_DIR)):
        if not entry.endswith(".db"):
            continue
        db_name = entry[:-3]
        try:
            _validate_name(db_name)
        except _InvalidName:
            continue
        full_path = _db_path(db_name)
        size = os.path.getsize(full_path)
        try:
            with closing(_open_read_only(full_path)) as connection:
                cursor = connection.execute(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type='table'"
                )
                table_count = cursor.fetchone()[0]
        except sqlite3.Error as exc:
            raise RuntimeError(
                f"database listing failed for {db_name}: {exc}"
            ) from exc
        databases.append({"name": db_name, "size": size, "tables": table_count})
    return {"databases": databases}
