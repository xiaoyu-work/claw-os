"""DingTalk (钉钉) custom-robot gateway.

Outbound-only baseline. ``send`` POSTs to a custom-robot webhook URL.
The robot URL is provisioned per group chat in the DingTalk app
(Group settings → Robots → Add → Custom robot).

Three robot security modes are supported:

  1. **Plain** -- bare URL, no extras. Robot must be configured with
     keyword filtering or IP whitelist on the DingTalk side.
  2. **Sign** (HMAC-SHA256) -- if ``dingtalk_secret`` is configured we
     sign every request with the standard
     ``timestamp + '\\n' + secret`` HMAC scheme and append
     ``&timestamp=<ms>&sign=<urlencoded-base64>`` to the webhook URL.
  3. **Keyword** -- supply ``--keyword`` (or set ``COS_DINGTALK_KEYWORD``)
     to prepend a required keyword to the message body so the robot
     accepts it. Useful when the robot is configured with keyword
     filtering instead of sign.

Body shape:

  * Default: ``msgtype=text`` with ``text.content`` (plain).
  * ``--markdown`` switches to ``msgtype=markdown`` with
    ``markdown.title`` (defaults to first line of body) and
    ``markdown.text``.
  * ``--at-mobiles`` and ``--at-user-ids`` mention specific users;
    ``--at-all`` mentions everyone.

Credentials needed:
  * ``dingtalk_webhook_url`` -- custom-robot webhook URL
                                 (env override: COS_DINGTALK_WEBHOOK_URL)
  * ``dingtalk_secret``       -- optional HMAC-SHA256 secret
                                 (env override: COS_DINGTALK_SECRET)

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
import urllib.parse


# Sibling ``_shared`` package import (script-mode invocation).
sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

from _shared import gateway_memory, safe_egress, safe_subprocess  # noqa: E402


PLATFORM = "dingtalk"
USER_AGENT = "ClawOSDingTalk/0.1.0"
SOFT_LEN = 19000  # DingTalk caps message bodies around 20 KB
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


def _split_csv(value) -> list[str]:
    if value is None:
        return []
    if isinstance(value, list):
        items = [str(x) for x in value]
    else:
        items = str(value).split(",")
    out = []
    for item in items:
        s = item.strip()
        if s:
            out.append(s)
    return out


def _sign_url(base_url: str, secret: str) -> str:
    """Append ``&timestamp=<ms>&sign=<base64>`` to ``base_url`` per
    DingTalk's signed-robot scheme.
    """
    timestamp = str(int(time.time() * 1000))
    payload = f"{timestamp}\n{secret}".encode("utf-8")
    digest = hmac.new(secret.encode("utf-8"), payload, hashlib.sha256).digest()
    sign = urllib.parse.quote_plus(base64.b64encode(digest))
    sep = "&" if "?" in base_url else "?"
    return f"{base_url}{sep}timestamp={timestamp}&sign={sign}"


def _send(
    text: str,
    *,
    markdown: bool = False,
    title: str | None = None,
    keyword: str | None = None,
    at_mobiles: list[str] | None = None,
    at_user_ids: list[str] | None = None,
    at_all: bool = False,
) -> dict:
    if not text or not str(text).strip():
        return {"ok": False, "error": "text required"}

    url, err = _env_or_credential("COS_DINGTALK_WEBHOOK_URL", "dingtalk_webhook_url")
    if not url:
        return {"ok": False, "error": err or "dingtalk_webhook_url required"}

    body_text = _truncate(str(text))

    # If a keyword was supplied (or env), prepend it so the robot accepts.
    keyword = keyword or os.environ.get("COS_DINGTALK_KEYWORD") or None
    if keyword:
        body_text = f"{keyword.strip()}\n{body_text}"

    payload: dict
    if markdown:
        md_title = title or body_text.splitlines()[0][:50] or DEFAULT_TITLE
        payload = {
            "msgtype": "markdown",
            "markdown": {"title": md_title, "text": body_text},
        }
    else:
        payload = {
            "msgtype": "text",
            "text": {"content": body_text},
        }

    if at_mobiles or at_user_ids or at_all:
        payload["at"] = {
            "atMobiles": at_mobiles or [],
            "atUserIds": at_user_ids or [],
            "isAtAll": bool(at_all),
        }

    secret, _secret_err = _env_or_credential("COS_DINGTALK_SECRET", "dingtalk_secret")
    final_url = _sign_url(url, secret) if secret else url

    body = json.dumps(payload).encode("utf-8")
    headers = {
        "Content-Type": "application/json",
        "User-Agent": USER_AGENT,
        "Accept": "application/json",
    }
    try:
        _, _, raw_resp = safe_egress.safe_urlopen(
            "POST",
            final_url,
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
        errcode = data.get("errcode") if isinstance(data, dict) else None
        errmsg = data.get("errmsg") if isinstance(data, dict) else None
        ok = errcode == 0
        return {
            "ok": ok,
            "platform": PLATFORM,
            "kind": "markdown" if markdown else "text",
            "signed": bool(secret),
            "errcode": errcode,
            "errmsg": errmsg,
        }
    except safe_egress.EgressBlocked as e:
        return {
            "ok": False,
            "platform": PLATFORM,
            "kind": "markdown" if markdown else "text",
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
            "kind": "markdown" if markdown else "text",
            "signed": bool(secret),
            "error": f"HTTP {e.code}: {err_body}",
        }
    except urllib.error.URLError as e:
        return {
            "ok": False,
            "platform": PLATFORM,
            "kind": "markdown" if markdown else "text",
            "signed": bool(secret),
            "error": f"URL error: {e.reason}",
        }
    except Exception as e:
        denial = getattr(e, "denial", None)
        if denial is not None:
            return {
                "ok": False,
                "platform": PLATFORM,
                "kind": "markdown" if markdown else "text",
                "signed": bool(secret),
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
            "Inbound DingTalk messages need a callback URL "
            "registered on the robot side. Use ``send <text>`` for "
            "outbound."
        ),
    }


def _status() -> dict:
    url, url_err = _env_or_credential("COS_DINGTALK_WEBHOOK_URL", "dingtalk_webhook_url")
    secret, _ = _env_or_credential("COS_DINGTALK_SECRET", "dingtalk_secret")
    return {
        "ok": True,
        "platform": PLATFORM,
        "running": False,
        "configured": url is not None,
        "config_error": url_err,
        "signed": bool(secret),
        "note": "Outbound-only via DingTalk custom-robot webhook (Sign/Keyword/IP modes).",
    }


def run(command: str, args):
    if command == "send":
        text = ""
        markdown = False
        title = None
        keyword = None
        at_mobiles: list[str] = []
        at_user_ids: list[str] = []
        at_all = False
        if isinstance(args, list):
            if args:
                text = str(args[0])
        elif isinstance(args, dict):
            text = str(args.get("text", "") or "")
            markdown = bool(args.get("markdown", False))
            title = args.get("title")
            keyword = args.get("keyword")
            at_mobiles = _split_csv(args.get("at_mobiles"))
            at_user_ids = _split_csv(args.get("at_user_ids"))
            at_all = bool(args.get("at_all", False))
        else:
            return {"ok": False, "error": "invalid args"}
        result = _send(
            text,
            markdown=markdown,
            title=title,
            keyword=keyword,
            at_mobiles=at_mobiles,
            at_user_ids=at_user_ids,
            at_all=at_all,
        )
        gateway_memory.remember_send(PLATFORM, result, channel_id="", text=text)
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
