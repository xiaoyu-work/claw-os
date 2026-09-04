"""Typed operations for the attached-browser App."""

from __future__ import annotations

import base64
import os
from urllib.parse import urlsplit, urlunsplit

from cos_runtime import browser_bridge, memory, policy

from _shared.atomic import atomic_create_bytes
from _shared.paths import safe_realpath

_MAX_TAB_ID = 2**31 - 1
_MAX_SCREENSHOT_BYTES = 5 * 1024 * 1024
_PNG_SIGNATURE = b"\x89PNG\r\n\x1a\n"


def _tab_id(value: int) -> int:
    if isinstance(value, bool) or not isinstance(value, int):
        raise ValueError("tab_id must be an integer")
    if not 1 <= value <= _MAX_TAB_ID:
        raise ValueError(f"tab_id must be between 1 and {_MAX_TAB_ID}")
    return value


def _required_text(value: str, name: str) -> str:
    if not isinstance(value, str) or not value.strip():
        raise ValueError(f"{name} must be non-empty text")
    return value


def _browser_url(value: str) -> tuple[str, str]:
    raw = _required_text(value, "URL")
    if "://" not in raw:
        raw = f"https://{raw}"
    parsed = urlsplit(raw)
    if parsed.scheme not in {"http", "https"} or not parsed.hostname:
        raise ValueError("browser URL must use http or https and name a host")
    if parsed.username is not None or parsed.password is not None:
        raise ValueError("browser URL must not contain credentials")
    try:
        port = parsed.port
    except ValueError as exc:
        raise ValueError("browser URL has an invalid port") from exc
    port = port or (443 if parsed.scheme == "https" else 80)
    hostname = parsed.hostname
    rendered_host = f"[{hostname}]" if ":" in hostname else hostname
    canonical = urlunsplit(
        (
            parsed.scheme,
            parsed.netloc,
            parsed.path or "/",
            parsed.query,
            parsed.fragment,
        )
    )
    return canonical, f"{rendered_host}:{port}"


def _bridge(action: str, **fields: object) -> dict[str, object]:
    result = browser_bridge.request(action, **fields)
    if not isinstance(result, dict):
        raise RuntimeError("browser bridge returned a non-object result")
    return result


def tabs_list() -> dict[str, object]:
    policy.require("browser.tabs.read", wild=True)
    result = _bridge("tabs.list")
    tabs = result.get("tabs")
    if not isinstance(tabs, list) or len(tabs) > 512:
        raise RuntimeError("browser bridge returned an invalid tab list")
    for tab in tabs:
        if not isinstance(tab, dict):
            raise RuntimeError("browser bridge returned an invalid tab entry")
        if (
            isinstance(tab.get("id"), bool)
            or not isinstance(tab.get("id"), int)
            or not isinstance(tab.get("title"), str)
            or not isinstance(tab.get("url"), str)
            or not isinstance(tab.get("active"), bool)
        ):
            raise RuntimeError("browser bridge returned an invalid tab entry")
    return {"tabs": tabs}


def tabs_activate(tab_id: int) -> dict[str, object]:
    tab_id = _tab_id(tab_id)
    policy.require("browser.tabs.read", wild=True)
    result = _bridge("tabs.activate", tab_id=tab_id)
    if result.get("activated") != tab_id:
        raise RuntimeError("browser bridge returned an invalid activation result")
    return result


def navigate(tab_id: int, url: str) -> dict[str, object]:
    tab_id = _tab_id(tab_id)
    url, scope = _browser_url(url)
    policy.require("browser.nav", host=scope)
    result = _bridge("nav.go", tab_id=tab_id, url=url)
    if result.get("navigated") != tab_id or not isinstance(result.get("url"), str):
        raise RuntimeError("browser bridge returned an invalid navigation result")
    memory.remember(
        text=f"Navigated browser tab {tab_id} to host {scope}",
        source="browser-attached",
    )
    return result


def dom_query(tab_id: int, selector: str, page_url: str) -> dict[str, object]:
    return _page_action(
        "dom.query",
        "browser.dom.read",
        tab_id,
        page_url,
        selector=_required_text(selector, "selector"),
    )


def dom_click(tab_id: int, reference: str, page_url: str) -> dict[str, object]:
    return _page_action(
        "dom.click",
        "browser.dom.write",
        tab_id,
        page_url,
        reference=_required_text(reference, "reference"),
    )


def dom_fill(
    tab_id: int,
    reference: str,
    value: str,
    page_url: str,
) -> dict[str, object]:
    if not isinstance(value, str):
        raise ValueError("value must be text")
    return _page_action(
        "dom.fill",
        "browser.dom.write",
        tab_id,
        page_url,
        reference=_required_text(reference, "reference"),
        value=value,
    )


def dom_fill_secret(
    tab_id: int,
    reference: str,
    value: str,
    page_url: str,
) -> dict[str, object]:
    if not isinstance(value, str):
        raise ValueError("value must be text")
    return _page_action(
        "dom.fill_secret",
        "browser.input.secret",
        tab_id,
        page_url,
        reference=_required_text(reference, "reference"),
        value=value,
    )


def page_snapshot(
    tab_id: int,
    page_url: str,
    kind: str = "ax",
) -> dict[str, object]:
    if kind not in {"ax", "text"}:
        raise ValueError("kind must be `ax` or `text`")
    return _page_action(
        "page.snapshot",
        "browser.dom.read",
        tab_id,
        page_url,
        kind=kind,
    )


def page_screenshot(tab_id: int, output: str, page_url: str) -> dict[str, object]:
    tab_id = _tab_id(tab_id)
    requested_output = os.path.abspath(_required_text(output, "output"))
    if os.path.lexists(requested_output):
        raise ValueError("output must be a new path")
    output = safe_realpath(requested_output)
    if os.path.lexists(output):
        raise ValueError("output must be a new path")
    _, scope = _browser_url(page_url)
    policy.require("browser.dom.read", host=scope)
    policy.require("fs.write", path=output)
    result = _bridge(
        "page.screenshot",
        tab_id=tab_id,
        page_url=page_url,
    )
    encoded = result.get("data")
    if not isinstance(encoded, str):
        raise RuntimeError("browser screenshot did not contain base64 data")
    if len(encoded) > ((_MAX_SCREENSHOT_BYTES + 2) // 3) * 4:
        raise RuntimeError("browser screenshot exceeds the 5 MiB limit")
    try:
        image = base64.b64decode(encoded, validate=True)
    except (ValueError, base64.binascii.Error) as exc:
        raise RuntimeError("browser screenshot contains invalid base64") from exc
    if len(image) > _MAX_SCREENSHOT_BYTES:
        raise RuntimeError("browser screenshot exceeds the 5 MiB limit")
    if not image.startswith(_PNG_SIGNATURE):
        raise RuntimeError("browser screenshot is not a PNG image")
    atomic_create_bytes(output, image, mode=0o600)
    return {"saved": output, "bytes": len(image)}


def page_eval(tab_id: int, expr: str, page_url: str) -> dict[str, object]:
    return _page_action(
        "eval",
        "browser.eval",
        tab_id,
        page_url,
        expr=_required_text(expr, "expr"),
    )


def _page_action(
    action: str,
    capability: str,
    tab_id: int,
    page_url: str,
    **fields: object,
) -> dict[str, object]:
    tab_id = _tab_id(tab_id)
    _, scope = _browser_url(page_url)
    policy.require(capability, host=scope)
    return _bridge(action, tab_id=tab_id, page_url=page_url, **fields)
