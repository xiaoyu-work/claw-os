"""Brokered egress for sandboxed Claw OS workers.

A sandboxed operation runs in an empty network namespace and, under the
worker seccomp filter, may create only ``AF_UNIX`` sockets. It therefore
cannot open a TCP connection, resolve a name, or fall back to the host's
network in any way — those calls fail with ``EPERM`` rather than
succeeding quietly.

When an operation legitimately holds ``net.dial`` for exact hosts, the
kernel gives it one Unix-domain socket instead. This module is the only
client of that socket. It speaks a bounded ``CONNECT`` exchange:

    CONNECT host:port HTTP/1.1
    Host: host:port

    HTTP/1.1 200 Connection established

and hands back a connected stream. The broker — trusted code outside the
sandbox — is what checks the request against the exact endpoints the
operation was granted, resolves the name itself, refuses every address
that is not globally routable, and connects to the address it resolved.
The worker never learns the address and never gets to choose it, so a
name that changes answers between the check and the connect cannot move
the tunnel.

TLS is established by the *caller*, over the returned stream, against
the hostname it asked for. That split is deliberate: the broker pins the
transport, the caller pins the identity, and neither can be talked out
of its half.

Every helper here fails closed. Outside a sandbox there is no broker
socket and :func:`available` is ``False``, so callers keep their
ordinary direct path; inside one, a refusal is an exception and never a
silent direct connection.
"""

from __future__ import annotations

import os
import socket
from typing import List, Optional, Tuple

__all__ = [
    "EgressError",
    "EgressDenied",
    "EgressUnavailable",
    "available",
    "socket_path",
    "allowed_endpoints",
    "create_connection",
]

# Largest status line + headers the broker may answer with. The reply is
# a fixed shape; anything larger is a broken or hostile peer.
_MAX_REPLY_BYTES = 8 * 1024

# Ceiling on one CONNECT request line, so a caller cannot push an
# unbounded host string at the broker.
_MAX_TARGET_BYTES = 300

_DEFAULT_TIMEOUT_S = 30.0

_SOCKET_ENV = "COS_EGRESS_SOCKET"
_ENDPOINTS_ENV = "COS_EGRESS_ENDPOINTS"


class EgressError(Exception):
    """Base class for every failure this module raises."""


class EgressUnavailable(EgressError):
    """No brokered egress endpoint is present for this operation."""


class EgressDenied(EgressError):
    """The broker refused this endpoint.

    Raised for a host or port outside the operation's grant, a name that
    resolves to a blocked address, and any other refusal. It is never
    converted into a direct connection.
    """


def socket_path() -> Optional[str]:
    """Path of the brokered egress socket, or ``None`` outside a grant."""
    value = os.environ.get(_SOCKET_ENV) or ""
    return value or None


def available() -> bool:
    """Is brokered egress the transport for this operation?"""
    return socket_path() is not None


def allowed_endpoints() -> List[Tuple[str, int]]:
    """Endpoints the kernel told the worker it may reach.

    Advisory only — the broker enforces the same list and is the
    authority. Useful for a clear local error before a round trip.
    """
    raw = os.environ.get(_ENDPOINTS_ENV) or ""
    endpoints: List[Tuple[str, int]] = []
    for item in raw.split(","):
        item = item.strip()
        if not item or ":" not in item:
            continue
        host, _, port = item.rpartition(":")
        try:
            endpoints.append((host.lower(), int(port)))
        except ValueError:
            continue
    return endpoints


def create_connection(
    host: str,
    port: int,
    timeout: Optional[float] = None,
) -> socket.socket:
    """Open a brokered TCP tunnel to ``host:port``.

    A drop-in replacement for :func:`socket.create_connection` for code
    running inside a worker sandbox. The returned object is an ordinary
    connected stream socket: wrap it with :mod:`ssl` for HTTPS, hand it
    to :mod:`smtplib`, or read and write it directly.

    Raises :class:`EgressUnavailable` when the operation was granted no
    egress at all, and :class:`EgressDenied` when the broker refused
    this endpoint.
    """
    path = socket_path()
    if path is None:
        raise EgressUnavailable(
            "this operation was not granted network access; declare an exact "
            "`net.dial` host scope in app.json"
        )
    target = _target(host, port)
    deadline = _DEFAULT_TIMEOUT_S if timeout is None else float(timeout)

    stream = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    try:
        stream.settimeout(deadline)
        stream.connect(path)
        request = f"CONNECT {target} HTTP/1.1\r\nHost: {target}\r\n\r\n"
        stream.sendall(request.encode("ascii"))
        status = _read_reply(stream)
    except EgressError:
        stream.close()
        raise
    except OSError as error:
        stream.close()
        raise EgressUnavailable(f"egress broker is unreachable: {error}") from error

    if status != 200:
        stream.close()
        raise EgressDenied(
            f"egress broker refused {target} (status {status}); the operation's "
            "`net.dial` scopes do not cover it, or it resolves to a blocked address"
        )
    return stream


def _target(host: str, port: int) -> str:
    host = (host or "").strip().strip("[]").rstrip(".").lower()
    if not host:
        raise EgressDenied("egress target has no host")
    if not isinstance(port, int) or not 0 < port < 65536:
        raise EgressDenied(f"egress target port {port!r} is out of range")
    # The host reached the broker as an IDNA/ASCII label set; anything
    # that still needs encoding is refused rather than guessed at.
    try:
        host.encode("ascii")
    except UnicodeEncodeError:
        try:
            host = host.encode("idna").decode("ascii")
        except UnicodeError as error:
            raise EgressDenied(f"egress target host is not a DNS name: {error}") from error
    if any(character in host for character in " \r\n\t/@"):
        raise EgressDenied("egress target host contains a separator")
    target = f"{host}:{port}"
    if len(target) > _MAX_TARGET_BYTES:
        raise EgressDenied("egress target is too long")
    return target


def _read_reply(stream: socket.socket) -> int:
    """Read the broker's status line and headers, bounded.

    One byte at a time on purpose: the tunnel's first payload bytes may
    arrive in the same segment as the reply, and a chunked read would
    swallow them into a buffer the caller never sees.
    """
    buffer = b""
    while not buffer.endswith(b"\r\n\r\n"):
        if len(buffer) >= _MAX_REPLY_BYTES:
            raise EgressDenied("egress broker reply exceeded its ceiling")
        chunk = stream.recv(1)
        if not chunk:
            raise EgressDenied("egress broker closed the connection")
        buffer += chunk
    status_line = buffer.split(b"\r\n", 1)[0].decode("latin-1")
    parts = status_line.split()
    if len(parts) < 2 or not parts[0].upper().startswith("HTTP/"):
        raise EgressDenied("egress broker answered with a malformed status line")
    try:
        return int(parts[1])
    except ValueError as error:
        raise EgressDenied("egress broker answered with a malformed status") from error
