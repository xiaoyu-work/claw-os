from claw_os_sdk.mcp import App

from main import (
    apply_layout,
    brightness,
    disable,
    enable,
    mirror,
    mode,
    position,
    restore,
    scale as set_scale,
    status,
)


app = App.from_manifest()


@app.tool("display-manager.status")
def display_status() -> dict:
    return status()


@app.tool("display-manager.enable")
def display_enable(output: str) -> dict:
    return enable(output)


@app.tool("display-manager.disable")
def display_disable(output: str) -> dict:
    return disable(output)


@app.tool("display-manager.mirror")
def display_mirror(output: str, source_output: str) -> dict:
    return mirror(output, source_output)


@app.tool("display-manager.position")
def display_position(output: str, x: int, y: int) -> dict:
    return position(output, x, y)


@app.tool("display-manager.mode")
def display_mode(
    output: str,
    width: int,
    height: int,
    adaptive_sync: str | None = None,
    refresh: float | None = None,
    scale: float | None = None,
    x: int | None = None,
    y: int | None = None,
    transform: str | None = None,
) -> dict:
    return mode(
        output,
        width,
        height,
        adaptive_sync,
        refresh,
        scale,
        x,
        y,
        transform,
    )


@app.tool("display-manager.scale")
def display_scale(output: str, scale: float) -> dict:
    return set_scale(output, scale)


@app.tool("display-manager.apply-layout")
def display_apply_layout(source: str, confirm: bool) -> dict:
    return apply_layout(source, confirm)


@app.tool("display-manager.brightness")
def display_brightness(backlight: str, percent: int) -> dict:
    return brightness(backlight, percent)


@app.tool("display-manager.restore")
def display_restore(backup_token: str, confirm: bool) -> dict:
    return restore(backup_token, confirm)


if __name__ == "__main__":
    app.serve()
