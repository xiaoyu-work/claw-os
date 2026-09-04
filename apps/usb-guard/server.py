from claw_os_sdk.mcp import App

from main import authorize, block, eject, restore, status, unblock


app = App.from_manifest()


@app.tool("usb-guard.status")
def usb_status() -> dict:
    return status()


@app.tool("usb-guard.authorize")
def usb_authorize(device: str, state: str, confirm: bool = False) -> dict:
    return authorize(device, state, confirm)


@app.tool("usb-guard.block")
def usb_block(device: str, confirm: bool) -> dict:
    return block(device, confirm)


@app.tool("usb-guard.unblock")
def usb_unblock(rule_id: str, confirm: bool) -> dict:
    return unblock(rule_id, confirm)


@app.tool("usb-guard.eject")
def usb_eject(device: str, confirm: bool) -> dict:
    return eject(device, confirm)


@app.tool("usb-guard.restore")
def usb_restore(backup_token: str, confirm: bool) -> dict:
    return restore(backup_token, confirm)


if __name__ == "__main__":
    app.serve()
