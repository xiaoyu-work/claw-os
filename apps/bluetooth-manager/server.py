from claw_os_sdk.mcp import App

from main import (
    connect,
    disconnect,
    forget,
    pair,
    pair_cancel,
    pair_respond,
    pair_status,
    power,
    scan,
    status,
    trust,
    untrust,
)


app = App.from_manifest()


@app.tool("bluetooth-manager.status")
def bluetooth_status() -> dict:
    return status()


@app.tool("bluetooth-manager.power")
def bluetooth_power(adapter: str, state: str) -> dict:
    return power(adapter, state)


@app.tool("bluetooth-manager.scan")
def bluetooth_scan(adapter: str, seconds: int = 10) -> dict:
    return scan(adapter, seconds)


@app.tool("bluetooth-manager.pair")
def bluetooth_pair(adapter: str, device: str) -> dict:
    return pair(adapter, device)


@app.tool("bluetooth-manager.pair-status")
def bluetooth_pair_status(pairing_id: str) -> dict:
    return pair_status(pairing_id)


@app.tool("bluetooth-manager.pair-respond")
def bluetooth_pair_respond(pairing_id: str, response: str) -> dict:
    return pair_respond(pairing_id, response)


@app.tool("bluetooth-manager.pair-cancel")
def bluetooth_pair_cancel(pairing_id: str) -> dict:
    return pair_cancel(pairing_id)


@app.tool("bluetooth-manager.connect")
def bluetooth_connect(adapter: str, device: str) -> dict:
    return connect(adapter, device)


@app.tool("bluetooth-manager.disconnect")
def bluetooth_disconnect(adapter: str, device: str) -> dict:
    return disconnect(adapter, device)


@app.tool("bluetooth-manager.trust")
def bluetooth_trust(adapter: str, device: str) -> dict:
    return trust(adapter, device)


@app.tool("bluetooth-manager.untrust")
def bluetooth_untrust(adapter: str, device: str) -> dict:
    return untrust(adapter, device)


@app.tool("bluetooth-manager.forget")
def bluetooth_forget(adapter: str, device: str) -> dict:
    return forget(adapter, device)


if __name__ == "__main__":
    app.serve()
