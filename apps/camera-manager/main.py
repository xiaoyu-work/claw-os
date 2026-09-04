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


def _cos_binary() -> str | None:
    return os.environ.get("COS_BIN") or shutil.which("cos")


def _broker(
    action: str,
    node_id: int | None = None,
    expected_serial: int | None = None,
    destination: str | None = None,
    image_format: str | None = None,
    width: int | None = None,
    height: int | None = None,
) -> dict:
    cos_bin = _cos_binary()
    if not cos_bin:
        raise FileNotFoundError(
            "cos binary not found; Camera Manager broker unavailable"
        )
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
    except FileNotFoundError as exc:
        raise FileNotFoundError(
            f"Camera Manager broker executable not found: {cos_bin}"
        ) from exc
    except PermissionError as exc:
        raise PermissionError(
            f"permission denied launching Camera Manager broker: {cos_bin}"
        ) from exc
    except subprocess.TimeoutExpired as exc:
        raise TimeoutError(
            f"Camera Manager broker exceeded {TIMEOUT_SECS}s"
        ) from exc
    payload_text = (result.stdout or "").strip() or (result.stderr or "").strip()
    try:
        payload = json.loads(payload_text) if payload_text else {}
    except json.JSONDecodeError as exc:
        raise RuntimeError("Camera Manager broker returned invalid JSON") from exc
    if not isinstance(payload, dict):
        raise RuntimeError("Camera Manager broker returned a non-object result")
    if "error" in payload:
        error = payload["error"]
        if not isinstance(error, str) or not error:
            raise RuntimeError("Camera Manager broker returned an invalid error payload")
        raise RuntimeError(error)
    if result.returncode != 0:
        raise RuntimeError(f"Camera Manager broker exited {result.returncode}")
    return payload


def status() -> dict:
    policy.require("sys.observe", name="camera")
    return _broker("status")


def capture(
    node_id: int,
    expected_serial: int,
    destination: str,
    image_format: str,
    width: int = 1280,
    height: int = 720,
) -> dict:
    if (
        type(node_id) is not int
        or type(expected_serial) is not int
        or type(width) is not int
        or type(height) is not int
    ):
        raise ValueError("node id, serial, and dimensions must be integers")
    if (
        not 1 <= node_id <= 2**32 - 1
        or expected_serial <= 0
        or not 16 <= width <= 7680
        or not 16 <= height <= 4320
    ):
        raise ValueError("node id, serial, or dimensions are out of bounds")
    if not isinstance(destination, str):
        raise ValueError("destination must be a path")
    canonical_destination = os.path.join(
        os.path.realpath(os.path.dirname(destination)),
        os.path.basename(destination),
    )
    if canonical_destination != destination or os.path.lexists(canonical_destination):
        raise ValueError("destination must be a canonical new path")
    if not isinstance(image_format, str) or image_format not in {"png", "jpeg"}:
        raise ValueError("format must be png or jpeg")
    policy.require("device.camera", name="capture")
    policy.require("fs.write", path=canonical_destination)
    return _broker(
        "capture",
        node_id=node_id,
        expected_serial=expected_serial,
        destination=canonical_destination,
        image_format=image_format,
        width=width,
        height=height,
    )
