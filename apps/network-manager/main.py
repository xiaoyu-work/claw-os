"""network-manager — NetworkManager control through the root clawd broker."""

import json
import os
import shutil
import subprocess
import sys
from typing import Literal

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from _shared.env_scrub import scrub_env  # noqa: E402

from cos_runtime import policy  # noqa: E402


TIMEOUT_SECS = int(os.environ.get("CLAW_NETWORK_MANAGER_TIMEOUT", "180"))
RadioState = Literal["on", "off"]


def _cos_binary() -> str | None:
    return os.environ.get("COS_BIN") or shutil.which("cos")


def _require_name(value: object, label: str) -> str:
    if not isinstance(value, str) or not value:
        raise ValueError(f"{label} must be a non-empty string")
    return value


def _broker(
    action: str,
    target: str | None = None,
    state: RadioState | None = None,
    credential: str | None = None,
) -> dict:
    cos_bin = _cos_binary()
    if not cos_bin:
        raise FileNotFoundError(
            "cos binary not found; Network Manager broker unavailable"
        )
    argv = [cos_bin, "__network", action]
    if target is not None:
        argv.extend(["--target", target])
    if state is not None:
        argv.extend(["--state", state])
    if credential is not None:
        argv.extend(["--credential", credential])
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
    except subprocess.TimeoutExpired as exc:
        raise RuntimeError(
            f"Network Manager broker exceeded {TIMEOUT_SECS}s"
        ) from exc
    payload_text = (result.stdout or "").strip() or (result.stderr or "").strip()
    try:
        payload = json.loads(payload_text) if payload_text else {}
    except json.JSONDecodeError as exc:
        raise RuntimeError("Network Manager broker returned invalid JSON") from exc
    if not isinstance(payload, dict):
        raise RuntimeError("Network Manager broker returned a non-object result")
    if "error" in payload:
        error = payload["error"]
        if not isinstance(error, str) or not error:
            raise RuntimeError("Network Manager broker returned an invalid error payload")
        raise RuntimeError(error)
    if result.returncode != 0:
        raise RuntimeError(f"Network Manager broker exited {result.returncode}")
    return payload


def status() -> dict:
    policy.require("sys.observe", name="network")
    return _broker("status")


def list_wifi() -> dict:
    policy.require("sys.observe", name="network")
    return _broker("wifi-list")


def list_connections() -> dict:
    policy.require("sys.observe", name="network")
    return _broker("connection-list")


def list_vpns() -> dict:
    policy.require("sys.observe", name="network")
    return _broker("vpn-list")


def connect_wifi(ssid: str, credential: str | None = None) -> dict:
    ssid = _require_name(ssid, "ssid")
    if credential is not None:
        credential = _require_name(credential, "credential")
    policy.require("net.manage", name="wifi")
    if credential is not None:
        policy.require("secret.read", name=credential)
    return _broker("wifi-connect", target=ssid, credential=credential)


def disconnect_wifi(device: str) -> dict:
    device = _require_name(device, "device")
    policy.require("net.manage", name="wifi")
    return _broker("wifi-disconnect", target=device)


def forget_wifi(connection: str) -> dict:
    connection = _require_name(connection, "connection")
    policy.require("net.manage", name="wifi")
    return _broker("wifi-forget", target=connection)


def _require_state(value: object, action: str) -> RadioState:
    if not isinstance(value, str) or value not in {"on", "off"}:
        raise ValueError(f"{action} requires on|off")
    return value


def set_wifi(state: RadioState) -> dict:
    state = _require_state(state, "wifi-toggle")
    policy.require("net.manage", name="wifi")
    return _broker("wifi-toggle", state=state)


def set_airplane_mode(state: RadioState) -> dict:
    state = _require_state(state, "airplane")
    policy.require("net.manage", name="airplane")
    return _broker("airplane", state=state)


def activate_vpn(profile: str) -> dict:
    profile = _require_name(profile, "profile")
    policy.require("net.manage", name="vpn")
    return _broker("vpn-up", target=profile)


def deactivate_vpn(profile: str) -> dict:
    profile = _require_name(profile, "profile")
    policy.require("net.manage", name="vpn")
    return _broker("vpn-down", target=profile)
