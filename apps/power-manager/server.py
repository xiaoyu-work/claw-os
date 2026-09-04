from claw_os_sdk.mcp import App

from main import request_power, status


app = App.from_manifest()


@app.tool("power-manager.status")
def power_status() -> dict:
    return status()


@app.tool("power-manager.suspend")
def suspend(confirm: bool) -> dict:
    return request_power("suspend", confirm)


@app.tool("power-manager.hibernate")
def hibernate(confirm: bool) -> dict:
    return request_power("hibernate", confirm)


@app.tool("power-manager.hybrid-sleep")
def hybrid_sleep(confirm: bool) -> dict:
    return request_power("hybrid-sleep", confirm)


@app.tool("power-manager.suspend-then-hibernate")
def suspend_then_hibernate(confirm: bool) -> dict:
    return request_power("suspend-then-hibernate", confirm)


@app.tool("power-manager.reboot")
def reboot(confirm: bool) -> dict:
    return request_power("reboot", confirm)


@app.tool("power-manager.poweroff")
def poweroff(confirm: bool) -> dict:
    return request_power("poweroff", confirm)


if __name__ == "__main__":
    app.serve()
