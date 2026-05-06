"""WhatsApp Cloud API gateway app.

Outbound-only baseline: ``send`` POSTs a text message via the Meta
Graph API at ``/{api_version}/{phone_number_id}/messages`` with a
Bearer access token.  Inbound (the verify-token + webhook receiver
HTTP server) is still a stub.

Credentials needed:
  * ``whatsapp_access_token``        — Meta system-user / app token
  * ``whatsapp_phone_number_id``     — sender phone number id (NOT
                                       the phone number itself —
                                       this is a Meta-issued id
                                       attached to the WhatsApp
                                       Business Account)

Optional env override: ``COS_WHATSAPP_TOKEN``.

Stdlib only.
"""

from __future__ import annotations

import json
import os
import subprocess
import sys
import urllib.error
import urllib.request


PLATFORM = "whatsapp"
USER_AGENT = "ClawOSWhatsApp/0.1.0 (+https://github.com/clawos/cos)"
GRAPH_API = "https://graph.facebook.com"
API_VERSION = "v21.0"
SOFT_LEN = 4096  # WhatsApp text body limit (per Meta docs, 4096 chars)


def _schema() -> dict:
    return {
        "name": f"gateway-{PLATFORM}",
        "version": "0.1.0",
        "description": (
            "WhatsApp Cloud API gateway. ``send`` posts text via "
            "Meta's Graph API. ``start``/``stop`` (webhook server) "
            "are not yet implemented."
        ),
        "commands": {
            "start": {
                "description": "Run inbound webhook server (NOT IMPLEMENTED)",
                "parameters": [],
                "example": "cos app gateway-whatsapp start",
            },
            "stop": {
                "description": "Stop a running gateway (NOT IMPLEMENTED)",
                "parameters": [],
                "example": "cos app gateway-whatsapp stop",
            },
            "status": {
                "description": "Show running state",
                "parameters": [],
                "example": "cos app gateway-whatsapp status",
            },
            "send": {
                "description": "Send a text message via WhatsApp Cloud API",
                "parameters": [
                    {
                        "name": "recipient_phone",
                        "type": "string",
                        "required": True,
                        "description": "Recipient's WhatsApp phone in E.164 (digits only, no + or spaces)",
                        "kind": "positional",
                    },
                    {
                        "name": "text",
                        "type": "string",
                        "required": True,
                        "description": "Plain-text message body (truncated to 4096 chars)",
                        "kind": "positional",
                    },
                ],
                "example": "cos app gateway-whatsapp send 15551234567 'hello'",
            },
        },
    }


def _load_credential(name: str) -> tuple[str | None, str | None]:
    try:
        proc = subprocess.run(
            ["cos", "credential", "load", name],
            capture_output=True,
            text=True,
            timeout=10,
        )
    except (FileNotFoundError, subprocess.TimeoutExpired) as e:
        return None, f"cos credential load failed: {e}"
    if proc.returncode != 0:
        return None, (
            f"cos credential load returned {proc.returncode}: "
            f"{proc.stderr.strip()}"
        )
    try:
        payload = json.loads(proc.stdout)
    except json.JSONDecodeError as e:
        return None, f"credential payload not JSON: {e}"
    val = payload.get("value") if isinstance(payload, dict) else None
    if not isinstance(val, str) or not val.strip():
        return None, f"credential '{name}' missing 'value'"
    return val.strip(), None


def _load_token() -> tuple[str | None, str | None]:
    env_tok = os.environ.get("COS_WHATSAPP_TOKEN")
    if env_tok:
        return env_tok.strip(), None
    return _load_credential("whatsapp_access_token")


def _load_phone_number_id() -> tuple[str | None, str | None]:
    env_id = os.environ.get("COS_WHATSAPP_PHONE_NUMBER_ID")
    if env_id:
        return env_id.strip(), None
    return _load_credential("whatsapp_phone_number_id")


def _truncate(text: str, limit: int = SOFT_LEN) -> str:
    if len(text) <= limit:
        return text
    return text[: limit - 1] + "\u2026"


def _normalise_phone(s: str) -> str:
    """Strip + and whitespace; Meta wants digits-only E.164."""
    return "".join(ch for ch in s if ch.isdigit())


def _send(recipient_phone: str, text: str) -> dict:
    if not recipient_phone or not str(recipient_phone).strip():
        return {"ok": False, "error": "recipient_phone required"}
    if not text or not str(text).strip():
        return {"ok": False, "error": "text required"}

    phone = _normalise_phone(str(recipient_phone))
    if not phone:
        return {
            "ok": False,
            "error": "recipient_phone has no digits after normalisation",
        }

    token, err = _load_token()
    if not token:
        return {"ok": False, "error": err or "no token"}
    pnid, err = _load_phone_number_id()
    if not pnid:
        return {"ok": False, "error": err or "no phone_number_id"}

    body = json.dumps(
        {
            "messaging_product": "whatsapp",
            "recipient_type": "individual",
            "to": phone,
            "type": "text",
            "text": {"body": _truncate(str(text)), "preview_url": False},
        }
    ).encode("utf-8")

    url = f"{GRAPH_API}/{API_VERSION}/{pnid}/messages"
    req = urllib.request.Request(
        url,
        data=body,
        method="POST",
        headers={
            "Authorization": f"Bearer {token}",
            "Content-Type": "application/json",
            "User-Agent": USER_AGENT,
        },
    )
    try:
        with urllib.request.urlopen(req, timeout=15) as resp:
            raw = resp.read().decode("utf-8", errors="replace")
            try:
                data = json.loads(raw)
            except json.JSONDecodeError:
                data = {"raw": raw}
            wamid = None
            if isinstance(data, dict):
                msgs = data.get("messages")
                if isinstance(msgs, list) and msgs:
                    first = msgs[0]
                    if isinstance(first, dict):
                        wamid = first.get("id")
            return {
                "ok": True,
                "platform": PLATFORM,
                "to": phone,
                "wamid": wamid,
            }
    except urllib.error.HTTPError as e:
        try:
            err_body = e.read().decode("utf-8", errors="replace")
        except Exception:
            err_body = str(e)
        return {
            "ok": False,
            "platform": PLATFORM,
            "error": f"HTTP {e.code}: {err_body}",
        }
    except urllib.error.URLError as e:
        return {"ok": False, "platform": PLATFORM, "error": f"URL error: {e}"}


def _not_yet(command: str) -> dict:
    return {
        "ok": False,
        "platform": PLATFORM,
        "command": command,
        "status": "not_yet_implemented",
        "note": (
            "WhatsApp inbound webhook server still pending. "
            "Use ``send <recipient_phone> <text>`` for outbound until then."
        ),
    }


def _status() -> dict:
    return {
        "ok": True,
        "platform": PLATFORM,
        "running": False,
        "api_version": API_VERSION,
        "note": "Outbound-only mode. Webhook receiver not yet implemented.",
    }


def run(command: str, args):
    if command == "__schema__":
        return _schema()
    if command == "send":
        if isinstance(args, list):
            recipient = args[0] if len(args) > 0 else ""
            text = args[1] if len(args) > 1 else ""
        elif isinstance(args, dict):
            recipient = args.get("recipient_phone", "")
            text = args.get("text", "")
        else:
            return {"ok": False, "error": "invalid args"}
        return _send(str(recipient), str(text))
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
