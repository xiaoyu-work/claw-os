"""Private broker bridge for the bundled network diagnostics App."""

from __future__ import annotations

from typing import Any

from . import _broker_bridge

_ACTIONS = frozenset({"interfaces", "routes", "dns", "tcp", "diagnose"})
_MAX_REQUEST_BYTES = 4 * 1024
_MAX_RESPONSE_BYTES = 2 * 1024 * 1024
_TIMEOUT_SECONDS = 30


class NetworkDiagnosticsError(RuntimeError):
    """Base error raised by the network diagnostics bridge."""


class PermissionDenied(NetworkDiagnosticsError):
    """The authenticated App session lacks a required capability."""


class NetworkDiagnosticsUnavailable(NetworkDiagnosticsError):
    """The daemon-owned diagnostics provider is unavailable."""


class NetworkDiagnosticsFailed(NetworkDiagnosticsError):
    """The provider rejected or failed the diagnostic request."""


class NetworkDiagnosticsIndeterminate(NetworkDiagnosticsError):
    """The bridge did not produce a trustworthy result."""


def request(action: str, **fields: Any) -> dict[str, Any]:
    if action not in _ACTIONS:
        raise NetworkDiagnosticsError("network diagnostic action is invalid")
    try:
        return _broker_bridge.exchange(
            "__netdiag",
            {"action": action, **fields},
            service="network diagnostics",
            max_request_bytes=_MAX_REQUEST_BYTES,
            max_response_bytes=_MAX_RESPONSE_BYTES,
            timeout_seconds=_TIMEOUT_SECONDS,
        )
    except _broker_bridge.BridgeRejected as exc:
        if exc.code == "PERMISSION_DENIED":
            raise PermissionDenied(str(exc)) from exc
        if exc.code == "EXECUTION_FAILED":
            raise NetworkDiagnosticsFailed(str(exc)) from exc
        if exc.code == "KERNEL_UNAVAILABLE":
            raise NetworkDiagnosticsUnavailable(str(exc)) from exc
        raise NetworkDiagnosticsIndeterminate(str(exc)) from exc
    except _broker_bridge.BridgeUnavailable as exc:
        raise NetworkDiagnosticsUnavailable(str(exc)) from exc
    except _broker_bridge.BridgeIndeterminate as exc:
        raise NetworkDiagnosticsIndeterminate(str(exc)) from exc
    except _broker_bridge.BridgeError as exc:
        raise NetworkDiagnosticsError(str(exc)) from exc
