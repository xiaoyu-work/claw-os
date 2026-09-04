from claw_os_sdk.mcp import App

from main import read, search, tail, write


app = App.from_manifest()


@app.tool("log.read")
def read_log(
    limit: int = 20,
    app: str | None = None,
    status: str | None = None,
) -> dict[str, object]:
    return read(limit, app, status)


@app.tool("log.tail")
def tail_log(n: int = 10) -> dict[str, object]:
    return tail(n)


@app.tool("log.search")
def search_log(
    query: str,
    limit: int = 20,
    app: str | None = None,
) -> dict[str, object]:
    return search(query, limit, app)


@app.tool("log.write")
def write_log(message: str, level: str = "info") -> dict[str, object]:
    return write(message, level)


if __name__ == "__main__":
    app.serve()
