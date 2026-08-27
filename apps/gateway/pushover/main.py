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
import sys
import urllib.error
import urllib.parse


# Sibling ``_shared`` package import (script-mode invocation).
sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

from _shared import gateway_args, gateway_memory, safe_egress, safe_subprocess  # noqa: E402


PLATFORM = "pushover"
USER_AGENT = "ClawOSPushover/0.1.0"
SOFT_LEN = 1024  # Pushover hard cap is 1024 chars
TITLE_LEN = 250
URL_LEN = 512
URL_TITLE_LEN = 100
API_URL = "https://api.pushover.net/1/messages.json"


def _load_credential(name: str) -> tuple[str | None, str | None]:
    return safe_subprocess.safe_credential_load(name)


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
    headers = {
        "Content-Type": "application/x-www-form-urlencoded",
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
            verb_id="net.dial",
        )
        raw = raw_resp.decode("utf-8", errors="replace")
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
        return {
            "ok": False,
            "platform": PLATFORM,
            "error": f"URL error: {e.reason}",
        }
    except Exception as e:
        denial = getattr(e, "denial", None)
        if denial is not None:
            return {"ok": False, "platform": PLATFORM, "error": "permission denied", "denial": denial}
        raise


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
            parsed_args, error = gateway_args.parse(
                args,
                positional=("text",),
                value_flags=(
                    "recipient",
                    "title",
                    "priority",
                    "sound",
                    "url",
                    "url-title",
                    "device",
                    "ttl",
                    "retry",
                    "expire",
                ),
                bool_flags=("html",),
            )
            if error:
                return {"ok": False, "error": error}
            text = parsed_args["text"]
            recipient = parsed_args["recipient"]
            title = parsed_args["title"]
            sound = parsed_args["sound"]
            url = parsed_args["url"]
            url_title = parsed_args["url-title"]
            device = parsed_args["device"]
            html = parsed_args["html"]
            for name in ("priority", "ttl", "retry", "expire"):
                parsed, error = _coerce_int(parsed_args[name], name)
                if error:
                    return {"ok": False, "error": error}
                if name == "priority":
                    priority = parsed
                elif name == "ttl":
                    ttl = parsed
                elif name == "retry":
                    retry = parsed
                else:
                    expire = parsed
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
        result = _send(
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
        gateway_memory.remember_send(
            PLATFORM,
            result,
            channel_id=str(recipient) if recipient else "",
            text=text,
        )
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
