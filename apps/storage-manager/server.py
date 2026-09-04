from claw_os_sdk.mcp import App

from main import check, eject, health, mount, status, unmount


app = App.from_manifest()


@app.tool("storage-manager.status")
def storage_status() -> dict:
    return status()


@app.tool("storage-manager.health")
def storage_health(device: str) -> dict:
    return health(device)


@app.tool("storage-manager.check")
def storage_check(device: str) -> dict:
    return check(device)


@app.tool("storage-manager.mount")
def storage_mount(device: str) -> dict:
    return mount(device)


@app.tool("storage-manager.unmount")
def storage_unmount(device: str) -> dict:
    return unmount(device)


@app.tool("storage-manager.eject")
def storage_eject(device: str) -> dict:
    return eject(device)


if __name__ == "__main__":
    app.serve()
