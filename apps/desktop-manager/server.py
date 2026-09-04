from claw_os_sdk.mcp import App

from main import close_window, focus_window, list_windows, restart_application


app = App.from_manifest()


@app.tool("desktop-manager.list")
def desktop_list() -> dict:
    return list_windows()


@app.tool("desktop-manager.focus")
def focus(identifier: str) -> dict:
    return focus_window(identifier)


@app.tool("desktop-manager.close")
def close(identifier: str) -> dict:
    return close_window(identifier)


@app.tool("desktop-manager.restart")
def restart(identifier: str, app_id: str) -> dict:
    return restart_application(identifier, app_id)


if __name__ == "__main__":
    app.serve()
