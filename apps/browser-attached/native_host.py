#!/usr/bin/env python3
"""Chromium Native Messaging host for the Claw Agent WebExtension.

Spawned by Chromium when the extension calls ``chrome.runtime.connectNative``.
Once running, it serves two channels:

    stdin  / stdout  — framed JSON to/from the extension service worker
                       (4-byte little-endian length prefix per Chromium's spec)
    AF_UNIX socket   — same framing, accepts requests from
                       ``cos app browser-attached`` invocations

The host is dumb-pipe by design: every request is forwarded to the extension,
which is the only side that talks to actual browser APIs.  Capability checks
happen in ``cos app browser-attached`` (Python side) *before* a request ever
reaches this host.

This host implicitly trusts:
  - the extension (because Chromium only spawned us because our manifest at
    ``/etc/chromium/native-messaging-hosts/com.clawos.browser.json`` listed
    that extension's ID in ``allowed_origins``), and
  - the local socket caller (it must be the same UID, because the socket is
    chmod 0600 inside ``$XDG_RUNTIME_DIR``).
"""

from __future__ import annotations

import json
import os
import queue
import socket
import stat
import struct
import sys
import threading
import time
import uuid


SOCK_PATH = os.environ.get(
    "CLAW_BROWSER_SOCK",
    os.path.join(os.environ.get("XDG_RUNTIME_DIR", "/tmp"), "claw-browser.sock"),
)
MAX_FRAME = 64 * 1024 * 1024


# ---------------------------------------------------------------------------
# Native Messaging framing (Chromium stdio)
# ---------------------------------------------------------------------------

def _nm_read(stream) -> dict | None:
    hdr = stream.read(4)
    if not hdr or len(hdr) < 4:
        return None
    (length,) = struct.unpack("<I", hdr)
    if length == 0 or length > MAX_FRAME:
        return None
    body = stream.read(length)
    if not body or len(body) < length:
        return None
    try:
        return json.loads(body.decode("utf-8"))
    except json.JSONDecodeError:
        return None


def _nm_write(stream, payload: dict) -> None:
    body = json.dumps(payload, ensure_ascii=False).encode("utf-8")
    stream.write(struct.pack("<I", len(body)))
    stream.write(body)
    stream.flush()


# ---------------------------------------------------------------------------
# Pending request table
# ---------------------------------------------------------------------------

class Bridge:
    """Tracks in-flight requests so reader thread can route replies to sockets."""

    def __init__(self):
        self._lock = threading.Lock()
        self._pending: dict[str, queue.Queue] = {}
        self._ext_stdin = sys.stdin.buffer
        self._ext_stdout = sys.stdout.buffer
        self._ext_lock = threading.Lock()

    def submit(self, request: dict, timeout: float) -> dict:
        rid = request.get("id") or uuid.uuid4().hex
        request["id"] = rid
        q: queue.Queue = queue.Queue(maxsize=1)
        with self._lock:
            self._pending[rid] = q

        try:
            with self._ext_lock:
                _nm_write(self._ext_stdout, request)
            try:
                return q.get(timeout=timeout)
            except queue.Empty:
                return {"id": rid, "ok": False, "error": "extension did not respond in time"}
        finally:
            with self._lock:
                self._pending.pop(rid, None)

    def deliver(self, response: dict) -> bool:
        rid = response.get("id")
        if not rid:
            return False
        with self._lock:
            q = self._pending.get(rid)
        if q is None:
            return False
        try:
            q.put_nowait(response)
        except queue.Full:
            return False
        return True


# ---------------------------------------------------------------------------
# Threads
# ---------------------------------------------------------------------------

def extension_reader(bridge: Bridge, shutdown: threading.Event) -> None:
    """Read frames from Chromium stdin and route them to waiting callers."""
    while not shutdown.is_set():
        try:
            msg = _nm_read(bridge._ext_stdin)
        except (OSError, ValueError):
            break
        if msg is None:
            break
        delivered = bridge.deliver(msg)
        if not delivered:
            # Unsolicited / late event from the extension.  Nothing to do for
            # MVP — a future version may broadcast to subscribers.
            continue
    shutdown.set()


def serve_socket(bridge: Bridge, shutdown: threading.Event) -> None:
    """Accept Unix-socket connections from cos app browser-attached callers."""
    try:
        os.unlink(SOCK_PATH)
    except FileNotFoundError:
        pass

    parent = os.path.dirname(SOCK_PATH) or "."
    os.makedirs(parent, exist_ok=True)

    srv = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    srv.bind(SOCK_PATH)
    os.chmod(SOCK_PATH, stat.S_IRUSR | stat.S_IWUSR)
    srv.listen(8)
    srv.settimeout(0.5)

    try:
        while not shutdown.is_set():
            try:
                conn, _ = srv.accept()
            except socket.timeout:
                continue
            except OSError:
                break
            threading.Thread(
                target=_handle_socket_client,
                args=(conn, bridge),
                daemon=True,
            ).start()
    finally:
        try:
            srv.close()
        except OSError:
            pass
        try:
            os.unlink(SOCK_PATH)
        except FileNotFoundError:
            pass


def _handle_socket_client(conn: socket.socket, bridge: Bridge) -> None:
    try:
        conn.settimeout(60)
        hdr = _recv_exact(conn, 4)
        if hdr is None:
            return
        (length,) = struct.unpack("<I", hdr)
        if length == 0 or length > MAX_FRAME:
            return
        body = _recv_exact(conn, length)
        if body is None:
            return
        try:
            request = json.loads(body.decode("utf-8"))
        except json.JSONDecodeError as exc:
            response = {"ok": False, "error": f"bad request JSON: {exc}"}
        else:
            response = bridge.submit(request, timeout=45.0)

        out = json.dumps(response, ensure_ascii=False).encode("utf-8")
        conn.sendall(struct.pack("<I", len(out)) + out)
    except OSError:
        return
    finally:
        try:
            conn.close()
        except OSError:
            pass


def _recv_exact(sock: socket.socket, n: int) -> bytes | None:
    buf = bytearray()
    while len(buf) < n:
        try:
            chunk = sock.recv(n - len(buf))
        except socket.timeout:
            return None
        if not chunk:
            return None
        buf.extend(chunk)
    return bytes(buf)


# ---------------------------------------------------------------------------
# main
# ---------------------------------------------------------------------------

def main() -> int:
    bridge = Bridge()
    shutdown = threading.Event()

    t_sock = threading.Thread(
        target=serve_socket, args=(bridge, shutdown), daemon=True
    )
    t_sock.start()

    extension_reader(bridge, shutdown)

    shutdown.set()
    time.sleep(0.1)
    return 0


if __name__ == "__main__":
    sys.exit(main())
