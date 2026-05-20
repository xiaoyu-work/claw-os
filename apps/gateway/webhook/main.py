"""Generic outbound webhook gateway app.

POSTs an arbitrary message body to any HTTPS endpoint with optional
auth. Designed for the long tail of integrations that don't justify
a dedicated platform-specific gateway (Zapier-style triggers,
custom on-call paging, in-house Slack-incompatible chat servers,
status pages, etc.).

Auth modes (mutually exclusive — last one specified wins):

  * ``--bearer <token>``           ``Authorization: Bearer <token>``
  * ``--basic <user:pass>``        ``Authorization: Basic base64(user:pass)``
  * ``--api-key <key>``            ``X-API-Key: <key>``
  * ``--hmac-sha256 <secret>``     ``X-Signature: sha256=<hex(hmac(secret, body))>``

Body shape: by default the body is wrapped in
``{"text": "<your message>", "platform": "webhook", "ts": <unix>}``.
Pass ``--raw`` to send the message body verbatim with
``Content-Type: text/plain`` instead.

Credentials needed (any subset, or none if you pass everything on
the command line):

  * ``webhook_default_url``    — fallback target if positional arg
                                 omitted (env override:
                                 COS_WEBHOOK_URL)
  * ``webhook_default_secret`` — fallback HMAC secret / bearer
                                 token (env override:
                                 COS_WEBHOOK_SECRET)

Stdlib only. Every outbound request funnels through
:func:`apps.gateway._shared.safe_egress.safe_urlopen` so the kernel
sees a ``policy.require("gateway.webhook.send", host=…)`` decision
before the bytes leave the box, and so the response cannot redirect
us back to an internal IMDS endpoint.
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


# Make sibling ``_shared`` package importable when this file is run
# as a script (``cos app gateway-webhook …`` execs main.py directly).
sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

from _shared import gateway_memory, safe_egress, safe_subprocess  # noqa: E402


PLATFORM = "webhook"
USER_AGENT = "ClawOSWebhook/0.1.0"
DEFAULT_TIMEOUT = 20


def _schema() -> dict:
    return {
        "name": f"gateway-{PLATFORM}",
        "version": "0.1.0",
        "description": (
            "Generic outbound webhook gateway. ``send`` POSTs a "
            "JSON-wrapped (or raw) body to any HTTPS endpoint with "
            "optional Bearer / Basic / X-API-Key / HMAC-SHA256 auth."
        ),
        "commands": {
            "send": {
                "description": "POST a message body to a webhook URL",
                "parameters": [
                    {
                        "name": "target",
                        "type": "string",
                        "required": False,
                        "description": (
                            "Webhook URL. Falls back to "
                            "``webhook_default_url`` credential / "
                            "COS_WEBHOOK_URL env if omitted."
                        ),
                        "kind": "positional",
                    },
                    {
                        "name": "text",
                        "type": "string",
                        "required": True,
                        "description": "Message body",
                        "kind": "positional",
                    },
                    {
                        "name": "raw",
                        "type": "boolean",
                        "required": False,
                        "description": "Send body verbatim as text/plain (no JSON envelope)",
                    },
                    {
                        "name": "bearer",
                        "type": "string",
                        "required": False,
                        "description": "Authorization: Bearer <token>",
                    },
                    {
                        "name": "basic",
                        "type": "string",
                        "required": False,
                        "description": "Authorization: Basic base64(user:pass)",
                    },
                    {
                        "name": "api-key",
                        "type": "string",
                        "required": False,
                        "description": "X-API-Key: <key>",
                    },
                    {
                        "name": "hmac-sha256",
                        "type": "string",
                        "required": False,
                        "description": "X-Signature: sha256=<hex hmac>",
                    },
                ],
                "example": (
                    "cos app gateway-webhook send "
                    "https://hooks.example/notify 'deploy ok'"
                ),
            },
            "status": {
                "description": (
                    "Show whether a default URL / secret is configured"
                ),
                "parameters": [],
                "example": "cos app gateway-webhook status",
            },
        },
    }


def _load_credential(name: str) -> tuple[str | None, str | None]:
    return safe_subprocess.safe_credential_load(name)


def _env_or_credential(env_var: str, cred_name: str) -> tuple[str | None, str | None]:
    val = os.environ.get(env_var)
    if val and val.strip():
        return val.strip(), None
    return _load_credential(cred_name)


def _default_url() -> tuple[str | None, str | None]:
    return _env_or_credential("COS_WEBHOOK_URL", "webhook_default_url")


def _default_secret() -> tuple[str | None, str | None]:
    return _env_or_credential("COS_WEBHOOK_SECRET", "webhook_default_secret")


def _build_body(text: str, raw: bool) -> tuple[bytes, str]:
    if raw:
        return text.encode("utf-8"), "text/plain; charset=utf-8"
    payload = {
        "text": text,
        "platform": PLATFORM,
        "ts": int(time.time()),
    }
    return json.dumps(payload).encode("utf-8"), "application/json"


def _build_headers(
    content_type: str,
    body: bytes,
    bearer: str | None,
    basic: str | None,
    api_key: str | None,
    hmac_secret: str | None,
) -> dict[str, str]:
    headers = {
        "Content-Type": content_type,
        "User-Agent": USER_AGENT,
        "Accept": "application/json, text/plain;q=0.5, */*;q=0.1",
    }
    # Auth precedence: explicit bearer > basic > api-key > hmac
    if bearer:
        headers["Authorization"] = f"Bearer {bearer}"
    elif basic:
        encoded = base64.b64encode(basic.encode("utf-8")).decode("ascii")
        headers["Authorization"] = f"Basic {encoded}"
    elif api_key:
        headers["X-API-Key"] = api_key

    if hmac_secret:
        digest = hmac.new(
            hmac_secret.encode("utf-8"),
            body,
            hashlib.sha256,
        ).hexdigest()
        headers["X-Signature"] = f"sha256={digest}"

    return headers


def _send(
    target: str,
    text: str,
    raw: bool,
    bearer: str | None,
    basic: str | None,
    api_key: str | None,
    hmac_secret: str | None,
) -> dict:
    if not text or not str(text).strip():
        return {"ok": False, "error": "text required"}
    if not target or not str(target).strip():
        url, err = _default_url()
        if not url:
            return {
                "ok": False,
                "error": err or "target URL required (positional or webhook_default_url)",
            }
        target = url

    if not target.startswith(("https://", "http://")):
        return {
            "ok": False,
            "error": f"target must be http(s) URL, got: {target!r}",
        }

    if hmac_secret is None:
        # Look up the default HMAC secret silently — if it's not
        # configured, just don't sign the body. The caller is in
        # control: if they want signing, they pass --hmac-sha256
        # explicitly OR set webhook_default_secret.
        secret, _ = _default_secret()
        hmac_secret = secret

    body, content_type = _build_body(text, raw)
    headers = _build_headers(
        content_type, body, bearer, basic, api_key, hmac_secret
    )

    try:
        status, _resp_headers, raw_resp = safe_egress.safe_urlopen(
            "POST",
            target,
            headers=headers,
            body=body,
            timeout=DEFAULT_TIMEOUT,
            verb_id="gateway.webhook.send",
        )
    except safe_egress.EgressBlocked as e:
        return {
            "ok": False,
            "platform": PLATFORM,
            "target": target,
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
            "target": target,
            "kind": "raw" if raw else "json",
            "signed": hmac_secret is not None,
            "error": f"HTTP {e.code}: {err_body}",
        }
    except urllib.error.URLError as e:
        return {
            "ok": False,
            "platform": PLATFORM,
            "target": target,
            "kind": "raw" if raw else "json",
            "error": f"URL error: {e.reason}",
        }
    except Exception as e:
        # Catches cos_runtime.policy.PermissionDenied (which is not
        # importable here without coupling to the kernel) and any
        # other unexpected denial path.
        denial = getattr(e, "denial", None)
        if denial is not None:
            return {
                "ok": False,
                "platform": PLATFORM,
                "target": target,
                "kind": "raw" if raw else "json",
                "error": "permission denied",
                "denial": denial,
            }
        raise

    raw_body = raw_resp.decode("utf-8", errors="replace")
    return {
        "ok": True,
        "platform": PLATFORM,
        "target": target,
        "status": status,
        "kind": "raw" if raw else "json",
        "signed": hmac_secret is not None,
        "body_preview": raw_body[:200],
    }


def _status() -> dict:
    url, url_err = _default_url()
    secret, _ = _default_secret()
    return {
        "ok": True,
        "platform": PLATFORM,
        "running": False,
        "default_url_configured": url is not None,
        "default_url_error": url_err,
        "default_secret_configured": secret is not None,
        "note": (
            "Outbound-only. ``send <url> <text>`` (or just ``send <text>`` "
            "if webhook_default_url is set)."
        ),
    }


def _coerce_bool(v) -> bool:
    if isinstance(v, bool):
        return v
    if isinstance(v, str):
        return v.lower() in {"1", "true", "yes", "on"}
    return bool(v)


def _parse_args_dict(args: dict) -> dict:
    return {
        "target": str(args.get("target", "") or ""),
        "text": str(args.get("text", "") or ""),
        "raw": _coerce_bool(args.get("raw", False)),
        "bearer": (args.get("bearer") or None),
        "basic": (args.get("basic") or None),
        "api_key": (args.get("api-key") or args.get("api_key") or None),
        "hmac_secret": (args.get("hmac-sha256") or args.get("hmac_sha256") or None),
    }


def _parse_args_list(args: list) -> dict:
    """Permissive positional + flag parser to keep parity with the
    CLI invocation shape ``cos app gateway-webhook send <target> <text>
    [--bearer X] [--raw] ...``"""
    out: dict = {
        "target": "",
        "text": "",
        "raw": False,
        "bearer": None,
        "basic": None,
        "api_key": None,
        "hmac_secret": None,
    }
    positional: list[str] = []
    i = 0
    while i < len(args):
        a = str(args[i])
        if a == "--raw":
            out["raw"] = True
            i += 1
            continue
        if a == "--bearer" and i + 1 < len(args):
            out["bearer"] = str(args[i + 1])
            i += 2
            continue
        if a == "--basic" and i + 1 < len(args):
            out["basic"] = str(args[i + 1])
            i += 2
            continue
        if a == "--api-key" and i + 1 < len(args):
            out["api_key"] = str(args[i + 1])
            i += 2
            continue
        if a == "--hmac-sha256" and i + 1 < len(args):
            out["hmac_secret"] = str(args[i + 1])
            i += 2
            continue
        positional.append(a)
        i += 1
    if len(positional) >= 2:
        out["target"], out["text"] = positional[0], positional[1]
    elif len(positional) == 1:
        # Single positional: treat as text, target falls back to default.
        out["text"] = positional[0]
    return out


def run(command: str, args):
    if command == "__schema__":
        return _schema()
    if command == "send":
        if isinstance(args, dict):
            parsed = _parse_args_dict(args)
        elif isinstance(args, list):
            parsed = _parse_args_list(args)
        else:
            return {"ok": False, "error": "invalid args"}
        try:
            result = _send(
                parsed["target"],
                parsed["text"],
                parsed["raw"],
                parsed["bearer"],
                parsed["basic"],
                parsed["api_key"],
                parsed["hmac_secret"],
            )
            gateway_memory.remember_send(
                PLATFORM,
                result,
                channel_id=str(parsed.get("target") or ""),
                text=str(parsed.get("text") or parsed.get("raw") or ""),
            )
            return result
        except Exception as e:  # surface PermissionDenied cleanly
            denial = getattr(e, "denial", None)
            if denial is not None:
                return {"ok": False, "platform": PLATFORM, "error": "permission denied", "denial": denial}
            raise
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
