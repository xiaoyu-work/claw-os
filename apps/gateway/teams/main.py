"""Microsoft Teams gateway app (Incoming Webhook backend).

Outbound-only baseline. ``send`` POSTs to the configured Teams
webhook URL.

Two webhook flavours exist in production:

  * **Workflows webhook** (modern, recommended; Adaptive Card v1.5
    body wrapped in the standard Adaptive Card payload).
  * **Connector webhook** (legacy; MessageCard v1 body).

We send the **Adaptive Card v1.5** envelope by default because that's
what Microsoft's current ``Workflows`` action expects. If the URL is
a legacy connector and the response is rejected, the gateway returns
``ok: false`` with the response body so the caller can switch to the
``--legacy`` form (which sends a plain MessageCard fallback).

Recipients in Teams webhooks are implicit (the channel the webhook
was created in), so ``recipient`` is informational-only and ignored
on the wire.

Credentials needed:
  * ``teams_webhook_url`` -- full webhook URL
                             (env override: COS_TEAMS_WEBHOOK_URL)

Stdlib only.
"""

from __future__ import annotations

import json
import os
import sys
import urllib.error


# Sibling ``_shared`` package import (script-mode invocation).
sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

from _shared import gateway_memory, safe_egress, safe_subprocess  # noqa: E402


PLATFORM = "teams"
USER_AGENT = "ClawOSTeams/0.1.0"
SOFT_LEN = 28000  # Adaptive Card text fields tolerate ~28k


def _schema() -> dict:
    return {
        "name": f"gateway-{PLATFORM}",
        "version": "0.1.0",
        "description": (
            "Microsoft Teams gateway via Incoming Webhook. ``send`` posts "
            "an Adaptive Card v1.5 by default; ``--legacy`` sends a "
            "MessageCard v1 fallback for the legacy connector."
        ),
        "commands": {
            "start": {
                "description": "Receive inbound messages (NOT IMPLEMENTED)",
                "parameters": [],
                "example": "cos app gateway-teams start",
            },
            "stop": {
                "description": "Stop a running gateway (NOT IMPLEMENTED)",
                "parameters": [],
                "example": "cos app gateway-teams stop",
            },
            "status": {
                "description": "Show whether the webhook URL is configured",
                "parameters": [],
                "example": "cos app gateway-teams status",
            },
            "send": {
                "description": "Send a text message to the channel the webhook is bound to",
                "parameters": [
                    {
                        "name": "recipient",
                        "type": "string",
                        "required": False,
                        "description": "Informational only (Teams webhook channel is implicit); empty is fine",
                        "kind": "positional",
                    },
                    {
                        "name": "text",
                        "type": "string",
                        "required": True,
                        "description": "Message body (truncated to 28000 chars)",
                        "kind": "positional",
                    },
                    {
                        "name": "title",
                        "type": "string",
                        "required": False,
                        "description": "Optional card title",
                    },
                    {
                        "name": "legacy",
                        "type": "boolean",
                        "required": False,
                        "description": "Send legacy MessageCard v1 instead of Adaptive Card",
                    },
                ],
                "example": "cos app gateway-teams send '' 'hello'",
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


def _load_webhook_url() -> tuple[str | None, str | None]:
    return _env_or_credential("COS_TEAMS_WEBHOOK_URL", "teams_webhook_url")


def _truncate(text: str, limit: int = SOFT_LEN) -> str:
    if len(text) <= limit:
        return text
    return text[: limit - 1] + "\u2026"


def _adaptive_card(text: str, title: str = "") -> dict:
    body = []
    if title and title.strip():
        body.append({
            "type": "TextBlock",
            "text": title.strip(),
            "weight": "Bolder",
            "size": "Medium",
            "wrap": True,
        })
    body.append({
        "type": "TextBlock",
        "text": text,
        "wrap": True,
    })
    return {
        "type": "message",
        "attachments": [
            {
                "contentType": "application/vnd.microsoft.card.adaptive",
                "contentUrl": None,
                "content": {
                    "$schema": "http://adaptivecards.io/schemas/adaptive-card.json",
                    "type": "AdaptiveCard",
                    "version": "1.5",
                    "body": body,
                },
            }
        ],
    }


def _legacy_messagecard(text: str, title: str = "") -> dict:
    payload: dict = {
        "@type": "MessageCard",
        "@context": "https://schema.org/extensions",
        "text": text,
    }
    if title and title.strip():
        payload["title"] = title.strip()
    return payload


def _send(
    recipient: str,
    text: str,
    title: str = "",
    legacy: bool = False,
) -> dict:
    if not text or not str(text).strip():
        return {"ok": False, "error": "text required"}

    url, err = _load_webhook_url()
    if not url:
        return {"ok": False, "error": err or "teams_webhook_url required"}

    payload = (
        _legacy_messagecard(_truncate(str(text)), title)
        if legacy
        else _adaptive_card(_truncate(str(text)), title)
    )
    body = json.dumps(payload).encode("utf-8")
    headers = {
        "Content-Type": "application/json",
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
            verb_id="net.dial",
        )
        raw = raw_resp.decode("utf-8", errors="replace")
        return {
            "ok": True,
            "platform": PLATFORM,
            "informational_recipient": recipient or None,
            "card_kind": "messagecard" if legacy else "adaptive",
            "response": raw,
        }
    except safe_egress.EgressBlocked as e:
        return {
            "ok": False,
            "platform": PLATFORM,
            "card_kind": "messagecard" if legacy else "adaptive",
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
            "card_kind": "messagecard" if legacy else "adaptive",
            "error": f"HTTP {e.code}: {err_body}",
        }
    except urllib.error.URLError as e:
        return {
            "ok": False,
            "platform": PLATFORM,
            "card_kind": "messagecard" if legacy else "adaptive",
            "error": f"URL error: {e.reason}",
        }
    except Exception as e:
        denial = getattr(e, "denial", None)
        if denial is not None:
            return {
                "ok": False,
                "platform": PLATFORM,
                "card_kind": "messagecard" if legacy else "adaptive",
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
            "Inbound Teams messages need a Bot Framework / Azure Bot "
            "registration (not a webhook). Use ``send '' <text>`` for "
            "outbound."
        ),
    }


def _status() -> dict:
    url, err = _load_webhook_url()
    return {
        "ok": True,
        "platform": PLATFORM,
        "running": False,
        "configured": url is not None,
        "config_error": err,
        "note": "Outbound-only via Teams Incoming Webhook (Adaptive Card v1.5).",
    }


def run(command: str, args):
    if command == "__schema__":
        return _schema()
    if command == "send":
        recipient = ""
        text = ""
        title = ""
        legacy = False
        if isinstance(args, list):
            if len(args) >= 2:
                recipient, text = str(args[0]), str(args[1])
            elif len(args) == 1:
                text = str(args[0])
        elif isinstance(args, dict):
            recipient = str(args.get("recipient", "") or "")
            text = str(args.get("text", "") or "")
            title = str(args.get("title", "") or "")
            legacy = bool(args.get("legacy", False))
        else:
            return {"ok": False, "error": "invalid args"}
        result = _send(recipient, text, title, legacy)
        gateway_memory.remember_send(PLATFORM, result, channel_id=recipient, text=text)
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
