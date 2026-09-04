from claw_os_sdk.mcp import App

from main import apply, inspect, restore, validate


app = App.from_manifest()


@app.tool("config-editor.inspect")
def config_inspect(target: str) -> dict:
    return inspect(target)


@app.tool("config-editor.validate")
def config_validate(target: str, source: str) -> dict:
    return validate(target, source)


@app.tool("config-editor.apply")
def config_apply(target: str, source: str, confirm: bool) -> dict:
    return apply(target, source, confirm)


@app.tool("config-editor.restore")
def config_restore(target: str, backup_token: str, confirm: bool) -> dict:
    return restore(target, backup_token, confirm)


if __name__ == "__main__":
    app.serve()
