from claw_os_sdk.mcp import App

from main import find, is_running as check_running, list_apps, open_app, recent


app = App.from_manifest()


@app.tool("launcher.list")
def launcher_list(
    include_no_display: bool = False,
    include_hidden: bool = False,
) -> dict[str, object]:
    return list_apps(include_no_display, include_hidden)


@app.tool("launcher.find")
def launcher_find(query: str, limit: int = 10) -> dict[str, object]:
    return find(query, limit)


@app.tool("launcher.open")
def launcher_open(
    app_id: str,
    uri: list[str] | None = None,
    path: list[str] | None = None,
) -> dict[str, object]:
    return open_app(app_id, uri, path)


@app.tool("launcher.recent")
def launcher_recent(limit: int = 20) -> dict[str, object]:
    return recent(limit)


@app.tool("launcher.is-running")
def is_running(app_id: str) -> dict[str, object]:
    return check_running(app_id)


if __name__ == "__main__":
    app.serve()
