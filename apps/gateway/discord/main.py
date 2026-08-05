"""Discord gateway app.

Outbound-only baseline: ``send`` POSTs a message to a Discord channel
using the Bot REST API (``Authorization: Bot <token>``).  Inbound (the
Gateway WebSocket loop) is still a stub - landing it requires a
real WebSocket client.  Stdlib only.

Credentials: ``cos credential store discord_bot_token`` (or env
``COS_DISCORD_TOKEN``).
"""

from __future__ import annotations

import json
import os
import sys
import urllib.error


# Sibling ``_shared`` package import (script-mode invocation).
sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

from _shared import gateway_memory, safe_egress, safe_subprocess  # noqa: E402


PLATFORM = "discord"
DISCORD_API = "https://discord.com/api/v10"
USER_AGENT = "DiscordBot (https://github.com/clawos/cos, 0.2.0)"
MAX_LEN = 2000  # Discord per-message hard limit


def _schema() -> dict:
    return {
        "name": f"gateway-{PLATFORM}",
        "version": "0.2.0",
        "description": (
            "Discord gateway. ``send`` works via the Bot REST API. "
            "``start``/``stop`` (WebSocket gateway) are not yet implemented."
        ),
        "commands": {
            "start": {
                "description": "Connect to the Discord gateway WebSocket (NOT IMPLEMENTED)",
                "parameters": [],
                "example": "cos app gateway-discord start",
            },
            "stop": {
                "description": "Stop a running gateway (NOT IMPLEMENTED)",
                "parameters": [],
                "example": "cos app gateway-discord stop",
            },
            "status": {
                "description": "Show running state (always 'stopped' until WS lands)",
                "parameters": [],
                "example": "cos app gateway-discord status",
            },
            "send": {
                "description": "Send a message to a channel via Bot REST API",
                "parameters": [
                    {
                        "name": "channel_id",
                        "type": "string",
                        "required": True,
                        "description": "Target Discord channel id (snowflake)",
                        "kind": "positional",
                    },
                    {
                        "name": "text",
                        "type": "string",
                        "required": True,
                        "description": "Message text (truncated to 2000 chars)",
                        "kind": "positional",
                    },
                ],
                "example": "cos app gateway-discord send 123456789012345678 'hello'",
            },
        },
    }


def _load_token() -> tuple[str | None, str | None]:
    """Returns (token, error)."""
    env_tok = os.environ.get("COS_DISCORD_TOKEN")
    if env_tok:
        return env_tok.strip(), None
    return safe_subprocess.safe_credential_load("discord_bot_token")


def _truncate(text: str, limit: int = MAX_LEN) -> str:
    if len(text) <= limit:
        return text
    return text[: limit - 1] + "\u2026"


def _send(channel_id: str, text: str) -> dict:
    if not channel_id or not channel_id.strip():
        return {"ok": False, "error": "channel_id required"}
    if not text or not str(text).strip():
        return {"ok": False, "error": "text required"}
    token, err = _load_token()
    if not token:
        return {"ok": False, "error": err or "no token"}

    body = json.dumps({"content": _truncate(str(text))}).encode("utf-8")
    url = f"{DISCORD_API}/channels/{channel_id}/messages"
    headers = {
        "Authorization": f"Bot {token}",
        "Content-Type": "application/json",
        "User-Agent": USER_AGENT,
    }
    try:
        _, _, raw_resp = safe_egress.safe_urlopen(
            "POST",
            url,
            headers=headers,
            body=body,
            timeout=15,
            verb_id="net.dial",
        )
        raw = raw_resp.decode("utf-8", errors="replace")
        try:
            data = json.loads(raw)
        except json.JSONDecodeError:
            data = {"raw": raw}
        return {
            "ok": True,
            "platform": PLATFORM,
            "channel_id": channel_id,
            "message_id": data.get("id") if isinstance(data, dict) else None,
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
        return {"ok": False, "platform": PLATFORM, "error": f"URL error: {e.reason}"}
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
            "Discord Gateway WebSocket loop still pending. "
            "Use ``send <channel_id> <text>`` for outbound until then."
        ),
    }


def _status() -> dict:
    return {
        "ok": True,
        "platform": PLATFORM,
        "running": False,
        "note": "Outbound-only mode. WebSocket gateway not yet implemented.",
    }


def run(command: str, args):
    if command == "__schema__":
        return _schema()
    if command == "send":
        if isinstance(args, list):
            channel_id = args[0] if len(args) > 0 else ""
            text = args[1] if len(args) > 1 else ""
        elif isinstance(args, dict):
            channel_id = args.get("channel_id", "")
            text = args.get("text", "")
        else:
            return {"ok": False, "error": "invalid args"}
        result = _send(str(channel_id), str(text))
        gateway_memory.remember_send(PLATFORM, result, channel_id=str(channel_id), text=str(text))
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
