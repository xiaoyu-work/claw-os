"""cos browser-attached — drive the user's running Chromium via a WebExtension.

The actual browser work happens inside the user's logged-in Chromium tabs (so
cookies, SSO, MFA, etc. are reused). This Python app is the kernel-facing side
of the bridge:

    cos app browser-attached <verb>      (this file)
        |
        v  Unix-domain socket  $XDG_RUNTIME_DIR/claw-browser.sock
    native_host.py            (spawned by Chromium per WebExtension load)
        |
        v  Chromium Native Messaging (stdio, 4-byte LE length-prefixed JSON)
    background service worker  (extensions/claw-agent-browser)
        |
        v  chrome.* APIs / content scripts
    user's tabs

Every verb here runs ``policy.require()`` *before* the request leaves the box.
"""

from __future__ import annotations

import base64
import json
import os
import socket
import struct
import sys
import urllib.parse
import uuid

from cos_runtime import memory, policy


def _resolve_sock_path() -> str:
    """Resolve the bridge socket path, refusing to fall back to /tmp.

    Matches the policy in ``native_host.py``: ``XDG_RUNTIME_DIR``
    must be set, else we error out instead of touching a world-
    writable directory.
    """
    override = os.environ.get("CLAW_BROWSER_SOCK")
    if override:
        return override
    runtime = os.environ.get("XDG_RUNTIME_DIR")
    if not runtime:
        raise RuntimeError(
            "$XDG_RUNTIME_DIR is not set and CLAW_BROWSER_SOCK is not "
            "overridden — refusing to fall back to /tmp."
        )
    return os.path.join(runtime, "claw-browser.sock")


try:
    SOCK_PATH = _resolve_sock_path()
except RuntimeError:
    SOCK_PATH = ""

TIMEOUT_S = int(os.environ.get("CLAW_BROWSER_TIMEOUT", "30"))
MAX_FRAME = 8 * 1024 * 1024  # 8 MiB; matches native_host.py


# ---------------------------------------------------------------------------
# Socket transport
# ---------------------------------------------------------------------------

def _send_request(verb: str, args: dict, timeout: float = TIMEOUT_S) -> dict:
    """Send a request to the native host over the Unix socket and read one reply.

    Wire format on the socket (same framing as Chromium Native Messaging):
        [4 bytes little-endian length][UTF-8 JSON body]
    """
    if not SOCK_PATH:
        return {
            "ok": False,
            "error": (
                "$XDG_RUNTIME_DIR is unset — refusing to talk to /tmp. "
                "Run the agent inside an interactive user session."
            ),
        }
    if not os.path.exists(SOCK_PATH):
        return {
            "ok": False,
            "error": (
                f"browser bridge socket not found at {SOCK_PATH}. "
                "Open Chromium with the Claw Agent extension installed first."
            ),
        }

    msg = json.dumps(
        {"id": uuid.uuid4().hex, "verb": verb, "args": args},
        ensure_ascii=False,
    ).encode("utf-8")

    s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    s.settimeout(timeout)
    try:
        s.connect(SOCK_PATH)
        s.sendall(struct.pack("<I", len(msg)) + msg)

        hdr = _recv_exact(s, 4)
        if hdr is None:
            return {"ok": False, "error": "bridge closed connection before responding"}
        (length,) = struct.unpack("<I", hdr)
        if length == 0 or length > MAX_FRAME:
            return {"ok": False, "error": f"bridge returned implausible frame size {length}"}
        body = _recv_exact(s, length)
        if body is None:
            return {"ok": False, "error": "bridge truncated its response"}
        try:
            return json.loads(body.decode("utf-8"))
        except json.JSONDecodeError as exc:
            return {"ok": False, "error": f"bridge returned non-JSON: {exc}"}
    except OSError as exc:
        return {"ok": False, "error": f"bridge socket I/O error: {exc}"}
    finally:
        try:
            s.close()
        except OSError:
            pass


def _recv_exact(sock: socket.socket, n: int) -> bytes | None:
    buf = bytearray()
    while len(buf) < n:
        chunk = sock.recv(n - len(buf))
        if not chunk:
            return None
        buf.extend(chunk)
    return bytes(buf)


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

def _host_of(url: str) -> str:
    try:
        parsed = urllib.parse.urlparse(url)
    except ValueError:
        return ""
    return (parsed.hostname or "").lower()


def _tab_id(args_value: str) -> int:
    try:
        return int(args_value)
    except (TypeError, ValueError):
        raise ValueError(f"tab id must be an integer, got {args_value!r}")


def _unwrap(reply: dict) -> dict:
    """Standardise bridge replies into the cos-app result envelope."""
    if not isinstance(reply, dict):
        return {"ok": False, "error": f"bridge returned non-object: {reply!r}"}
    if "ok" in reply:
        return reply
    if "error" in reply:
        return {"ok": False, "error": reply["error"]}
    return {"ok": True, "result": reply}


# ---------------------------------------------------------------------------
# Verb handlers
# ---------------------------------------------------------------------------

def _require_host(verb: str, host: str) -> None:
    """Run ``policy.require(verb, host=...)`` after refusing an empty
    or ``None`` host.

    SECURITY: ``policy.require("browser.nav", host="")`` silently
    matches the wild grant on some kernels, opening every host. We
    refuse that case at the gate so a bogus URL never leaks past the
    cap check.
    """
    if not host or not isinstance(host, str):
        raise policy.PermissionDenied(
            {
                "decision": "deny",
                "summary": f"refusing {verb}: no host could be derived",
                "verb": verb,
                "reason": "empty-host",
            }
        )
    policy.require(verb, host=host)


def _cmd_tabs_list(_argv):
    policy.require("browser.tabs.read", wild=True)
    return _unwrap(_send_request("tabs.list", {}))


def _cmd_tabs_activate(argv):
    args = _parse_kv(argv, required=("id",))
    policy.require("browser.tabs.read", wild=True)
    return _unwrap(_send_request("tabs.activate", {"id": _tab_id(args["id"])}))


def _cmd_nav_go(argv):
    args = _parse_kv(argv, required=("id", "url"))
    host = _host_of(args["url"])
    if not host:
        return {"ok": False, "error": f"could not parse a host out of url={args['url']!r}"}
    _require_host("browser.nav", host)
    result = _unwrap(
        _send_request("nav.go", {"id": _tab_id(args["id"]), "url": args["url"]})
    )
    if isinstance(result, dict) and result.get("ok", True) and "error" not in result:
        _remember_nav(args["id"], args["url"], host)
    return result


def _remember_nav(tab_id, url, host):
    try:
        memory.remember(
            source="browser-attached",
            text=f"Navigated tab {tab_id} to {url}",
            kind="event",
            entity_id=url,
            tags=["browser", "nav", host],
            link=f"cos app browser-attached page.snapshot --id {tab_id}",
        )
    except memory.MemoryError:
        pass


def _cmd_dom_query(argv):
    args = _parse_kv(argv, required=("id", "selector"))
    info = _send_request("tabs.info", {"id": _tab_id(args["id"])})
    host = (info.get("result") or {}).get("host") or info.get("host") or ""
    _require_host("browser.dom.read", host)
    return _unwrap(
        _send_request(
            "dom.query",
            {"id": _tab_id(args["id"]), "selector": args["selector"]},
        )
    )


def _cmd_dom_click(argv):
    args = _parse_kv(argv, required=("id", "ref"))
    info = _send_request("tabs.info", {"id": _tab_id(args["id"])})
    host = (info.get("result") or {}).get("host") or ""
    _require_host("browser.dom.write", host)
    return _unwrap(
        _send_request("dom.click", {"id": _tab_id(args["id"]), "ref": args["ref"]})
    )


def _cmd_dom_fill(argv):
    args = _parse_kv(argv, required=("id", "ref", "value"))
    info = _send_request("tabs.info", {"id": _tab_id(args["id"])})
    host = (info.get("result") or {}).get("host") or ""
    _require_host("browser.dom.write", host)
    return _unwrap(
        _send_request(
            "dom.fill",
            {
                "id": _tab_id(args["id"]),
                "ref": args["ref"],
                "value": args["value"],
                "allow_secret": False,
            },
        )
    )


def _cmd_dom_fill_secret(argv):
    args = _parse_kv(argv, required=("id", "ref", "value"))
    info = _send_request("tabs.info", {"id": _tab_id(args["id"])})
    host = (info.get("result") or {}).get("host") or ""
    _require_host("browser.input.secret", host)
    return _unwrap(
        _send_request(
            "dom.fill",
            {
                "id": _tab_id(args["id"]),
                "ref": args["ref"],
                "value": args["value"],
                "allow_secret": True,
            },
        )
    )


def _cmd_page_snapshot(argv):
    args = _parse_kv(argv, required=("id",), optional=("kind",))
    info = _send_request("tabs.info", {"id": _tab_id(args["id"])})
    host = (info.get("result") or {}).get("host") or ""
    _require_host("browser.dom.read", host)
    return _unwrap(
        _send_request(
            "page.snapshot",
            {"id": _tab_id(args["id"]), "kind": args.get("kind", "ax")},
        )
    )


def _cmd_page_screenshot(argv):
    args = _parse_kv(argv, required=("id", "output"))
    info = _send_request("tabs.info", {"id": _tab_id(args["id"])})
    host = (info.get("result") or {}).get("host") or ""
    _require_host("browser.dom.read", host)
    # ``realpath`` so a symlink under the output dir can't redirect
    # the screenshot write somewhere the caller doesn't have fs.write
    # on. Matches the fs app's symlink-safety policy.
    out_path = os.path.realpath(args["output"])
    policy.require("fs.write", path=out_path)

    reply = _send_request("page.screenshot", {"id": _tab_id(args["id"])})
    if not reply or reply.get("ok") is False:
        return _unwrap(reply or {})
    data_b64 = (reply.get("result") or {}).get("png_base64")
    if not data_b64:
        return {"ok": False, "error": "bridge did not return png_base64"}
    try:
        png = base64.b64decode(data_b64)
    except (ValueError, TypeError) as exc:
        return {"ok": False, "error": f"could not decode screenshot base64: {exc}"}
    try:
        parent = os.path.dirname(out_path)
        if parent:
            os.makedirs(parent, exist_ok=True)
        with open(out_path, "wb") as fh:
            fh.write(png)
    except OSError as exc:
        return {"ok": False, "error": f"could not write screenshot: {exc}"}
    return {"ok": True, "result": {"output": out_path, "bytes": len(png)}}


def _cmd_eval(argv):
    args = _parse_kv(argv, required=("id", "expr"))
    info = _send_request("tabs.info", {"id": _tab_id(args["id"])})
    host = (info.get("result") or {}).get("host") or ""
    _require_host("browser.eval", host)
    return _unwrap(
        _send_request("eval", {"id": _tab_id(args["id"]), "expr": args["expr"]})
    )


# ---------------------------------------------------------------------------
# Tiny --key=value argv parser
# ---------------------------------------------------------------------------

def _parse_kv(argv, required=(), optional=()):
    out: dict[str, str] = {}
    positionals: list[str] = []
    for token in argv:
        if token.startswith("--"):
            if "=" in token:
                k, v = token[2:].split("=", 1)
            else:
                k, v = token[2:], "true"
            out[k] = v
        else:
            positionals.append(token)
    for name, value in zip(required, positionals):
        out.setdefault(name, value)
    for name, value in zip(optional, positionals[len(required):]):
        out.setdefault(name, value)
    for name in required:
        if name not in out:
            raise ValueError(f"missing required arg --{name}")
    return out


# ---------------------------------------------------------------------------
# Entry point
# ---------------------------------------------------------------------------

HANDLERS = {
    "tabs.list": _cmd_tabs_list,
    "tabs.activate": _cmd_tabs_activate,
    "nav.go": _cmd_nav_go,
    "dom.query": _cmd_dom_query,
    "dom.click": _cmd_dom_click,
    "dom.fill": _cmd_dom_fill,
    "dom.fill_secret": _cmd_dom_fill_secret,
    "page.snapshot": _cmd_page_snapshot,
    "page.screenshot": _cmd_page_screenshot,
    "eval": _cmd_eval,
}


def run(command, argv):
    from canonical_argv import parse_canonical_argv
    try:
        argv, _ = parse_canonical_argv(argv)
    except ValueError as error:
        return {"error": str(error)}
    handler = HANDLERS.get(command)
    if handler is None:
        return {"ok": False, "error": f"unknown command: {command}"}
    try:
        return handler(argv)
    except policy.PermissionDenied as denied:
        return {"ok": False, "error": str(denied), "denial": denied.denial}
    except policy.PolicyUnavailable as exc:
        return {"ok": False, "error": f"capability check failed: {exc}"}
    except ValueError as exc:
        return {"ok": False, "error": str(exc)}


def main():
    argv = sys.argv[1:]
    if not argv:
        print(json.dumps({
            "ok": False,
            "error": "usage: cos app browser-attached <verb> [args...]",
            "commands": sorted(HANDLERS),
        }))
        sys.exit(1)
    cmd, rest = argv[0], argv[1:]
    result = run(cmd, rest)
    print(json.dumps(result, indent=2, ensure_ascii=False))
    if isinstance(result, dict) and result.get("ok") is False:
        sys.exit(1)


if __name__ == "__main__":
    main()
