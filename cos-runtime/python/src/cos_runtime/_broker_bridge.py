"""Shared transport for private bundled-App broker bridges."""

from __future__ import annotations

import json
import os
import subprocess
from typing import Any

WIRE_VERSION = 1


class BridgeError(RuntimeError):
    """Base error for invalid local bridge requests."""


class BridgeUnavailable(BridgeError):
    """The bridge process could not be started."""


class BridgeIndeterminate(BridgeError):
    """A launched bridge did not produce a trustworthy result."""


class BridgeRejected(BridgeError):
    """The broker returned a classified failure."""

    def __init__(self, code: str, message: str):
        super().__init__(message)
        self.code = code


def _cos_binary() -> str:
    binary = os.environ.get("CLAW_COS_BIN")
    if not binary or not os.path.isabs(binary):
        raise BridgeUnavailable("CLAW_COS_BIN is not an absolute runtime path")
    return binary


def _stop_process(process: subprocess.Popen[bytes]) -> None:
    if process.poll() is None:
        process.kill()
    process.communicate()


def _validate_envelope(envelope: object, service: str) -> dict[str, Any]:
    if not isinstance(envelope, dict):
        raise BridgeIndeterminate(
            f"{service} broker returned a non-object wire envelope"
        )
    if (
        type(envelope.get("wire_version")) is not int
        or envelope["wire_version"] != WIRE_VERSION
        or type(envelope.get("ok")) is not bool
    ):
        raise BridgeIndeterminate(
            f"{service} broker returned an incompatible wire envelope"
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
            raise BridgeIndeterminate(
                f"{service} broker returned an invalid failure envelope"
            )
    if set(envelope) - allowed:
        raise BridgeIndeterminate(
            f"{service} broker returned undeclared wire fields"
        )
    return envelope


def exchange(
    bridge: str,
    payload: dict[str, Any],
    *,
    service: str,
    max_request_bytes: int,
    max_response_bytes: int,
    timeout_seconds: float,
) -> dict[str, Any]:
    try:
        encoded = json.dumps(
            payload,
            separators=(",", ":"),
            ensure_ascii=False,
        ).encode("utf-8")
    except (TypeError, ValueError) as exc:
        raise BridgeError(f"{service} request is not JSON serializable") from exc
    if len(encoded) > max_request_bytes:
        raise BridgeError(f"{service} request exceeds the internal bridge limit")

    command = [
        _cos_binary(),
        f"--wire={WIRE_VERSION}",
        bridge,
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
        raise BridgeUnavailable(f"run {service} broker bridge: {exc}") from exc
    try:
        stdout, _stderr = process.communicate(encoded, timeout=timeout_seconds)
    except subprocess.TimeoutExpired as exc:
        try:
            _stop_process(process)
        except OSError as cleanup_error:
            raise BridgeIndeterminate(
                f"{service} broker timed out and cleanup failed: {cleanup_error}"
            ) from exc
        raise BridgeIndeterminate(
            f"{service} broker bridge timed out after process launch"
        ) from exc
    except OSError as exc:
        try:
            _stop_process(process)
        except OSError as cleanup_error:
            raise BridgeIndeterminate(
                f"{service} broker communication and cleanup failed: {cleanup_error}"
            ) from exc
        raise BridgeIndeterminate(
            f"{service} broker communication failed after process launch: {exc}"
        ) from exc

    if len(stdout) > max_response_bytes:
        raise BridgeIndeterminate(
            f"{service} broker response exceeds the runtime limit"
        )
    try:
        envelope = _validate_envelope(json.loads(stdout), service)
    except (UnicodeDecodeError, json.JSONDecodeError) as exc:
        raise BridgeIndeterminate(
            f"{service} broker returned an invalid wire envelope"
        ) from exc

    if envelope["ok"] is False:
        if process.returncode == 0:
            raise BridgeIndeterminate(
                f"{service} broker returned failure with a successful exit status"
            )
        raise BridgeRejected(envelope["code"], envelope["error"])
    if process.returncode != 0:
        raise BridgeIndeterminate(
            f"{service} broker exited unsuccessfully after returning success"
        )

    data = envelope.get("data")
    if not isinstance(data, dict):
        raise BridgeIndeterminate(
            f"{service} broker returned invalid response data"
        )
    return data
