"""Private broker bridge for the bundled attached-browser App."""

from __future__ import annotations

from typing import Any

from . import _broker_bridge

_MAX_REQUEST_BYTES = 128 * 1024
_MAX_RESPONSE_BYTES = 16 * 1024 * 1024
_TIMEOUT_SECONDS = 60


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


def request(action: str, **fields: Any) -> dict[str, Any]:
    if not action or any(ch not in "abcdefghijklmnopqrstuvwxyz._" for ch in action):
        raise BrowserBridgeError("browser action is invalid")
    payload = {"action": action, **fields}
    try:
        return _broker_bridge.exchange(
            "__browser",
            payload,
            service="browser",
            max_request_bytes=_MAX_REQUEST_BYTES,
            max_response_bytes=_MAX_RESPONSE_BYTES,
            timeout_seconds=_TIMEOUT_SECONDS,
        )
    except _broker_bridge.BridgeRejected as exc:
        if exc.code == "PERMISSION_DENIED":
            raise PermissionDenied(str(exc)) from exc
        if exc.code == "EXECUTION_FAILED":
            raise BrowserActionFailed(str(exc)) from exc
        if exc.code == "KERNEL_UNAVAILABLE":
            raise BrowserUnavailable(str(exc)) from exc
        raise BrowserActionIndeterminate(str(exc)) from exc
    except _broker_bridge.BridgeUnavailable as exc:
        raise BrowserUnavailable(str(exc)) from exc
    except _broker_bridge.BridgeIndeterminate as exc:
        raise BrowserActionIndeterminate(str(exc)) from exc
    except _broker_bridge.BridgeError as exc:
        raise BrowserBridgeError(str(exc)) from exc
