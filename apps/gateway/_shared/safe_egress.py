"""Hardened outbound HTTP helper used by every gateway app.

Every external request from a gateway must funnel through
:func:`safe_urlopen`. The wrapper enforces four invariants that the
raw :mod:`urllib.request` API does not:

1. **Scheme allowlist.** Only ``http://`` and ``https://`` are accepted.
   The default :class:`urllib.request.OpenerDirector` will happily
   issue ``file://``, ``ftp://``, ``data://``, etc; this helper does
   not.

2. **Private-network reject.** The resolved hostname is checked
   against RFC1918 / RFC4193 / link-local / loopback / multicast.
   This blocks the classic SSRF target list — most importantly
   AWS / GCP / Azure metadata services on ``169.254.169.254``,
   ``fd00:ec2::254``, etc. Operators who actually need to hit
   ``localhost`` (e.g. a sidecar ``signal-cli-rest-api`` container)
   opt in by exporting ``COS_GATEWAY_ALLOW_PRIVATE=1``.

3. **No redirect following.** The opener built here has no
   ``HTTPRedirectHandler``, so a 30x response surfaces as
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
import os
import socket
import urllib.error
import urllib.request
from typing import Any, Mapping, Optional, Tuple

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


# Build the opener once — it is stateless and thread-safe for the
# blocking-IO use the gateway apps make of it.
_OPENER = urllib.request.build_opener(_NoRedirectHandler())


def _allow_private() -> bool:
    """True if the operator has explicitly opted in to private hosts."""
    val = os.environ.get(_PRIVATE_OK_ENV, "")
    return val.strip().lower() in {"1", "true", "yes", "on"}


def _is_private_address(addr: str) -> bool:
    """Return True if ``addr`` parses as a private/loopback/link-local IP.

    Hostnames that don't parse as IPs are *not* considered private here
    — the caller resolves them in :func:`_resolved_ips` first.
    """
    try:
        ip = ipaddress.ip_address(addr)
    except ValueError:
        return False
    return (
        ip.is_private
        or ip.is_loopback
        or ip.is_link_local
        or ip.is_multicast
        or ip.is_reserved
        or ip.is_unspecified
    )


def _resolved_ips(host: str) -> list[str]:
    """Best-effort DNS lookup for ``host``. Returns [] on failure."""
    try:
        infos = socket.getaddrinfo(host, None)
    except socket.gaierror:
        return []
    out: list[str] = []
    for info in infos:
        sockaddr = info[4]
        if not sockaddr:
            continue
        ip = sockaddr[0]
        if ip and ip not in out:
            out.append(ip)
    return out


def _enforce_target(url: str) -> Tuple[str, str]:
    """Validate ``url``. Returns ``(scheme, hostname)``.

    Raises :class:`EgressBlocked` on any local rejection.
    """
    try:
        parsed = urllib.parse.urlparse(url)
    except ValueError as exc:
        raise EgressBlocked(f"invalid URL: {exc}") from None
    scheme = (parsed.scheme or "").lower()
    if scheme not in {"http", "https"}:
        raise EgressBlocked(
            f"scheme {scheme!r} not allowed; only http/https are permitted"
        )
    host = parsed.hostname
    if not host:
        raise EgressBlocked("URL has no hostname")
    if _allow_private():
        return scheme, host
    # Check both the literal host (in case it's an IP) and any
    # resolved IPs. We *do not* allow an attacker to bypass the check
    # by encoding their target as a hostname that resolves to a
    # private IP.
    if _is_private_address(host):
        raise EgressBlocked(
            f"host {host!r} resolves to a private/loopback/link-local "
            f"address; set {_PRIVATE_OK_ENV}=1 to override"
        )
    ips = _resolved_ips(host)
    for ip in ips:
        if _is_private_address(ip):
            raise EgressBlocked(
                f"host {host!r} resolves to private address {ip!r}; "
                f"set {_PRIVATE_OK_ENV}=1 to override"
            )
    return scheme, host


# urllib.parse imported lazily so the module stays importable in
# minimal stdlib environments. It's part of the stdlib so this is
# really just a circular-import dodge inside the type-check path.
import urllib.parse  # noqa: E402  (intentional late import)


# Public API ----------------------------------------------------------------


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
                 here — they are forwarded as-is to ``urllib`` and not
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
    scheme, host = _enforce_target(url)

    if policy is None:
        # Fail closed: no kernel runtime means no gateway can call
        # out. Tests stub `policy` in by monkey-patching the module.
        raise EgressBlocked(
            "cos_runtime.policy unavailable; refusing outbound request"
        )
    _ = verb_id
    policy.require("net.dial", host=host)

    req = urllib.request.Request(
        url, data=body, method=method.upper(), headers=dict(headers or {})
    )
    with _OPENER.open(req, timeout=timeout) as resp:
        raw = resp.read()
        # ``resp.headers`` is an :class:`email.message.Message`; cast
        # to a plain dict for predictable JSON serialisation.
        hdrs = {k: v for k, v in resp.headers.items()}
        status = resp.getcode() if hasattr(resp, "getcode") else 200
    return status, hdrs, raw


def parsed_host(url: str) -> Optional[str]:
    """Return the hostname from ``url`` or ``None`` if it doesn't parse.

    Useful when a caller needs the host for :func:`policy.require`
    *before* it knows it wants to issue the request (e.g. to surface
    a clean denial message rather than a generic egress error).
    """
    try:
        parsed = urllib.parse.urlparse(url)
    except ValueError:
        return None
    return parsed.hostname or None


__all__ = ["safe_urlopen", "parsed_host", "EgressBlocked", "EgressError"]
