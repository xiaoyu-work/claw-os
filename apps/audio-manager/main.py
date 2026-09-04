"""audio-manager — PipeWire and WirePlumber control through clawd."""

import json
import os
import shutil
import subprocess
import sys

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from _shared.env_scrub import scrub_env  # noqa: E402

from cos_runtime import policy  # noqa: E402


TIMEOUT_SECS = int(os.environ.get("CLAW_AUDIO_MANAGER_TIMEOUT", "180"))
MUTE_STATES = frozenset({"on", "off", "toggle"})


def _cos_binary() -> str | None:
    return os.environ.get("COS_BIN") or shutil.which("cos")


def _broker(
    action: str,
    target: int | None = None,
    value: int | str | None = None,
) -> dict:
    cos_bin = _cos_binary()
    if not cos_bin:
        raise FileNotFoundError(
            "cos binary not found; Audio Manager broker unavailable"
        )
    argv = [cos_bin, "__audio", action]
    if target is not None:
        argv.extend(["--target", str(target)])
    if value is not None:
        argv.extend(["--value", str(value)])
    try:
        result = subprocess.run(
            argv,
            capture_output=True,
            text=True,
            timeout=TIMEOUT_SECS,
            stdin=subprocess.DEVNULL,
            env=scrub_env(),
            check=False,
        )
    except FileNotFoundError as exc:
        raise FileNotFoundError(
            f"Audio Manager broker executable not found: {cos_bin}"
        ) from exc
    except PermissionError as exc:
        raise PermissionError(
            f"permission denied launching Audio Manager broker: {cos_bin}"
        ) from exc
    except subprocess.TimeoutExpired as exc:
        raise TimeoutError(
            f"Audio Manager broker exceeded {TIMEOUT_SECS}s for {action}"
        ) from exc
    payload_text = (result.stdout or "").strip() or (result.stderr or "").strip()
    try:
        payload = json.loads(payload_text) if payload_text else {}
    except json.JSONDecodeError as exc:
        raise RuntimeError("Audio Manager broker returned invalid JSON") from exc
    if not isinstance(payload, dict):
        raise RuntimeError("Audio Manager broker returned a non-object result")
    if "error" in payload:
        error = payload["error"]
        if not isinstance(error, str) or not error:
            raise RuntimeError("Audio Manager broker returned an invalid error payload")
        raise RuntimeError(error)
    if result.returncode != 0:
        raise RuntimeError(f"Audio Manager broker exited {result.returncode}")
    return payload


def _integer(
    value: object,
    name: str,
    minimum: int = 0,
    maximum: int = 4096,
) -> int:
    if isinstance(value, bool) or not isinstance(value, int):
        raise ValueError(f"{name} must be an integer")
    if not minimum <= value <= maximum:
        raise ValueError(f"{name} must be {minimum}..{maximum}")
    return value


def _mute_state(state: object) -> str:
    if not isinstance(state, str) or state not in MUTE_STATES:
        raise ValueError("mute state must be on, off, or toggle")
    return state


def status() -> dict:
    policy.require("sys.observe", name="audio")
    return _broker("status")


def output_volume(percent: int) -> dict:
    percent = _integer(percent, "percentage", maximum=150)
    policy.require("device.audio", name="output")
    return _broker("output-volume", value=percent)


def input_volume(percent: int) -> dict:
    percent = _integer(percent, "percentage", maximum=100)
    policy.require("device.microphone", name="input")
    return _broker("input-volume", value=percent)


def output_mute(state: str) -> dict:
    state = _mute_state(state)
    policy.require("device.audio", name="output")
    return _broker("output-mute", value=state)


def input_mute(state: str) -> dict:
    state = _mute_state(state)
    policy.require("device.microphone", name="input")
    return _broker("input-mute", value=state)


def output_default(node_id: int) -> dict:
    node_id = _integer(node_id, "node id", minimum=1)
    policy.require("device.media-route", name="pipewire")
    return _broker("output-default", target=node_id)


def input_default(node_id: int) -> dict:
    node_id = _integer(node_id, "node id", minimum=1)
    policy.require("device.media-route", name="pipewire")
    return _broker("input-default", target=node_id)


def output_route(node_id: int, route_index: int) -> dict:
    node_id = _integer(node_id, "node id", minimum=1)
    route_index = _integer(route_index, "route index")
    policy.require("device.media-route", name="pipewire")
    return _broker("output-route", target=node_id, value=route_index)


def input_route(node_id: int, route_index: int) -> dict:
    node_id = _integer(node_id, "node id", minimum=1)
    route_index = _integer(route_index, "route index")
    policy.require("device.media-route", name="pipewire")
    return _broker("input-route", target=node_id, value=route_index)


def profile(device_id: int, profile_index: int) -> dict:
    device_id = _integer(device_id, "device id", minimum=1)
    profile_index = _integer(profile_index, "profile index")
    policy.require("device.media-route", name="pipewire")
    return _broker("profile", target=device_id, value=profile_index)
