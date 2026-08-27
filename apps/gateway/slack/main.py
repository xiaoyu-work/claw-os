"""Slack gateway app.

Outbound-only baseline: ``send`` POSTs a message via Slack Web API
``chat.postMessage`` using a bot token (``xoxb-...``).  Inbound (Socket
Mode / Events HTTP) is still a stub - landing it requires real
WebSocket / HTTP server plumbing.  Stdlib only.

Credentials: ``cos credential store slack_bot_token`` (or env
``COS_SLACK_TOKEN``).
"""

from __future__ import annotations

import json
import os
import sys
import urllib.error


# Sibling ``_shared`` package import (script-mode invocation).
sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

from _shared import gateway_memory, safe_egress, safe_subprocess  # noqa: E402


PLATFORM = "slack"
SLACK_API = "https://slack.com/api"
USER_AGENT = "cos-gateway-slack/0.2.0"
# Slack hard limit per chat.postMessage text field ~40k chars; keep a
# conservative cap that still covers normal agent replies cleanly.
MAX_LEN = 4000


def _load_token() -> tuple[str | None, str | None]:
    """Returns (token, error)."""
    env_tok = os.environ.get("COS_SLACK_TOKEN")
    if env_tok:
        return env_tok.strip(), None
    return safe_subprocess.safe_credential_load("slack_bot_token")


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

    body = json.dumps({"channel": channel_id, "text": _truncate(str(text))}).encode("utf-8")
    headers = {
        "Authorization": f"Bearer {token}",
        "Content-Type": "application/json; charset=utf-8",
        "User-Agent": USER_AGENT,
    }
    try:
        _, _, raw_resp = safe_egress.safe_urlopen(
            "POST",
            f"{SLACK_API}/chat.postMessage",
            headers=headers,
            body=body,
            timeout=15,
            verb_id="net.dial",
        )
        raw = raw_resp.decode("utf-8", errors="replace")
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

    try:
        data = json.loads(raw)
    except json.JSONDecodeError:
        return {"ok": False, "platform": PLATFORM, "error": f"non-JSON response: {raw}"}

    if not isinstance(data, dict) or not data.get("ok"):
        # Slack always returns 200 OK and signals errors via the JSON
        # ``ok`` field plus an ``error`` string. Surface that even on
        # HTTP success so callers don't think the post succeeded.
        return {
            "ok": False,
            "platform": PLATFORM,
            "error": data.get("error", "unknown") if isinstance(data, dict) else "unknown",
            "response": data,
        }

    return {
        "ok": True,
        "platform": PLATFORM,
        "channel_id": channel_id,
        "ts": data.get("ts"),
    }


def _status() -> dict:
    return {
        "ok": True,
        "platform": PLATFORM,
        "running": False,
        "note": "Outbound-only mode. Socket Mode / Events HTTP not yet implemented.",
    }


def run(command: str, args):
    from canonical_argv import normalize_canonical_argv
    if isinstance(args, list):
        args = normalize_canonical_argv(args)
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
    return {"ok": False, "error": f"unknown command: {command}"}


if __name__ == "__main__":
    cmd = os.environ.get("COS_COMMAND") or (sys.argv[1] if len(sys.argv) > 1 else "")
    raw_args = os.environ.get("COS_ARGS_JSON")
    if raw_args:
        parsed_args = json.loads(raw_args)
    else:
        parsed_args = sys.argv[2:]
    print(json.dumps(run(cmd, parsed_args)))
