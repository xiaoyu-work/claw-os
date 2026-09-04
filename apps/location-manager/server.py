from claw_os_sdk.mcp import App

from main import query


app = App.from_manifest()


@app.tool("location-manager.locate")
def locate(accuracy="city"):
    return query("locate", accuracy)


@app.tool("location-manager.timezone")
def timezone(accuracy="city"):
    return query("timezone", accuracy)


if __name__ == "__main__":
    app.serve()
