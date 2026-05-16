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


def _resolve_sock_path() -> str:
    """Resolve the bridge socket path, refusing to fall back to /tmp.

    SECURITY: prior versions silently fell back to
    ``/tmp/claw-browser.sock`` when ``XDG_RUNTIME_DIR`` was unset.
    ``/tmp`` is world-writable, opening the bridge to local-user
    spoofing attacks (another user pre-creates the path as a
    symlink they control, then races our ``os.unlink`` /
    ``socket.bind`` to land the socket under their tree).
    """
    override = os.environ.get("CLAW_BROWSER_SOCK")
    if override:
        return override
    runtime = os.environ.get("XDG_RUNTIME_DIR")
    if not runtime:
        raise RuntimeError(
            "$XDG_RUNTIME_DIR is not set and CLAW_BROWSER_SOCK is not "
            "overridden — refusing to fall back to /tmp. Start the "
            "host inside a user session (loginctl ensures XDG_RUNTIME_DIR)."
        )
    return os.path.join(runtime, "claw-browser.sock")


# Resolved lazily: import-time errors would break ``--probe`` paths
# in dev. Callers that need the value should call _resolve_sock_path()
# directly so the failure surfaces at the use site.
try:
    SOCK_PATH = _resolve_sock_path()
except RuntimeError:
    SOCK_PATH = ""

# Drop the per-frame ceiling from 64 MiB to 8 MiB. Browser-bridge
# traffic is small JSON RPC: tab metadata, click/fill requests, a
# truncated DOM snapshot, etc. The largest legitimate frame is a
# page screenshot, which the extension chunks. 64 MiB was an
# allocation grenade waiting for a single bogus length header.
MAX_FRAME = 8 * 1024 * 1024


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
        self._extension_alive = True

    def submit(self, request: dict, timeout: float) -> dict:
        rid = request.get("id") or uuid.uuid4().hex
        request["id"] = rid
        q: queue.Queue = queue.Queue(maxsize=1)
        with self._lock:
            if not self._extension_alive:
                return {"id": rid, "ok": False, "error": "extension is not connected"}
            self._pending[rid] = q

        try:
            try:
                with self._ext_lock:
                    _nm_write(self._ext_stdout, request)
            except OSError as exc:
                return {"id": rid, "ok": False, "error": f"failed to send to extension: {exc}"}
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

    def fail_all_pending(self, reason: str) -> None:
        """Mark the extension as dead and wake every blocked caller.

        Called when the extension reader thread observes the stdio
        port has closed. Without this, in-flight ``submit`` calls
        would each wait the full ``timeout`` window before returning
        "extension did not respond" — wasting up to 45s per stuck
        request.
        """
        with self._lock:
            self._extension_alive = False
            pending = list(self._pending.items())
            self._pending.clear()
        for rid, q in pending:
            try:
                q.put_nowait({"id": rid, "ok": False, "error": reason})
            except queue.Full:
                pass


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
    # Extension stdio closed: wake every blocked socket caller so they
    # don't sit on their 45 s timeout window pointlessly.
    bridge.fail_all_pending("extension disconnected")
    shutdown.set()


def serve_socket(bridge: Bridge, shutdown: threading.Event) -> None:
    """Accept Unix-socket connections from cos app browser-attached callers."""
    try:
        sock_path = _resolve_sock_path()
    except RuntimeError as exc:
        sys.stderr.write(f"native_host: refusing to bind socket: {exc}\n")
        shutdown.set()
        return

    # If a previous instance left a socket behind, only remove it
    # after confirming it's dead — never unconditionally ``os.unlink``
    # a path that could have been swapped in by another user.
    if os.path.exists(sock_path):
        if _socket_alive(sock_path):
            sys.stderr.write(
                f"native_host: socket {sock_path} is alive; refusing to start a second host\n"
            )
            shutdown.set()
            return
        try:
            os.unlink(sock_path)
        except FileNotFoundError:
            pass
        except OSError as exc:
            sys.stderr.write(f"native_host: cannot clean stale socket {sock_path}: {exc}\n")
            shutdown.set()
            return

    parent = os.path.dirname(sock_path) or "."
    os.makedirs(parent, exist_ok=True)

    srv = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    try:
        srv.bind(sock_path)
    except OSError as exc:
        sys.stderr.write(f"native_host: bind({sock_path}) failed: {exc}\n")
        shutdown.set()
        return
    os.chmod(sock_path, stat.S_IRUSR | stat.S_IWUSR)
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
            os.unlink(sock_path)
        except FileNotFoundError:
            pass


def _socket_alive(path: str) -> bool:
    """Best-effort liveness probe for an existing socket file.

    Tries a non-blocking connect; if it succeeds the socket has a
    process bound to it and we must not unlink it. If the connect
    fails with ECONNREFUSED / FileNotFoundError we treat it as
    stale and safe to remove.
    """
    try:
        s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    except OSError:
        return False
    try:
        s.settimeout(0.2)
        try:
            s.connect(path)
            return True
        except (ConnectionRefusedError, FileNotFoundError):
            return False
        except OSError:
            # Any other error: assume alive and refuse to unlink. Safer
            # to fail to start than to risk hijacking another user's
            # socket.
            return True
    finally:
        try:
            s.close()
        except OSError:
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
