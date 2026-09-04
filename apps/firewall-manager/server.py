from claw_os_sdk.mcp import App

from main import add, clear, delete, restore, status


app = App.from_manifest()


@app.tool("firewall-manager.status")
def firewall_status() -> dict:
    return status()


@app.tool("firewall-manager.add")
def firewall_add(
    action: str,
    direction: str,
    protocol: str,
    port: int,
    remote: str | None = None,
    interface: str | None = None,
) -> dict:
    return add(action, direction, protocol, port, remote, interface)


@app.tool("firewall-manager.delete")
def firewall_delete(rule_id: str) -> dict:
    return delete(rule_id)


@app.tool("firewall-manager.clear")
def firewall_clear(confirm: bool) -> dict:
    return clear(confirm)


@app.tool("firewall-manager.restore")
def firewall_restore(backup_token: str, confirm: bool) -> dict:
    return restore(backup_token, confirm)


if __name__ == "__main__":
    app.serve()
