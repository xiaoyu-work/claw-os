"""user-manager — critical local identity management through clawd."""

import json
import os
import re
import shutil
import subprocess
import sys

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from _shared.env_scrub import scrub_env  # noqa: E402

from cos_runtime import policy  # noqa: E402


NAME_RE = re.compile(r"^[a-z_][a-z0-9_-]{0,31}$")
CREDENTIAL_RE = re.compile(r"^[A-Za-z0-9_.:-]+/[A-Za-z0-9_.:-]+$")
TOKEN_RE = re.compile(r"^[0-9A-Fa-f]{32}$")
TIMEOUT_SECS = int(os.environ.get("CLAW_USER_MANAGER_TIMEOUT", "300"))


def _cos_binary():
    return os.environ.get("COS_BIN") or shutil.which("cos")


def _broker(action, **values):
    cos_bin = _cos_binary()
    if not cos_bin:
        return {"error": "cos binary not found; User Manager broker unavailable"}
    argv = [cos_bin, "__users", action]
    for key in ["user", "group", "full_name", "shell", "groups", "credential", "token"]:
        value = values.get(key)
        if value is not None:
            argv.extend([f"--{key.replace('_', '-')}", value])
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
        return {"error": f"User Manager broker exceeded {TIMEOUT_SECS}s"}
    payload_text = (result.stdout or "").strip() or (result.stderr or "").strip()
    try:
        payload = json.loads(payload_text) if payload_text else {}
    except json.JSONDecodeError:
        return {"error": "User Manager broker returned invalid JSON"}
    if result.returncode != 0 and "error" not in payload:
        payload["error"] = f"User Manager broker exited {result.returncode}"
    return payload


def _name(raw, kind):
    if NAME_RE.fullmatch(raw or "") is None:
        raise ValueError(f"invalid {kind} name")
    return raw


def _manage():
    policy.require("sys.identity", name="manage")


def run(command, args):
    if command == "status":
        if args:
            return {"error": "status takes no arguments"}
        policy.require("sys.observe", name="identities")
        return _broker(command)
    if command == "create-user":
        if not args:
            return {"error": "create-user requires <user> [--full-name NAME] [--shell PATH] [--groups A,B]"}
        try:
            user = _name(args[0], "user")
        except ValueError as exc:
            return {"error": str(exc)}
        values = {"user": user}
        index = 1
        while index < len(args):
            if args[index] in {"--full-name", "--shell", "--groups"} and index + 1 < len(args):
                values[args[index][2:].replace("-", "_")] = args[index + 1]
                index += 2
            else:
                return {"error": f"unexpected create-user argument: {args[index]}"}
        _manage()
        return _broker(command, **values)
    if command in {"delete-user", "delete-group"}:
        if len(args) != 2 or args[1] != "--confirm":
            return {"error": f"{command} requires <name> --confirm"}
        try:
            name = _name(args[0], "user" if command == "delete-user" else "group")
        except ValueError as exc:
            return {"error": str(exc)}
        _manage()
        return _broker(command, confirm=True, **({"user": name} if command == "delete-user" else {"group": name}))
    if command in {"lock-user", "unlock-user", "create-group"}:
        if len(args) != 1:
            return {"error": f"{command} requires one name"}
        key = "group" if command == "create-group" else "user"
        try:
            name = _name(args[0], key)
        except ValueError as exc:
            return {"error": str(exc)}
        _manage()
        return _broker(command, **{key: name})
    if command == "set-shell":
        if len(args) != 2 or not os.path.isabs(args[1]):
            return {"error": "set-shell requires <user> <absolute-shell>"}
        try:
            user = _name(args[0], "user")
        except ValueError as exc:
            return {"error": str(exc)}
        _manage()
        return _broker(command, user=user, shell=args[1])
    if command == "set-password":
        if len(args) != 2 or CREDENTIAL_RE.fullmatch(args[1]) is None:
            return {"error": "set-password requires <user> <namespace/name credential>"}
        try:
            user = _name(args[0], "user")
        except ValueError as exc:
            return {"error": str(exc)}
        _manage()
        policy.require("secret.read", name=args[1])
        return _broker(command, user=user, credential=args[1])
    if command in {"add-to-group", "remove-from-group"}:
        if len(args) != 2:
            return {"error": f"{command} requires <user> <group>"}
        try:
            user = _name(args[0], "user")
            group = _name(args[1], "group")
        except ValueError as exc:
            return {"error": str(exc)}
        _manage()
        return _broker(command, user=user, group=group)
    if command == "restore":
        if len(args) != 2 or args[1] != "--confirm" or TOKEN_RE.fullmatch(args[0]) is None:
            return {"error": "restore requires <backup-token> --confirm"}
        _manage()
        return _broker(command, token=args[0].lower(), confirm=True)
    return {"error": f"unknown command: {command}"}
