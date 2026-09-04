"""firewall-manager — scoped, reversible nftables rules."""

import ipaddress
import json
import os
import re
import shutil
import subprocess
import sys

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from _shared.env_scrub import scrub_env  # noqa: E402

from cos_runtime import policy  # noqa: E402


ID_RE = re.compile(r"^[0-9A-Fa-f]{32}$")
IFACE_RE = re.compile(r"^[A-Za-z0-9_.:-]{1,15}$")
TIMEOUT_SECS = int(os.environ.get("CLAW_FIREWALL_MANAGER_TIMEOUT", "180"))
ACTIONS = frozenset({"allow", "deny"})
DIRECTIONS = frozenset({"input", "output"})
PROTOCOLS = frozenset({"tcp", "udp"})


def _cos_binary() -> str | None:
    return os.environ.get("COS_BIN") or shutil.which("cos")


def _choice(value: object, label: str, choices: frozenset[str]) -> str:
    if not isinstance(value, str) or value not in choices:
        expected = " or ".join(sorted(choices))
        raise ValueError(f"{label} must be {expected}")
    return value


def _port(value: object) -> int:
    if isinstance(value, bool) or not isinstance(value, int):
        raise ValueError("port must be an integer")
    if not 1 <= value <= 65535:
        raise ValueError("port must be 1..65535")
    return value


def _remote_cidr(value: object) -> str:
    if not isinstance(value, str):
        raise ValueError("remote must be a CIDR string")
    try:
        return str(ipaddress.ip_network(value, strict=False))
    except ValueError as exc:
        raise ValueError("invalid remote CIDR") from exc


def _interface(value: object) -> str:
    if not isinstance(value, str) or IFACE_RE.fullmatch(value) is None:
        raise ValueError("invalid interface name")
    return value


def _identifier(value: object, label: str) -> str:
    if not isinstance(value, str) or ID_RE.fullmatch(value) is None:
        raise ValueError(f"{label} must be exactly 32 hexadecimal characters")
    return value.lower()


def _require_confirmation(action: str, confirm: object) -> None:
    if confirm is not True:
        raise ValueError(f"{action} requires confirm=true")


def _parse_payload(payload_text: str) -> dict:
    try:
        payload = json.loads(payload_text)
    except json.JSONDecodeError as exc:
        raise RuntimeError("Firewall Manager broker returned invalid JSON") from exc
    if not isinstance(payload, dict):
        raise RuntimeError("Firewall Manager broker returned a non-object result")
    if "error" in payload:
        error = payload["error"]
        if not isinstance(error, str) or not error:
            raise RuntimeError(
                "Firewall Manager broker returned an invalid error payload"
            )
        raise RuntimeError(error)
    return payload


def _broker(
    action: str,
    *,
    rule_action: str | None = None,
    direction: str | None = None,
    protocol: str | None = None,
    port: int | None = None,
    remote: str | None = None,
    interface: str | None = None,
    rule_id: str | None = None,
    token: str | None = None,
    confirm: bool = False,
) -> dict:
    cos_bin = _cos_binary()
    if not cos_bin:
        raise FileNotFoundError(
            "cos binary not found; Firewall Manager broker unavailable"
        )
    argv = [cos_bin, "__firewall", action]
    for flag, value in [
        ("--rule-action", rule_action),
        ("--direction", direction),
        ("--protocol", protocol),
        ("--port", str(port) if port is not None else None),
        ("--remote", remote),
        ("--interface", interface),
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
    except FileNotFoundError as exc:
        raise FileNotFoundError(
            f"Firewall Manager broker executable not found: {cos_bin}"
        ) from exc
    except PermissionError as exc:
        raise PermissionError(
            f"permission denied launching Firewall Manager broker: {cos_bin}"
        ) from exc
    except subprocess.TimeoutExpired as exc:
        raise TimeoutError(
            f"Firewall Manager broker exceeded {TIMEOUT_SECS}s for {action}"
        ) from exc

    payloads = []
    for output in (result.stdout.strip(), result.stderr.strip()):
        if not output:
            continue
        payloads.append(_parse_payload(output))
        if result.returncode == 0:
            break
    if result.returncode != 0:
        raise RuntimeError(f"Firewall Manager broker exited {result.returncode}")
    if not payloads:
        raise RuntimeError("Firewall Manager broker returned invalid JSON")
    return payloads[0]


def status() -> dict:
    policy.require("sys.observe", name="firewall")
    return _broker("status")


def add(
    action: str,
    direction: str,
    protocol: str,
    port: int,
    remote: str | None = None,
    interface: str | None = None,
) -> dict:
    action = _choice(action, "action", ACTIONS)
    direction = _choice(direction, "direction", DIRECTIONS)
    protocol = _choice(protocol, "protocol", PROTOCOLS)
    port = _port(port)
    if remote is not None:
        remote = _remote_cidr(remote)
    if interface is not None:
        interface = _interface(interface)
    policy.require("net.firewall", name="manage")
    return _broker(
        "add",
        rule_action=action,
        direction=direction,
        protocol=protocol,
        port=port,
        remote=remote,
        interface=interface,
    )


def delete(rule_id: str) -> dict:
    rule_id = _identifier(rule_id, "rule_id")
    policy.require("net.firewall", name="manage")
    return _broker("delete", rule_id=rule_id)


def clear(confirm: bool) -> dict:
    _require_confirmation("clear", confirm)
    policy.require("net.firewall", name="manage")
    return _broker("clear", confirm=True)


def restore(backup_token: str, confirm: bool) -> dict:
    backup_token = _identifier(backup_token, "backup_token")
    _require_confirmation("restore", confirm)
    policy.require("net.firewall", name="manage")
    return _broker("restore", token=backup_token, confirm=True)
