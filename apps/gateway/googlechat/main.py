"""Google Chat gateway app (Incoming Webhook backend).

Outbound-only baseline. ``send`` POSTs to a Google Chat space's
[Incoming Webhook](https://developers.google.com/workspace/chat/quickstart/webhooks)
URL. The webhook is bound to a single space; ``recipient`` is
informational-only and ignored on the wire.

Two body shapes are supported:

  * **Plain text** (default): ``{"text": "..."}``.
  * **Card v2**: pass ``--title`` to wrap the text in a single
    ``cardsV2`` envelope with a header.

Threaded replies are supported via ``--thread-key`` (Google Chat
auto-creates the thread on first use; subsequent posts with the same
key reply to it).

Credentials needed:
  * ``googlechat_webhook_url`` -- full webhook URL with key+token
                                  query params
                                  (env override: COS_GOOGLECHAT_WEBHOOK_URL)

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


PLATFORM = "googlechat"
USER_AGENT = "ClawOSGoogleChat/0.1.0"
SOFT_LEN = 4000  # Google Chat text per message


def _load_credential(name: str) -> tuple[str | None, str | None]:
    return safe_subprocess.safe_credential_load(name)


def _env_or_credential(env_var: str, cred_name: str) -> tuple[str | None, str | None]:
    val = os.environ.get(env_var)
    if val and val.strip():
        return val.strip(), None
    return _load_credential(cred_name)


def _load_webhook_url() -> tuple[str | None, str | None]:
    return _env_or_credential("COS_GOOGLECHAT_WEBHOOK_URL", "googlechat_webhook_url")


def _truncate(text: str, limit: int = SOFT_LEN) -> str:
    if len(text) <= limit:
        return text
    return text[: limit - 1] + "\u2026"


def _card_v2(text: str, title: str) -> dict:
    card = {
        "cardId": "cos-card",
        "card": {
            "header": {"title": title.strip()},
            "sections": [
                {
                    "widgets": [
                        {"textParagraph": {"text": text}},
                    ],
                }
            ],
        },
    }
    return {"cardsV2": [card]}


def _attach_thread_key(url: str, thread_key: str) -> str:
    """Add ``threadKey=...&messageReplyOption=REPLY_MESSAGE_OR_FAIL``
    so subsequent posts reply within the thread instead of starting a
    new top-level message."""
    parts = urllib.parse.urlsplit(url)
    qs = urllib.parse.parse_qsl(parts.query, keep_blank_values=True)
    qs = [(k, v) for k, v in qs if k not in {"threadKey", "messageReplyOption"}]
    qs.append(("threadKey", thread_key))
    qs.append(
        ("messageReplyOption", "REPLY_MESSAGE_FALLBACK_TO_NEW_THREAD"),
    )
    new_query = urllib.parse.urlencode(qs)
    return urllib.parse.urlunsplit(
        (parts.scheme, parts.netloc, parts.path, new_query, parts.fragment)
    )


def _send(
    recipient: str,
    text: str,
    title: str = "",
    thread_key: str = "",
) -> dict:
    if not text or not str(text).strip():
        return {"ok": False, "error": "text required"}

    url, err = _load_webhook_url()
    if not url:
        return {"ok": False, "error": err or "googlechat_webhook_url required"}

    if title and title.strip():
        payload = _card_v2(_truncate(str(text)), title)
        kind = "cardsV2"
    else:
        payload = {"text": _truncate(str(text))}
        kind = "text"

    if thread_key and thread_key.strip():
        url = _attach_thread_key(url, thread_key.strip())

    body = json.dumps(payload).encode("utf-8")
    headers = {
        "Content-Type": "application/json; charset=UTF-8",
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
        return {
            "ok": True,
            "platform": PLATFORM,
            "informational_recipient": recipient or None,
            "kind": kind,
            "thread_key": thread_key or None,
            "name": data.get("name") if isinstance(data, dict) else None,
        }
    except safe_egress.EgressBlocked as e:
        return {"ok": False, "platform": PLATFORM, "kind": kind, "error": f"egress blocked: {e}"}
    except urllib.error.HTTPError as e:
        try:
            err_body = e.read().decode("utf-8", errors="replace")
        except Exception:
            err_body = str(e)
        return {
            "ok": False,
            "platform": PLATFORM,
            "kind": kind,
            "error": f"HTTP {e.code}: {err_body}",
        }
    except urllib.error.URLError as e:
        return {
            "ok": False,
            "platform": PLATFORM,
            "kind": kind,
            "error": f"URL error: {e.reason}",
        }
    except Exception as e:
        denial = getattr(e, "denial", None)
        if denial is not None:
            return {"ok": False, "platform": PLATFORM, "kind": kind, "error": "permission denied", "denial": denial}
        raise


def _status() -> dict:
    url, err = _load_webhook_url()
    return {
        "ok": True,
        "platform": PLATFORM,
        "running": False,
        "configured": url is not None,
        "config_error": err,
        "note": "Outbound-only via Google Chat Incoming Webhook (text or cardsV2).",
    }


def run(command: str, args):
    if command == "send":
        recipient = ""
        text = ""
        title = ""
        thread_key = ""
        if isinstance(args, list):
            parsed, error = gateway_args.parse(
                args,
                positional=("text",),
                value_flags=("recipient", "title", "thread-key"),
            )
            if error:
                return {"ok": False, "error": error}
            text = parsed["text"]
            recipient = parsed["recipient"] or ""
            title = parsed["title"] or ""
            thread_key = parsed["thread-key"] or ""
        elif isinstance(args, dict):
            recipient = str(args.get("recipient", "") or "")
            text = str(args.get("text", "") or "")
            title = str(args.get("title", "") or "")
            thread_key = str(args.get("thread_key", "") or "")
        else:
            return {"ok": False, "error": "invalid args"}
        result = _send(recipient, text, title, thread_key)
        gateway_memory.remember_send(PLATFORM, result, channel_id=recipient, text=text)
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
