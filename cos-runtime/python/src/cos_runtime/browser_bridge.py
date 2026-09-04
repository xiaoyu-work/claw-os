"""Private broker bridge for the bundled attached-browser App."""

from __future__ import annotations

import json
import os
import subprocess
from typing import Any

_MAX_REQUEST_BYTES = 128 * 1024
_MAX_RESPONSE_BYTES = 16 * 1024 * 1024
_TIMEOUT_SECONDS = 60
_WIRE_VERSION = 1


class BrowserBridgeError(RuntimeError):
    """Base error raised by the attached-browser broker bridge."""


class PermissionDenied(BrowserBridgeError):
    """The authenticated App session lacks the required browser capability."""


class BrowserUnavailable(BrowserBridgeError):
    """The browser provider or its Native Messaging bridge is unavailable."""


class BrowserActionFailed(BrowserBridgeError):
    """The browser provider completed the request with an explicit failure."""


class BrowserActionIndeterminate(BrowserBridgeError):
    """The browser action may have taken effect before transport failure."""


def _cos_binary() -> str:
    binary = os.environ.get("CLAW_COS_BIN")
    if not binary or not os.path.isabs(binary):
        raise BrowserUnavailable("CLAW_COS_BIN is not an absolute runtime path")
    return binary


def _stop_process(process: subprocess.Popen[bytes]) -> None:
    if process.poll() is None:
        process.kill()
    process.communicate()


def _validate_envelope(envelope: object) -> dict[str, Any]:
    if not isinstance(envelope, dict):
        raise BrowserActionIndeterminate(
            "browser broker returned a non-object wire envelope"
        )
    if (
        type(envelope.get("wire_version")) is not int
        or envelope["wire_version"] != _WIRE_VERSION
        or type(envelope.get("ok")) is not bool
    ):
        raise BrowserActionIndeterminate(
            "browser broker returned an incompatible wire envelope"
        )
    allowed = {"ok", "wire_version", "data"}
    if envelope["ok"] is False:
        allowed = {"ok", "wire_version", "code", "error", "audit_id", "detail"}
        if (
            not isinstance(envelope.get("code"), str)
            or not envelope["code"]
            or not isinstance(envelope.get("error"), str)
            or not envelope["error"]
            or ("audit_id" in envelope and not isinstance(envelope["audit_id"], str))
            or ("detail" in envelope and not isinstance(envelope["detail"], dict))
        ):
            raise BrowserActionIndeterminate(
                "browser broker returned an invalid failure envelope"
            )
    if set(envelope) - allowed:
        raise BrowserActionIndeterminate(
            "browser broker returned undeclared wire fields"
        )
    return envelope


def request(action: str, **fields: Any) -> dict[str, Any]:
    if not action or any(ch not in "abcdefghijklmnopqrstuvwxyz._" for ch in action):
        raise BrowserBridgeError("browser action is invalid")
    payload = {"action": action, **fields}
    encoded = json.dumps(payload, separators=(",", ":"), ensure_ascii=False).encode("utf-8")
    if len(encoded) > _MAX_REQUEST_BYTES:
        raise BrowserBridgeError("browser request exceeds the internal bridge limit")

    command = [
        _cos_binary(),
        "--wire=1",
        "__browser",
        "request",
        "--request-stdin",
    ]
    try:
        process = subprocess.Popen(
            command,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
    except OSError as exc:
        raise BrowserUnavailable(f"run browser broker bridge: {exc}") from exc
    try:
        stdout, _stderr = process.communicate(encoded, timeout=_TIMEOUT_SECONDS)
    except subprocess.TimeoutExpired as exc:
        try:
            _stop_process(process)
        except OSError as cleanup_error:
            raise BrowserActionIndeterminate(
                f"browser broker timed out and cleanup failed: {cleanup_error}"
            ) from exc
        raise BrowserActionIndeterminate(
            "browser broker bridge timed out after process launch"
        ) from exc
    except OSError as exc:
        try:
            _stop_process(process)
        except OSError as cleanup_error:
            raise BrowserActionIndeterminate(
                f"browser broker communication and cleanup failed: {cleanup_error}"
            ) from exc
        raise BrowserActionIndeterminate(
            f"browser broker communication failed after process launch: {exc}"
        ) from exc

    if len(stdout) > _MAX_RESPONSE_BYTES:
        raise BrowserActionIndeterminate(
            "browser broker response exceeds the runtime limit"
        )
    try:
        envelope = _validate_envelope(json.loads(stdout))
    except (UnicodeDecodeError, json.JSONDecodeError) as exc:
        raise BrowserActionIndeterminate(
            "browser broker returned an invalid wire envelope"
        ) from exc

    if envelope.get("ok") is not True:
        if process.returncode == 0:
            raise BrowserActionIndeterminate(
                "browser broker returned failure with a successful exit status"
            )
        message = envelope["error"]
        if envelope.get("code") == "PERMISSION_DENIED":
            raise PermissionDenied(message)
        if envelope.get("code") == "EXECUTION_FAILED":
            raise BrowserActionFailed(message)
        if envelope.get("code") == "INDETERMINATE":
            raise BrowserActionIndeterminate(message)
        if envelope.get("code") == "KERNEL_UNAVAILABLE":
            raise BrowserUnavailable(message)
        raise BrowserActionIndeterminate(message)
    if process.returncode != 0:
        raise BrowserActionIndeterminate(
            "browser broker exited unsuccessfully after returning success"
        )

    data = envelope.get("data")
    if not isinstance(data, dict):
        raise BrowserActionIndeterminate(
            "browser broker returned invalid response data"
        )
    return data
