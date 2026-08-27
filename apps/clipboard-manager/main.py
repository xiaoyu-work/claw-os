"""clipboard-manager — sensitive Wayland clipboard access."""

import json
import os
import shutil
import subprocess
import sys

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from _shared.env_scrub import scrub_env  # noqa: E402

from cos_runtime import policy  # noqa: E402


TIMEOUT_SECS = int(os.environ.get("CLAW_CLIPBOARD_MANAGER_TIMEOUT", "120"))


def _cos_binary():
    return os.environ.get("COS_BIN") or shutil.which("cos")


def _broker(action, mime=None, source=None, primary=False, confirm=False):
    cos_bin = _cos_binary()
    if not cos_bin:
        return {"error": "cos binary not found; Clipboard Manager broker unavailable"}
    argv = [cos_bin, "__clipboard", action]
    if mime is not None:
        argv.extend(["--mime", mime])
    if source is not None:
        argv.extend(["--source", source])
    if primary:
        argv.append("--primary")
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
        return {"error": f"Clipboard Manager broker exceeded {TIMEOUT_SECS}s"}
    payload_text = (result.stdout or "").strip() or (result.stderr or "").strip()
    try:
        payload = json.loads(payload_text) if payload_text else {}
    except json.JSONDecodeError:
        return {"error": "Clipboard Manager broker returned invalid JSON"}
    if result.returncode != 0 and "error" not in payload:
        payload["error"] = f"Clipboard Manager broker exited {result.returncode}"
    return payload


def _options(args):
    primary = "--primary" in args
    return primary, [arg for arg in args if arg != "--primary"]


def _mime(value):
    if (
        not value
        or len(value) > 255
        or "/" not in value
        or value.startswith("-")
        or any(character.isspace() or ord(character) < 32 for character in value)
    ):
        raise ValueError("invalid MIME type")
    return value


def run(command, args):
    primary, values = _options(args)
    if command in {"status", "types"}:
        if values:
            return {"error": f"{command} accepts only --primary"}
        policy.require("clipboard.read", name="selection")
        return _broker(command, primary=primary)
    if command == "read":
        if len(values) > 1:
            return {"error": "read accepts [mime] [--primary]"}
        try:
            mime = _mime(values[0]) if values else None
        except ValueError as exc:
            return {"error": str(exc)}
        policy.require("clipboard.read", name="selection")
        return _broker(command, mime=mime, primary=primary)
    if command == "write":
        if not 1 <= len(values) <= 2:
            return {"error": "write requires <source> [mime] [--primary]"}
        source = os.path.realpath(values[0])
        if source != values[0] or os.path.islink(values[0]) or not os.path.isabs(source):
            return {"error": "source must be a canonical non-symlink path"}
        try:
            mime = _mime(values[1]) if len(values) == 2 else None
        except ValueError as exc:
            return {"error": str(exc)}
        policy.require("clipboard.write", name="selection")
        policy.require("fs.read", path=source)
        return _broker(command, mime=mime, source=source, primary=primary)
    if command == "clear":
        confirm = "--confirm" in values
        remaining = [value for value in values if value != "--confirm"]
        if remaining or not confirm:
            return {"error": "clear requires --confirm [--primary]"}
        policy.require("clipboard.write", name="selection")
        return _broker(command, primary=primary, confirm=True)
    return {"error": f"unknown command: {command}"}
