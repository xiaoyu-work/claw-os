from claw_os_sdk.mcp import App

from main import configure, index, search, status


app = App.from_manifest()


@app.tool("docs.search")
def docs_search(query: str, max_results: int = 20) -> dict[str, object]:
    return search(query, max_results)


@app.tool("docs.index")
def docs_index() -> dict[str, object]:
    return index()


@app.tool("docs.status")
def docs_status() -> dict[str, object]:
    return status()


@app.tool("docs.configure")
def docs_configure() -> dict[str, object]:
    return configure()


if __name__ == "__main__":
    app.serve()
