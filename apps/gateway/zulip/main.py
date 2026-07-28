"""Zulip chat gateway app.

Outbound-only baseline. ``send`` POSTs to
``<site>/api/v1/messages`` with HTTP basic auth
(``bot_email:bot_api_key``).

Recipient routing (the recipient string is parsed into Zulip's
two-flavour addressing):

  * ``stream:topic``      → ``type=stream``, ``to=stream``, ``topic=topic``
  * ``stream``            → ``type=stream``, ``to=stream``, ``topic="(no topic)"``
  * ``user@example.com``  → ``type=private``, ``to=[user@example.com]``
  * ``a@x.com,b@y.com``   → ``type=private`` to multiple recipients
                            (comma-separated emails, no whitespace
                            inside an individual address)

Body: rendered as Zulip-flavour Markdown (the Zulip API treats
``content`` as markdown by default).

Credentials needed:

  * ``zulip_site``         — full origin, e.g. ``https://chat.example.com``
                             or ``https://yourrealm.zulipchat.com``
                             (env override: COS_ZULIP_SITE)
  * ``zulip_bot_email``    — bot user e-mail address
                             (env override: COS_ZULIP_BOT_EMAIL)
  * ``zulip_bot_api_key``  — bot API key from ``/zulip/personal``
                             (env override: COS_ZULIP_BOT_API_KEY)

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

from _shared import gateway_memory, safe_egress, safe_subprocess  # noqa: E402


PLATFORM = "zulip"
USER_AGENT = "ClawOSZulip/0.1.0"
SOFT_LEN = 9900  # Zulip caps message content around 10000 chars
EMAIL_RE = re.compile(r"^[^@\s,]+@[^@\s,]+\.[^@\s,]+$")


def _schema() -> dict:
    return {
        "name": f"gateway-{PLATFORM}",
        "version": "0.1.0",
        "description": (
            "Zulip gateway via REST API. ``send`` posts a markdown "
            "message to a stream:topic or private DM (single or "
            "multi-recipient). Bot credentials only (no OAuth)."
        ),
        "commands": {
            "start": {
                "description": "Receive inbound messages (NOT IMPLEMENTED)",
                "parameters": [],
                "example": "cos app gateway-zulip start",
            },
            "stop": {
                "description": "Stop a running gateway (NOT IMPLEMENTED)",
                "parameters": [],
                "example": "cos app gateway-zulip stop",
            },
            "status": {
                "description": "Show whether the bot is configured",
                "parameters": [],
                "example": "cos app gateway-zulip status",
            },
            "send": {
                "description": "Send a markdown message",
                "parameters": [
                    {
                        "name": "recipient",
                        "type": "string",
                        "required": True,
                        "description": (
                            "stream:topic | stream | user@host | "
                            "user@host,user2@host"
                        ),
                        "kind": "positional",
                    },
                    {
                        "name": "text",
                        "type": "string",
                        "required": True,
                        "description": "Message body (truncated to ~9900 chars)",
                        "kind": "positional",
                    },
                ],
                "example": (
                    "cos app gateway-zulip send 'engineering:deploys' "
                    "'deploy ok'"
                ),
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
    site, err = _env_or_credential("COS_ZULIP_SITE", "zulip_site")
    if not site:
        return None, err or "zulip_site required"
    email, err = _env_or_credential("COS_ZULIP_BOT_EMAIL", "zulip_bot_email")
    if not email:
        return None, err or "zulip_bot_email required"
    api_key, err = _env_or_credential("COS_ZULIP_BOT_API_KEY", "zulip_bot_api_key")
    if not api_key:
        return None, err or "zulip_bot_api_key required"
    site = site.rstrip("/")
    if not site.startswith(("https://", "http://")):
        return None, f"zulip_site must be http(s) URL, got: {site!r}"
    return {"site": site, "email": email, "api_key": api_key}, None


def _truncate(text: str, limit: int = SOFT_LEN) -> str:
    if len(text) <= limit:
        return text
    return text[: limit - 1] + "\u2026"


def _classify_recipient(recipient: str) -> tuple[str, dict]:
    """Return ``("stream"|"private", form_fields)`` where
    ``form_fields`` is the per-routing payload to merge into the
    Zulip /messages POST."""
    s = recipient.strip()
    # All-emails (one or comma-separated): private DM.
    parts = [p.strip() for p in s.split(",") if p.strip()]
    if parts and all(EMAIL_RE.match(p) for p in parts):
        return "private", {
            "type": "private",
            "to": json.dumps(parts),
        }
    # Stream form: "stream:topic" or just "stream".
    if ":" in s:
        stream, _, topic = s.partition(":")
        stream = stream.strip()
        topic = topic.strip() or "(no topic)"
    else:
        stream = s
        topic = "(no topic)"
    return "stream", {
        "type": "stream",
        "to": stream,
        "topic": topic,
    }


def _send(recipient: str, text: str) -> dict:
    if not recipient or not str(recipient).strip():
        return {"ok": False, "error": "recipient required"}
    if not text or not str(text).strip():
        return {"ok": False, "error": "text required"}

    cfg, err = _load_config()
    if not cfg:
        return {"ok": False, "error": err or "zulip not configured"}

    body_text = _truncate(str(text))
    routing, fields = _classify_recipient(recipient)
    if routing == "stream" and not fields.get("to"):
        return {"ok": False, "error": "stream name empty"}

    form = {**fields, "content": body_text}
    body = urllib.parse.urlencode(form).encode("utf-8")

    creds = f"{cfg['email']}:{cfg['api_key']}".encode("utf-8")
    auth = base64.b64encode(creds).decode("ascii")

    url = f"{cfg['site']}/api/v1/messages"
    headers = {
        "Authorization": f"Basic {auth}",
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
            verb_id="net.dial",
        )
        raw = raw_resp.decode("utf-8", errors="replace")
        try:
            data = json.loads(raw)
        except json.JSONDecodeError:
            data = {"raw": raw}
        ok = (
            isinstance(data, dict)
            and data.get("result") == "success"
        )
        return {
            "ok": ok,
            "platform": PLATFORM,
            "routing": routing,
            "site": cfg["site"],
            "id": data.get("id") if isinstance(data, dict) else None,
            "result": data.get("result")
            if isinstance(data, dict)
            else None,
            "msg": data.get("msg") if isinstance(data, dict) else None,
        }
    except safe_egress.EgressBlocked as e:
        return {
            "ok": False,
            "platform": PLATFORM,
            "routing": routing,
            "site": cfg["site"],
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
            "routing": routing,
            "site": cfg["site"],
            "error": f"HTTP {e.code}: {err_body}",
        }
    except urllib.error.URLError as e:
        return {
            "ok": False,
            "platform": PLATFORM,
            "routing": routing,
            "site": cfg["site"],
            "error": f"URL error: {e.reason}",
        }
    except Exception as e:
        denial = getattr(e, "denial", None)
        if denial is not None:
            return {
                "ok": False,
                "platform": PLATFORM,
                "routing": routing,
                "site": cfg["site"],
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
            "Inbound Zulip messages need either long-poll on "
            "/api/v1/events or a webhook integration. Use "
            "``send <recipient> <text>`` for outbound."
        ),
    }


def _status() -> dict:
    cfg, err = _load_config()
    return {
        "ok": True,
        "platform": PLATFORM,
        "running": False,
        "configured": cfg is not None,
        "site": cfg["site"] if cfg else None,
        "config_error": err,
        "note": "Outbound-only via Zulip REST API (HTTP basic auth).",
    }


def run(command: str, args):
    if command == "__schema__":
        return _schema()
    if command == "send":
        recipient = ""
        text = ""
        if isinstance(args, list):
            if len(args) >= 2:
                recipient, text = str(args[0]), str(args[1])
            elif len(args) == 1:
                text = str(args[0])
        elif isinstance(args, dict):
            recipient = str(args.get("recipient", "") or "")
            text = str(args.get("text", "") or "")
        else:
            return {"ok": False, "error": "invalid args"}
        result = _send(recipient, text)
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
