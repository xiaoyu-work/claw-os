"""container-manager — Docker, Podman, and containerd inspection/control."""

import json
import os
import re
import shutil
import subprocess
import sys

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from _shared.env_scrub import scrub_env  # noqa: E402

from cos_runtime import policy  # noqa: E402


RUNTIMES = frozenset({"docker", "podman", "podman-root", "containerd"})
READ_ACTIONS = frozenset({"status", "list", "inspect", "logs", "processes", "stats", "namespaces"})
MUTATING = frozenset({"start", "stop", "restart", "pause", "unpause", "kill", "remove"})
SIGNALS = frozenset({"TERM", "KILL", "HUP", "INT", "USR1", "USR2"})
IDENTIFIER_RE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._:-]{0,254}$")
TIMEOUT_SECS = int(os.environ.get("CLAW_CONTAINER_MANAGER_TIMEOUT", "300"))


def _cos_binary():
    return os.environ.get("COS_BIN") or shutil.which("cos")


def _broker(action, runtime=None, target=None, namespace=None, lines=None, signal=None, confirm=False):
    cos_bin = _cos_binary()
    if not cos_bin:
        return {"error": "cos binary not found; Container Manager broker unavailable"}
    argv = [cos_bin, "__container", action]
    for flag, value in [
        ("--runtime", runtime),
        ("--target", target),
        ("--namespace", namespace),
        ("--lines", lines),
        ("--signal", signal),
    ]:
        if value is not None:
            argv.extend([flag, str(value)])
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
        return {"error": str(exc)}
    except subprocess.TimeoutExpired:
        return {"error": f"Container Manager broker exceeded {TIMEOUT_SECS}s"}
    payload_text = (result.stdout or "").strip() or (result.stderr or "").strip()
    try:
        payload = json.loads(payload_text) if payload_text else {}
    except json.JSONDecodeError:
        return {"error": "Container Manager broker returned invalid JSON"}
    if result.returncode != 0 and "error" not in payload:
        payload["error"] = f"Container Manager broker exited {result.returncode}"
    return payload


def _runtime(raw):
    if raw not in RUNTIMES:
        raise ValueError("runtime must be docker, podman, podman-root, or containerd")
    return raw


def _identifier(raw, name):
    if not isinstance(raw, str) or IDENTIFIER_RE.fullmatch(raw) is None:
        raise ValueError(f"{name} is invalid")
    return raw


def _base(args, target=True):
    minimum = 2 if target else 1
    if len(args) < minimum:
        raise ValueError("missing runtime or container target")
    runtime = _runtime(args[0])
    container = _identifier(args[1], "container") if target else None
    remainder = args[2:] if target else args[1:]
    namespace = None
    if runtime == "containerd":
        if not remainder:
            raise ValueError("containerd requires a namespace")
        namespace = _identifier(remainder[0], "namespace")
        remainder = remainder[1:]
    elif remainder and remainder[0] not in {"--confirm"} and not remainder[0].isdigit() and remainder[0] not in SIGNALS:
        raise ValueError("only containerd accepts a namespace")
    return runtime, container, namespace, remainder


def run(command, args):
    if command == "status":
        if args:
            return {"error": "status takes no arguments"}
        policy.require("sys.container", name="observe")
        return _broker(command)
    if command == "list":
        try:
            runtime, _, namespace, rest = _base(args, target=False)
        except ValueError as exc:
            return {"error": str(exc)}
        if rest:
            return {"error": "list received unexpected arguments"}
        policy.require("sys.container", name="observe")
        return _broker(command, runtime=runtime, namespace=namespace)
    if command in {"inspect", "processes", "stats", "namespaces"}:
        try:
            runtime, target, namespace, rest = _base(args)
        except ValueError as exc:
            return {"error": str(exc)}
        if rest:
            return {"error": f"{command} received unexpected arguments"}
        policy.require("sys.container", name="observe")
        return _broker(command, runtime=runtime, target=target, namespace=namespace)
    if command == "logs":
        try:
            runtime, target, namespace, rest = _base(args)
            lines = int(rest[0]) if rest else 100
        except (ValueError, TypeError) as exc:
            return {"error": str(exc)}
        if len(rest) > 1 or not 1 <= lines <= 1000:
            return {"error": "logs lines must be 1..1000"}
        policy.require("sys.container", name="observe")
        return _broker(command, runtime=runtime, target=target, namespace=namespace, lines=lines)
    if command in {"start", "stop", "restart", "pause", "unpause"}:
        try:
            runtime, target, namespace, rest = _base(args)
        except ValueError as exc:
            return {"error": str(exc)}
        if rest:
            return {"error": f"{command} received unexpected arguments"}
        policy.require("sys.container", name="control")
        return _broker(command, runtime=runtime, target=target, namespace=namespace)
    if command == "kill":
        if len(args) not in {3, 4}:
            return {"error": f"kill requires <runtime> <target> <signal> [namespace]"}
        try:
            runtime = _runtime(args[0])
            target = _identifier(args[1], "container")
            signal = args[2].upper()
            namespace = (
                _identifier(args[3], "namespace")
                if runtime == "containerd" and len(args) == 4
                else None
            )
        except ValueError as exc:
            return {"error": str(exc)}
        if signal not in SIGNALS:
            return {"error": f"kill requires one signal: {', '.join(sorted(SIGNALS))}"}
        if runtime == "containerd" and namespace is None:
            return {"error": "containerd requires a namespace"}
        if runtime != "containerd" and len(args) != 3:
            return {"error": "only containerd accepts a namespace"}
        policy.require("sys.container", name="control")
        return _broker(
            command,
            runtime=runtime,
            target=target,
            namespace=namespace,
            signal=signal,
        )
    if command == "remove":
        confirm = "--confirm" in args
        filtered = [arg for arg in args if arg != "--confirm"]
        try:
            runtime, target, namespace, rest = _base(filtered)
        except ValueError as exc:
            return {"error": str(exc)}
        if rest or not confirm:
            return {"error": "remove requires <runtime> <target> [namespace] --confirm"}
        policy.require("sys.container", name="control")
        return _broker(
            command,
            runtime=runtime,
            target=target,
            namespace=namespace,
            confirm=True,
        )
    return {"error": f"unknown command: {command}"}
