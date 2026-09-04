from claw_os_sdk.mcp import App

from main import (
    create_snapshot,
    delete_snapshot,
    list_snapshots,
    rollback_snapshot,
    status,
)


app = App.from_manifest()


@app.tool("system-snapshot.status")
def snapshot_status() -> dict:
    return status()


@app.tool("system-snapshot.list")
def snapshot_list() -> dict:
    return list_snapshots()


@app.tool("system-snapshot.create")
def snapshot_create(description: str | None = None) -> dict:
    return create_snapshot(description)


@app.tool("system-snapshot.delete")
def snapshot_delete(id: str) -> dict:
    return delete_snapshot(id)


@app.tool("system-snapshot.rollback")
def snapshot_rollback(id: str, confirm: bool) -> dict:
    return rollback_snapshot(id, confirm)


if __name__ == "__main__":
    app.serve()
