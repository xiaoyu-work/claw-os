from claw_os_sdk.mcp import App

from main import recent, status, watch_pid


app = App.from_manifest()


@app.tool("event-center.status")
def event_status() -> dict:
    return status()


@app.tool("event-center.recent")
def recent_events(limit: int = 100, source: str | None = None) -> dict:
    return recent(limit, source)


@app.tool("event-center.watch-pid")
def watch_process(pid: int) -> dict:
    return watch_pid(pid)


if __name__ == "__main__":
    app.serve()
