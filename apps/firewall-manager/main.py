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


def _cos_binary():
    return os.environ.get("COS_BIN") or shutil.which("cos")


def _broker(action, **values):
    cos_bin = _cos_binary()
    if not cos_bin:
        return {"error": "cos binary not found; Firewall Manager broker unavailable"}
    argv = [cos_bin, "__firewall", action]
    for key in [
        "rule_action",
        "direction",
        "protocol",
        "port",
        "remote",
        "interface",
        "rule_id",
        "token",
    ]:
        value = values.get(key)
        if value is not None:
            argv.extend([f"--{key.replace('_', '-')}", str(value)])
    if values.get("confirm"):
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
        return {"error": str(exc)}
    except subprocess.TimeoutExpired:
        return {"error": f"Firewall Manager broker exceeded {TIMEOUT_SECS}s"}
    payload_text = (result.stdout or "").strip() or (result.stderr or "").strip()
    try:
        payload = json.loads(payload_text) if payload_text else {}
    except json.JSONDecodeError:
        return {"error": "Firewall Manager broker returned invalid JSON"}
    if result.returncode != 0 and "error" not in payload:
        payload["error"] = f"Firewall Manager broker exited {result.returncode}"
    return payload


def run(command, args):
    from canonical_argv import normalize_canonical_argv
    args = normalize_canonical_argv(args, bool_flags={"confirm"})
    if command == "status":
        if args:
            return {"error": "status takes no arguments"}
        policy.require("sys.observe", name="firewall")
        return _broker(command)
    if command == "add":
        if len(args) < 4:
            return {"error": "add requires <allow|deny> <input|output> <tcp|udp> <port> [--remote CIDR] [--interface IFACE]"}
        rule_action, direction, protocol = args[:3]
        if rule_action not in {"allow", "deny"} or direction not in {"input", "output"} or protocol not in {"tcp", "udp"}:
            return {"error": "invalid firewall action, direction, or protocol"}
        try:
            port = int(args[3])
        except ValueError:
            return {"error": "port must be an integer"}
        if not 1 <= port <= 65535:
            return {"error": "port must be 1..65535"}
        remote = None
        interface = None
        index = 4
        while index < len(args):
            if args[index] == "--remote" and index + 1 < len(args):
                try:
                    remote = str(ipaddress.ip_network(args[index + 1], strict=False))
                except ValueError:
                    return {"error": "invalid remote CIDR"}
                index += 2
            elif args[index] == "--interface" and index + 1 < len(args):
                if IFACE_RE.fullmatch(args[index + 1]) is None:
                    return {"error": "invalid interface name"}
                interface = args[index + 1]
                index += 2
            else:
                return {"error": f"unexpected add argument: {args[index]}"}
        policy.require("net.firewall", name="manage")
        return _broker(
            command,
            rule_action=rule_action,
            direction=direction,
            protocol=protocol,
            port=port,
            remote=remote,
            interface=interface,
        )
    if command == "delete":
        if len(args) != 1 or ID_RE.fullmatch(args[0]) is None:
            return {"error": "delete requires one managed rule id"}
        policy.require("net.firewall", name="manage")
        return _broker(command, rule_id=args[0].lower())
    if command == "clear":
        if args != ["--confirm"]:
            return {"error": "clear requires --confirm"}
        policy.require("net.firewall", name="manage")
        return _broker(command, confirm=True)
    if command == "restore":
        confirm = "--confirm" in args
        values = [value for value in args if value != "--confirm"]
        if len(values) != 1 or not confirm or ID_RE.fullmatch(values[0]) is None:
            return {"error": "restore requires <backup-token> --confirm"}
        policy.require("net.firewall", name="manage")
        return _broker(command, token=values[0].lower(), confirm=True)
    return {"error": f"unknown command: {command}"}
