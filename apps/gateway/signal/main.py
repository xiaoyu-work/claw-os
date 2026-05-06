"""Signal gateway app (signal-cli-rest-api backend).

Outbound-only baseline: ``send`` POSTs to ``/v2/send`` of a local
[signal-cli-rest-api](https://github.com/bbernhard/signal-cli-rest-api)
server. Inbound polling (``/v1/receive``) is still a stub.

Why a local REST proxy and not the Signal protocol directly: the
Signal protocol is heavyweight (libsignal, Sealed Sender, Storage
Service v2). The official tooling is signal-cli; signal-cli-rest-api
is a thin HTTP wrapper around it that's the de facto standard for
bot integrations and is trivial to run in Docker.

Credentials needed:
  * ``signal_base_url`` -- e.g. ``http://localhost:8080``
                            (env override: COS_SIGNAL_BASE_URL)
  * ``signal_number``   -- linked account phone in E.164
                            (env override: COS_SIGNAL_NUMBER)

Stdlib only.
"""

from __future__ import annotations

import json
import os
import re
import subprocess
import sys
import urllib.error
import urllib.request


PLATFORM = "signal"
USER_AGENT = "ClawOSSignal/0.1.0"
DEFAULT_BASE_URL = "http://localhost:8080"
SOFT_LEN = 4000  # signal-cli-rest-api passes through; cap for sanity


def _schema() -> dict:
    return {
        "name": f"gateway-{PLATFORM}",
        "version": "0.1.0",
        "description": (
            "Signal gateway via signal-cli-rest-api. ``send`` posts to "
            "/v2/send. ``start``/``stop`` (inbound /v1/receive polling) "
            "are not yet implemented."
        ),
        "commands": {
            "start": {
                "description": "Poll /v1/receive for inbound messages (NOT IMPLEMENTED)",
                "parameters": [],
                "example": "cos app gateway-signal start",
            },
            "stop": {
                "description": "Stop a running gateway (NOT IMPLEMENTED)",
                "parameters": [],
                "example": "cos app gateway-signal stop",
            },
            "status": {
                "description": "Show running state and reachability of the configured base URL",
                "parameters": [],
                "example": "cos app gateway-signal status",
            },
            "send": {
                "description": "Send a text message to one or more recipients",
                "parameters": [
                    {
                        "name": "recipient",
                        "type": "string",
                        "required": True,
                        "description": "Recipient phone in E.164 (e.g. +14155552671) or group id",
                        "kind": "positional",
                    },
                    {
                        "name": "text",
                        "type": "string",
                        "required": True,
                        "description": "Message body (truncated to 4000 chars)",
                        "kind": "positional",
                    },
                ],
                "example": "cos app gateway-signal send '+14155552671' 'hello'",
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


def _env_or_credential(env_var: str, cred_name: str) -> tuple[str | None, str | None]:
    val = os.environ.get(env_var)
    if val and val.strip():
        return val.strip(), None
    return _load_credential(cred_name)


def _load_base_url() -> str:
    val, _ = _env_or_credential("COS_SIGNAL_BASE_URL", "signal_base_url")
    if val:
        return val.rstrip("/")
    return DEFAULT_BASE_URL


def _load_number() -> tuple[str | None, str | None]:
    return _env_or_credential("COS_SIGNAL_NUMBER", "signal_number")


def _truncate(text: str, limit: int = SOFT_LEN) -> str:
    if len(text) <= limit:
        return text
    return text[: limit - 1] + "\u2026"


def _looks_like_group(rid: str) -> bool:
    """Group ids are base64-ish blobs prefixed by `group.` in some
    deployments, or raw long base64 in others. Heuristic: anything
    longer than 20 chars that doesn't start with '+' is a group."""
    rid = rid.strip()
    return not rid.startswith("+") and len(rid) > 20


def _normalize_recipient(rid: str) -> str:
    s = str(rid).strip()
    if not s:
        return ""
    if s.startswith("+"):
        digits = re.sub(r"[^0-9]", "", s)
        return f"+{digits}"
    return s  # group id passes through as-is


def _send(recipient: str, text: str) -> dict:
    if not recipient or not str(recipient).strip():
        return {"ok": False, "error": "recipient required"}
    if not text or not str(text).strip():
        return {"ok": False, "error": "text required"}

    number, err = _load_number()
    if not number:
        return {"ok": False, "error": err or "signal_number required"}

    base_url = _load_base_url()
    rcp = _normalize_recipient(recipient)
    if not rcp:
        return {"ok": False, "error": "recipient invalid"}

    payload: dict = {
        "message": _truncate(str(text)),
        "number": number,
    }
    if _looks_like_group(rcp):
        payload["recipients"] = [rcp]
    else:
        payload["recipients"] = [rcp]
        if not rcp.startswith("+"):
            return {
                "ok": False,
                "platform": PLATFORM,
                "error": f"recipient must be E.164 or group id, got {recipient!r}",
            }

    body = json.dumps(payload).encode("utf-8")
    url = f"{base_url}/v2/send"
    req = urllib.request.Request(
        url,
        data=body,
        method="POST",
        headers={
            "Content-Type": "application/json",
            "User-Agent": USER_AGENT,
            "Accept": "application/json",
        },
    )
    try:
        with urllib.request.urlopen(req, timeout=20) as resp:
            raw = resp.read().decode("utf-8", errors="replace")
            try:
                data = json.loads(raw)
            except json.JSONDecodeError:
                data = {"raw": raw}
            return {
                "ok": True,
                "platform": PLATFORM,
                "recipient": rcp,
                "number": number,
                "base_url": base_url,
                "timestamp": data.get("timestamp") if isinstance(data, dict) else None,
            }
    except urllib.error.HTTPError as e:
        try:
            err_body = e.read().decode("utf-8", errors="replace")
        except Exception:
            err_body = str(e)
        return {
            "ok": False,
            "platform": PLATFORM,
            "base_url": base_url,
            "error": f"HTTP {e.code}: {err_body}",
        }
    except urllib.error.URLError as e:
        return {
            "ok": False,
            "platform": PLATFORM,
            "base_url": base_url,
            "error": f"URL error: {e}",
        }


def _not_yet(command: str) -> dict:
    return {
        "ok": False,
        "platform": PLATFORM,
        "command": command,
        "status": "not_yet_implemented",
        "note": (
            "Inbound /v1/receive polling still pending. "
            "Use ``send <recipient> <text>`` for outbound until then."
        ),
    }


def _status() -> dict:
    number, err = _load_number()
    base_url = _load_base_url()
    return {
        "ok": True,
        "platform": PLATFORM,
        "running": False,
        "configured": number is not None,
        "base_url": base_url,
        "number": number,
        "config_error": err,
        "note": "Outbound-only mode. /v1/receive polling not yet implemented.",
    }


def run(command: str, args):
    if command == "__schema__":
        return _schema()
    if command == "send":
        if isinstance(args, list):
            recipient = args[0] if len(args) > 0 else ""
            text = args[1] if len(args) > 1 else ""
        elif isinstance(args, dict):
            recipient = args.get("recipient", "")
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
