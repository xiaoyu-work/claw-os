"""display-manager — COSMIC output and backlight control."""

import json
import os
import re
import shutil
import subprocess
import sys

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from _shared.env_scrub import scrub_env  # noqa: E402

from cos_runtime import policy  # noqa: E402


NAME_RE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9_.:-]{0,63}$")
TOKEN_RE = re.compile(r"^[0-9A-Fa-f]{32}$")
TRANSFORMS = frozenset({"normal", "rotate90", "rotate180", "rotate270", "flipped", "flipped90", "flipped180", "flipped270"})
ADAPTIVE = frozenset({"true", "automatic", "false"})
TIMEOUT_SECS = int(os.environ.get("CLAW_DISPLAY_MANAGER_TIMEOUT", "180"))


def _cos_binary():
    return os.environ.get("COS_BIN") or shutil.which("cos")


def _broker(action, **values):
    cos_bin = _cos_binary()
    if not cos_bin:
        return {"error": "cos binary not found; Display Manager broker unavailable"}
    argv = [cos_bin, "__display", action]
    for key in ["output", "from", "width", "height", "refresh", "scale", "x", "y", "transform", "adaptive_sync", "source", "backlight", "percent", "token"]:
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
        return {"error": f"Display Manager broker exceeded {TIMEOUT_SECS}s"}
    payload_text = (result.stdout or "").strip() or (result.stderr or "").strip()
    try:
        payload = json.loads(payload_text) if payload_text else {}
    except json.JSONDecodeError:
        return {"error": "Display Manager broker returned invalid JSON"}
    if result.returncode != 0 and "error" not in payload:
        payload["error"] = f"Display Manager broker exited {result.returncode}"
    return payload


def _name(raw, kind):
    if NAME_RE.fullmatch(raw or "") is None:
        raise ValueError(f"invalid {kind}")
    return raw


def _manage():
    policy.require("device.display", name="manage")


def run(command, args):
    if command == "__schema__":
        return _schema()
    if command == "status":
        if args:
            return {"error": "status takes no arguments"}
        policy.require("sys.observe", name="display")
        return _broker(command)
    if command in {"enable", "disable"}:
        if len(args) != 1:
            return {"error": f"{command} requires <output>"}
        try:
            output = _name(args[0], "output")
        except ValueError as exc:
            return {"error": str(exc)}
        _manage()
        return _broker(command, output=output)
    if command == "mirror":
        if len(args) != 2:
            return {"error": "mirror requires <output> <from-output>"}
        try:
            output = _name(args[0], "output")
            source = _name(args[1], "source output")
        except ValueError as exc:
            return {"error": str(exc)}
        _manage()
        return _broker(command, output=output, **{"from": source})
    if command == "position":
        if len(args) != 3:
            return {"error": "position requires <output> <x> <y>"}
        try:
            output = _name(args[0], "output")
            x, y = int(args[1]), int(args[2])
        except ValueError as exc:
            return {"error": str(exc)}
        _manage()
        return _broker(command, output=output, x=x, y=y)
    if command == "mode":
        if len(args) < 3:
            return {"error": "mode requires <output> <width> <height> [options]"}
        try:
            output = _name(args[0], "output")
            width, height = int(args[1]), int(args[2])
        except ValueError as exc:
            return {"error": str(exc)}
        values = {"output": output, "width": width, "height": height}
        index = 3
        while index < len(args):
            if args[index] in {"--refresh", "--scale", "--x", "--y", "--transform", "--adaptive-sync"} and index + 1 < len(args):
                key = args[index][2:].replace("-", "_")
                raw = args[index + 1]
                try:
                    values[key] = float(raw) if key in {"refresh", "scale"} else int(raw) if key in {"x", "y"} else raw
                except ValueError:
                    return {"error": f"invalid {key}"}
                index += 2
            else:
                return {"error": f"unexpected mode argument: {args[index]}"}
        if values.get("transform") is not None and values["transform"] not in TRANSFORMS:
            return {"error": "invalid transform"}
        if values.get("adaptive_sync") is not None and values["adaptive_sync"] not in ADAPTIVE:
            return {"error": "invalid adaptive-sync mode"}
        _manage()
        return _broker(command, **values)
    if command == "scale":
        if len(args) != 2:
            return {"error": "scale requires <output> <scale>"}
        try:
            output = _name(args[0], "output")
            scale = float(args[1])
        except ValueError as exc:
            return {"error": str(exc)}
        _manage()
        return _broker(command, output=output, scale=scale)
    if command == "apply-layout":
        if len(args) != 2 or args[1] != "--confirm":
            return {"error": "apply-layout requires <source-kdl> --confirm"}
        source = os.path.realpath(args[0])
        if source != args[0] or os.path.islink(args[0]) or not os.path.isabs(source):
            return {"error": "layout source must be a canonical non-symlink path"}
        _manage()
        policy.require("fs.read", path=source)
        return _broker(command, source=source, confirm=True)
    if command == "brightness":
        if len(args) != 2:
            return {"error": "brightness requires <backlight> <percent>"}
        try:
            backlight = _name(args[0], "backlight")
            percent = int(args[1])
        except ValueError as exc:
            return {"error": str(exc)}
        if not 1 <= percent <= 100:
            return {"error": "brightness percent must be 1..100"}
        _manage()
        return _broker(command, backlight=backlight, percent=percent)
    if command == "restore":
        if len(args) != 2 or args[1] != "--confirm" or TOKEN_RE.fullmatch(args[0]) is None:
            return {"error": "restore requires <backup-token> --confirm"}
        _manage()
        return _broker(command, token=args[0].lower(), confirm=True)
    return {"error": f"unknown command: {command}"}


def _schema():
    return {
        command: {"description": f"{command} display operation", "parameters": []}
        for command in ["status", "enable", "disable", "mirror", "position", "mode", "scale", "apply-layout", "brightness", "restore"]
    }
