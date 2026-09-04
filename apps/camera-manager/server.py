from claw_os_sdk.mcp import App

from main import capture, status


app = App.from_manifest()


@app.tool("camera-manager.status")
def camera_status() -> dict:
    return status()


@app.tool("camera-manager.capture")
def capture_image(
    node_id: int,
    expected_serial: int,
    destination: str,
    format: str,
    width: int = 1280,
    height: int = 720,
) -> dict:
    return capture(
        node_id,
        expected_serial,
        destination,
        format,
        width,
        height,
    )


if __name__ == "__main__":
    app.serve()
