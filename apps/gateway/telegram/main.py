"""Telegram gateway scaffold.

Phase 9 placeholder. Exposes the cos agent as a Telegram bot.
The real implementation will:

  * Read the bot token from `cos credential load telegram_bot_token`.
  * Long-poll the Telegram Bot API or run an HTTPS webhook (cos
    network-app HTTP server).
  * Forward each inbound message to the agent runtime and reply
    with the response, streaming if possible.
  * Persist message-id ↔ session-id mappings in cos kv so multi-turn
    conversations survive restarts.

For now `start` / `stop` / `status` / `send` return a "not yet
implemented" payload so the cos app loader sees a working app.
"""

from __future__ import annotations

import json
import os
import sys


PLATFORM = "telegram"


def _schema():
    return {
        "name": f"gateway-{PLATFORM}",
        "version": "0.1.0-scaffold",
        "description": "Telegram bot gateway (scaffold)",
        "commands": {
            "start": {
                "description": "Start the gateway long-poll / webhook loop",
                "parameters": [],
                "example": "cos app gateway-telegram start",
            },
            "stop": {
                "description": "Stop a running gateway",
                "parameters": [],
                "example": "cos app gateway-telegram stop",
            },
            "status": {
                "description": "Show running state",
                "parameters": [],
                "example": "cos app gateway-telegram status",
            },
            "send": {
                "description": "Send a message to a chat",
                "parameters": [
                    {"name": "chat_id", "type": "string", "required": True, "description": "Target chat id", "kind": "positional"},
                    {"name": "text", "type": "string", "required": True, "description": "Message text", "kind": "positional"},
                ],
                "example": "cos app gateway-telegram send 12345 'hello'",
            },
        },
    }


def _stub(command, args):
    return {
        "ok": False,
        "platform": PLATFORM,
        "command": command,
        "args": args,
        "status": "not_yet_implemented",
        "note": (
            "Phase 9 scaffold. Wire credentials via `cos credential store "
            "telegram_bot_token`, then implement the long-poll loop here."
        ),
    }


def run(command, args):
    if command == "__schema__":
        return _schema()
    if command not in {"start", "stop", "status", "send"}:
        return {"error": f"unknown command: {command}"}
    return _stub(command, args)


if __name__ == "__main__":
    cmd = os.environ.get("COS_COMMAND") or (sys.argv[1] if len(sys.argv) > 1 else "")
    raw_args = os.environ.get("COS_ARGS_JSON")
    if raw_args:
        args = json.loads(raw_args)
    else:
        args = sys.argv[2:]
    print(json.dumps(run(cmd, args)))
