"""network-manager — NetworkManager control through the root clawd broker."""

import json
import os
import shutil
import subprocess
import sys

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from _shared.env_scrub import scrub_env  # noqa: E402

from cos_runtime import policy  # noqa: E402


TIMEOUT_SECS = int(os.environ.get("CLAW_NETWORK_MANAGER_TIMEOUT", "180"))
READ_ACTIONS = frozenset({"status", "wifi-list", "connection-list", "vpn-list"})


def _cos_binary():
    return os.environ.get("COS_BIN") or shutil.which("cos")


def _broker(action, target=None, state=None, credential=None):
    cos_bin = _cos_binary()
    if not cos_bin:
        return {"error": "cos binary not found; NetworkManager broker unavailable"}
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
    except (FileNotFoundError, PermissionError) as exc:
        return {"error": str(exc)}
    except subprocess.TimeoutExpired:
        return {"error": f"NetworkManager broker exceeded {TIMEOUT_SECS}s"}
    payload_text = (result.stdout or "").strip() or (result.stderr or "").strip()
    try:
        payload = json.loads(payload_text) if payload_text else {}
    except json.JSONDecodeError:
        return {"error": "NetworkManager broker returned invalid JSON"}
    if result.returncode != 0 and "error" not in payload:
        payload["error"] = f"NetworkManager broker exited {result.returncode}"
    return payload


def run(command, args):
    from canonical_argv import normalize_canonical_argv
    args = normalize_canonical_argv(args)
    if command in READ_ACTIONS:
        if args:
            return {"error": f"{command} takes no arguments"}
        policy.require("sys.observe", name="network")
        return _broker(command)
    if command == "wifi-connect":
        if not 1 <= len(args) <= 2:
            return {"error": "wifi-connect requires <ssid> [credential]"}
        ssid = args[0]
        credential = args[1] if len(args) == 2 else None
        policy.require("net.manage", name="wifi")
        if credential:
            policy.require("secret.read", name=credential)
        return _broker(command, ssid, None, credential)
    if command in {"wifi-disconnect", "wifi-forget", "vpn-up", "vpn-down"}:
        if len(args) != 1:
            return {"error": f"{command} requires one target"}
        policy.require(
            "net.manage",
            name="vpn" if command.startswith("vpn-") else "wifi",
        )
        return _broker(command, args[0])
    if command in {"wifi-toggle", "airplane"}:
        if len(args) != 1 or args[0] not in {"on", "off"}:
            return {"error": f"{command} requires on|off"}
        policy.require("net.manage", name="wifi" if command == "wifi-toggle" else "airplane")
        return _broker(command, None, args[0])
    return {"error": f"unknown command: {command}"}
