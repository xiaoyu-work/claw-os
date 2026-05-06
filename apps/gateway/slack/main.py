"""Slack gateway scaffold.

Phase 9 placeholder. Mirrors apps/gateway/telegram/main.py for
Slack. Real implementation will use Socket Mode (preferred for
self-hosted) or HTTP Events.
"""

from __future__ import annotations

import json
import os
import sys


PLATFORM = "slack"


def _schema():
    return {
        "name": f"gateway-{PLATFORM}",
        "version": "0.1.0-scaffold",
        "description": "Slack bot gateway (scaffold)",
        "commands": {
            "start": {
                "description": "Start the gateway (Socket Mode preferred)",
                "parameters": [],
                "example": "cos app gateway-slack start",
            },
            "stop": {
                "description": "Stop a running gateway",
                "parameters": [],
                "example": "cos app gateway-slack stop",
            },
            "status": {
                "description": "Show running state",
                "parameters": [],
                "example": "cos app gateway-slack status",
            },
            "send": {
                "description": "Send a message to a channel",
                "parameters": [
                    {"name": "channel_id", "type": "string", "required": True, "description": "Target channel id", "kind": "positional"},
                    {"name": "text", "type": "string", "required": True, "description": "Message text", "kind": "positional"},
                ],
                "example": "cos app gateway-slack send C123 'hello'",
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
            "slack_bot_token` + `slack_app_token`, then implement Socket Mode here."
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
