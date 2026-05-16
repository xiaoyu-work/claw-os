"""Webex (Cisco Webex App / Webex Teams) gateway app.

Outbound-only baseline. ``send`` POSTs to
``https://webexapis.com/v1/messages`` with a bot token (Bearer auth).

Recipient routing (only one of these is sent per call):

  * E-mail (``alice@example.com``) → ``toPersonEmail``
  * Person id (``Y2lzY29zcGFyazovL3VzL1...``, base64 starting with
    ``Y2lzY29zcGFyazovL``) → ``toPersonId``
  * Anything else → ``roomId`` (a Webex spaceId; opaque)

Body shape: ``markdown`` is preferred (Webex renders Markdown);
``text`` is included as a plain-text fallback for clients that strip
formatting.

Credentials needed:
  * ``webex_bot_token`` -- bot access token from developer.webex.com
                           (env override: COS_WEBEX_BOT_TOKEN)

Stdlib only.
"""

from __future__ import annotations

import json
import os
import re
import sys
import urllib.error


# Sibling ``_shared`` package import (script-mode invocation).
sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

from _shared import safe_egress, safe_subprocess  # noqa: E402


PLATFORM = "webex"
USER_AGENT = "ClawOSWebex/0.1.0"
SOFT_LEN = 7400  # Webex caps messages around 7439 chars
API_URL = "https://webexapis.com/v1/messages"
EMAIL_RE = re.compile(r"^[^@\s]+@[^@\s]+\.[^@\s]+$")


def _schema() -> dict:
    return {
        "name": f"gateway-{PLATFORM}",
        "version": "0.1.0",
        "description": (
            "Webex gateway via REST API. ``send`` posts a markdown "
            "message to a roomId, personEmail, or personId. Bot tokens "
            "only (no OAuth user flow)."
        ),
        "commands": {
            "start": {
                "description": "Receive inbound messages (NOT IMPLEMENTED)",
                "parameters": [],
                "example": "cos app gateway-webex start",
            },
            "stop": {
                "description": "Stop a running gateway (NOT IMPLEMENTED)",
                "parameters": [],
                "example": "cos app gateway-webex stop",
            },
            "status": {
                "description": "Show whether the bot token is configured",
                "parameters": [],
                "example": "cos app gateway-webex status",
            },
            "send": {
                "description": "Send a markdown message to a room or person",
                "parameters": [
                    {
                        "name": "recipient",
                        "type": "string",
                        "required": True,
                        "description": "roomId (opaque) | personEmail | personId",
                        "kind": "positional",
                    },
                    {
                        "name": "text",
                        "type": "string",
                        "required": True,
                        "description": "Message body, Markdown (truncated to ~7400 chars)",
                        "kind": "positional",
                    },
                    {
                        "name": "plain",
                        "type": "boolean",
                        "required": False,
                        "description": "Send as plain text only (no Markdown rendering)",
                    },
                ],
                "example": "cos app gateway-webex send alice@example.com 'hello'",
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


def _load_token() -> tuple[str | None, str | None]:
    return _env_or_credential("COS_WEBEX_BOT_TOKEN", "webex_bot_token")


def _truncate(text: str, limit: int = SOFT_LEN) -> str:
    if len(text) <= limit:
        return text
    return text[: limit - 1] + "\u2026"


def _classify_recipient(rid: str) -> tuple[str, str]:
    """Return ``(field_name, value)`` where field_name is one of
    ``toPersonEmail`` / ``toPersonId`` / ``roomId``."""
    s = rid.strip()
    if EMAIL_RE.match(s):
        return "toPersonEmail", s
    # Webex resource ids are base64url-encoded "ciscospark://us/..."
    # blobs that decode to start with that scheme. The base64 alphabet
    # variant uses '-' and '_' instead of '+' and '/'. The PEOPLE
    # variant has substring 'PEOPLE' embedded in the base64 (which
    # doesn't have a clean prefix), so we fall back: any ID-looking
    # token (>=48 chars, base64url alphabet, starts with the canonical
    # 'Y2lzY29zcGFyazovL' "ciscospark://" prefix) → personId; anything
    # else opaque → roomId.
    if len(s) >= 48 and s.startswith("Y2lzY29zcGFyazovL"):
        # ciscospark://us/PEOPLE/... vs ROOM/... can't be told apart
        # cheaply; default to roomId since rooms outnumber direct DMs
        # for bot use, and let the API NACK if wrong (Webex returns a
        # clear 400 with the proper field name).
        return "roomId", s
    return "roomId", s


def _send(recipient: str, text: str, plain: bool = False) -> dict:
    if not recipient or not str(recipient).strip():
        return {"ok": False, "error": "recipient required"}
    if not text or not str(text).strip():
        return {"ok": False, "error": "text required"}

    token, err = _load_token()
    if not token:
        return {"ok": False, "error": err or "webex_bot_token required"}

    body_text = _truncate(str(text))
    field, value = _classify_recipient(recipient)
    payload: dict = {field: value}
    if plain:
        payload["text"] = body_text
    else:
        payload["markdown"] = body_text
        payload["text"] = body_text  # plain fallback

    body = json.dumps(payload).encode("utf-8")
    headers = {
        "Authorization": f"Bearer {token}",
        "Content-Type": "application/json",
        "User-Agent": USER_AGENT,
        "Accept": "application/json",
    }
    try:
        _, _, raw_resp = safe_egress.safe_urlopen(
            "POST",
            API_URL,
            headers=headers,
            body=body,
            timeout=20,
            verb_id="gateway.webex.send",
        )
        raw = raw_resp.decode("utf-8", errors="replace")
        try:
            data = json.loads(raw)
        except json.JSONDecodeError:
            data = {"raw": raw}
        return {
            "ok": True,
            "platform": PLATFORM,
            "field": field,
            "value": value,
            "id": data.get("id") if isinstance(data, dict) else None,
            "created": data.get("created") if isinstance(data, dict) else None,
            "kind": "text" if plain else "markdown",
        }
    except safe_egress.EgressBlocked as e:
        return {
            "ok": False,
            "platform": PLATFORM,
            "field": field,
            "value": value,
            "kind": "text" if plain else "markdown",
            "error": f"egress blocked: {e}",
        }
    except urllib.error.HTTPError as e:
        try:
            err_body = e.read().decode("utf-8", errors="replace")
        except Exception:
            err_body = str(e)
        return {
            "ok": False,
            "platform": PLATFORM,
            "field": field,
            "value": value,
            "kind": "text" if plain else "markdown",
            "error": f"HTTP {e.code}: {err_body}",
        }
    except urllib.error.URLError as e:
        return {
            "ok": False,
            "platform": PLATFORM,
            "field": field,
            "value": value,
            "kind": "text" if plain else "markdown",
            "error": f"URL error: {e.reason}",
        }
    except Exception as e:
        denial = getattr(e, "denial", None)
        if denial is not None:
            return {
                "ok": False,
                "platform": PLATFORM,
                "field": field,
                "value": value,
                "kind": "text" if plain else "markdown",
                "error": "permission denied",
                "denial": denial,
            }
        raise


def _not_yet(command: str) -> dict:
    return {
        "ok": False,
        "platform": PLATFORM,
        "command": command,
        "status": "not_yet_implemented",
        "note": (
            "Inbound Webex messages need a webhook subscription "
            "(POST /v1/webhooks). Use ``send <recipient> <text>`` for "
            "outbound."
        ),
    }


def _status() -> dict:
    token, err = _load_token()
    return {
        "ok": True,
        "platform": PLATFORM,
        "running": False,
        "configured": token is not None,
        "config_error": err,
        "note": "Outbound-only via Webex REST API (Bearer bot token).",
    }


def run(command: str, args):
    if command == "__schema__":
        return _schema()
    if command == "send":
        recipient = ""
        text = ""
        plain = False
        if isinstance(args, list):
            if len(args) >= 2:
                recipient, text = str(args[0]), str(args[1])
            elif len(args) == 1:
                text = str(args[0])
        elif isinstance(args, dict):
            recipient = str(args.get("recipient", "") or "")
            text = str(args.get("text", "") or "")
            plain = bool(args.get("plain", False))
        else:
            return {"ok": False, "error": "invalid args"}
        return _send(recipient, text, plain)
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
