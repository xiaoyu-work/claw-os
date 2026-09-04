from claw_os_sdk.mcp import App

from main import databases, execute, query, schema, tables


app = App.from_manifest()


@app.tool("db.query")
def db_query(database: str, sql: str) -> dict[str, object]:
    return query(database, sql)


@app.tool("db.exec")
def db_execute(database: str, sql: str) -> dict[str, object]:
    return execute(database, sql)


@app.tool("db.tables")
def db_tables(database: str) -> dict[str, object]:
    return tables(database)


@app.tool("db.schema")
def db_schema(database: str, table: str) -> dict[str, object]:
    return schema(database, table)


@app.tool("db.databases")
def db_databases() -> dict[str, object]:
    return databases()


if __name__ == "__main__":
    app.serve()
