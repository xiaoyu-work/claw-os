"""Email (SMTP) gateway app.

Outbound-only baseline: ``send`` delivers a one-shot text email via
the configured SMTP server. Inbound (IMAP polling) is still a stub.

TLS strategy (hardened in the post-incident sweep):
  * port 465  -> implicit TLS (smtplib.SMTP_SSL)
  * port 587  -> STARTTLS upgrade after EHLO. STARTTLS is **forced**:
                 if the server doesn't advertise it (which a MITM
                 actively strips it from EHLO would simulate), the
                 send aborts rather than fall back to cleartext.
  * port 25   -> plain unencrypted (only useful on internal relays,
                 and the kernel must explicitly authorise the host).

Credentials needed:
  * ``smtp_host``     -- hostname (env override: COS_SMTP_HOST)
  * ``smtp_port``     -- port number (env override: COS_SMTP_PORT, default 587)
  * ``smtp_user``     -- auth username (env override: COS_SMTP_USER)
  * ``smtp_password`` -- auth password / app password (env override: COS_SMTP_PASSWORD)
  * ``smtp_from``     -- optional From: address (env override: COS_SMTP_FROM);
                          falls back to ``smtp_user``.

Stdlib only. Policy gating routes via
``policy.require("gateway.email.send", host=smtp_host)`` before any
login or send.
"""

from __future__ import annotations

import json
import os
import smtplib
import socket
import ssl
import sys
from email import charset as email_charset
from email.message import EmailMessage


# Sibling ``_shared`` package import (script-mode invocation).
sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

from _shared import gateway_memory, safe_subprocess  # noqa: E402

try:
    from cos_runtime import policy  # type: ignore[import-not-found]
except Exception:  # pragma: no cover - missing only outside the kernel
    policy = None  # type: ignore[assignment]


PLATFORM = "email"
USER_AGENT = "ClawOSEmail/0.1.0"
DEFAULT_PORT = 587
SOFT_LEN = 200_000  # 200 KB body cap; SMTP servers usually allow far more


# Force quoted-printable for UTF-8 text bodies. The stdlib default is
# base64 which is fine but harder for downstream MTAs to introspect /
# anti-spam scan. QP keeps Content-Transfer-Encoding explicit and
# stable across hop rewrites. We pass the encoding to set_content via
# the cte kwarg below; this constant is kept for documentation /
# tooling that wants to inspect the policy.
_BODY_CHARSET = email_charset.Charset("utf-8")
_BODY_CHARSET.body_encoding = email_charset.QP


def _schema() -> dict:
    return {
        "name": f"gateway-{PLATFORM}",
        "version": "0.1.0",
        "description": (
            "SMTP email gateway. ``send`` delivers one-shot text emails. "
            "``start``/``stop`` (IMAP polling) are not yet implemented."
        ),
        "commands": {
            "start": {
                "description": "Connect to IMAP and stream inbound mail (NOT IMPLEMENTED)",
                "parameters": [],
                "example": "cos app gateway-email start",
            },
            "stop": {
                "description": "Stop a running gateway (NOT IMPLEMENTED)",
                "parameters": [],
                "example": "cos app gateway-email stop",
            },
            "status": {
                "description": "Show running state (always 'stopped' until IMAP polling lands)",
                "parameters": [],
                "example": "cos app gateway-email status",
            },
            "send": {
                "description": "Send a one-shot text email",
                "parameters": [
                    {
                        "name": "to",
                        "type": "string",
                        "required": True,
                        "description": "Recipient address (single, e.g. user@example.com)",
                        "kind": "positional",
                    },
                    {
                        "name": "subject",
                        "type": "string",
                        "required": True,
                        "description": "Subject line",
                        "kind": "positional",
                    },
                    {
                        "name": "body",
                        "type": "string",
                        "required": True,
                        "description": "Plain-text body (truncated to 200 KB)",
                        "kind": "positional",
                    },
                    {
                        "name": "cc",
                        "type": "string",
                        "required": False,
                        "description": "Comma-separated Cc addresses",
                        "kind": "named",
                    },
                ],
                "example": "cos app gateway-email send 'user@example.com' 'hello' 'body text'",
            },
        },
    }


def _load_credential(name: str) -> tuple[str | None, str | None]:
    return safe_subprocess.safe_credential_load(name)


def _env_or_credential(env_var: str, cred_name: str) -> tuple[str | None, str | None]:
    val = os.environ.get(env_var)
    if val and val.strip():
        return val.strip(), None
    return _load_credential(cred_name)


def _load_config() -> tuple[dict | None, str | None]:
    """Return ({host, port, user, password, from}, None) or (None, error)."""
    host, err = _env_or_credential("COS_SMTP_HOST", "smtp_host")
    if not host:
        return None, err or "smtp_host required"
    user, err = _env_or_credential("COS_SMTP_USER", "smtp_user")
    if not user:
        return None, err or "smtp_user required"
    password, err = _env_or_credential("COS_SMTP_PASSWORD", "smtp_password")
    if not password:
        return None, err or "smtp_password required"
    port_str, _ = _env_or_credential("COS_SMTP_PORT", "smtp_port")
    if port_str:
        try:
            port = int(port_str)
        except ValueError:
            return None, f"smtp_port not an integer: {port_str!r}"
    else:
        port = DEFAULT_PORT
    from_addr, _ = _env_or_credential("COS_SMTP_FROM", "smtp_from")
    if not from_addr:
        from_addr = user
    return (
        {
            "host": host,
            "port": port,
            "user": user,
            "password": password,
            "from": from_addr,
        },
        None,
    )


def _truncate(text: str, limit: int = SOFT_LEN) -> str:
    if len(text) <= limit:
        return text
    return text[: limit - 1] + "\u2026"


def _split_csv(value: str) -> list[str]:
    """Split a comma-separated address list, trimming whitespace + empties."""
    return [s.strip() for s in value.split(",") if s.strip()]


def _build_message(cfg: dict, to: str, subject: str, body: str, cc: str) -> EmailMessage:
    msg = EmailMessage()
    msg["From"] = cfg["from"]
    msg["To"] = to
    if cc:
        msg["Cc"] = cc
    msg["Subject"] = subject
    msg["User-Agent"] = USER_AGENT
    # Pin charset and content-transfer-encoding to UTF-8 / QP so
    # downstream MTAs don't silently mangle the encoding choice.
    msg.set_content(_truncate(body), charset="utf-8", cte="quoted-printable")
    return msg


class _StartTLSUnavailable(smtplib.SMTPException):
    """Raised when the SMTP server doesn't advertise STARTTLS on the
    submission port. A MITM stripping STARTTLS from EHLO would
    surface as exactly this, and we'd rather fail loud than ship
    credentials in cleartext."""


def _send(to: str, subject: str, body: str, cc: str = "") -> dict:
    if not to or not str(to).strip():
        return {"ok": False, "error": "to required"}
    if not subject or not str(subject).strip():
        return {"ok": False, "error": "subject required"}
    if body is None or str(body) == "":
        return {"ok": False, "error": "body required"}
    cfg, err = _load_config()
    if not cfg:
        return {"ok": False, "error": err or "config error"}

    # Kernel gate before login: a denied host means we never even
    # send the password.
    if policy is not None:
        try:
            policy.require("gateway.email.send", host=cfg["host"])
        except Exception as e:
            denial = getattr(e, "denial", None)
            return {
                "ok": False,
                "platform": PLATFORM,
                "host": cfg["host"],
                "error": "permission denied",
                "denial": denial,
            }

    msg = _build_message(cfg, str(to).strip(), str(subject), str(body), str(cc).strip())
    recipients = [str(to).strip()] + _split_csv(str(cc))

    try:
        if cfg["port"] == 465:
            # Implicit TLS.
            ctx = ssl.create_default_context()
            with smtplib.SMTP_SSL(cfg["host"], cfg["port"], context=ctx, timeout=30) as smtp:
                smtp.login(cfg["user"], cfg["password"])
                smtp.send_message(msg, from_addr=cfg["from"], to_addrs=recipients)
        else:
            with smtplib.SMTP(cfg["host"], cfg["port"], timeout=30) as smtp:
                smtp.ehlo()
                if cfg["port"] == 587:
                    # STARTTLS is mandatory on 587 — even if the server
                    # claims (or a MITM forges) that it doesn't support
                    # it, we refuse to keep going in cleartext.
                    if not smtp.has_extn("starttls"):
                        raise _StartTLSUnavailable(
                            f"server {cfg['host']!r} did not advertise "
                            "STARTTLS on the submission port; refusing "
                            "to send credentials in cleartext"
                        )
                    smtp.starttls(context=ssl.create_default_context())
                    smtp.ehlo()
                elif cfg["port"] == 25:
                    # Plain port 25 is an internal-relay-only mode.
                    # We still try STARTTLS opportunistically when the
                    # server offers it, but don't force it.
                    if smtp.has_extn("starttls"):
                        smtp.starttls(context=ssl.create_default_context())
                        smtp.ehlo()
                smtp.login(cfg["user"], cfg["password"])
                smtp.send_message(msg, from_addr=cfg["from"], to_addrs=recipients)
    except smtplib.SMTPException as e:
        return {
            "ok": False,
            "platform": PLATFORM,
            "host": cfg["host"],
            "port": cfg["port"],
            "error": f"SMTP error: {e}",
        }
    except (socket.gaierror, OSError) as e:
        return {
            "ok": False,
            "platform": PLATFORM,
            "host": cfg["host"],
            "port": cfg["port"],
            "error": f"network error: {e}",
        }
    return {
        "ok": True,
        "platform": PLATFORM,
        "host": cfg["host"],
        "port": cfg["port"],
        "from": cfg["from"],
        "to": recipients,
        "subject": str(subject),
        "bytes": len(body.encode("utf-8")),
    }


def _not_yet(command: str) -> dict:
    return {
        "ok": False,
        "platform": PLATFORM,
        "command": command,
        "status": "not_yet_implemented",
        "note": (
            "IMAP polling loop still pending. "
            "Use ``send <to> <subject> <body>`` for outbound until then."
        ),
    }


def _status() -> dict:
    cfg, err = _load_config()
    return {
        "ok": True,
        "platform": PLATFORM,
        "running": False,
        "configured": cfg is not None,
        "host": cfg["host"] if cfg else None,
        "port": cfg["port"] if cfg else None,
        "from": cfg["from"] if cfg else None,
        "config_error": err,
        "note": "Outbound-only mode. IMAP polling loop not yet implemented.",
    }


def run(command: str, args):
    if command == "__schema__":
        return _schema()
    if command == "send":
        if isinstance(args, list):
            to = args[0] if len(args) > 0 else ""
            subject = args[1] if len(args) > 1 else ""
            body = args[2] if len(args) > 2 else ""
            cc = args[3] if len(args) > 3 else ""
        elif isinstance(args, dict):
            to = args.get("to", "")
            subject = args.get("subject", "")
            body = args.get("body", "")
            cc = args.get("cc", "")
        else:
            return {"ok": False, "error": "invalid args"}
        result = _send(str(to), str(subject), str(body), str(cc))
        gateway_memory.remember_send(
            PLATFORM,
            result,
            channel_id=str(to),
            text=f"{subject}: {body}" if subject else str(body),
        )
        return result
    if command == "status":
        return _status()
    if command in {"start", "stop"}:
        return _not_yet(command)
    return {"ok": False, "error": f"unknown command: {command}"}


if __name__ == "__main__":
    cmd = os.environ.get("COS_COMMAND") or (sys.argv[1] if len(sys.argv) > 1 else "")
    raw_args = os.environ.get("COS_ARGS_JSON")
    if raw_args:
        parsed_args = json.loads(raw_args)
    else:
        parsed_args = sys.argv[2:]
    print(json.dumps(run(cmd, parsed_args)))
