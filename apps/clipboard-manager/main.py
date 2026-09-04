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


def _cos_binary() -> str | None:
    return os.environ.get("COS_BIN") or shutil.which("cos")


def _broker(
    action: str,
    mime: str | None = None,
    source: str | None = None,
    primary: bool = False,
    confirm: bool = False,
) -> dict:
    cos_bin = _cos_binary()
    if not cos_bin:
        raise FileNotFoundError(
            "cos binary not found; Clipboard Manager broker unavailable"
        )
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
    except FileNotFoundError as exc:
        raise FileNotFoundError(
            f"Clipboard Manager broker executable not found: {cos_bin}"
        ) from exc
    except PermissionError as exc:
        raise PermissionError(
            f"permission denied launching Clipboard Manager broker: {cos_bin}"
        ) from exc
    except subprocess.TimeoutExpired as exc:
        raise TimeoutError(
            f"Clipboard Manager broker exceeded {TIMEOUT_SECS}s"
        ) from exc
    payload_text = (result.stdout or "").strip() or (result.stderr or "").strip()
    try:
        payload = json.loads(payload_text) if payload_text else {}
    except json.JSONDecodeError as exc:
        raise RuntimeError(
            "Clipboard Manager broker returned invalid JSON"
        ) from exc
    if not isinstance(payload, dict):
        raise RuntimeError("Clipboard Manager broker returned a non-object result")
    if "error" in payload:
        error = payload["error"]
        if not isinstance(error, str) or not error:
            raise RuntimeError(
                "Clipboard Manager broker returned an invalid error payload"
            )
        raise RuntimeError(error)
    if result.returncode != 0:
        raise RuntimeError(f"Clipboard Manager broker exited {result.returncode}")
    return payload


def _validate_selection(primary: bool) -> None:
    if type(primary) is not bool:
        raise ValueError("primary must be a boolean")


def _validate_mime(mime: str | None) -> None:
    if mime is None:
        return
    if (
        not isinstance(mime, str)
        or not mime
        or len(mime) > 255
        or "/" not in mime
        or mime.startswith("-")
        or any(character.isspace() or ord(character) < 32 for character in mime)
    ):
        raise ValueError("invalid MIME type")


def status(primary: bool = False) -> dict:
    _validate_selection(primary)
    policy.require("clipboard.read", name="selection")
    return _broker("status", primary=primary)


def list_types(primary: bool = False) -> dict:
    _validate_selection(primary)
    policy.require("clipboard.read", name="selection")
    return _broker("types", primary=primary)


def read(mime: str | None = None, primary: bool = False) -> dict:
    _validate_mime(mime)
    _validate_selection(primary)
    policy.require("clipboard.read", name="selection")
    return _broker("read", mime=mime, primary=primary)


def write(
    source: str,
    mime: str | None = None,
    primary: bool = False,
) -> dict:
    if (
        not isinstance(source, str)
        or not source
        or len(source) > 4096
        or any(ord(character) < 32 for character in source)
    ):
        raise ValueError("source must be a canonical non-symlink path")
    canonical_source = os.path.realpath(source)
    if (
        canonical_source != source
        or os.path.islink(source)
        or not os.path.isabs(canonical_source)
    ):
        raise ValueError("source must be a canonical non-symlink path")
    _validate_mime(mime)
    _validate_selection(primary)
    policy.require("clipboard.write", name="selection")
    policy.require("fs.read", path=canonical_source)
    return _broker(
        "write",
        mime=mime,
        source=canonical_source,
        primary=primary,
    )


def clear(confirm: bool, primary: bool = False) -> dict:
    if type(confirm) is not bool or not confirm:
        raise ValueError("clear requires confirmation")
    _validate_selection(primary)
    policy.require("clipboard.write", name="selection")
    return _broker("clear", primary=primary, confirm=True)
