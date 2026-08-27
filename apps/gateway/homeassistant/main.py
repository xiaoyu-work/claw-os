"""Home Assistant gateway.

Outbound-only baseline. ``send`` calls a notify service (default
``notify.notify``); ``call`` calls any HA service with arbitrary
JSON payload. Both endpoints hit
``<base>/api/services/<domain>/<service>`` with a Bearer
long-lived access token.

Common notification flows::

    cos app gateway-homeassistant send notify.mobile_app_pixel "deploy ok"
    cos app gateway-homeassistant send notify "deploy ok"   # uses notify.notify
    cos app gateway-homeassistant call light.turn_on '{"entity_id":"light.kitchen"}'

Credentials needed:

  * ``homeassistant_url``   — full base URL, e.g.
                               ``https://ha.example.com`` or
                               ``http://homeassistant.local:8123``
                               (env override: COS_HOMEASSISTANT_URL)
  * ``homeassistant_token`` — long-lived access token from
                               Profile → Long-Lived Access Tokens
                               (env override: COS_HOMEASSISTANT_TOKEN)

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


PLATFORM = "homeassistant"
USER_AGENT = "ClawOSHomeAssistant/0.1.0"


def _load_credential(name: str) -> tuple[str | None, str | None]:
    return safe_subprocess.safe_credential_load(name)


def _env_or_credential(env_var: str, cred_name: str) -> tuple[str | None, str | None]:
    val = os.environ.get(env_var)
    if val and val.strip():
        return val.strip(), None
    return _load_credential(cred_name)


def _load_config() -> tuple[dict | None, str | None]:
    base, err = _env_or_credential("COS_HOMEASSISTANT_URL", "homeassistant_url")
    if not base:
        return None, err or "homeassistant_url required"
    token, err = _env_or_credential(
        "COS_HOMEASSISTANT_TOKEN", "homeassistant_token"
    )
    if not token:
        return None, err or "homeassistant_token required"
    base = base.rstrip("/")
    if not base.startswith(("https://", "http://")):
        return None, f"homeassistant_url must be http(s) URL, got: {base!r}"
    return {"base": base, "token": token}, None


def _split_service(s: str) -> tuple[str | None, str | None, str | None]:
    """Return ``(domain, service, error)``. Accepts ``notify`` (short
    for ``notify.notify``) or ``<domain>.<service>``."""
    s = s.strip()
    if not s:
        return None, None, "service required"
    if "." not in s:
        if s == "notify":
            return "notify", "notify", None
        return None, None, (
            f"service must be domain.service, got: {s!r} "
            "(only 'notify' is auto-expanded)"
        )
    domain, _, service = s.partition(".")
    domain = domain.strip()
    service = service.strip()
    if not domain or not service:
        return None, None, f"empty domain or service in: {s!r}"
    return domain, service, None


def _post_service(domain: str, service: str, payload: dict) -> dict:
    cfg, err = _load_config()
    if not cfg:
        return {"ok": False, "error": err or "homeassistant not configured"}

    body = json.dumps(payload).encode("utf-8")
    url = f"{cfg['base']}/api/services/{domain}/{service}"
    headers = {
        "Authorization": f"Bearer {cfg['token']}",
        "Content-Type": "application/json",
        "User-Agent": USER_AGENT,
        "Accept": "application/json",
    }
    try:
        status, _, raw_resp = safe_egress.safe_urlopen(
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
            data = raw
        return {
            "ok": True,
            "platform": PLATFORM,
            "service": f"{domain}.{service}",
            "status": status,
            "result": data,
        }
    except safe_egress.EgressBlocked as e:
        return {
            "ok": False,
            "platform": PLATFORM,
            "service": f"{domain}.{service}",
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
            "service": f"{domain}.{service}",
            "error": f"HTTP {e.code}: {err_body}",
        }
    except urllib.error.URLError as e:
        return {
            "ok": False,
            "platform": PLATFORM,
            "service": f"{domain}.{service}",
            "error": f"URL error: {e.reason}",
        }
    except Exception as e:
        denial = getattr(e, "denial", None)
        if denial is not None:
            return {
                "ok": False,
                "platform": PLATFORM,
                "service": f"{domain}.{service}",
                "error": "permission denied",
                "denial": denial,
            }
        raise


def _send(service: str, text: str, title: str | None) -> dict:
    if not text or not str(text).strip():
        return {"ok": False, "error": "text required"}
    domain, svc, err = _split_service(service)
    if err or not domain or not svc:
        return {"ok": False, "error": err or "invalid service"}
    if domain != "notify":
        return {
            "ok": False,
            "error": (
                f"send is for notify.* services only; got '{domain}.{svc}'. "
                "Use 'call' for other domains."
            ),
        }
    payload: dict = {"message": str(text)}
    if title and str(title).strip():
        payload["title"] = str(title).strip()
    return _post_service(domain, svc, payload)


def _call(service: str, raw_json: str) -> dict:
    domain, svc, err = _split_service(service)
    if err or not domain or not svc:
        return {"ok": False, "error": err or "invalid service"}
    if not raw_json or not str(raw_json).strip():
        return {"ok": False, "error": "json payload required"}
    try:
        payload = json.loads(raw_json)
    except json.JSONDecodeError as e:
        return {"ok": False, "error": f"json payload invalid: {e}"}
    if not isinstance(payload, dict):
        return {"ok": False, "error": "json payload must be an object"}
    return _post_service(domain, svc, payload)


def _status() -> dict:
    cfg, err = _load_config()
    return {
        "ok": True,
        "platform": PLATFORM,
        "running": False,
        "configured": cfg is not None,
        "base": cfg["base"] if cfg else None,
        "config_error": err,
        "note": "Outbound-only via HA REST /api/services/<domain>/<service>.",
    }


def run(command: str, args):
    from canonical_argv import normalize_canonical_argv
    if isinstance(args, list):
        args = normalize_canonical_argv(args)
    if command == "send":
        service = ""
        text = ""
        title = None
        if isinstance(args, list):
            if len(args) >= 2:
                service, text = str(args[0]), str(args[1])
            elif len(args) == 1:
                text = str(args[0])
            i = 2
            while i + 1 < len(args):
                if str(args[i]) == "--title":
                    title = str(args[i + 1])
                    i += 2
                else:
                    i += 1
        elif isinstance(args, dict):
            service = str(args.get("service") or "")
            text = str(args.get("text", "") or "")
            t = args.get("title")
            title = str(t) if t is not None else None
        else:
            return {"ok": False, "error": "invalid args"}
        result = _send(service, text, title)
        gateway_memory.remember_send(PLATFORM, result, channel_id=service, text=text)
        return result
    if command == "call":
        service = ""
        raw = ""
        if isinstance(args, list):
            if len(args) >= 2:
                service, raw = str(args[0]), str(args[1])
            elif len(args) == 1:
                service = str(args[0])
        elif isinstance(args, dict):
            service = str(args.get("service") or "")
            raw_payload = args.get("json")
            if isinstance(raw_payload, str):
                raw = raw_payload
            elif raw_payload is not None:
                raw = json.dumps(raw_payload)
        else:
            return {"ok": False, "error": "invalid args"}
        return _call(service, raw)
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
