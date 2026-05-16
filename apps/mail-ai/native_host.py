#!/usr/bin/env python3
"""Thunderbird Native Messaging host for the claw-mail-ai MailExtension.

Thunderbird spawns this script when the extension calls
``browser.runtime.connectNative("os.claw.mail_ai")``. We translate each
inbound NM frame into a ``cos app mail-ai <verb> …`` style invocation by
directly importing :mod:`apps.mail-ai.main`, then write the result back
out in the same NM framing (4-byte little-endian length prefix + JSON
body, per Chromium / Mozilla spec).

This host is intentionally simple and stateless: there is no socket
listener (unlike apps/browser-attached, where the agent side also
needs a way in), because for mail-ai the **only** caller is the
MailExtension itself. The extension is the user-driven half; the
Python verbs are pure compute.

Capability / budget / safety enforcement all happen inside
``main._ai_call()`` via ``cos_runtime.policy`` and ``claw_os_sdk.ai``, so this host
does not need its own gate.
"""

from __future__ import annotations

import json
import os
import struct
import sys
import traceback
from typing import Any, Dict, List


MAX_FRAME = 64 * 1024 * 1024  # 64 MiB — Mozilla's documented ceiling


# ---------------------------------------------------------------------------
# Make `from claw_os_sdk import …` resolve when running as a system script.
# ---------------------------------------------------------------------------
# Layout when shipped via the rootfs feature:
#     /usr/lib/cos/mail-ai/native_host.py            (this file)
#     /usr/lib/cos/mail-ai/main.py                   (copy of apps/mail-ai/main.py)
#     /usr/lib/cos/python/claw_os_sdk/               (system copy of the Python SDK)
# Layout when running from a source checkout (dev / tests):
#     <repo>/apps/mail-ai/native_host.py
#     <repo>/apps/mail-ai/main.py
#     <repo>/claw-os-sdk/python/src/claw_os_sdk/
# In both cases we want ``main.py`` importable as ``main`` and
# ``claw_os_sdk`` importable as a package. Prepend the script's own
# directory and walk up looking for a sibling ``claw-os-sdk/python/src``
# (dev) or a system-wide ``/usr/lib/cos/python`` (rootfs).
_HERE = os.path.dirname(os.path.abspath(__file__))
if _HERE not in sys.path:
    sys.path.insert(0, _HERE)


def _bootstrap_sdk_path() -> None:
    candidates = []
    env_override = os.environ.get("CLAW_PYTHON_LIB")
    if env_override:
        candidates.append(env_override)
    # Walk up from this file looking for the source-checkout SDK path.
    cur = _HERE
    for _ in range(6):
        cur = os.path.dirname(cur)
        candidates.append(os.path.join(cur, "claw-os-sdk", "python", "src"))
    # System install paths.
    candidates.extend([
        "/usr/lib/cos/python",
        "/opt/claw/python",
        "/usr/lib/claw/python",
    ])
    for cand in candidates:
        if cand and os.path.isdir(os.path.join(cand, "claw_os_sdk")):
            if cand not in sys.path:
                sys.path.insert(0, cand)
            return


_bootstrap_sdk_path()

import main as mail_ai  # noqa: E402  — sys.path was just rewired


# ---------------------------------------------------------------------------
# NM framing
# ---------------------------------------------------------------------------

def _read_frame(stream) -> dict | None:
    hdr = stream.read(4)
    if not hdr or len(hdr) < 4:
        return None
    (length,) = struct.unpack("<I", hdr)
    if length == 0 or length > MAX_FRAME:
        return None
    body = stream.read(length)
    if body is None or len(body) < length:
        return None
    try:
        return json.loads(body.decode("utf-8"))
    except json.JSONDecodeError:
        return None


def _write_frame(stream, payload: dict) -> None:
    body = json.dumps(payload, ensure_ascii=False).encode("utf-8")
    stream.write(struct.pack("<I", len(body)))
    stream.write(body)
    stream.flush()


# ---------------------------------------------------------------------------
# Verb args → argparse argv
# ---------------------------------------------------------------------------
# The extension sends ``{id, verb, args: {…}}`` where args is a flat
# JSON object. The Python ``main.cmd_*`` handlers expect argv-style
# ``--key value`` strings. We translate here.

def _args_to_argv(args: Dict[str, Any]) -> List[str]:
    argv: List[str] = []
    for k, v in args.items():
        if v is None:
            continue
        flag = "--" + str(k).replace("_", "-")
        if isinstance(v, bool):
            if v:
                argv.append(flag)
            continue
        if isinstance(v, (dict, list)):
            argv.extend([flag, json.dumps(v, ensure_ascii=False)])
            continue
        argv.extend([flag, str(v)])
    return argv


def _dispatch(verb: str, args: Dict[str, Any]) -> Dict[str, Any]:
    if verb == "__schema__":
        return mail_ai.run("__schema__", [])
    if verb not in mail_ai.HANDLERS:
        return {"error": f"unknown verb: {verb}"}
    argv = _args_to_argv(args or {})
    return mail_ai.run(verb, argv)


# ---------------------------------------------------------------------------
# Main loop
# ---------------------------------------------------------------------------

def main() -> int:
    in_stream = sys.stdin.buffer
    out_stream = sys.stdout.buffer

    while True:
        req = _read_frame(in_stream)
        if req is None:
            # Thunderbird closed the port (extension unloaded or browser
            # shutdown). Exit cleanly.
            return 0

        rid = req.get("id") or ""
        verb = req.get("verb") or ""
        args = req.get("args") or {}

        try:
            result = _dispatch(verb, args if isinstance(args, dict) else {})
            if isinstance(result, dict) and "error" in result and "ok" not in result:
                _write_frame(out_stream, {"id": rid, "ok": False, "error": result["error"], "detail": result})
            else:
                _write_frame(out_stream, {"id": rid, "ok": True, "result": result})
        except Exception as exc:  # pragma: no cover  — last-resort guard
            tb = traceback.format_exc(limit=4)
            _write_frame(out_stream, {
                "id": rid,
                "ok": False,
                "error": f"native host crashed: {exc}",
                "traceback": tb,
            })


if __name__ == "__main__":
    if "--probe" in sys.argv:
        # Diagnostic helper invoked by tools/install-mail-ai.sh: prints
        # the schema as one JSON document so the operator can sanity-
        # check that the host loads and verbs are wired up.
        try:
            schema = _dispatch("__schema__", {})
            print(json.dumps({"ok": True, "schema": schema}, indent=2, ensure_ascii=False))
            sys.exit(0)
        except Exception as exc:
            print(json.dumps({"ok": False, "error": str(exc)}, indent=2))
            sys.exit(1)
    sys.exit(main())
