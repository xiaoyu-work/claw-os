"""bluetooth-manager — BlueZ discovery and lifecycle control."""

import json
import os
import re
import shutil
import subprocess
import sys
import unicodedata

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from _shared.env_scrub import scrub_env  # noqa: E402

from cos_runtime import policy  # noqa: E402


ADDRESS_RE = re.compile(r"^(?:[0-9A-Fa-f]{2}:){5}[0-9A-Fa-f]{2}$")
PAIRING_ID_RE = re.compile(r"^[0-9A-Fa-f]{32}$")
TIMEOUT_SECS = int(os.environ.get("CLAW_BLUETOOTH_MANAGER_TIMEOUT", "240"))


def _cos_binary() -> str | None:
    return os.environ.get("COS_BIN") or shutil.which("cos")


def _address(raw: object, name: str) -> str:
    if not isinstance(raw, str) or ADDRESS_RE.fullmatch(raw) is None:
        raise ValueError(f"{name} must be a Bluetooth MAC address")
    return raw.upper()


def _state(raw: object) -> str:
    if not isinstance(raw, str) or raw not in {"on", "off"}:
        raise ValueError("state must be on or off")
    return raw


def _scan_seconds(raw: object) -> int:
    if isinstance(raw, bool) or not isinstance(raw, int):
        raise ValueError("scan seconds must be an integer")
    if not 1 <= raw <= 60:
        raise ValueError("scan seconds must be 1..60")
    return raw


def _pairing_id(raw: object) -> str:
    if not isinstance(raw, str) or PAIRING_ID_RE.fullmatch(raw) is None:
        raise ValueError("pairing id must be exactly 32 hexadecimal characters")
    return raw.lower()


def _pairing_response(raw: object) -> str:
    if (
        not isinstance(raw, str)
        or not 1 <= len(raw) <= 32
        or any(unicodedata.category(character) == "Cc" for character in raw)
    ):
        raise ValueError(
            "pairing response must be a string of 1..32 characters without controls"
        )
    return raw


def _parse_payload(payload_text: str) -> dict:
    try:
        payload = json.loads(payload_text)
    except json.JSONDecodeError as exc:
        raise RuntimeError(
            "Bluetooth Manager broker returned invalid JSON"
        ) from exc
    if not isinstance(payload, dict):
        raise RuntimeError("Bluetooth Manager broker returned a non-object result")
    if "error" in payload:
        error = payload["error"]
        if not isinstance(error, str) or not error:
            raise RuntimeError(
                "Bluetooth Manager broker returned an invalid error payload"
            )
        raise RuntimeError(error)
    return payload


def _broker(
    action: str,
    *,
    adapter: str | None = None,
    device: str | None = None,
    state: str | None = None,
    seconds: int | None = None,
    pairing_id: str | None = None,
    response: str | None = None,
) -> dict:
    cos_bin = _cos_binary()
    if not cos_bin:
        raise FileNotFoundError(
            "cos binary not found; Bluetooth Manager broker unavailable"
        )
    argv = [cos_bin, "__bluetooth", action]
    if adapter is not None:
        argv.extend(["--adapter", adapter])
    if device is not None:
        argv.extend(["--device", device])
    if state is not None:
        argv.extend(["--state", state])
    if seconds is not None:
        argv.extend(["--seconds", str(seconds)])
    if pairing_id is not None:
        argv.extend(["--pairing-id", pairing_id])
    if response is not None:
        argv.append("--response-stdin")
    run_options = {
        "capture_output": True,
        "text": True,
        "timeout": TIMEOUT_SECS,
        "env": scrub_env(),
        "check": False,
    }
    if response is None:
        run_options["stdin"] = subprocess.DEVNULL
    else:
        run_options["input"] = f"{response}\n"
    try:
        result = subprocess.run(argv, **run_options)
    except FileNotFoundError as exc:
        raise FileNotFoundError(
            f"Bluetooth Manager broker executable not found: {cos_bin}"
        ) from exc
    except PermissionError as exc:
        raise PermissionError(
            f"permission denied launching Bluetooth Manager broker: {cos_bin}"
        ) from exc
    except subprocess.TimeoutExpired as exc:
        raise TimeoutError(
            f"Bluetooth Manager broker exceeded {TIMEOUT_SECS}s for {action}"
        ) from exc

    payloads = []
    for output in (
        (result.stdout or "").strip(),
        (result.stderr or "").strip(),
    ):
        if not output:
            continue
        payloads.append(_parse_payload(output))
        if result.returncode == 0:
            break
    if result.returncode != 0:
        raise RuntimeError(f"Bluetooth Manager broker exited {result.returncode}")
    if not payloads:
        raise RuntimeError("Bluetooth Manager broker returned invalid JSON")
    return payloads[0]


def status() -> dict:
    policy.require("sys.observe", name="bluetooth")
    return _broker("status")


def power(adapter: str, state: str) -> dict:
    adapter = _address(adapter, "adapter")
    state = _state(state)
    policy.require("device.bluetooth", name="control")
    return _broker("power", adapter=adapter, state=state)


def scan(adapter: str, seconds: int = 10) -> dict:
    adapter = _address(adapter, "adapter")
    seconds = _scan_seconds(seconds)
    policy.require("device.bluetooth", name="control")
    return _broker("scan", adapter=adapter, seconds=seconds)


def _device_action(action: str, adapter: str, device: str) -> dict:
    adapter = _address(adapter, "adapter")
    device = _address(device, "device")
    policy.require("device.bluetooth", name="control")
    return _broker(action, adapter=adapter, device=device)


def pair(adapter: str, device: str) -> dict:
    return _device_action("pair", adapter, device)


def pair_status(pairing_id: str) -> dict:
    pairing_id = _pairing_id(pairing_id)
    policy.require("device.bluetooth", name="control")
    return _broker("pair-status", pairing_id=pairing_id)


def pair_respond(pairing_id: str, response: str) -> dict:
    pairing_id = _pairing_id(pairing_id)
    response = _pairing_response(response)
    policy.require("device.bluetooth", name="control")
    return _broker(
        "pair-respond",
        pairing_id=pairing_id,
        response=response,
    )


def pair_cancel(pairing_id: str) -> dict:
    pairing_id = _pairing_id(pairing_id)
    policy.require("device.bluetooth", name="control")
    return _broker("pair-cancel", pairing_id=pairing_id)


def connect(adapter: str, device: str) -> dict:
    return _device_action("connect", adapter, device)


def disconnect(adapter: str, device: str) -> dict:
    return _device_action("disconnect", adapter, device)


def trust(adapter: str, device: str) -> dict:
    return _device_action("trust", adapter, device)


def untrust(adapter: str, device: str) -> dict:
    return _device_action("untrust", adapter, device)


def forget(adapter: str, device: str) -> dict:
    return _device_action("forget", adapter, device)
