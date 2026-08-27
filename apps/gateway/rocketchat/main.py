"""Rocket.Chat gateway.

Outbound-only baseline. ``send`` POSTs JSON to
``<site>/api/v1/chat.postMessage`` authenticated by a personal
access token (``X-Auth-Token`` + ``X-User-Id`` headers).

Recipient routing:

  * ``#general``        → channel name (kept verbatim, leading ``#``)
  * ``general``         → channel name (no prefix; Rocket.Chat treats
                          unprefixed strings as channel names)
  * ``@alice``          → direct message to user ``alice``

Body: rendered as Rocket.Chat-flavour Markdown (the API treats
``text`` as markdown by default).

Credentials needed:

  * ``rocketchat_site``     — full origin, e.g. ``https://chat.example.com``
                               (env override: COS_ROCKETCHAT_SITE)
  * ``rocketchat_user_id``  — bot user ID from Profile → Personal Access Tokens
                               (env override: COS_ROCKETCHAT_USER_ID)
  * ``rocketchat_token``    — personal access token
                               (env override: COS_ROCKETCHAT_TOKEN)

Stdlib only.
"""

from __future__ import annotations

import json
import os
import sys
import urllib.error


# Sibling ``_shared`` package import (script-mode invocation).
sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

from _shared import gateway_memory, safe_egress, safe_subprocess  # noqa: E402


PLATFORM = "rocketchat"
USER_AGENT = "ClawOSRocketChat/0.1.0"
SOFT_LEN = 5000


def _load_credential(name: str) -> tuple[str | None, str | None]:
    return safe_subprocess.safe_credential_load(name)


def _env_or_credential(env_var: str, cred_name: str) -> tuple[str | None, str | None]:
    val = os.environ.get(env_var)
    if val and val.strip():
        return val.strip(), None
    return _load_credential(cred_name)


def _load_config() -> tuple[dict | None, str | None]:
    site, err = _env_or_credential("COS_ROCKETCHAT_SITE", "rocketchat_site")
    if not site:
        return None, err or "rocketchat_site required"
    user_id, err = _env_or_credential(
        "COS_ROCKETCHAT_USER_ID", "rocketchat_user_id"
    )
    if not user_id:
        return None, err or "rocketchat_user_id required"
    token, err = _env_or_credential("COS_ROCKETCHAT_TOKEN", "rocketchat_token")
    if not token:
        return None, err or "rocketchat_token required"
    site = site.rstrip("/")
    if not site.startswith(("https://", "http://")):
        return None, f"rocketchat_site must be http(s) URL, got: {site!r}"
    return {"site": site, "user_id": user_id, "token": token}, None


def _truncate(text: str, limit: int = SOFT_LEN) -> str:
    if len(text) <= limit:
        return text
    return text[: limit - 1] + "\u2026"


def _send(target: str, text: str) -> dict:
    if not target or not str(target).strip():
        return {"ok": False, "error": "target required"}
    if not text or not str(text).strip():
        return {"ok": False, "error": "text required"}

    cfg, err = _load_config()
    if not cfg:
        return {"ok": False, "error": err or "rocketchat not configured"}

    body_text = _truncate(str(text))
    payload = json.dumps({"channel": target, "text": body_text}).encode("utf-8")
    url = f"{cfg['site']}/api/v1/chat.postMessage"
    headers = {
        "X-Auth-Token": cfg["token"],
        "X-User-Id": cfg["user_id"],
        "Content-Type": "application/json",
        "User-Agent": USER_AGENT,
        "Accept": "application/json",
    }
    try:
        _, _, raw_resp = safe_egress.safe_urlopen(
            "POST",
            url,
            headers=headers,
            body=payload,
            timeout=20,
            verb_id="net.dial",
        )
        raw = raw_resp.decode("utf-8", errors="replace")
        try:
            data = json.loads(raw)
        except json.JSONDecodeError:
            data = {"raw": raw}
        success = isinstance(data, dict) and bool(data.get("success"))
        msg = data.get("message") if isinstance(data, dict) else None
        return {
            "ok": success,
            "platform": PLATFORM,
            "site": cfg["site"],
            "channel": target,
            "id": (msg or {}).get("_id") if isinstance(msg, dict) else None,
            "ts": (msg or {}).get("ts") if isinstance(msg, dict) else None,
            "raw": data if not success else None,
        }
    except safe_egress.EgressBlocked as e:
        return {
            "ok": False,
            "platform": PLATFORM,
            "site": cfg["site"],
            "channel": target,
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
            "site": cfg["site"],
            "channel": target,
            "error": f"HTTP {e.code}: {err_body}",
        }
    except urllib.error.URLError as e:
        return {
            "ok": False,
            "platform": PLATFORM,
            "site": cfg["site"],
            "channel": target,
            "error": f"URL error: {e.reason}",
        }
    except Exception as e:
        denial = getattr(e, "denial", None)
        if denial is not None:
            return {
                "ok": False,
                "platform": PLATFORM,
                "site": cfg["site"],
                "channel": target,
                "error": "permission denied",
                "denial": denial,
            }
        raise


def _status() -> dict:
    cfg, err = _load_config()
    return {
        "ok": True,
        "platform": PLATFORM,
        "running": False,
        "configured": cfg is not None,
        "site": cfg["site"] if cfg else None,
        "user_id": cfg["user_id"] if cfg else None,
        "config_error": err,
        "note": "Outbound-only via Rocket.Chat REST chat.postMessage.",
    }


def run(command: str, args):
    from canonical_argv import normalize_canonical_argv
    if isinstance(args, list):
        args = normalize_canonical_argv(args)
    if command == "send":
        target = ""
        text = ""
        if isinstance(args, list):
            if len(args) >= 2:
                target, text = str(args[0]), str(args[1])
            elif len(args) == 1:
                text = str(args[0])
        elif isinstance(args, dict):
            target = str(args.get("target") or args.get("channel") or "")
            text = str(args.get("text", "") or "")
        else:
            return {"ok": False, "error": "invalid args"}
        result = _send(target, text)
        gateway_memory.remember_send(PLATFORM, result, channel_id=target, text=text)
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
