from claw_os_sdk.mcp import App

from main import clear, list_types, read, status, write


app = App.from_manifest()


@app.tool("clipboard-manager.status")
def clipboard_status(primary: bool = False) -> dict:
    return status(primary)


@app.tool("clipboard-manager.types")
def clipboard_types(primary: bool = False) -> dict:
    return list_types(primary)


@app.tool("clipboard-manager.read")
def read_clipboard(mime: str | None = None, primary: bool = False) -> dict:
    return read(mime, primary)


@app.tool("clipboard-manager.write")
def write_clipboard(
    source: str,
    mime: str | None = None,
    primary: bool = False,
) -> dict:
    return write(source, mime, primary)


@app.tool("clipboard-manager.clear")
def clear_clipboard(confirm: bool, primary: bool = False) -> dict:
    return clear(confirm, primary)


if __name__ == "__main__":
    app.serve()
