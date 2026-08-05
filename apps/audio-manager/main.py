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
OUTPUT_COMMANDS = frozenset({"output-volume", "output-mute"})
INPUT_COMMANDS = frozenset({"input-volume", "input-mute"})


def _cos_binary():
    return os.environ.get("COS_BIN") or shutil.which("cos")


def _broker(action, target=None, value=None):
    cos_bin = _cos_binary()
    if not cos_bin:
        return {"error": "cos binary not found; Audio Manager broker unavailable"}
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
    except (FileNotFoundError, PermissionError) as exc:
        return {"error": str(exc)}
    except subprocess.TimeoutExpired:
        return {"error": f"Audio Manager broker exceeded {TIMEOUT_SECS}s"}
    payload_text = (result.stdout or "").strip() or (result.stderr or "").strip()
    try:
        payload = json.loads(payload_text) if payload_text else {}
    except json.JSONDecodeError:
        return {"error": "Audio Manager broker returned invalid JSON"}
    if result.returncode != 0 and "error" not in payload:
        payload["error"] = f"Audio Manager broker exited {result.returncode}"
    return payload


def _integer(raw, name, minimum=0, maximum=4096):
    try:
        value = int(raw)
    except (TypeError, ValueError) as exc:
        raise ValueError(f"{name} must be an integer") from exc
    if not minimum <= value <= maximum:
        raise ValueError(f"{name} must be {minimum}..{maximum}")
    return value


def run(command, args):
    if command == "__schema__":
        return _schema()
    if command == "status":
        if args:
            return {"error": "status takes no arguments"}
        policy.require("sys.observe", name="audio")
        return _broker(command)
    if command in {"output-volume", "input-volume"}:
        if len(args) != 1:
            return {"error": f"{command} requires one percentage"}
        maximum = 150 if command == "output-volume" else 100
        try:
            percent = _integer(args[0], "percentage", 0, maximum)
        except ValueError as exc:
            return {"error": str(exc)}
        _require_direction(command)
        return _broker(command, value=percent)
    if command in {"output-mute", "input-mute"}:
        if len(args) != 1 or args[0] not in {"on", "off", "toggle"}:
            return {"error": f"{command} requires on|off|toggle"}
        _require_direction(command)
        return _broker(command, value=args[0])
    if command in {"output-default", "input-default"}:
        if len(args) != 1:
            return {"error": f"{command} requires one node id"}
        try:
            node_id = _integer(args[0], "node id", 1)
        except ValueError as exc:
            return {"error": str(exc)}
        policy.require("device.media-route", name="pipewire")
        return _broker(command, target=node_id)
    if command in {"output-route", "input-route"}:
        if len(args) != 2:
            return {"error": f"{command} requires <node-id> <route-index>"}
        try:
            node_id = _integer(args[0], "node id", 1)
            route = _integer(args[1], "route index")
        except ValueError as exc:
            return {"error": str(exc)}
        policy.require("device.media-route", name="pipewire")
        return _broker(command, target=node_id, value=route)
    if command == "profile":
        if len(args) != 2:
            return {"error": "profile requires <device-id> <profile-index>"}
        try:
            device_id = _integer(args[0], "device id", 1)
            profile = _integer(args[1], "profile index")
        except ValueError as exc:
            return {"error": str(exc)}
        policy.require("device.media-route", name="pipewire")
        return _broker(command, target=device_id, value=profile)
    return {"error": f"unknown command: {command}"}


def _require_direction(command):
    if command in OUTPUT_COMMANDS:
        policy.require("device.audio", name="output")
    elif command in INPUT_COMMANDS:
        policy.require("device.microphone", name="input")


def _schema():
    return {
        "status": {
            "description": "List PipeWire devices, nodes, streams, defaults, and volume",
            "parameters": [],
        },
        "output-volume": _one("Set default output volume (0-150%)", "percent"),
        "input-volume": _one("Set default input volume (0-100%)", "percent"),
        "output-mute": _one("Set output mute state: on, off, or toggle", "state"),
        "input-mute": _one("Set input mute state: on, off, or toggle", "state"),
        "output-default": _one("Set the default output node", "node_id"),
        "input-default": _one("Set the default input node", "node_id"),
        "output-route": _two("Set an output node route", "node_id", "route_index"),
        "input-route": _two("Set an input node route", "node_id", "route_index"),
        "profile": _two("Set an audio device profile", "device_id", "profile_index"),
    }


def _one(description, name):
    return {
        "description": description,
        "parameters": [
            {"name": name, "type": "string", "kind": "positional", "required": True}
        ],
    }


def _two(description, first, second):
    return {
        "description": description,
        "parameters": [
            {"name": first, "type": "string", "kind": "positional", "required": True},
            {"name": second, "type": "string", "kind": "positional", "required": True},
        ],
    }
