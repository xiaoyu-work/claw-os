#!/usr/bin/env python3
"""Chromium Native Messaging host for the daemon-owned browser provider."""

from __future__ import annotations

import json
import os
import select
import socket
import stat
import struct
import sys
from typing import BinaryIO

MAX_FRAME_BYTES = 8 * 1024 * 1024
_SOCKET_NAME = "claw-browser.sock"


def _read_exact(stream: BinaryIO | socket.socket, count: int) -> bytes:
    chunks = bytearray()
    while len(chunks) < count:
        if isinstance(stream, socket.socket):
            chunk = stream.recv(count - len(chunks))
        else:
            chunk = stream.read(count - len(chunks))
        if not chunk:
            raise EOFError
        chunks.extend(chunk)
    return bytes(chunks)


def _read_frame(stream: BinaryIO | socket.socket) -> dict:
    length = struct.unpack("<I", _read_exact(stream, 4))[0]
    if length == 0 or length > MAX_FRAME_BYTES:
        raise ValueError(f"invalid Native Messaging frame length: {length}")
    payload = _read_exact(stream, length)
    message = json.loads(payload.decode("utf-8"))
    if not isinstance(message, dict):
        raise ValueError("Native Messaging payload must be an object")
    return message


def _write_frame(stream: BinaryIO | socket.socket, message: dict) -> None:
    payload = json.dumps(message, separators=(",", ":")).encode("utf-8")
    if not payload or len(payload) > MAX_FRAME_BYTES:
        raise ValueError("Native Messaging response exceeds frame limit")
    frame = struct.pack("<I", len(payload)) + payload
    if isinstance(stream, socket.socket):
        stream.sendall(frame)
    else:
        stream.write(frame)
        stream.flush()


def _socket_path() -> str:
    runtime = os.environ.get("XDG_RUNTIME_DIR")
    if not runtime:
        raise RuntimeError("XDG_RUNTIME_DIR is required")
    uid = os.getuid()
    expected = f"/run/user/{uid}"
    resolved = os.path.realpath(runtime)
    if resolved != expected:
        raise RuntimeError(f"XDG_RUNTIME_DIR must resolve to {expected}")
    info = os.stat(resolved, follow_symlinks=False)
    if not stat.S_ISDIR(info.st_mode) or info.st_uid != uid or info.st_mode & 0o077:
        raise RuntimeError("XDG_RUNTIME_DIR must be an owner-only directory")
    return os.path.join(resolved, _SOCKET_NAME)


def _socket_alive(path: str) -> bool:
    probe = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    probe.settimeout(0.2)
    try:
        probe.connect(path)
        return True
    except OSError:
        return False
    finally:
        probe.close()


def _peer_uid(conn: socket.socket) -> int:
    if not hasattr(socket, "SO_PEERCRED"):
        raise RuntimeError("SO_PEERCRED is unavailable")
    credentials = conn.getsockopt(
        socket.SOL_SOCKET,
        socket.SO_PEERCRED,
        struct.calcsize("3i"),
    )
    _, uid, _ = struct.unpack("3i", credentials)
    return uid


def _validate_request(request: dict) -> None:
    if set(request) != {"id", "verb", "args"}:
        raise ValueError("browser bridge request has unexpected fields")
    if not isinstance(request["id"], str) or not request["id"] or len(request["id"]) > 128:
        raise ValueError("browser bridge request id is invalid")
    if not isinstance(request["verb"], str) or not request["verb"] or len(request["verb"]) > 128:
        raise ValueError("browser bridge request verb is invalid")
    if not isinstance(request["args"], dict):
        raise ValueError("browser bridge request args must be an object")


def _handle_client(conn: socket.socket) -> None:
    try:
        request = _read_frame(conn)
        _validate_request(request)
        _write_frame(sys.stdout.buffer, request)
        response = _read_frame(sys.stdin.buffer)
        if response.get("id") != request["id"]:
            raise ValueError("extension response id does not match request")
        _write_frame(conn, response)
    except EOFError:
        raise
    except Exception as exc:
        try:
            _write_frame(conn, {"id": "", "ok": False, "error": str(exc)})
        except Exception:
            pass


def _serve_bridge(path: str) -> None:
    if os.path.lexists(path):
        if _socket_alive(path):
            raise RuntimeError("attached browser Native Messaging host is already running")
        os.unlink(path)

    listener = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    try:
        listener.bind(path)
        os.chmod(path, 0o600)
        listener.listen(8)
        while True:
            readable, _, _ = select.select([listener, sys.stdin.buffer], [], [])
            if sys.stdin.buffer in readable:
                if sys.stdin.buffer.read(1) == b"":
                    return
                raise RuntimeError("unsolicited Native Messaging input")

            conn, _ = listener.accept()
            with conn:
                try:
                    if _peer_uid(conn) != 0:
                        continue
                    _handle_client(conn)
                except EOFError:
                    return
    finally:
        listener.close()
        try:
            os.unlink(path)
        except FileNotFoundError:
            pass


def main() -> None:
    _serve_bridge(_socket_path())


if __name__ == "__main__":
    try:
        main()
    except Exception as exc:
        print(f"claw-browser-host: {exc}", file=sys.stderr)
        raise SystemExit(1)
