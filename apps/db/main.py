"""SQLite database — create, query, and manage databases."""

import os
import sqlite3

DATA_DIR = os.environ.get("COS_DATA_DIR", "/var/lib/cos")
DB_DIR = os.path.join(DATA_DIR, "db")
MAX_ROWS = 1000  # Maximum rows returned from a single query


def _db_path(name):
    """Return the full path for a database name, creating the directory if needed."""
    os.makedirs(DB_DIR, exist_ok=True)
    return os.path.join(DB_DIR, f"{name}.db")


def cmd_query(args):
    """Run a SELECT query on a database."""
    if len(args) < 2:
        return {"error": "usage: db query <database> <sql>"}
    name = args[0]
    sql = " ".join(args[1:])
    path = _db_path(name)
    try:
        with sqlite3.connect(path) as conn:
            cur = conn.execute(sql)
            columns = [desc[0] for desc in cur.description] if cur.description else []
            rows = cur.fetchall()
            total_rows = len(rows)
            truncated = total_rows > MAX_ROWS
            if truncated:
                rows = rows[:MAX_ROWS]
            result = {
                "database": name,
                "columns": columns,
                "rows": [list(r) for r in rows],
                "count": len(rows),
            }
            if truncated:
                result["truncated"] = True
                result["total_rows"] = total_rows
            return result
    except sqlite3.Error as e:
        return {"error": str(e)}


def cmd_exec(args):
    """Execute SQL statements (CREATE, INSERT, UPDATE, DELETE)."""
    if len(args) < 2:
        return {"error": "usage: db exec <database> <sql>"}
    name = args[0]
    sql = " ".join(args[1:])
    path = _db_path(name)
    try:
        with sqlite3.connect(path) as conn:
            cur = conn.execute(sql)
            conn.commit()
            return {
                "database": name,
                "statement": sql,
                "rows_affected": cur.rowcount,
            }
    except sqlite3.Error as e:
        return {"error": str(e)}


def cmd_tables(args):
    """List tables in a database."""
    if len(args) < 1:
        return {"error": "usage: db tables <database>"}
    name = args[0]
    path = _db_path(name)
    try:
        with sqlite3.connect(path) as conn:
            cur = conn.execute(
                "SELECT name FROM sqlite_master WHERE type='table' ORDER BY name"
            )
            tables = [row[0] for row in cur.fetchall()]
            return {"database": name, "tables": tables}
    except sqlite3.Error as e:
        return {"error": str(e)}


def cmd_schema(args):
    """Show the CREATE TABLE statement for a table."""
    if len(args) < 2:
        return {"error": "usage: db schema <database> <table>"}
    name = args[0]
    table = args[1]
    path = _db_path(name)
    try:
        with sqlite3.connect(path) as conn:
            cur = conn.execute(
                "SELECT sql FROM sqlite_master WHERE type='table' AND name=?",
                (table,),
            )
            row = cur.fetchone()
            if row is None:
                return {"error": f"table not found: {table}"}
            return {"database": name, "table": table, "schema": row[0]}
    except sqlite3.Error as e:
        return {"error": str(e)}


def cmd_databases(args):
    """List all databases in the data directory."""
    os.makedirs(DB_DIR, exist_ok=True)
    databases = []
    for entry in sorted(os.listdir(DB_DIR)):
        if not entry.endswith(".db"):
            continue
        db_name = entry[:-3]
        full_path = os.path.join(DB_DIR, entry)
        size = os.path.getsize(full_path)
        try:
            with sqlite3.connect(full_path) as conn:
                cur = conn.execute(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type='table'"
                )
                table_count = cur.fetchone()[0]
        except sqlite3.Error:
            table_count = 0
        databases.append({"name": db_name, "size": size, "tables": table_count})
    return {"databases": databases}


COMMANDS = {
    "query": cmd_query,
    "exec": cmd_exec,
    "tables": cmd_tables,
    "schema": cmd_schema,
    "databases": cmd_databases,
}


def run(command, args):
    """Entry point called by cos."""
    handler = COMMANDS.get(command)
    if handler is None:
        return {"error": f"unknown command: {command}"}
    return handler(args)
