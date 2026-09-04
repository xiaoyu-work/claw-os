from claw_os_sdk.mcp import App

from main import backtrace, diagnose, recent


app = App.from_manifest()


@app.tool("crash-doctor.recent")
def recent_crashes(since_minutes: int = 60, limit: int = 20) -> dict:
    return recent(since_minutes, limit)


@app.tool("crash-doctor.diagnose")
def diagnose_crashes(since_minutes: int = 60, limit: int = 20) -> dict:
    return diagnose(since_minutes, limit)


@app.tool("crash-doctor.backtrace")
def inspect_backtrace(id: str) -> dict:
    return backtrace(id)


if __name__ == "__main__":
    app.serve()
