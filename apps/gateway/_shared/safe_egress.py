"""Hardened outbound HTTP helper used by every gateway app.

Every external request from a gateway must funnel through
:func:`safe_urlopen`. The wrapper enforces four invariants that the
raw :mod:`urllib.request` API does not:

1. **Scheme allowlist.** Only ``http://`` and ``https://`` are accepted.
   The default :class:`urllib.request.OpenerDirector` will happily
   issue ``file://``, ``ftp://``, ``data://``, etc; this helper does
   not.

2. **Pinned private-network reject.** The resolved hostname is checked
   against RFC1918 / RFC4193 / link-local / loopback / multicast.
   This blocks the classic SSRF target list — most importantly
   AWS / GCP / Azure metadata services on ``169.254.169.254``,
   ``fd00:ec2::254``, etc. Operators who actually need to hit
   ``localhost`` (e.g. a sidecar ``signal-cli-rest-api`` container)
   opt in by exporting ``COS_GATEWAY_ALLOW_PRIVATE=1``. DNS failures
   fail closed, and the HTTP connection is made to the exact validated
   socket address so a second lookup cannot rebind to an internal IP.

3. **No redirects or environment proxies.** Requests use a direct
   :mod:`http.client` connection rather than urllib's proxy-aware opener,
   so ``HTTP_PROXY`` / ``HTTPS_PROXY`` / ``NO_PROXY`` cannot change the
   network destination. A 30x response surfaces as
   :class:`urllib.error.HTTPError` rather than triggering a second
   request to a Location: header the attacker chose. Without this,
   even a "good" URL like ``https://hooks.example.com`` is a
   redirect-SSRF jumping-off point.

4. **Policy gating.** Every call routes through
   ``cos_runtime.policy.require("net.dial", host=hostname)``. The kernel
   is the source of truth on whether *this* session can talk to
   *that* host; the helper raises :class:`PermissionDenied` otherwise.

Errors raised by this module never echo header values or request
bodies — credentials such as Bearer tokens commonly live in headers
and we do not want them spilling into logs.
"""

from __future__ import annotations

import ipaddress
import http.client
import io
import math
import os
import re
import socket
import ssl
import urllib.error
import urllib.parse
import urllib.request
from typing import Mapping, Optional, Tuple

try:
    from cos_runtime import policy  # type: ignore[import-not-found]
except Exception:  # pragma: no cover - exercised only when runtime is absent
    policy = None  # type: ignore[assignment]


# Public exceptions ---------------------------------------------------------


class EgressError(Exception):
    """Base class for egress failures raised by :func:`safe_urlopen`."""


class EgressBlocked(EgressError):
    """Egress refused locally (scheme, private host, redirect, …).

    Distinct from :class:`urllib.error.HTTPError` so callers can tell
    a *local* policy rejection apart from an actual server response.
    """


# Internals -----------------------------------------------------------------


_PRIVATE_OK_ENV = "COS_GATEWAY_ALLOW_PRIVATE"
_MAX_CONNECT_TARGETS = 8
_HTTP_TOKEN = re.compile(r"^[!#$%&'*+\-.^_`|~0-9A-Za-z]+$")


class _NoRedirectHandler(urllib.request.HTTPRedirectHandler):
    """Refuse to follow any redirect.

    Without this, ``urllib`` happily chases ``Location:`` headers. A
    server that accepts our request and responds with
    ``Location: http://169.254.169.254/latest/meta-data/`` would then
    leak that response back into the gateway. Returning ``None`` from
    :meth:`redirect_request` raises :class:`urllib.error.HTTPError`
    with the original 30x status, which is what the caller wants.
    """

    def redirect_request(self, req, fp, code, msg, headers, newurl):  # noqa: D401
        return None


# Kept for source compatibility with callers that imported these private
# symbols before the transport became DNS-pinned. safe_urlopen does not use
# this opener; disabling ProxyHandler still keeps such callers fail-safe.
_OPENER = urllib.request.build_opener(
    urllib.request.ProxyHandler({}),
    _NoRedirectHandler(),
)


def _allow_private() -> bool:
    """True if the operator has explicitly opted in to private hosts."""
    val = os.environ.get(_PRIVATE_OK_ENV, "")
    return val.strip().lower() in {"1", "true", "yes", "on"}


def _is_private_address(addr: str) -> bool:
    """Return True if ``addr`` parses as any non-public IP address.

    Hostnames that don't parse as IPs are *not* considered private here
    — the caller resolves them in :func:`_resolved_ips` first.
    """
    try:
        ip = ipaddress.ip_address(addr)
    except ValueError:
        return False
    return (
        not ip.is_global
        or ip.is_private
        or ip.is_loopback
        or ip.is_link_local
        or ip.is_multicast
        or ip.is_reserved
        or ip.is_unspecified
    )


def _parse_target(url: str) -> Tuple[str, str, int, str]:
    """Parse an absolute HTTP URL into scheme, host, port, and request path."""
    if not isinstance(url, str) or not url.strip():
        raise EgressBlocked("URL must be a non-empty string")
    try:
        parsed = urllib.parse.urlparse(url)
    except ValueError as exc:
        raise EgressBlocked(f"invalid URL: {exc}") from None
    scheme = (parsed.scheme or "").lower()
    if scheme not in {"http", "https"}:
        raise EgressBlocked(
            f"scheme {scheme!r} not allowed; only http/https are permitted"
        )
    try:
        if parsed.username is not None or parsed.password is not None:
            raise EgressBlocked("URL userinfo is not permitted")
        host = (parsed.hostname or "").rstrip(".").lower()
        if not host:
            raise EgressBlocked("URL has no hostname")
        parsed_port = parsed.port
        if parsed_port is None:
            port = 443 if scheme == "https" else 80
        else:
            port = parsed_port
    except ValueError as exc:
        raise EgressBlocked(f"invalid URL authority: {exc}") from None
    if not 1 <= port <= 65535:
        raise EgressBlocked("URL port is outside 1..65535")
    path = urllib.parse.urlunsplit(("", "", parsed.path or "/", parsed.query, ""))
    return scheme, host, port, path


def _resolve_targets(host: str, port: int) -> list[tuple[int, int, int, tuple]]:
    """Resolve once, reject unsafe answers, and retain exact socket addresses."""
    try:
        infos = socket.getaddrinfo(
            host,
            port,
            family=socket.AF_UNSPEC,
            type=socket.SOCK_STREAM,
            proto=socket.IPPROTO_TCP,
        )
    except socket.gaierror as exc:
        raise EgressBlocked(f"DNS resolution failed for host {host!r}: {exc}") from None

    targets: list[tuple[int, int, int, tuple]] = []
    seen: set[tuple[int, tuple]] = set()
    for info in infos:
        family, socktype, proto, _canonname, sockaddr = info
        if family not in {socket.AF_INET, socket.AF_INET6} or not sockaddr:
            continue
        key = (family, sockaddr)
        if key in seen:
            continue
        seen.add(key)
        targets.append((family, socktype, proto, sockaddr))
    if not targets:
        raise EgressBlocked(f"DNS resolution returned no usable address for {host!r}")

    if not _allow_private():
        for _family, _socktype, _proto, sockaddr in targets:
            ip = sockaddr[0]
            if _is_private_address(ip):
                raise EgressBlocked(
                    f"host {host!r} resolves to non-public address {ip!r}; "
                    f"set {_PRIVATE_OK_ENV}=1 to override"
                )
    return targets[:_MAX_CONNECT_TARGETS]


def _open_pinned_socket(
    target: tuple[int, int, int, tuple],
    timeout: float,
) -> socket.socket:
    """Connect directly to a previously validated getaddrinfo result."""
    family, socktype, proto, sockaddr = target
    sock = socket.socket(family, socktype, proto)
    try:
        sock.settimeout(timeout)
        if proto == socket.IPPROTO_TCP:
            sock.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)
        sock.connect(sockaddr)
        return sock
    except Exception:
        sock.close()
        raise


class _PinnedHTTPConnection(http.client.HTTPConnection):
    """HTTPConnection whose connect step never performs DNS."""

    def __init__(self, host: str, port: int, target, timeout: float):
        super().__init__(host, port=port, timeout=timeout)
        self._pinned_target = target

    def connect(self) -> None:
        if self._tunnel_host:
            raise EgressBlocked("HTTP CONNECT tunnels are not permitted")
        self.sock = _open_pinned_socket(self._pinned_target, self.timeout)


_TLS_CONTEXT = ssl.create_default_context()
try:
    _TLS_CONTEXT.set_alpn_protocols(["http/1.1"])
except NotImplementedError:  # pragma: no cover - platform TLS limitation
    pass


class _PinnedHTTPSConnection(http.client.HTTPSConnection):
    """HTTPSConnection pinned to an IP while retaining hostname SNI."""

    def __init__(self, host: str, port: int, target, timeout: float):
        super().__init__(
            host,
            port=port,
            timeout=timeout,
            context=_TLS_CONTEXT,
        )
        self._pinned_target = target

    def connect(self) -> None:
        if self._tunnel_host:
            raise EgressBlocked("HTTPS CONNECT tunnels are not permitted")
        raw_sock = _open_pinned_socket(self._pinned_target, self.timeout)
        try:
            self.sock = self._context.wrap_socket(
                raw_sock,
                server_hostname=self.host,
            )
        except Exception:
            raw_sock.close()
            raise


class _ConnectFailure(Exception):
    """A failure before any HTTP request bytes were sent."""

    def __init__(self, reason: BaseException):
        super().__init__(str(reason))
        self.reason = reason


def _request_once(
    scheme: str,
    host: str,
    port: int,
    path: str,
    target: tuple[int, int, int, tuple],
    method: str,
    headers: Mapping[str, str],
    body: Optional[bytes],
    timeout: float,
    url: str,
) -> Tuple[int, dict[str, str], bytes]:
    """Send one direct request to one validated socket address."""
    connection_cls = (
        _PinnedHTTPSConnection if scheme == "https" else _PinnedHTTPConnection
    )
    connection = connection_cls(host, port, target, timeout)
    try:
        try:
            connection.connect()
        except (OSError, http.client.HTTPException) as exc:
            raise _ConnectFailure(exc) from exc
        try:
            connection.request(method, path, body=body, headers=dict(headers))
            response = connection.getresponse()
            raw = response.read()
            status = response.status
            if not 200 <= status < 300:
                raise urllib.error.HTTPError(
                    url,
                    status,
                    response.reason,
                    response.headers,
                    io.BytesIO(raw),
                )
            return status, {k: v for k, v in response.headers.items()}, raw
        except urllib.error.HTTPError:
            raise
        except (ValueError, http.client.InvalidURL):
            raise
        except (OSError, http.client.HTTPException) as exc:
            raise urllib.error.URLError(exc) from exc
    finally:
        connection.close()


def _validate_headers(headers: Mapping[str, str]) -> dict[str, str]:
    """Reject headers that could retarget a direct origin connection."""
    out: dict[str, str] = {}
    for key, value in headers.items():
        if not isinstance(key, str) or not isinstance(value, str):
            raise EgressBlocked("request header names and values must be strings")
        if not _HTTP_TOKEN.fullmatch(key) or any(char in value for char in "\r\n\0"):
            raise EgressBlocked("request contains an invalid header")
        lowered = key.strip().lower()
        if lowered in {"host", "proxy-authorization", "proxy-connection"}:
            raise EgressBlocked(f"request header {key!r} is not permitted")
        out[key] = str(value)
    return out


def _resolved_ips(host: str) -> list[str]:
    """Compatibility helper that now fails closed on DNS errors."""
    targets = _resolve_targets(host, 443)
    out: list[str] = []
    for _family, _socktype, _proto, sockaddr in targets:
        ip = sockaddr[0]
        if ip and ip not in out:
            out.append(ip)
    return out


def _enforce_target(url: str) -> Tuple[str, str]:
    """Validate ``url``. Returns ``(scheme, hostname)``.

    Raises :class:`EgressBlocked` on any local rejection.
    """
    scheme, host, port, _path = _parse_target(url)
    if not _allow_private() and _is_private_address(host):
        raise EgressBlocked(
            f"host {host!r} resolves to a private/loopback/link-local "
            f"address; set {_PRIVATE_OK_ENV}=1 to override"
        )
    _resolve_targets(host, port)
    return scheme, host


# Public API ----------------------------------------------------------------


def safe_tls_connect(
    host: str,
    port: int = 443,
    *,
    timeout: float = 30.0,
    verb_id: str,
) -> ssl.SSLSocket:
    """Open a policy-authorised, DNS-pinned TLS connection.

    This is the streaming counterpart to :func:`safe_urlopen`, intended
    for protocols such as WebSocket that begin with an HTTP upgrade and
    then keep the socket open. DNS is resolved exactly once, every answer
    is checked against the private-address policy, and TLS still verifies
    the original hostname through SNI.
    """
    if not isinstance(host, str):
        raise EgressBlocked("host must be a string")
    host = host.strip().rstrip(".").lower()
    if not host or any(char in host for char in "/\\@?#\r\n\0"):
        raise EgressBlocked("host is invalid")
    if isinstance(port, bool):
        raise EgressBlocked("port must be an integer")
    try:
        port = int(port)
    except (TypeError, ValueError):
        raise EgressBlocked("port must be an integer") from None
    if not 1 <= port <= 65535:
        raise EgressBlocked("port is outside 1..65535")
    try:
        timeout = float(timeout)
    except (TypeError, ValueError):
        raise EgressBlocked("timeout must be a positive number") from None
    if timeout <= 0 or not math.isfinite(timeout):
        raise EgressBlocked("timeout must be a positive number")
    if not _allow_private() and _is_private_address(host):
        raise EgressBlocked(
            f"host {host!r} is a non-public address; "
            f"set {_PRIVATE_OK_ENV}=1 to override"
        )
    if policy is None:
        raise EgressBlocked(
            "cos_runtime.policy unavailable; refusing outbound connection"
        )
    _ = verb_id
    policy.require("net.dial", host=host)

    targets = _resolve_targets(host, port)
    last_error: Optional[BaseException] = None
    for target in targets:
        raw_sock: Optional[socket.socket] = None
        try:
            raw_sock = _open_pinned_socket(target, timeout)
            tls_sock = _TLS_CONTEXT.wrap_socket(raw_sock, server_hostname=host)
            tls_sock.settimeout(timeout)
            return tls_sock
        except (OSError, ValueError) as exc:
            last_error = exc
            if raw_sock is not None:
                raw_sock.close()
    raise urllib.error.URLError(last_error or "all validated addresses failed")


def safe_urlopen(
    method: str,
    url: str,
    *,
    headers: Optional[Mapping[str, str]] = None,
    body: Optional[bytes] = None,
    timeout: float = 30.0,
    verb_id: str,
    name: Optional[str] = None,  # noqa: ARG001 - reserved for future use
) -> Tuple[int, dict[str, str], bytes]:
    """Issue an HTTP request the kernel has authorised.

    Args:
        method:  HTTP method (``GET``, ``POST``, …).
        url:     Absolute http(s) URL.
        headers: Optional request headers. Authorization tokens live
                 here — they are forwarded as-is to the origin and not
                 logged anywhere in this module.
        body:    Optional request body bytes.
        timeout: Socket timeout in seconds.
        verb_id: Legacy call-site label retained for source compatibility.
                 Authorization always uses the kernel's ``net.dial`` verb.

    Returns:
        ``(status_code, response_headers_dict, response_body_bytes)``.

    Raises:
        EgressBlocked: Local policy rejected the call (bad scheme,
                       private host, redirect, no kernel available).
        urllib.error.HTTPError: Server returned a non-2xx status,
                       *including* 30x redirects (which are blocked).
        urllib.error.URLError: Network-level failure.
        cos_runtime.policy.PermissionDenied: Kernel denied the verb.
    """
    scheme, host, port, path = _parse_target(url)
    if not _allow_private() and _is_private_address(host):
        raise EgressBlocked(
            f"host {host!r} is a non-public address; "
            f"set {_PRIVATE_OK_ENV}=1 to override"
        )

    if policy is None:
        # Fail closed: no kernel runtime means no gateway can call
        # out. Tests stub `policy` in by monkey-patching the module.
        raise EgressBlocked(
            "cos_runtime.policy unavailable; refusing outbound request"
        )
    _ = verb_id
    policy.require("net.dial", host=host)

    if not isinstance(method, str) or not method.strip():
        raise EgressBlocked("HTTP method must be a non-empty string")
    method = method.strip().upper()
    if not _HTTP_TOKEN.fullmatch(method):
        raise EgressBlocked("HTTP method contains invalid characters")
    try:
        timeout = float(timeout)
    except (TypeError, ValueError):
        raise EgressBlocked("timeout must be a positive number") from None
    if timeout <= 0 or not math.isfinite(timeout):
        raise EgressBlocked("timeout must be a positive number")
    if body is not None:
        if not isinstance(body, (bytes, bytearray, memoryview)):
            raise EgressBlocked("request body must be bytes")
        body = bytes(body)

    safe_headers = _validate_headers(headers or {})
    targets = _resolve_targets(host, port)
    last_error: Optional[BaseException] = None
    for target in targets:
        try:
            return _request_once(
                scheme,
                host,
                port,
                path,
                target,
                method,
                safe_headers,
                body,
                timeout,
                url,
            )
        except urllib.error.HTTPError:
            raise
        except (ValueError, http.client.InvalidURL):
            raise EgressBlocked("invalid HTTP request") from None
        except _ConnectFailure as exc:
            last_error = exc.reason
    raise urllib.error.URLError(last_error or "all validated addresses failed")


def parsed_host(url: str) -> Optional[str]:
    """Return the hostname from ``url`` or ``None`` if it doesn't parse.

    Useful when a caller needs the host for :func:`policy.require`
    *before* it knows it wants to issue the request (e.g. to surface
    a clean denial message rather than a generic egress error).
    """
    try:
        parsed = urllib.parse.urlparse(url)
        if parsed.username is not None or parsed.password is not None:
            return None
        host = (parsed.hostname or "").rstrip(".").lower()
    except (TypeError, ValueError):
        return None
    return host or None


__all__ = [
    "safe_tls_connect",
    "safe_urlopen",
    "parsed_host",
    "EgressBlocked",
    "EgressError",
]
