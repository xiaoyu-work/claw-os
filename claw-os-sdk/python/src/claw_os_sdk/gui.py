"""Desktop GUI bootstrap for Claw OS apps.

This is the GUI counterpart to :mod:`claw_os_sdk.serve` (agent tools) and
:mod:`claw_os_sdk.ai` (model access). It does **not** wrap a UI toolkit:
a Claw OS desktop app draws its own window in whatever toolkit/language
it likes ("World A"). All this module does is hand the app the small
amount of kernel context it needs once it has been kernel-spawned, plus
the one privileged action a GUI commonly wants — summoning the system
agent overlay.

## How a GUI app is launched

When an app declares a ``desktop`` block in ``app.json``, ``cos app
install`` writes a launcher whose ``Exec`` is ``cos app <id> --gui``.
Clicking it routes the launch back through the kernel, which spawns the
app's entry with ``COS_APP_GUI=1`` and ``COS_APP_ID`` set. Routing the
launch through ``cos app`` (rather than exec-ing the binary) is what
makes identity, audit, and consent apply to the GUI exactly as they do
to the headless operation path.

## Author surface

A Python desktop app keeps the same ``run(command, args)`` entry point
it uses for one-shot operations and branches on the GUI launch::

    from claw_os_sdk import gui

    def run(command, args):
        if gui.is_gui_launch(command):
            ctx = gui.context(files=args)
            start_my_window(ctx)        # your toolkit, your loop
            return None
        # ... handle one-shot operations here ...

``ctx`` exposes:

* ``ctx.app_id``  — the kernel-assigned app identity.
* ``ctx.files``   — file paths the launcher passed (``%F``), if any.
* ``ctx.ai``      — the :mod:`claw_os_sdk.ai` module (gated model access).
* ``ctx.tools``   — the :mod:`claw_os_sdk.tools` module (call other apps).
* ``ctx.open_agent_overlay(hint=...)`` — summon the "Ask Claw" overlay.

The app's own files, sockets, and rendering are World A: use plain
stdlib for them. The kernel only interposes when you reach for ``ai`` /
``tools`` or a declared capability verb.
"""

from __future__ import annotations

import json
import os
import select
import socket
import stat
import struct
import subprocess
import threading
from dataclasses import dataclass, field
from typing import Any, List, Optional

from . import ai, tools

#: Command value the bridge passes (and the default ``desktop.exec``) when
#: an app is launched as a GUI. Authors can override ``desktop.exec`` in
#: their manifest; :func:`is_gui_launch` also honours ``COS_APP_GUI``.
GUI_COMMAND = "--gui"
ASK_CLAW_LAUNCHER = "/usr/local/bin/cos-ask-claw-launcher"
ASK_CLAW_PROTOCOL = 1
ASK_CLAW_REQUEST_LIMIT = 32 * 1024


def is_gui_launch(command: Optional[str] = None) -> bool:
    """Return ``True`` when the current invocation is a desktop GUI launch.

    Detection prefers the ``COS_APP_GUI`` environment variable the bridge
    sets for the long-lived GUI process. As a fallback (and so apps with a
    custom ``desktop.exec`` still work) it also treats a ``command`` equal
    to :data:`GUI_COMMAND` as a GUI launch.
    """
    if os.environ.get("COS_APP_GUI") == "1":
        return True
    return command is not None and command == GUI_COMMAND


@dataclass
class GuiContext:
    """The kernel context handed to a desktop app at launch.

    Construct with :func:`context`; do not instantiate directly in app
    code unless you are writing a test.
    """

    app_id: str
    files: List[str] = field(default_factory=list)
    #: The gated model-access module (:mod:`claw_os_sdk.ai`).
    ai: Any = ai
    #: The cross-app verb-call module (:mod:`claw_os_sdk.tools`).
    tools: Any = tools

    def open_agent_overlay(self, hint: Optional[str] = None) -> None:
        """Summon the system "Ask Claw" agent overlay.

        This uses the fixed packaged launcher's authenticated Unix-socket
        protocol. Pass ``hint`` to ground the agent's first response
        in the app's current state (e.g. the open document) without
        polluting the visible chat transcript.

        Raises :class:`FileNotFoundError` if the overlay binary is not
        installed (e.g. a headless box with no desktop shell).
        """
        _validate_launcher()
        child = subprocess.Popen(  # noqa: S603 - fixed trusted absolute path
            [ASK_CLAW_LAUNCHER, "--protocol", "1"],
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
        )
        try:
            assert child.stdout is not None
            announced, _, _ = select.select([child.stdout], [], [], 5)
            announcement = child.stdout.readline(257) if announced else b""
            prefix = f"SOCKET {ASK_CLAW_PROTOCOL} @".encode()
            if (
                len(announcement) > 256
                or not announcement.startswith(prefix)
                or not announcement.endswith(b"\n")
            ):
                raise RuntimeError("invalid Ask Claw socket announcement")
            endpoint = announcement[len(prefix) : -1]
            if not endpoint:
                raise RuntimeError("empty Ask Claw socket endpoint")
            request = {"protocol": ASK_CLAW_PROTOCOL, "app": self.app_id, "hint": hint}
            payload = json.dumps(request, separators=(",", ":")).encode()
            if len(payload) > ASK_CLAW_REQUEST_LIMIT:
                raise ValueError("Ask Claw request exceeds the protocol limit")
            with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as channel:
                channel.settimeout(5)
                channel.connect(b"\0" + endpoint)
                if _read_exact(channel, 8) != b"READY 1\n":
                    raise RuntimeError("unexpected Ask Claw launcher handshake")
                channel.sendall(struct.pack(">I", len(payload)) + payload)
                channel.shutdown(socket.SHUT_WR)
                if _read_exact(channel, 11) != b"ACCEPTED 1\n":
                    raise RuntimeError("unexpected Ask Claw acceptance response")
        except Exception:
            child.kill()
            child.wait()
            raise
        try:
            threading.Thread(
                target=child.wait, name="ask-claw-sdk-reaper", daemon=True
            ).start()
        except Exception:
            child.kill()
            child.wait()
            raise


def _read_exact(channel: socket.socket, length: int) -> bytes:
    chunks = bytearray()
    while len(chunks) < length:
        chunk = channel.recv(length - len(chunks))
        if not chunk:
            raise RuntimeError("Ask Claw launcher closed the socket")
        chunks.extend(chunk)
    return bytes(chunks)


def _validate_launcher() -> None:
    for path in ("/usr", "/usr/local", "/usr/local/bin"):
        info = os.lstat(path)
        if (
            stat.S_ISLNK(info.st_mode)
            or not stat.S_ISDIR(info.st_mode)
            or info.st_uid != 0
            or info.st_mode & 0o022
        ):
            raise PermissionError(f"untrusted Ask Claw launcher parent: {path}")
    info = os.lstat(ASK_CLAW_LAUNCHER)
    if (
        stat.S_ISLNK(info.st_mode)
        or not stat.S_ISREG(info.st_mode)
        or info.st_uid != 0
        or not info.st_mode & 0o111
        or info.st_mode & 0o022
    ):
        raise PermissionError("untrusted Ask Claw launcher")


def context(files: Optional[List[str]] = None) -> GuiContext:
    """Build the :class:`GuiContext` for the current GUI launch.

    ``app_id`` is read from ``COS_APP_ID`` (set by the kernel when it
    spawns the GUI). ``files`` defaults to the launcher's file arguments,
    decoded from ``COS_ARGS_JSON`` when not supplied explicitly.
    """
    app_id = os.environ.get("COS_APP_ID") or "unknown"
    if files is None:
        files = _files_from_env()
    return GuiContext(app_id=app_id, files=list(files))


def _files_from_env() -> List[str]:
    raw = os.environ.get("COS_ARGS_JSON")
    if not raw:
        return []
    try:
        parsed = json.loads(raw)
    except json.JSONDecodeError:
        return []
    if isinstance(parsed, list):
        return [str(x) for x in parsed]
    return []
