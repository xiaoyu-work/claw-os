"""bluetooth-manager — BlueZ discovery and lifecycle control."""

import json
import os
import re
import shutil
import subprocess
import sys

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from _shared.env_scrub import scrub_env  # noqa: E402

from cos_runtime import policy  # noqa: E402


ADDRESS_RE = re.compile(r"^(?:[0-9A-Fa-f]{2}:){5}[0-9A-Fa-f]{2}$")
PAIRING_ID_RE = re.compile(r"^[0-9A-Fa-f]{32}$")
TIMEOUT_SECS = int(os.environ.get("CLAW_BLUETOOTH_MANAGER_TIMEOUT", "240"))
DEVICE_COMMANDS = frozenset(
    {"pair", "connect", "disconnect", "trust", "untrust", "forget"}
)


def _cos_binary():
    return os.environ.get("COS_BIN") or shutil.which("cos")


def _address(raw, name):
    if not isinstance(raw, str) or ADDRESS_RE.fullmatch(raw) is None:
        raise ValueError(f"{name} must be a Bluetooth MAC address")
    return raw.upper()


def _broker(
    action,
    adapter=None,
    device=None,
    state=None,
    seconds=None,
    pairing_id=None,
    response=None,
):
    cos_bin = _cos_binary()
    if not cos_bin:
        return {"error": "cos binary not found; Bluetooth Manager broker unavailable"}
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
    except (FileNotFoundError, PermissionError) as exc:
        return {"error": str(exc)}
    except subprocess.TimeoutExpired:
        return {"error": f"Bluetooth Manager broker exceeded {TIMEOUT_SECS}s"}
    payload_text = (result.stdout or "").strip() or (result.stderr or "").strip()
    try:
        payload = json.loads(payload_text) if payload_text else {}
    except json.JSONDecodeError:
        return {"error": "Bluetooth Manager broker returned invalid JSON"}
    if result.returncode != 0 and "error" not in payload:
        payload["error"] = f"Bluetooth Manager broker exited {result.returncode}"
    return payload


def run(command, args):
    if command == "__schema__":
        return _schema()
    if command == "status":
        if args:
            return {"error": "status takes no arguments"}
        policy.require("sys.observe", name="bluetooth")
        return _broker(command)
    if command == "power":
        if len(args) != 2 or args[1] not in {"on", "off"}:
            return {"error": "power requires <adapter-address> on|off"}
        try:
            adapter = _address(args[0], "adapter")
        except ValueError as exc:
            return {"error": str(exc)}
        policy.require("device.bluetooth", name="control")
        return _broker(command, adapter=adapter, state=args[1])
    if command == "scan":
        if not 1 <= len(args) <= 2:
            return {"error": "scan requires <adapter-address> [seconds]"}
        try:
            adapter = _address(args[0], "adapter")
            seconds = int(args[1]) if len(args) == 2 else 10
        except (ValueError, TypeError) as exc:
            return {"error": str(exc)}
        if not 1 <= seconds <= 60:
            return {"error": "scan seconds must be 1..60"}
        policy.require("device.bluetooth", name="control")
        return _broker(command, adapter=adapter, seconds=seconds)
    if command in DEVICE_COMMANDS:
        if len(args) != 2:
            return {"error": f"{command} requires <adapter-address> <device-address>"}
        try:
            adapter = _address(args[0], "adapter")
            device = _address(args[1], "device")
        except ValueError as exc:
            return {"error": str(exc)}
        policy.require("device.bluetooth", name="control")
        return _broker(command, adapter=adapter, device=device)
    if command in {"pair-status", "pair-cancel"}:
        if len(args) != 1 or PAIRING_ID_RE.fullmatch(args[0]) is None:
            return {"error": f"{command} requires one pairing id returned by pair"}
        policy.require("device.bluetooth", name="control")
        return _broker(command, pairing_id=args[0].lower())
    if command == "pair-respond":
        if (
            len(args) != 2
            or PAIRING_ID_RE.fullmatch(args[0]) is None
            or not 1 <= len(args[1]) <= 32
            or any(character in "\r\n\x00" or ord(character) < 32 for character in args[1])
        ):
            return {"error": "pair-respond requires <pairing-id> <response>"}
        policy.require("device.bluetooth", name="control")
        return _broker(
            command,
            pairing_id=args[0].lower(),
            response=args[1],
        )
    return {"error": f"unknown command: {command}"}


def _schema():
    return {
        "status": {"description": "List BlueZ adapters and known devices", "parameters": []},
        "power": _two("Power a Bluetooth adapter on or off", "adapter", "state"),
        "scan": _two("Scan for nearby devices for 1-60 seconds", "adapter", "seconds"),
        "pair": _two("Pair a device; BlueZ may request desktop confirmation", "adapter", "device"),
        "pair-status": _one("Check an active pairing session", "pairing_id"),
        "pair-respond": _two("Respond to a confirmation, PIN, or passkey prompt", "pairing_id", "response"),
        "pair-cancel": _one("Cancel an active pairing session", "pairing_id"),
        "connect": _two("Connect a paired Bluetooth device", "adapter", "device"),
        "disconnect": _two("Disconnect a Bluetooth device", "adapter", "device"),
        "trust": _two("Trust a Bluetooth device", "adapter", "device"),
        "untrust": _two("Remove trust from a Bluetooth device", "adapter", "device"),
        "forget": _two("Forget pairing and saved device state", "adapter", "device"),
    }


def _two(description, first, second):
    return {
        "description": description,
        "parameters": [
            {"name": first, "type": "string", "kind": "positional", "required": True},
            {"name": second, "type": "string", "kind": "positional", "required": True},
        ],
    }


def _one(description, name):
    return {
        "description": description,
        "parameters": [
            {"name": name, "type": "string", "kind": "positional", "required": True}
        ],
    }
