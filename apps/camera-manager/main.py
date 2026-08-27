"""camera-manager — PipeWire camera discovery and still capture."""

import json
import os
import shutil
import subprocess
import sys

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from _shared.env_scrub import scrub_env  # noqa: E402

from cos_runtime import policy  # noqa: E402


TIMEOUT_SECS = int(os.environ.get("CLAW_CAMERA_MANAGER_TIMEOUT", "180"))


def _cos_binary():
    return os.environ.get("COS_BIN") or shutil.which("cos")


def _broker(
    action,
    node_id=None,
    expected_serial=None,
    destination=None,
    image_format=None,
    width=None,
    height=None,
):
    cos_bin = _cos_binary()
    if not cos_bin:
        return {"error": "cos binary not found; Camera Manager broker unavailable"}
    argv = [cos_bin, "__camera", action]
    for flag, value in [
        ("--node-id", node_id),
        ("--expected-serial", expected_serial),
        ("--destination", destination),
        ("--format", image_format),
        ("--width", width),
        ("--height", height),
    ]:
        if value is not None:
            argv.extend([flag, str(value)])
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
        return {"error": f"Camera Manager broker exceeded {TIMEOUT_SECS}s"}
    payload_text = (result.stdout or "").strip() or (result.stderr or "").strip()
    try:
        payload = json.loads(payload_text) if payload_text else {}
    except json.JSONDecodeError:
        return {"error": "Camera Manager broker returned invalid JSON"}
    if result.returncode != 0 and "error" not in payload:
        payload["error"] = f"Camera Manager broker exited {result.returncode}"
    return payload


def run(command, args):
    if command == "status":
        if args:
            return {"error": "status takes no arguments"}
        policy.require("sys.observe", name="camera")
        return _broker(command)
    if command == "capture":
        if not 4 <= len(args) <= 6:
            return {"error": "capture requires <node-id> <serial> <destination> <png|jpeg> [width] [height]"}
        try:
            node_id = int(args[0])
            expected_serial = int(args[1])
            width = int(args[4]) if len(args) > 4 else 1280
            height = int(args[5]) if len(args) > 5 else 720
        except ValueError:
            return {"error": "node id, serial, and dimensions must be integers"}
        if (
            not 1 <= node_id <= 2**32 - 1
            or expected_serial <= 0
            or not 16 <= width <= 7680
            or not 16 <= height <= 4320
        ):
            return {"error": "node id, serial, or dimensions are out of bounds"}
        destination = os.path.join(
            os.path.realpath(os.path.dirname(args[2])),
            os.path.basename(args[2]),
        )
        if destination != args[2] or os.path.lexists(destination):
            return {"error": "destination must be a canonical new path"}
        image_format = args[3]
        if image_format not in {"png", "jpeg"}:
            return {"error": "format must be png or jpeg"}
        policy.require("device.camera", name="capture")
        policy.require("fs.write", path=destination)
        return _broker(
            command,
            node_id=node_id,
            expected_serial=expected_serial,
            destination=destination,
            image_format=image_format,
            width=width,
            height=height,
        )
    return {"error": f"unknown command: {command}"}
