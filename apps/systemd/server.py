from claw_os_sdk.mcp import App

from main import control, status


app = App.from_manifest()


@app.tool("systemd.status")
def systemd_status(unit: str) -> dict:
    return status(unit)


@app.tool("systemd.start")
def start(unit: str) -> dict:
    return control("start", unit)


@app.tool("systemd.stop")
def stop(unit: str) -> dict:
    return control("stop", unit)


@app.tool("systemd.restart")
def restart(unit: str) -> dict:
    return control("restart", unit)


@app.tool("systemd.reload")
def reload(unit: str) -> dict:
    return control("reload", unit)


@app.tool("systemd.enable")
def enable(unit: str) -> dict:
    return control("enable", unit)


@app.tool("systemd.disable")
def disable(unit: str) -> dict:
    return control("disable", unit)


if __name__ == "__main__":
    app.serve()
