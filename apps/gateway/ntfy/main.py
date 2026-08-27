"""ntfy.sh push-notification gateway.

Outbound-only. ``send`` POSTs a UTF-8 plain-text body to
``<server>/<topic>``. Optional metadata is shipped via headers
that ntfy understands (``Title``, ``Priority``, ``Tags``,
``Click``, ``Markdown``).

Auth modes (mutually exclusive; checked in order):

  1. ``--bearer <token>`` or ``ntfy_token`` credential / env
     ``COS_NTFY_TOKEN``  → ``Authorization: Bearer <token>``
  2. ``--basic <user:pass>`` → ``Authorization: Basic <base64>``
  3. None (anonymous, only works on public topics)

Body is sent as ``text/plain; charset=utf-8`` so it works against
both ntfy.sh and self-hosted ntfy servers without going through
the JSON-publish endpoint.

Server defaults to ``https://ntfy.sh`` (override via
``--server`` arg, ``COS_NTFY_SERVER`` env, or ``ntfy_server``
credential). Topic defaults to ``ntfy_default_topic`` /
``COS_NTFY_DEFAULT_TOPIC`` when only the body is supplied.

Stdlib only.
"""

from __future__ import annotations

import base64
import json
import os
import sys
import urllib.error


# Sibling ``_shared`` package import (script-mode invocation).
sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

from _shared import gateway_memory, safe_egress, safe_subprocess  # noqa: E402


PLATFORM = "ntfy"
USER_AGENT = "ClawOSNtfy/0.1.0"
DEFAULT_SERVER = "https://ntfy.sh"
SOFT_LEN = 4000
ALLOWED_PRIORITIES = {"min", "low", "default", "high", "max", "urgent"}


def _load_credential(name: str) -> tuple[str | None, str | None]:
    return safe_subprocess.safe_credential_load(name)


def _env_or_credential(env_var: str, cred_name: str) -> str | None:
    val = os.environ.get(env_var)
    if val and val.strip():
        return val.strip()
    cred, _ = _load_credential(cred_name)
    return cred


def _truncate(text: str, limit: int = SOFT_LEN) -> str:
    if len(text) <= limit:
        return text
    return text[: limit - 1] + "\u2026"


def _resolve_topic(topic: str | None) -> str | None:
    if topic and topic.strip():
        return topic.strip()
    return _env_or_credential("COS_NTFY_DEFAULT_TOPIC", "ntfy_default_topic")


def _resolve_server(server: str | None) -> str:
    if server and server.strip():
        s = server.strip().rstrip("/")
    else:
        cred = _env_or_credential("COS_NTFY_SERVER", "ntfy_server")
        s = (cred or DEFAULT_SERVER).rstrip("/")
    return s


def _resolve_auth_header(
    bearer: str | None, basic: str | None
) -> str | None:
    if bearer and bearer.strip():
        return f"Bearer {bearer.strip()}"
    if basic and basic.strip():
        token = base64.b64encode(basic.encode("utf-8")).decode("ascii")
        return f"Basic {token}"
    cred = _env_or_credential("COS_NTFY_TOKEN", "ntfy_token")
    if cred:
        return f"Bearer {cred}"
    return None


def _send(
    topic: str | None,
    text: str,
    *,
    title: str | None = None,
    priority: str | None = None,
    tags: str | None = None,
    click: str | None = None,
    markdown: bool = False,
    server: str | None = None,
    bearer: str | None = None,
    basic: str | None = None,
) -> dict:
    if not text or not str(text).strip():
        return {"ok": False, "error": "text required"}
    resolved_topic = _resolve_topic(topic)
    if not resolved_topic:
        return {"ok": False, "error": "topic required"}
    resolved_server = _resolve_server(server)
    if not resolved_server.startswith(("https://", "http://")):
        return {
            "ok": False,
            "error": f"ntfy server must be http(s) URL, got: {resolved_server!r}",
        }

    pri_norm: str | None = None
    if priority is not None:
        pri_norm = str(priority).strip().lower()
        if pri_norm and pri_norm not in ALLOWED_PRIORITIES:
            return {
                "ok": False,
                "error": (
                    f"priority must be one of {sorted(ALLOWED_PRIORITIES)}, "
                    f"got: {priority!r}"
                ),
            }

    body_text = _truncate(str(text)).encode("utf-8")
    headers: dict[str, str] = {
        "Content-Type": "text/plain; charset=utf-8",
        "User-Agent": USER_AGENT,
    }
    if title and str(title).strip():
        headers["Title"] = str(title).strip()
    if pri_norm:
        headers["Priority"] = pri_norm
    if tags and str(tags).strip():
        headers["Tags"] = str(tags).strip()
    if click and str(click).strip():
        headers["Click"] = str(click).strip()
    if markdown:
        headers["Markdown"] = "yes"
    auth = _resolve_auth_header(bearer, basic)
    if auth:
        headers["Authorization"] = auth

    url = f"{resolved_server}/{resolved_topic}"
    try:
        status, _, raw_resp = safe_egress.safe_urlopen(
            "POST",
            url,
            headers=headers,
            body=body_text,
            timeout=20,
            verb_id="net.dial",
        )
        raw = raw_resp.decode("utf-8", errors="replace")
        try:
            data = json.loads(raw)
        except json.JSONDecodeError:
            data = {"raw": raw}
        ok = 200 <= status < 300
        return {
            "ok": ok,
            "platform": PLATFORM,
            "server": resolved_server,
            "topic": resolved_topic,
            "status": status,
            "id": data.get("id") if isinstance(data, dict) else None,
            "result": data,
        }
    except safe_egress.EgressBlocked as e:
        return {
            "ok": False,
            "platform": PLATFORM,
            "server": resolved_server,
            "topic": resolved_topic,
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
            "server": resolved_server,
            "topic": resolved_topic,
            "error": f"HTTP {e.code}: {err_body}",
        }
    except urllib.error.URLError as e:
        return {
            "ok": False,
            "platform": PLATFORM,
            "server": resolved_server,
            "topic": resolved_topic,
            "error": f"URL error: {e.reason}",
        }
    except Exception as e:
        denial = getattr(e, "denial", None)
        if denial is not None:
            return {
                "ok": False,
                "platform": PLATFORM,
                "server": resolved_server,
                "topic": resolved_topic,
                "error": "permission denied",
                "denial": denial,
            }
        raise


def _status() -> dict:
    server = _env_or_credential("COS_NTFY_SERVER", "ntfy_server") or DEFAULT_SERVER
    topic = _env_or_credential("COS_NTFY_DEFAULT_TOPIC", "ntfy_default_topic")
    has_token = bool(_env_or_credential("COS_NTFY_TOKEN", "ntfy_token"))
    return {
        "ok": True,
        "platform": PLATFORM,
        "running": False,
        "server": server,
        "default_topic": topic,
        "token_configured": has_token,
        "note": (
            "ntfy supports anonymous publish on public topics; "
            "ntfy_token only needed for protected topics."
        ),
    }


_FLAG_KEYS = {
    "--title": "title",
    "--priority": "priority",
    "--tags": "tags",
    "--click": "click",
    "--server": "server",
    "--bearer": "bearer",
    "--basic": "basic",
}


def _parse_send_args(args) -> tuple[str | None, str, dict]:
    flags: dict = {"markdown": False}
    if isinstance(args, dict):
        topic = args.get("topic")
        text = args.get("text", "")
        for k in (
            "title",
            "priority",
            "tags",
            "click",
            "server",
            "bearer",
            "basic",
        ):
            v = args.get(k)
            if v is not None:
                flags[k] = str(v)
        if args.get("markdown"):
            flags["markdown"] = True
        return (
            str(topic) if topic is not None else None,
            str(text or ""),
            flags,
        )
    if not isinstance(args, list):
        return None, "", flags
    positional: list[str] = []
    i = 0
    while i < len(args):
        a = str(args[i])
        if a == "--markdown":
            flags["markdown"] = True
            i += 1
            continue
        key = _FLAG_KEYS.get(a)
        if key is not None and i + 1 < len(args):
            flags[key] = str(args[i + 1])
            i += 2
            continue
        positional.append(a)
        i += 1
    if len(positional) >= 2:
        return positional[0], positional[1], flags
    if len(positional) == 1:
        return None, positional[0], flags
    return None, "", flags


def run(command: str, args):
    if command == "send":
        topic, text, flags = _parse_send_args(args)
        result = _send(
            topic,
            text,
            title=flags.get("title"),
            priority=flags.get("priority"),
            tags=flags.get("tags"),
            click=flags.get("click"),
            markdown=bool(flags.get("markdown")),
            server=flags.get("server"),
            bearer=flags.get("bearer"),
            basic=flags.get("basic"),
        )
        gateway_memory.remember_send(
            PLATFORM,
            result,
            channel_id=str(topic) if topic else "",
            text=str(text) if text else "",
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
