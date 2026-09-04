from claw_os_sdk.mcp import App

from main import list_notifications, send


app = App.from_manifest()


@app.tool("notify.send")
def notify_send(message: str, urgent: bool = False) -> dict[str, object]:
    return send(message, urgent)


@app.tool("notify.list")
def notify_list(limit: int = 20) -> dict[str, object]:
    return list_notifications(limit)


if __name__ == "__main__":
    app.serve()
