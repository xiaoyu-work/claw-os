"""Safe SMTP transport for sandboxed Claw OS workers.

`smtplib` dials with :func:`socket.create_connection`, which a
sandboxed operation may not do: it holds no route and, under the worker
seccomp filter, may create only ``AF_UNIX`` sockets. This module gives
the same API over the brokered egress tunnel instead.

The extension point is :meth:`smtplib.SMTP._get_socket`, which every
connect path in :mod:`smtplib` funnels through — including
``SMTP_SSL``'s implicit-TLS variant and the reconnect ``starttls``
performs. Overriding it is what makes the substitution total: there is
no code path left in the class that reaches
:func:`socket.create_connection`, so a broker refusal is an error and
never a direct dial.

TLS keeps its ordinary meaning. Implicit TLS wraps the tunnel with the
caller's context and the original hostname as SNI, and ``STARTTLS``
upgrades the same tunnel against the same hostname, so the broker pins
the transport while the certificate still has to name the host the
operation asked for.

Outside a sandbox there is no broker socket and these helpers dial
normally, so the same App code runs unchanged in both places.
"""

from __future__ import annotations

import smtplib
import ssl
from typing import Optional

from . import egress

__all__ = ["SafeSMTP", "SafeSMTP_SSL", "connect"]

# Ceiling on one SMTP exchange. `smtplib` reads line by line, so this
# bounds a server that answers forever without a terminator.
_DEFAULT_TIMEOUT_S = 30.0


class _BrokeredSocketMixin:
    """Replace `smtplib`'s dial with the brokered tunnel."""

    def _get_socket(self, host, port, timeout):  # noqa: D401 - smtplib API
        if self.debuglevel > 0:
            # `smtplib` logs the endpoint, never credentials; keep that.
            self._print_debug("connect: brokered", (host, port))
        if not egress.available():
            return super()._get_socket(host, port, timeout)
        deadline = _DEFAULT_TIMEOUT_S if timeout is None else float(timeout)
        return egress.create_connection(host, port, deadline)


class SafeSMTP(_BrokeredSocketMixin, smtplib.SMTP):
    """`smtplib.SMTP` whose transport is the egress broker."""


class SafeSMTP_SSL(_BrokeredSocketMixin, smtplib.SMTP_SSL):  # noqa: N801 - mirrors smtplib
    """`smtplib.SMTP_SSL` whose transport is the egress broker.

    ``SMTP_SSL._get_socket`` wraps whatever this returns with its own
    context and ``server_hostname=host``, so implicit TLS still verifies
    the hostname the operation named.
    """

    def _get_socket(self, host, port, timeout):  # noqa: D401 - smtplib API
        if not egress.available():
            return super()._get_socket(host, port, timeout)
        deadline = _DEFAULT_TIMEOUT_S if timeout is None else float(timeout)
        raw = egress.create_connection(host, port, deadline)
        return self.context.wrap_socket(raw, server_hostname=host)


def connect(
    host: str,
    port: int,
    *,
    timeout: float = _DEFAULT_TIMEOUT_S,
    implicit_tls: bool = False,
    starttls: Optional[bool] = None,
    context: Optional[ssl.SSLContext] = None,
) -> smtplib.SMTP:
    """Open an SMTP session over the operation's approved transport.

    * ``implicit_tls`` — wrap the tunnel in TLS immediately, the port
      465 shape.
    * ``starttls`` — upgrade after ``EHLO``. Defaults to "yes unless
      implicit TLS is already on", which is the port 587 shape and the
      safe default for 25.
    * ``context`` — TLS settings; a verifying default context when
      omitted.

    The returned object is an ordinary :class:`smtplib.SMTP`, so
    ``login``, ``send_message`` and the context-manager protocol behave
    exactly as they always have. Credentials are handed to
    :meth:`smtplib.SMTP.login`, which never logs them.
    """
    context = context or ssl.create_default_context()
    if implicit_tls:
        server: smtplib.SMTP = SafeSMTP_SSL(
            host, port, timeout=timeout, context=context
        )
    else:
        server = SafeSMTP(host, port, timeout=timeout)
    upgrade = (not implicit_tls) if starttls is None else starttls
    if upgrade and not implicit_tls:
        server.ehlo()
        server.starttls(context=context)
        server.ehlo()
    return server
