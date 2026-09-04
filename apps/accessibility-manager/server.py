from claw_os_sdk.mcp import App

from main import set_filter, set_toggle, status


app = App.from_manifest()


@app.tool("accessibility-manager.status")
def accessibility_status():
    return status()


@app.tool("accessibility-manager.screen-reader")
def screen_reader(state):
    return set_toggle("screen-reader", state)


@app.tool("accessibility-manager.magnifier")
def magnifier(state):
    return set_toggle("magnifier", state)


@app.tool("accessibility-manager.invert")
def invert(state):
    return set_toggle("invert", state)


@app.tool("accessibility-manager.filter")
def color_filter(filter):
    return set_filter(filter)


if __name__ == "__main__":
    app.serve()
