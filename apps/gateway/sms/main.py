"""SMS gateway app (Twilio backend).

Outbound-only baseline: ``send`` POSTs to the Twilio Messages REST
API at
``https://api.twilio.com/2010-04-01/Accounts/{AccountSid}/Messages.json``
using HTTP Basic auth (Account SID + Auth Token). Inbound (Twilio
webhook receiver) is still a stub.

Credentials needed:
  * ``twilio_account_sid`` -- Account SID (starts with ``AC...``)
                              (env override: COS_TWILIO_ACCOUNT_SID)
  * ``twilio_auth_token``  -- Auth Token
                              (env override: COS_TWILIO_AUTH_TOKEN)
  * ``twilio_from``        -- Sender phone (E.164, e.g. +14155552671)
                              OR Messaging Service SID (``MG...``)
                              (env override: COS_TWILIO_FROM)

Stdlib only.
"""

from __future__ import annotations

import base64
import json
import os
import re
import sys
import urllib.error
import urllib.parse


# Sibling ``_shared`` package import (script-mode invocation).
sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

from _shared import safe_egress, safe_subprocess  # noqa: E402


PLATFORM = "sms"
USER_AGENT = "ClawOSSMS/0.1.0"
TWILIO_API = "https://api.twilio.com"
API_VERSION = "2010-04-01"
SOFT_LEN = 1600  # Twilio caps a single Body at 1600 chars (auto-segmented)


def _schema() -> dict:
    return {
        "name": f"gateway-{PLATFORM}",
        "version": "0.1.0",
        "description": (
            "SMS gateway via Twilio. ``send`` delivers one-shot text "
            "messages. ``start``/``stop`` (inbound webhook) are not "
            "yet implemented."
        ),
        "commands": {
            "start": {
                "description": "Start an inbound webhook receiver (NOT IMPLEMENTED)",
                "parameters": [],
                "example": "cos app gateway-sms start",
            },
            "stop": {
                "description": "Stop a running gateway (NOT IMPLEMENTED)",
                "parameters": [],
                "example": "cos app gateway-sms stop",
            },
            "status": {
                "description": "Show running state (always 'stopped' until inbound webhook lands)",
                "parameters": [],
                "example": "cos app gateway-sms status",
            },
            "send": {
                "description": "Send a one-shot SMS message",
                "parameters": [
                    {
                        "name": "to",
                        "type": "string",
                        "required": True,
                        "description": "Recipient phone in E.164 format, e.g. +14155552671",
                        "kind": "positional",
                    },
                    {
                        "name": "text",
                        "type": "string",
                        "required": True,
                        "description": "Message body (truncated to 1600 chars)",
                        "kind": "positional",
                    },
                ],
                "example": "cos app gateway-sms send '+14155552671' 'hello'",
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
    sid, err = _env_or_credential("COS_TWILIO_ACCOUNT_SID", "twilio_account_sid")
    if not sid:
        return None, err or "twilio_account_sid required"
    token, err = _env_or_credential("COS_TWILIO_AUTH_TOKEN", "twilio_auth_token")
    if not token:
        return None, err or "twilio_auth_token required"
    sender, err = _env_or_credential("COS_TWILIO_FROM", "twilio_from")
    if not sender:
        return None, err or "twilio_from required"
    return {"sid": sid, "token": token, "from": sender}, None


def _truncate(text: str, limit: int = SOFT_LEN) -> str:
    if len(text) <= limit:
        return text
    return text[: limit - 1] + "\u2026"


def _normalize_e164(phone: str) -> str:
    """E.164 keep '+' and digits only. Twilio rejects formatted numbers."""
    s = str(phone).strip()
    if not s:
        return ""
    plus = "+" if s.startswith("+") else ""
    digits = re.sub(r"[^0-9]", "", s)
    return f"{plus}{digits}"


def _basic_auth(sid: str, token: str) -> str:
    raw = f"{sid}:{token}".encode("utf-8")
    return "Basic " + base64.b64encode(raw).decode("ascii")


def _send(to: str, text: str) -> dict:
    if not to or not str(to).strip():
        return {"ok": False, "error": "to required"}
    if not text or not str(text).strip():
        return {"ok": False, "error": "text required"}

    cfg, err = _load_config()
    if not cfg:
        return {"ok": False, "error": err or "config error"}

    to_e164 = _normalize_e164(to)
    if not to_e164.startswith("+") or len(to_e164) < 8:
        return {
            "ok": False,
            "platform": PLATFORM,
            "error": f"to must be E.164 (e.g. +14155552671), got {to!r}",
        }

    sender = cfg["from"]
    # Twilio accepts either From=<phone> or MessagingServiceSid=<MGxxxx>.
    form: dict[str, str] = {"To": to_e164, "Body": _truncate(str(text))}
    if sender.startswith("MG"):
        form["MessagingServiceSid"] = sender
    else:
        form["From"] = _normalize_e164(sender)

    body = urllib.parse.urlencode(form).encode("utf-8")
    url = f"{TWILIO_API}/{API_VERSION}/Accounts/{cfg['sid']}/Messages.json"
    headers = {
        "Authorization": _basic_auth(cfg["sid"], cfg["token"]),
        "Content-Type": "application/x-www-form-urlencoded",
        "User-Agent": USER_AGENT,
        "Accept": "application/json",
    }
    try:
        _, _, raw_resp = safe_egress.safe_urlopen(
            "POST",
            url,
            headers=headers,
            body=body,
            timeout=20,
            verb_id="gateway.sms.send",
        )
        raw = raw_resp.decode("utf-8", errors="replace")
        try:
            data = json.loads(raw)
        except json.JSONDecodeError:
            data = {"raw": raw}
        return {
            "ok": True,
            "platform": PLATFORM,
            "to": to_e164,
            "from": form.get("From") or form.get("MessagingServiceSid"),
            "sid": data.get("sid") if isinstance(data, dict) else None,
            "status": data.get("status") if isinstance(data, dict) else None,
            "num_segments": data.get("num_segments") if isinstance(data, dict) else None,
        }
    except safe_egress.EgressBlocked as e:
        return {"ok": False, "platform": PLATFORM, "error": f"egress blocked: {e}"}
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
        return {
            "ok": False,
            "platform": PLATFORM,
            "error": f"URL error: {e.reason}",
        }
    except Exception as e:
        denial = getattr(e, "denial", None)
        if denial is not None:
            return {"ok": False, "platform": PLATFORM, "error": "permission denied", "denial": denial}
        raise


def _not_yet(command: str) -> dict:
    return {
        "ok": False,
        "platform": PLATFORM,
        "command": command,
        "status": "not_yet_implemented",
        "note": (
            "Inbound webhook receiver still pending. "
            "Use ``send <to> <text>`` for outbound until then."
        ),
    }


def _status() -> dict:
    cfg, err = _load_config()
    return {
        "ok": True,
        "platform": PLATFORM,
        "running": False,
        "configured": cfg is not None,
        "from": cfg["from"] if cfg else None,
        "config_error": err,
        "note": "Outbound-only mode. Inbound webhook receiver not yet implemented.",
    }


def run(command: str, args):
    if command == "__schema__":
        return _schema()
    if command == "send":
        if isinstance(args, list):
            to = args[0] if len(args) > 0 else ""
            text = args[1] if len(args) > 1 else ""
        elif isinstance(args, dict):
            to = args.get("to", "")
            text = args.get("text", "")
        else:
            return {"ok": False, "error": "invalid args"}
        return _send(str(to), str(text))
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
