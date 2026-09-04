from claw_os_sdk.mcp import App

from main import (
    input_default,
    input_mute,
    input_route,
    input_volume,
    output_default,
    output_mute,
    output_route,
    output_volume,
    profile,
    status,
)


app = App.from_manifest()


@app.tool("audio-manager.status")
def audio_status() -> dict:
    return status()


@app.tool("audio-manager.output-volume")
def set_output_volume(percent: int) -> dict:
    return output_volume(percent)


@app.tool("audio-manager.input-volume")
def set_input_volume(percent: int) -> dict:
    return input_volume(percent)


@app.tool("audio-manager.output-mute")
def set_output_mute(state: str) -> dict:
    return output_mute(state)


@app.tool("audio-manager.input-mute")
def set_input_mute(state: str) -> dict:
    return input_mute(state)


@app.tool("audio-manager.output-default")
def set_output_default(node_id: int) -> dict:
    return output_default(node_id)


@app.tool("audio-manager.input-default")
def set_input_default(node_id: int) -> dict:
    return input_default(node_id)


@app.tool("audio-manager.output-route")
def set_output_route(node_id: int, route_index: int) -> dict:
    return output_route(node_id, route_index)


@app.tool("audio-manager.input-route")
def set_input_route(node_id: int, route_index: int) -> dict:
    return input_route(node_id, route_index)


@app.tool("audio-manager.profile")
def set_profile(device_id: int, profile_index: int) -> dict:
    return profile(device_id, profile_index)


if __name__ == "__main__":
    app.serve()
