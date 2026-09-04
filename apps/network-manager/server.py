from claw_os_sdk.mcp import App

from main import (
    activate_vpn,
    connect_wifi,
    deactivate_vpn,
    disconnect_wifi,
    forget_wifi,
    list_connections,
    list_vpns,
    list_wifi,
    set_airplane_mode,
    set_wifi,
    status,
)


app = App.from_manifest()


@app.tool("network-manager.status")
def network_status() -> dict:
    return status()


@app.tool("network-manager.wifi-list")
def wifi_list() -> dict:
    return list_wifi()


@app.tool("network-manager.connection-list")
def connection_list() -> dict:
    return list_connections()


@app.tool("network-manager.vpn-list")
def vpn_list() -> dict:
    return list_vpns()


@app.tool("network-manager.wifi-connect")
def wifi_connect(ssid: str, credential: str | None = None) -> dict:
    return connect_wifi(ssid, credential)


@app.tool("network-manager.wifi-disconnect")
def wifi_disconnect(device: str) -> dict:
    return disconnect_wifi(device)


@app.tool("network-manager.wifi-forget")
def wifi_forget(connection: str) -> dict:
    return forget_wifi(connection)


@app.tool("network-manager.wifi-toggle")
def wifi_toggle(state: str) -> dict:
    return set_wifi(state)


@app.tool("network-manager.airplane")
def airplane(state: str) -> dict:
    return set_airplane_mode(state)


@app.tool("network-manager.vpn-up")
def vpn_up(profile: str) -> dict:
    return activate_vpn(profile)


@app.tool("network-manager.vpn-down")
def vpn_down(profile: str) -> dict:
    return deactivate_vpn(profile)


if __name__ == "__main__":
    app.serve()
