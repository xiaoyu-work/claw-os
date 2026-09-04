from claw_os_sdk.mcp import App

from main import inspect


app = App.from_manifest()


@app.tool("hardware-center.summary")
def summary():
    return inspect("summary")


@app.tool("hardware-center.cpu")
def cpu():
    return inspect("cpu")


@app.tool("hardware-center.gpu")
def gpu():
    return inspect("gpu")


@app.tool("hardware-center.pci")
def pci():
    return inspect("pci")


@app.tool("hardware-center.usb")
def usb():
    return inspect("usb")


@app.tool("hardware-center.memory")
def memory():
    return inspect("memory")


@app.tool("hardware-center.storage")
def storage():
    return inspect("storage")


@app.tool("hardware-center.drivers")
def drivers():
    return inspect("drivers")


@app.tool("hardware-center.thermal")
def thermal():
    return inspect("thermal")


if __name__ == "__main__":
    app.serve()
