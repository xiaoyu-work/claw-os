"""Matrix gateway app.

Outbound-only baseline: ``send`` PUTs an ``m.room.message`` event to
the Matrix Client-Server API at
``/_matrix/client/v3/rooms/{room_id}/send/m.room.message/{txn}``.
Inbound (``/sync`` long-poll) is still a stub.

Credentials needed:
  * ``matrix_access_token`` — bearer token for the bot/user account
  * ``matrix_homeserver``   — base URL, e.g. ``https://matrix.org``
                              (no trailing slash). Optional env
                              override: ``COS_MATRIX_HOMESERVER``.

Stdlib only.
"""

from __future__ import annotations

import json
import os
import sys
import time
import urllib.error
import urllib.parse
import uuid


# Sibling ``_shared`` package import (script-mode invocation).
sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

from _shared import gateway_memory, safe_egress, safe_subprocess  # noqa: E402


PLATFORM = "matrix"
USER_AGENT = "ClawOSMatrix/0.1.0 (+https://github.com/clawos/cos)"
DEFAULT_HOMESERVER = "https://matrix.org"
SOFT_LEN = 4000  # Matrix has no hard cap on body, but keep replies sane


def _schema() -> dict:
    return {
        "name": f"gateway-{PLATFORM}",
        "version": "0.1.0",
        "description": (
            "Matrix gateway. ``send`` posts text to a room via the "
            "Client-Server REST API. ``start``/``stop`` (sync loop) "
            "are not yet implemented."
        ),
        "commands": {
            "start": {
                "description": "Connect to a Matrix homeserver and stream events (NOT IMPLEMENTED)",
                "parameters": [],
                "example": "cos app gateway-matrix start",
            },
            "stop": {
                "description": "Stop a running gateway (NOT IMPLEMENTED)",
                "parameters": [],
                "example": "cos app gateway-matrix stop",
            },
            "status": {
                "description": "Show running state (always 'stopped' until /sync lands)",
                "parameters": [],
                "example": "cos app gateway-matrix status",
            },
            "send": {
                "description": "Send a text message to a Matrix room",
                "parameters": [
                    {
                        "name": "room_id",
                        "type": "string",
                        "required": True,
                        "description": "Target room id (e.g. !abcdef:matrix.org) or alias",
                        "kind": "positional",
                    },
                    {
                        "name": "text",
                        "type": "string",
                        "required": True,
                        "description": "Plain-text message body (truncated to 4000 chars)",
                        "kind": "positional",
                    },
                ],
                "example": "cos app gateway-matrix send '!abcdef:matrix.org' 'hello'",
            },
        },
    }


def _load_credential(name: str) -> tuple[str | None, str | None]:
    """Load a single named credential via `cos credential load`."""
    return safe_subprocess.safe_credential_load(name)


def _load_token() -> tuple[str | None, str | None]:
    env_tok = os.environ.get("COS_MATRIX_TOKEN")
    if env_tok:
        return env_tok.strip(), None
    return _load_credential("matrix_access_token")


def _load_homeserver() -> str:
    env_hs = os.environ.get("COS_MATRIX_HOMESERVER")
    if env_hs and env_hs.strip():
        return env_hs.strip().rstrip("/")
    val, _err = _load_credential("matrix_homeserver")
    if val:
        return val.rstrip("/")
    return DEFAULT_HOMESERVER


def _truncate(text: str, limit: int = SOFT_LEN) -> str:
    if len(text) <= limit:
        return text
    return text[: limit - 1] + "\u2026"


def _txn_id() -> str:
    """Matrix transaction id — must be unique per access token."""
    return f"cos-{int(time.time() * 1000)}-{uuid.uuid4().hex[:8]}"


def _send(room_id: str, text: str) -> dict:
    if not room_id or not room_id.strip():
        return {"ok": False, "error": "room_id required"}
    if not text or not str(text).strip():
        return {"ok": False, "error": "text required"}
    token, err = _load_token()
    if not token:
        return {"ok": False, "error": err or "no token"}

    homeserver = _load_homeserver()
    txn = _txn_id()

    body = json.dumps({"msgtype": "m.text", "body": _truncate(str(text))}).encode("utf-8")
    # Path-escape the room id (e.g. ! and : are reserved).
    encoded_room = urllib.parse.quote(room_id, safe="")
    url = (
        f"{homeserver}/_matrix/client/v3/rooms/{encoded_room}"
        f"/send/m.room.message/{txn}"
    )
    headers = {
        "Authorization": f"Bearer {token}",
        "Content-Type": "application/json",
        "User-Agent": USER_AGENT,
    }
    try:
        _, _, raw_resp = safe_egress.safe_urlopen(
            "PUT",
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
            "room_id": room_id,
            "event_id": data.get("event_id") if isinstance(data, dict) else None,
            "homeserver": homeserver,
        }
    except safe_egress.EgressBlocked as e:
        return {
            "ok": False,
            "platform": PLATFORM,
            "homeserver": homeserver,
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
            "homeserver": homeserver,
            "error": f"HTTP {e.code}: {err_body}",
        }
    except urllib.error.URLError as e:
        return {
            "ok": False,
            "platform": PLATFORM,
            "homeserver": homeserver,
            "error": f"URL error: {e.reason}",
        }
    except Exception as e:
        denial = getattr(e, "denial", None)
        if denial is not None:
            return {
                "ok": False,
                "platform": PLATFORM,
                "homeserver": homeserver,
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
            "Matrix /sync long-poll loop still pending. "
            "Use ``send <room_id> <text>`` for outbound until then."
        ),
    }


def _status() -> dict:
    return {
        "ok": True,
        "platform": PLATFORM,
        "running": False,
        "homeserver": _load_homeserver(),
        "note": "Outbound-only mode. /sync loop not yet implemented.",
    }


def run(command: str, args):
    if command == "__schema__":
        return _schema()
    if command == "send":
        if isinstance(args, list):
            room_id = args[0] if len(args) > 0 else ""
            text = args[1] if len(args) > 1 else ""
        elif isinstance(args, dict):
            room_id = args.get("room_id", "")
            text = args.get("text", "")
        else:
            return {"ok": False, "error": "invalid args"}
        result = _send(str(room_id), str(text))
        gateway_memory.remember_send(PLATFORM, result, channel_id=str(room_id), text=str(text))
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
