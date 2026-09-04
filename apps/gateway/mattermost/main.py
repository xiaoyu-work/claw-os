"""Mattermost gateway app (Incoming Webhook backend).

Outbound-only baseline: ``send`` POSTs to the configured Mattermost
[Incoming Webhook](https://developers.mattermost.com/integrate/webhooks/incoming/)
URL. Inbound (slash commands / outgoing webhooks) requires a public
endpoint and is out of scope.

Channel routing follows the Mattermost incoming-webhook convention:
the webhook is created against a default channel; ``--channel`` (or
``recipient``) overrides it. Channel can be either a name (``town-square``)
or a direct-message handle (``@username``). Username and icon overrides
are passed through.

Credentials needed:
  * ``mattermost_webhook_url`` -- full webhook URL
                                  (env override: COS_MATTERMOST_WEBHOOK_URL)

Stdlib only.
"""

from __future__ import annotations

import json
import os
import sys
import urllib.error


# Sibling ``_shared`` package import (script-mode invocation).
sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

from _shared import gateway_args, gateway_memory, safe_egress, safe_subprocess  # noqa: E402


PLATFORM = "mattermost"
USER_AGENT = "ClawOSMattermost/0.1.0"
SOFT_LEN = 16000  # Mattermost recommends ~16k for posts


def _load_credential(name: str) -> tuple[str | None, str | None]:
    return safe_subprocess.safe_credential_load(name)


def _env_or_credential(env_var: str, cred_name: str) -> tuple[str | None, str | None]:
    val = os.environ.get(env_var)
    if val and val.strip():
        return val.strip(), None
    return _load_credential(cred_name)


def _load_webhook_url() -> tuple[str | None, str | None]:
    return _env_or_credential("COS_MATTERMOST_WEBHOOK_URL", "mattermost_webhook_url")


def _truncate(text: str, limit: int = SOFT_LEN) -> str:
    if len(text) <= limit:
        return text
    return text[: limit - 1] + "\u2026"


def _send(recipient: str, text: str, username: str = "", icon_url: str = "") -> dict:
    if not text or not str(text).strip():
        return {"ok": False, "error": "text required"}

    url, err = _load_webhook_url()
    if not url:
        return {"ok": False, "error": err or "mattermost_webhook_url required"}

    payload: dict = {"text": _truncate(str(text))}
    rcp = (recipient or "").strip()
    if rcp:
        # Mattermost: 'channel' field accepts both names and @handles.
        payload["channel"] = rcp
    if username and username.strip():
        payload["username"] = username.strip()
    if icon_url and icon_url.strip():
        payload["icon_url"] = icon_url.strip()

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
        return {
            "ok": True,
            "platform": PLATFORM,
            "channel": rcp or None,
            "response": raw,  # Mattermost returns "ok" or a small JSON
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
    url, err = _load_webhook_url()
    return {
        "ok": True,
        "platform": PLATFORM,
        "running": False,
        "configured": url is not None,
        "config_error": err,
        "note": "Outbound-only mode via Mattermost Incoming Webhook.",
    }


def run(command: str, args):
    if command == "send":
        recipient = ""
        text = ""
        username = ""
        icon_url = ""
        if isinstance(args, list):
            parsed, error = gateway_args.parse(
                args,
                positional=("text",),
                value_flags=("recipient", "username", "icon-url"),
            )
            if error:
                return {"ok": False, "error": error}
            text = parsed["text"]
            recipient = parsed["recipient"] or ""
            username = parsed["username"] or ""
            icon_url = parsed["icon-url"] or ""
        elif isinstance(args, dict):
            recipient = str(args.get("recipient", "") or "")
            text = str(args.get("text", "") or "")
            username = str(args.get("username", "") or "")
            icon_url = str(args.get("icon_url", "") or "")
        else:
            return {"ok": False, "error": "invalid args"}
        result = _send(recipient, text, username, icon_url)
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
