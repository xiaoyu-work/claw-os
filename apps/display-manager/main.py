"""display-manager — COSMIC output and backlight control."""

import json
import math
import os
import re
import shutil
import subprocess
import sys
import unicodedata

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from _shared.env_scrub import scrub_env  # noqa: E402

from cos_runtime import policy  # noqa: E402


NAME_RE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9_.:-]*$")
TOKEN_RE = re.compile(r"^[0-9A-Fa-f]{32}$")
TRANSFORMS = frozenset(
    {
        "normal",
        "rotate90",
        "rotate180",
        "rotate270",
        "flipped",
        "flipped90",
        "flipped180",
        "flipped270",
    }
)
ADAPTIVE = frozenset({"true", "automatic", "false"})
TIMEOUT_SECS = int(os.environ.get("CLAW_DISPLAY_MANAGER_TIMEOUT", "180"))


def _cos_binary() -> str | None:
    return os.environ.get("COS_BIN") or shutil.which("cos")


def _name(raw: object, kind: str, maximum: int) -> str:
    if (
        not isinstance(raw, str)
        or not 1 <= len(raw) <= maximum
        or NAME_RE.fullmatch(raw) is None
    ):
        raise ValueError(f"invalid {kind}")
    return raw


def _output(raw: object) -> str:
    return _name(raw, "output", 64)


def _backlight(raw: object) -> str:
    return _name(raw, "backlight", 128)


def _integer(raw: object, name: str, minimum: int, maximum: int) -> int:
    if isinstance(raw, bool) or not isinstance(raw, int):
        raise ValueError(f"{name} must be an integer")
    if not minimum <= raw <= maximum:
        raise ValueError(f"{name} must be {minimum}..{maximum}")
    return raw


def _number(
    raw: object,
    name: str,
    minimum: float,
    maximum: float,
) -> float:
    if isinstance(raw, bool) or not isinstance(raw, (int, float)):
        raise ValueError(f"{name} must be a number")
    value = float(raw)
    if not math.isfinite(value):
        raise ValueError(f"{name} must be finite")
    if not minimum <= value <= maximum:
        raise ValueError(f"{name} must be {minimum:g}..{maximum:g}")
    return value


def _choice(raw: object, name: str, choices: frozenset[str]) -> str:
    if not isinstance(raw, str) or raw not in choices:
        raise ValueError(f"invalid {name}")
    return raw


def _layout_source(raw: object) -> str:
    message = (
        "layout source must be an absolute canonical non-symlink path "
        "without control characters"
    )
    if (
        not isinstance(raw, str)
        or not os.path.isabs(raw)
        or len(raw) > 4096
        or any(unicodedata.category(character) == "Cc" for character in raw)
    ):
        raise ValueError(message)
    canonical = os.path.realpath(raw)
    if canonical != raw or os.path.islink(raw):
        raise ValueError(message)
    return canonical


def _backup_token(raw: object) -> str:
    if not isinstance(raw, str) or TOKEN_RE.fullmatch(raw) is None:
        raise ValueError("backup token must be exactly 32 hexadecimal characters")
    return raw.lower()


def _require_confirmation(action: str, confirm: object) -> None:
    if confirm is not True:
        raise ValueError(f"{action} requires confirm=true")


def _parse_payload(payload_text: str) -> dict:
    try:
        payload = json.loads(payload_text)
    except json.JSONDecodeError as exc:
        raise RuntimeError("Display Manager broker returned invalid JSON") from exc
    if not isinstance(payload, dict):
        raise RuntimeError("Display Manager broker returned a non-object result")
    if "error" in payload:
        error = payload["error"]
        if not isinstance(error, str) or not error:
            raise RuntimeError(
                "Display Manager broker returned an invalid error payload"
            )
        raise RuntimeError(error)
    return payload


def _broker(
    action: str,
    *,
    output: str | None = None,
    source_output: str | None = None,
    width: int | None = None,
    height: int | None = None,
    refresh: float | None = None,
    scale: float | None = None,
    x: int | None = None,
    y: int | None = None,
    transform: str | None = None,
    adaptive_sync: str | None = None,
    source: str | None = None,
    backlight: str | None = None,
    percent: int | None = None,
    token: str | None = None,
    confirm: bool = False,
) -> dict:
    cos_bin = _cos_binary()
    if not cos_bin:
        raise FileNotFoundError(
            "cos binary not found; Display Manager broker unavailable"
        )
    argv = [cos_bin, "__display", action]
    options = (
        ("--output", output),
        ("--from", source_output),
        ("--width", width),
        ("--height", height),
        ("--refresh", refresh),
        ("--scale", scale),
        ("--x", x),
        ("--y", y),
        ("--transform", transform),
        ("--adaptive-sync", adaptive_sync),
        ("--source", source),
        ("--backlight", backlight),
        ("--percent", percent),
        ("--token", token),
    )
    for flag, value in options:
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
    except FileNotFoundError as exc:
        raise FileNotFoundError(
            f"Display Manager broker executable not found: {cos_bin}"
        ) from exc
    except PermissionError as exc:
        raise PermissionError(
            f"permission denied launching Display Manager broker: {cos_bin}"
        ) from exc
    except subprocess.TimeoutExpired as exc:
        raise TimeoutError(
            f"Display Manager broker exceeded {TIMEOUT_SECS}s for {action}"
        ) from exc

    payloads = []
    for output_text in (
        (result.stdout or "").strip(),
        (result.stderr or "").strip(),
    ):
        if not output_text:
            continue
        payloads.append(_parse_payload(output_text))
        if result.returncode == 0:
            break
    if result.returncode != 0:
        raise RuntimeError(f"Display Manager broker exited {result.returncode}")
    if not payloads:
        raise RuntimeError("Display Manager broker returned invalid JSON")
    return payloads[0]


def _manage() -> None:
    policy.require("device.display", name="manage")


def status() -> dict:
    policy.require("sys.observe", name="display")
    return _broker("status")


def enable(output: str) -> dict:
    output = _output(output)
    _manage()
    return _broker("enable", output=output)


def disable(output: str) -> dict:
    output = _output(output)
    _manage()
    return _broker("disable", output=output)


def mirror(output: str, source_output: str) -> dict:
    output = _output(output)
    source_output = _output(source_output)
    _manage()
    return _broker("mirror", output=output, source_output=source_output)


def position(output: str, x: int, y: int) -> dict:
    output = _output(output)
    x = _integer(x, "x", -32768, 32768)
    y = _integer(y, "y", -32768, 32768)
    _manage()
    return _broker("position", output=output, x=x, y=y)


def mode(
    output: str,
    width: int,
    height: int,
    adaptive_sync: str | None = None,
    refresh: float | None = None,
    scale: float | None = None,
    x: int | None = None,
    y: int | None = None,
    transform: str | None = None,
) -> dict:
    output = _output(output)
    width = _integer(width, "width", 1, 16384)
    height = _integer(height, "height", 1, 16384)
    if adaptive_sync is not None:
        adaptive_sync = _choice(adaptive_sync, "adaptive-sync mode", ADAPTIVE)
    if refresh is not None:
        refresh = _number(refresh, "refresh", 1.0, 1000.0)
    if scale is not None:
        scale = _number(scale, "scale", 0.5, 4.0)
    if x is not None:
        x = _integer(x, "x", -32768, 32768)
    if y is not None:
        y = _integer(y, "y", -32768, 32768)
    if (x is None) != (y is None):
        raise ValueError("x and y must be provided together")
    if transform is not None:
        transform = _choice(transform, "transform", TRANSFORMS)
    _manage()
    return _broker(
        "mode",
        output=output,
        width=width,
        height=height,
        refresh=refresh,
        scale=scale,
        x=x,
        y=y,
        transform=transform,
        adaptive_sync=adaptive_sync,
    )


def scale(output: str, scale: float) -> dict:
    output = _output(output)
    scale = _number(scale, "scale", 0.5, 4.0)
    _manage()
    return _broker("scale", output=output, scale=scale)


def apply_layout(source: str, confirm: bool) -> dict:
    source = _layout_source(source)
    _require_confirmation("apply-layout", confirm)
    _manage()
    policy.require("fs.read", path=source)
    return _broker("apply-layout", source=source, confirm=True)


def brightness(backlight: str, percent: int) -> dict:
    backlight = _backlight(backlight)
    percent = _integer(percent, "brightness percent", 1, 100)
    _manage()
    return _broker("brightness", backlight=backlight, percent=percent)


def restore(backup_token: str, confirm: bool) -> dict:
    backup_token = _backup_token(backup_token)
    _require_confirmation("restore", confirm)
    _manage()
    return _broker("restore", token=backup_token, confirm=True)
