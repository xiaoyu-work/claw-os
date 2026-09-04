from claw_os_sdk.mcp import App

from main import (
    backup,
    check,
    forget,
    init_repository,
    restore,
    retention,
    snapshots,
)


app = App.from_manifest()


@app.tool("backup-center.init")
def backup_init(repo: str, credential: str) -> dict:
    return init_repository(repo, credential)


@app.tool("backup-center.snapshots")
def backup_snapshots(repo: str, credential: str) -> dict:
    return snapshots(repo, credential)


@app.tool("backup-center.check")
def backup_check(repo: str, credential: str) -> dict:
    return check(repo, credential)


@app.tool("backup-center.backup")
def backup_create(
    repo: str,
    source: str,
    credential: str,
    tag: str | None = None,
) -> dict:
    return backup(repo, source, credential, tag)


@app.tool("backup-center.restore")
def backup_restore(
    repo: str,
    snapshot: str,
    destination: str,
    credential: str,
    confirm: bool,
) -> dict:
    return restore(repo, snapshot, destination, credential, confirm)


@app.tool("backup-center.forget")
def backup_forget(
    repo: str,
    snapshot: str,
    credential: str,
    confirm: bool,
) -> dict:
    return forget(repo, snapshot, credential, confirm)


@app.tool("backup-center.retention")
def backup_retention(
    repo: str,
    credential: str,
    keep_daily: int,
    keep_weekly: int,
    keep_monthly: int,
    confirm: bool,
) -> dict:
    return retention(
        repo,
        credential,
        keep_daily,
        keep_weekly,
        keep_monthly,
        confirm,
    )


if __name__ == "__main__":
    app.serve()
