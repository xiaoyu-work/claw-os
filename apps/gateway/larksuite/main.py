"""Lark / Feishu (飞书) custom-bot gateway.

Outbound-only baseline. ``send`` POSTs to the bot's webhook URL.
The bot URL is provisioned per chat in the Lark / Feishu admin
(Group settings → Bots → Add custom bot).

Two security modes:

  1. **Plain** -- bare URL. Bot must be configured with keyword
     filtering or IP whitelist on Lark side.
  2. **Sign** (HMAC-SHA256) -- if ``lark_secret`` is configured we
     include ``timestamp`` and ``sign`` fields in the request body.
     The sign scheme follows Lark's docs: HMAC-SHA256 over
     ``<timestamp>\\n<secret>`` (key = secret, message = the same
     string), then base64 the digest.

Body shape:

  * Default: ``msg_type=text`` with ``content.text`` (plain).
  * ``--post``: ``msg_type=post`` with a single-paragraph rich text
    block (one ``post.zh_cn.title`` + one paragraph of text).
  * ``--card``: pass a fully-formed JSON card body via
    ``--card-json <json>``. We don't validate the card shape; Lark
    will return errcode != 0 on malformed card.

Credentials needed:
  * ``lark_webhook_url`` -- custom-bot webhook URL
                             (env override: COS_LARK_WEBHOOK_URL)
  * ``lark_secret``      -- optional HMAC-SHA256 secret
                             (env override: COS_LARK_SECRET)

Stdlib only.
"""

from __future__ import annotations

import base64
import hashlib
import hmac
import json
import os
import sys
import time
import urllib.error


# Sibling ``_shared`` package import (script-mode invocation).
sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

from _shared import gateway_memory, safe_egress, safe_subprocess  # noqa: E402


PLATFORM = "larksuite"
USER_AGENT = "ClawOSLark/0.1.0"
SOFT_LEN = 30000  # Lark messages cap around 30 KB
DEFAULT_TITLE = "ClawOS notice"


def _load_credential(name: str) -> tuple[str | None, str | None]:
    return safe_subprocess.safe_credential_load(name)


def _env_or_credential(env_var: str, cred_name: str) -> tuple[str | None, str | None]:
    val = os.environ.get(env_var)
    if val and val.strip():
        return val.strip(), None
    return _load_credential(cred_name)


def _truncate(text: str, limit: int = SOFT_LEN) -> str:
    if len(text) <= limit:
        return text
    return text[: limit - 1] + "\u2026"


def _sign(timestamp: str, secret: str) -> str:
    """Compute Lark sign: base64(HMAC-SHA256(key=secret, msg=<ts>\\n<secret>))."""
    payload = f"{timestamp}\n{secret}".encode("utf-8")
    digest = hmac.new(secret.encode("utf-8"), payload, hashlib.sha256).digest()
    return base64.b64encode(digest).decode("utf-8")


def _send(
    text: str,
    *,
    post: bool = False,
    title: str | None = None,
    card: bool = False,
    card_json: str | None = None,
) -> dict:
    if not text and not card:
        return {"ok": False, "error": "text required"}
    if card and not card_json:
        return {"ok": False, "error": "--card-json required when --card is set"}

    url, err = _env_or_credential("COS_LARK_WEBHOOK_URL", "lark_webhook_url")
    if not url:
        return {"ok": False, "error": err or "lark_webhook_url required"}

    payload: dict
    if card:
        try:
            card_obj = json.loads(card_json)  # type: ignore[arg-type]
        except json.JSONDecodeError as e:
            return {"ok": False, "error": f"--card-json invalid: {e}"}
        payload = {"msg_type": "interactive", "card": card_obj}
        kind = "interactive"
    elif post:
        body_text = _truncate(str(text))
        post_title = title or body_text.splitlines()[0][:50] or DEFAULT_TITLE
        payload = {
            "msg_type": "post",
            "content": {
                "post": {
                    "zh_cn": {
                        "title": post_title,
                        "content": [[{"tag": "text", "text": body_text}]],
                    }
                }
            },
        }
        kind = "post"
    else:
        body_text = _truncate(str(text))
        payload = {"msg_type": "text", "content": {"text": body_text}}
        kind = "text"

    secret, _ = _env_or_credential("COS_LARK_SECRET", "lark_secret")
    if secret:
        ts = str(int(time.time()))
        payload["timestamp"] = ts
        payload["sign"] = _sign(ts, secret)

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
        try:
            data = json.loads(raw)
        except json.JSONDecodeError:
            data = {"raw": raw}
        code = data.get("code") if isinstance(data, dict) else None
        msg = data.get("msg") if isinstance(data, dict) else None
        ok = code == 0
        return {
            "ok": ok,
            "platform": PLATFORM,
            "kind": kind,
            "signed": bool(secret),
            "code": code,
            "msg": msg,
        }
    except safe_egress.EgressBlocked as e:
        return {
            "ok": False,
            "platform": PLATFORM,
            "kind": kind,
            "signed": bool(secret),
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
            "kind": kind,
            "signed": bool(secret),
            "error": f"HTTP {e.code}: {err_body}",
        }
    except urllib.error.URLError as e:
        return {
            "ok": False,
            "platform": PLATFORM,
            "kind": kind,
            "signed": bool(secret),
            "error": f"URL error: {e.reason}",
        }
    except Exception as e:
        denial = getattr(e, "denial", None)
        if denial is not None:
            return {
                "ok": False,
                "platform": PLATFORM,
                "kind": kind,
                "signed": bool(secret),
                "error": "permission denied",
                "denial": denial,
            }
        raise


def _status() -> dict:
    url, url_err = _env_or_credential("COS_LARK_WEBHOOK_URL", "lark_webhook_url")
    secret, _ = _env_or_credential("COS_LARK_SECRET", "lark_secret")
    return {
        "ok": True,
        "platform": PLATFORM,
        "running": False,
        "configured": url is not None,
        "config_error": url_err,
        "signed": bool(secret),
        "note": "Outbound-only via Lark / Feishu custom-bot webhook (Sign / Keyword / IP modes).",
    }


def run(command: str, args):
    if command == "send":
        text = ""
        post = False
        title = None
        card = False
        card_json = None
        if isinstance(args, list):
            if args:
                text = str(args[0])
        elif isinstance(args, dict):
            text = str(args.get("text", "") or "")
            post = bool(args.get("post", False))
            title = args.get("title")
            card = bool(args.get("card", False))
            cj = args.get("card_json")
            card_json = json.dumps(cj) if isinstance(cj, (dict, list)) else (
                str(cj) if cj is not None else None
            )
        else:
            return {"ok": False, "error": "invalid args"}
        result = _send(
            text,
            post=post,
            title=title,
            card=card,
            card_json=card_json,
        )
        gateway_memory.remember_send(PLATFORM, result, channel_id="", text=text)
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
