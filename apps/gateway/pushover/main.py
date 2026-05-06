"""Pushover (pushover.net) push-notification gateway.

Outbound-only. ``send`` POSTs to ``https://api.pushover.net/1/messages.json``.

Pushover notifications go to a single user-key or a group-key (which
fans out to all members). Both the *application* token (identifies the
sender) and the *user / group* key (identifies the recipient) are
required. The user key can be overridden per-call by passing
``recipient`` as a positional argument; otherwise the default
``pushover_user_key`` credential is used.

Optional fields surfaced:

  * ``title``        -- notification title (default: app's name)
  * ``priority``     -- -2 .. 2 (2 = emergency, requires retry/expire)
  * ``sound``        -- one of pushover's named sounds (e.g. "magic")
  * ``url`` / ``url_title`` -- supplementary action URL
  * ``device``       -- comma-separated device names (default: all)
  * ``html``         -- 1 to enable a small HTML subset in message body
  * ``ttl``          -- seconds before auto-deletion on the device
  * ``retry`` / ``expire`` -- required when ``priority=2``

Credentials needed:
  * ``pushover_app_token`` -- application API token
                               (env override: COS_PUSHOVER_APP_TOKEN)
  * ``pushover_user_key``  -- default user / group key
                               (env override: COS_PUSHOVER_USER_KEY)

Stdlib only.
"""

from __future__ import annotations

import json
import os
import subprocess
import sys
import urllib.error
import urllib.parse
import urllib.request


PLATFORM = "pushover"
USER_AGENT = "ClawOSPushover/0.1.0"
SOFT_LEN = 1024  # Pushover hard cap is 1024 chars
TITLE_LEN = 250
URL_LEN = 512
URL_TITLE_LEN = 100
API_URL = "https://api.pushover.net/1/messages.json"


def _schema() -> dict:
    return {
        "name": f"gateway-{PLATFORM}",
        "version": "0.1.0",
        "description": (
            "Pushover (pushover.net) push-notification gateway. "
            "``send`` posts a message to the configured user / group "
            "key with optional title, priority, sound, and URL action."
        ),
        "commands": {
            "start": {
                "description": "Receive inbound messages (NOT IMPLEMENTED)",
                "parameters": [],
                "example": "cos app gateway-pushover start",
            },
            "stop": {
                "description": "Stop a running gateway (NOT IMPLEMENTED)",
                "parameters": [],
                "example": "cos app gateway-pushover stop",
            },
            "status": {
                "description": "Show whether the app token and default user key are configured",
                "parameters": [],
                "example": "cos app gateway-pushover status",
            },
            "send": {
                "description": "Send a push notification to a user or group",
                "parameters": [
                    {
                        "name": "text",
                        "type": "string",
                        "required": True,
                        "description": "Message body (truncated to 1024 chars)",
                        "kind": "positional",
                    },
                    {
                        "name": "recipient",
                        "type": "string",
                        "required": False,
                        "description": "User or group key to receive (default: pushover_user_key credential)",
                    },
                    {
                        "name": "title",
                        "type": "string",
                        "required": False,
                        "description": "Notification title (default: app name)",
                    },
                    {
                        "name": "priority",
                        "type": "integer",
                        "required": False,
                        "description": "-2 .. 2 (lowest .. emergency)",
                    },
                    {
                        "name": "sound",
                        "type": "string",
                        "required": False,
                        "description": "Pushover named sound (e.g. magic, siren, none)",
                    },
                    {
                        "name": "url",
                        "type": "string",
                        "required": False,
                        "description": "Supplementary action URL (max 512 chars)",
                    },
                    {
                        "name": "url_title",
                        "type": "string",
                        "required": False,
                        "description": "Title for the action URL (max 100 chars)",
                    },
                    {
                        "name": "device",
                        "type": "string",
                        "required": False,
                        "description": "Comma-separated device names to target",
                    },
                    {
                        "name": "html",
                        "type": "boolean",
                        "required": False,
                        "description": "Enable Pushover's HTML subset in the body",
                    },
                    {
                        "name": "ttl",
                        "type": "integer",
                        "required": False,
                        "description": "Seconds before auto-deletion on the device",
                    },
                    {
                        "name": "retry",
                        "type": "integer",
                        "required": False,
                        "description": "Seconds between retries (priority=2 only; >=30)",
                    },
                    {
                        "name": "expire",
                        "type": "integer",
                        "required": False,
                        "description": "Seconds before retries expire (priority=2 only; <=10800)",
                    },
                ],
                "example": "cos app gateway-pushover send 'build green'",
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


def _truncate(text: str, limit: int) -> str:
    if len(text) <= limit:
        return text
    return text[: limit - 1] + "\u2026"


def _coerce_int(value, name: str) -> tuple[int | None, str | None]:
    if value is None or value == "":
        return None, None
    try:
        return int(value), None
    except (TypeError, ValueError):
        return None, f"{name} must be an integer (got {value!r})"


def _send(
    text: str,
    *,
    recipient: str | None = None,
    title: str | None = None,
    priority: int | None = None,
    sound: str | None = None,
    url: str | None = None,
    url_title: str | None = None,
    device: str | None = None,
    html: bool = False,
    ttl: int | None = None,
    retry: int | None = None,
    expire: int | None = None,
) -> dict:
    if not text or not str(text).strip():
        return {"ok": False, "error": "text required"}

    app_token, app_err = _env_or_credential("COS_PUSHOVER_APP_TOKEN", "pushover_app_token")
    if not app_token:
        return {"ok": False, "error": app_err or "pushover_app_token required"}

    user_key = recipient
    if not user_key:
        user_key, user_err = _env_or_credential("COS_PUSHOVER_USER_KEY", "pushover_user_key")
        if not user_key:
            return {"ok": False, "error": user_err or "pushover_user_key required"}

    body_text = _truncate(str(text), SOFT_LEN)

    fields: dict[str, str] = {
        "token": app_token,
        "user": user_key,
        "message": body_text,
    }
    if title:
        fields["title"] = _truncate(str(title), TITLE_LEN)
    if priority is not None:
        if priority < -2 or priority > 2:
            return {"ok": False, "error": "priority must be -2..2"}
        fields["priority"] = str(priority)
        if priority == 2:
            if retry is None or expire is None:
                return {
                    "ok": False,
                    "error": "priority=2 requires both --retry and --expire",
                }
            if retry < 30:
                return {"ok": False, "error": "retry must be >=30"}
            if expire > 10800:
                return {"ok": False, "error": "expire must be <=10800"}
            fields["retry"] = str(retry)
            fields["expire"] = str(expire)
    if sound:
        fields["sound"] = str(sound)
    if url:
        fields["url"] = _truncate(str(url), URL_LEN)
    if url_title:
        fields["url_title"] = _truncate(str(url_title), URL_TITLE_LEN)
    if device:
        fields["device"] = str(device)
    if html:
        fields["html"] = "1"
    if ttl is not None:
        fields["ttl"] = str(ttl)

    body = urllib.parse.urlencode(fields).encode("utf-8")
    req = urllib.request.Request(
        API_URL,
        data=body,
        method="POST",
        headers={
            "Content-Type": "application/x-www-form-urlencoded",
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
            status = data.get("status") if isinstance(data, dict) else None
            return {
                "ok": status == 1,
                "platform": PLATFORM,
                "request_id": data.get("request") if isinstance(data, dict) else None,
                "status": status,
                "errors": data.get("errors") if isinstance(data, dict) else None,
                "receipt": data.get("receipt") if isinstance(data, dict) else None,
            }
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
            "error": f"URL error: {e}",
        }


def _not_yet(command: str) -> dict:
    return {
        "ok": False,
        "platform": PLATFORM,
        "command": command,
        "status": "not_yet_implemented",
        "note": (
            "Pushover doesn't deliver inbound messages to apps. Use "
            "``send <text>`` for outbound notifications only."
        ),
    }


def _status() -> dict:
    app_token, app_err = _env_or_credential("COS_PUSHOVER_APP_TOKEN", "pushover_app_token")
    user_key, user_err = _env_or_credential("COS_PUSHOVER_USER_KEY", "pushover_user_key")
    return {
        "ok": True,
        "platform": PLATFORM,
        "running": False,
        "configured": app_token is not None and user_key is not None,
        "config_error": app_err or user_err,
        "note": "Outbound-only via Pushover REST API (https://api.pushover.net/1/messages.json).",
    }


def run(command: str, args):
    if command == "__schema__":
        return _schema()
    if command == "send":
        text = ""
        recipient = None
        title = None
        priority: int | None = None
        sound = None
        url = None
        url_title = None
        device = None
        html = False
        ttl: int | None = None
        retry: int | None = None
        expire: int | None = None
        if isinstance(args, list):
            if args:
                text = str(args[0])
        elif isinstance(args, dict):
            text = str(args.get("text", "") or "")
            recipient = args.get("recipient") or None
            title = args.get("title")
            sound = args.get("sound")
            url = args.get("url")
            url_title = args.get("url_title")
            device = args.get("device")
            html = bool(args.get("html", False))
            for name, val in (
                ("priority", args.get("priority")),
                ("ttl", args.get("ttl")),
                ("retry", args.get("retry")),
                ("expire", args.get("expire")),
            ):
                parsed, err = _coerce_int(val, name)
                if err:
                    return {"ok": False, "error": err}
                if name == "priority":
                    priority = parsed
                elif name == "ttl":
                    ttl = parsed
                elif name == "retry":
                    retry = parsed
                elif name == "expire":
                    expire = parsed
        else:
            return {"ok": False, "error": "invalid args"}
        return _send(
            text,
            recipient=str(recipient) if recipient else None,
            title=str(title) if title else None,
            priority=priority,
            sound=str(sound) if sound else None,
            url=str(url) if url else None,
            url_title=str(url_title) if url_title else None,
            device=str(device) if device else None,
            html=html,
            ttl=ttl,
            retry=retry,
            expire=expire,
        )
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
