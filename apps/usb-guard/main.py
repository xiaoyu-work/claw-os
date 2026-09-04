"""usb-guard — sysfs authorization, managed udev blocking, and safe eject."""

import json
import os
import re
import shutil
import subprocess
import sys

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from _shared.env_scrub import scrub_env  # noqa: E402

from cos_runtime import policy  # noqa: E402


DEVICE_RE = re.compile(r"^(?=.{3,64}$)[0-9]+-[0-9]+(?:\.[0-9]+)*$")
TOKEN_RE = re.compile(r"^[0-9A-Fa-f]{32}$")
TIMEOUT_SECS = int(os.environ.get("CLAW_USB_GUARD_TIMEOUT", "180"))
USB_ACTIONS = frozenset({"status", "authorize", "block", "unblock", "eject", "restore"})
USB_STATES = frozenset({"on", "off"})


def _cos_binary() -> str | None:
    return os.environ.get("COS_BIN") or shutil.which("cos")


def _validate_action(action: object) -> str:
    if not isinstance(action, str) or action not in USB_ACTIONS:
        raise ValueError(f"unknown USB Guard action: {action}")
    return action


def _validate_device(device: object) -> str:
    if not isinstance(device, str) or DEVICE_RE.fullmatch(device) is None:
        raise ValueError("device must be a USB sysfs ID such as 1-2 or 1-2.3")
    return device


def _validate_token(value: object, label: str) -> str:
    if not isinstance(value, str) or TOKEN_RE.fullmatch(value) is None:
        raise ValueError(f"{label} must be exactly 32 hexadecimal characters")
    return value.lower()


def _require_confirmation(action: str, confirm: object) -> None:
    if confirm is not True:
        raise ValueError(f"{action} requires confirm=true")


def _broker(
    action: str,
    device: str | None = None,
    state: str | None = None,
    rule_id: str | None = None,
    token: str | None = None,
    confirm: bool = False,
) -> dict:
    cos_bin = _cos_binary()
    if not cos_bin:
        raise FileNotFoundError(
            "cos binary not found; USB Guard broker unavailable"
        )
    argv = [cos_bin, "__usb", action]
    for flag, value in [
        ("--device", device),
        ("--state", state),
        ("--rule-id", rule_id),
        ("--token", token),
    ]:
        if value is not None:
            argv.extend([flag, value])
    if confirm:
        argv.append("--confirm")
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
        raise RuntimeError(f"USB Guard broker execution failed: {exc}") from exc
    except subprocess.TimeoutExpired as exc:
        raise RuntimeError(f"USB Guard broker exceeded {TIMEOUT_SECS}s") from exc
    payload_text = (result.stdout or "").strip() or (result.stderr or "").strip()
    try:
        payload = json.loads(payload_text) if payload_text else {}
    except json.JSONDecodeError as exc:
        raise RuntimeError("USB Guard broker returned invalid JSON") from exc
    if not isinstance(payload, dict):
        raise RuntimeError("USB Guard broker returned a non-object result")
    if "error" in payload:
        error = payload["error"]
        if not isinstance(error, str) or not error:
            raise RuntimeError("USB Guard broker returned an invalid error payload")
        raise RuntimeError(error)
    if result.returncode != 0:
        raise RuntimeError(f"USB Guard broker exited {result.returncode}")
    return payload


def _execute(
    action: str,
    *,
    device: str | None = None,
    state: str | None = None,
    rule_id: str | None = None,
    token: str | None = None,
    confirm: bool = False,
) -> dict:
    action = _validate_action(action)
    if action == "status":
        policy.require("sys.observe", name="usb")
    else:
        policy.require("device.usb", name="control")
    return _broker(
        action,
        device=device,
        state=state,
        rule_id=rule_id,
        token=token,
        confirm=confirm,
    )


def status() -> dict:
    return _execute("status")


def authorize(device: str, state: str, confirm: bool = False) -> dict:
    device = _validate_device(device)
    if not isinstance(state, str) or state not in USB_STATES:
        raise ValueError("state must be on or off")
    if not isinstance(confirm, bool):
        raise ValueError("confirm must be a boolean")
    if state == "off" and not confirm:
        raise ValueError("deauthorization requires confirm=true")
    if state == "on" and confirm:
        raise ValueError("authorization does not accept confirm=true")
    return _execute("authorize", device=device, state=state, confirm=confirm)


def block(device: str, confirm: bool) -> dict:
    device = _validate_device(device)
    _require_confirmation("block", confirm)
    return _execute("block", device=device, confirm=True)


def unblock(rule_id: str, confirm: bool) -> dict:
    rule_id = _validate_token(rule_id, "rule_id")
    _require_confirmation("unblock", confirm)
    return _execute("unblock", rule_id=rule_id, confirm=True)


def eject(device: str, confirm: bool) -> dict:
    device = _validate_device(device)
    _require_confirmation("eject", confirm)
    return _execute("eject", device=device, confirm=True)


def restore(backup_token: str, confirm: bool) -> dict:
    backup_token = _validate_token(backup_token, "backup_token")
    _require_confirmation("restore", confirm)
    return _execute("restore", token=backup_token, confirm=True)
