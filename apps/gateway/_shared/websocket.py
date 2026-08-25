"""Small, policy-gated RFC 6455 client for gateway adapters.

The gateway apps deliberately avoid third-party runtime dependencies. This
module supplies the subset of WebSocket needed by Discord Gateway and Slack
Socket Mode while preserving the egress guarantees in :mod:`safe_egress`:
direct DNS-pinned TLS, no environment proxy, and a kernel ``net.dial`` check.
"""

from __future__ import annotations

import base64
import hashlib
import json
import secrets
import socket
import struct
import threading
import urllib.parse
from typing import Any, Mapping

from . import safe_egress


_GUID = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11"
_MAX_HTTP_HEADERS = 64 * 1024
_DEFAULT_MAX_MESSAGE = 1024 * 1024


class WebSocketError(Exception):
    """Base class for WebSocket transport errors."""


class WebSocketProtocolError(WebSocketError):
    """The peer sent an invalid RFC 6455 handshake or frame."""


class WebSocketClosed(WebSocketError):
    """The peer sent a close frame or closed the underlying socket."""

    def __init__(self, code: int = 1006, reason: str = ""):
        self.code = code
        self.reason = reason
        suffix = f": {reason}" if reason else ""
        super().__init__(f"websocket closed ({code}){suffix}")


class WebSocketClient:
    """Connected WebSocket with JSON convenience methods."""

    def __init__(
        self,
        sock: socket.socket,
        *,
        initial_bytes: bytes = b"",
        max_message_bytes: int = _DEFAULT_MAX_MESSAGE,
    ):
        self._sock = sock
        self._buffer = bytearray(initial_bytes)
        self._max_message_bytes = max_message_bytes
        self._send_lock = threading.Lock()
        self._close_sent = False

    def fileno(self) -> int:
        return self._sock.fileno()

    def settimeout(self, timeout: float) -> None:
        self._sock.settimeout(timeout)

    def _read_exact(self, size: int) -> bytes:
        while len(self._buffer) < size:
            chunk = self._sock.recv(max(4096, size - len(self._buffer)))
            if not chunk:
                raise WebSocketClosed()
            self._buffer.extend(chunk)
        out = bytes(self._buffer[:size])
        del self._buffer[:size]
        return out

    def _read_frame(self) -> tuple[bool, int, bytes]:
        first, second = self._read_exact(2)
        fin = bool(first & 0x80)
        if first & 0x70:
            raise WebSocketProtocolError("RSV bits set without an extension")
        opcode = first & 0x0F
        masked = bool(second & 0x80)
        if masked:
            raise WebSocketProtocolError("server frames must not be masked")

        length = second & 0x7F
        if length == 126:
            length = struct.unpack("!H", self._read_exact(2))[0]
        elif length == 127:
            length = struct.unpack("!Q", self._read_exact(8))[0]
            if length & (1 << 63):
                raise WebSocketProtocolError("invalid 64-bit payload length")

        is_control = opcode >= 0x8
        if is_control and (not fin or length > 125):
            raise WebSocketProtocolError("invalid control frame")
        if length > self._max_message_bytes:
            raise WebSocketProtocolError(
                f"frame exceeds {self._max_message_bytes} byte limit"
            )
        return fin, opcode, self._read_exact(length)

    def send_frame(self, opcode: int, payload: bytes = b"") -> None:
        if not isinstance(payload, bytes):
            raise TypeError("payload must be bytes")
        if opcode >= 0x8 and len(payload) > 125:
            raise ValueError("control frame payload exceeds 125 bytes")

        mask = secrets.token_bytes(4)
        length = len(payload)
        header = bytearray([0x80 | opcode])
        if length < 126:
            header.append(0x80 | length)
        elif length <= 0xFFFF:
            header.append(0x80 | 126)
            header.extend(struct.pack("!H", length))
        else:
            header.append(0x80 | 127)
            header.extend(struct.pack("!Q", length))
        header.extend(mask)
        masked = bytes(byte ^ mask[i % 4] for i, byte in enumerate(payload))
        with self._send_lock:
            self._sock.sendall(bytes(header) + masked)

    def send_text(self, text: str) -> None:
        self.send_frame(0x1, text.encode("utf-8"))

    def send_json(self, payload: Any) -> None:
        self.send_text(json.dumps(payload, separators=(",", ":"), ensure_ascii=False))

    def recv_message(self) -> str | bytes:
        fragments = bytearray()
        message_opcode: int | None = None
        while True:
            fin, opcode, payload = self._read_frame()
            if opcode == 0x8:
                code = 1000
                reason = ""
                if len(payload) == 1:
                    raise WebSocketProtocolError("one-byte close payload")
                if len(payload) >= 2:
                    code = struct.unpack("!H", payload[:2])[0]
                    reason = payload[2:].decode("utf-8", errors="replace")
                if not self._close_sent:
                    self.send_frame(0x8, payload)
                    self._close_sent = True
                raise WebSocketClosed(code, reason)
            if opcode == 0x9:
                self.send_frame(0xA, payload)
                continue
            if opcode == 0xA:
                continue
            if opcode in {0x1, 0x2}:
                if message_opcode is not None:
                    raise WebSocketProtocolError("new data frame during fragmentation")
                message_opcode = opcode
                fragments.extend(payload)
            elif opcode == 0x0:
                if message_opcode is None:
                    raise WebSocketProtocolError("unexpected continuation frame")
                fragments.extend(payload)
            else:
                raise WebSocketProtocolError(f"unsupported opcode {opcode}")

            if len(fragments) > self._max_message_bytes:
                raise WebSocketProtocolError(
                    f"message exceeds {self._max_message_bytes} byte limit"
                )
            if not fin:
                continue
            if message_opcode == 0x1:
                try:
                    return bytes(fragments).decode("utf-8")
                except UnicodeDecodeError as exc:
                    raise WebSocketProtocolError("text frame is not UTF-8") from exc
            return bytes(fragments)

    def recv_json(self) -> Mapping[str, Any]:
        message = self.recv_message()
        if not isinstance(message, str):
            raise WebSocketProtocolError("expected a text JSON message")
        try:
            payload = json.loads(message)
        except json.JSONDecodeError as exc:
            raise WebSocketProtocolError("invalid JSON message") from exc
        if not isinstance(payload, dict):
            raise WebSocketProtocolError("JSON message must be an object")
        return payload

    def close(self, code: int = 1000, reason: str = "") -> None:
        if not self._close_sent:
            encoded_reason = reason.encode("utf-8")[:123]
            try:
                self.send_frame(0x8, struct.pack("!H", code) + encoded_reason)
            except OSError:
                pass
            self._close_sent = True
        try:
            self._sock.shutdown(socket.SHUT_RDWR)
        except OSError:
            pass
        self._sock.close()


def _read_upgrade_response(sock: socket.socket) -> tuple[bytes, bytes]:
    data = bytearray()
    marker = b"\r\n\r\n"
    while marker not in data:
        chunk = sock.recv(4096)
        if not chunk:
            raise WebSocketProtocolError("connection closed during HTTP upgrade")
        data.extend(chunk)
        if len(data) > _MAX_HTTP_HEADERS:
            raise WebSocketProtocolError("HTTP upgrade headers are too large")
    head, rest = bytes(data).split(marker, 1)
    return head, rest


def _validate_upgrade_response(raw_headers: bytes, key: str) -> None:
    try:
        lines = raw_headers.decode("iso-8859-1").split("\r\n")
    except UnicodeDecodeError as exc:
        raise WebSocketProtocolError("invalid HTTP upgrade response") from exc
    status = lines[0].split(" ", 2)
    if len(status) < 2 or status[1] != "101":
        raise WebSocketProtocolError(f"WebSocket upgrade rejected: {lines[0]}")

    headers: dict[str, str] = {}
    for line in lines[1:]:
        if not line or ":" not in line:
            continue
        name, value = line.split(":", 1)
        lowered = name.strip().lower()
        headers[lowered] = ",".join(
            value for value in (headers.get(lowered), value.strip()) if value
        )
    upgrade_tokens = {
        token.strip().lower() for token in headers.get("upgrade", "").split(",")
    }
    connection_tokens = {
        token.strip().lower() for token in headers.get("connection", "").split(",")
    }
    expected = base64.b64encode(
        hashlib.sha1((key + _GUID).encode("ascii")).digest()
    ).decode("ascii")
    if "websocket" not in upgrade_tokens:
        raise WebSocketProtocolError("missing Upgrade: websocket")
    if "upgrade" not in connection_tokens:
        raise WebSocketProtocolError("missing Connection: Upgrade")
    if not secrets.compare_digest(headers.get("sec-websocket-accept", ""), expected):
        raise WebSocketProtocolError("invalid Sec-WebSocket-Accept")


def connect(
    url: str,
    *,
    user_agent: str,
    timeout: float = 15.0,
    max_message_bytes: int = _DEFAULT_MAX_MESSAGE,
    verb_id: str = "net.dial",
) -> WebSocketClient:
    """Connect to a ``wss://`` endpoint without proxies or redirects."""
    try:
        parsed = urllib.parse.urlsplit(url)
        host = (parsed.hostname or "").rstrip(".").lower()
        port = parsed.port or 443
    except (TypeError, ValueError) as exc:
        raise WebSocketProtocolError(f"invalid WebSocket URL: {exc}") from None
    if parsed.scheme.lower() != "wss":
        raise WebSocketProtocolError("only wss:// WebSocket URLs are permitted")
    if not host or parsed.username is not None or parsed.password is not None:
        raise WebSocketProtocolError("WebSocket URL has an invalid authority")
    if parsed.fragment:
        raise WebSocketProtocolError("WebSocket URL fragments are not permitted")
    if (
        not user_agent
        or not user_agent.isascii()
        or any(char in user_agent for char in "\r\n\0")
    ):
        raise WebSocketProtocolError("invalid WebSocket user agent")
    if max_message_bytes <= 0:
        raise WebSocketProtocolError("max_message_bytes must be positive")

    sock = safe_egress.safe_tls_connect(
        host,
        port,
        timeout=timeout,
        verb_id=verb_id,
    )
    key = base64.b64encode(secrets.token_bytes(16)).decode("ascii")
    path = urllib.parse.urlunsplit(("", "", parsed.path or "/", parsed.query, ""))
    host_header = f"[{host}]" if ":" in host else host
    if port != 443:
        host_header = f"{host_header}:{port}"
    request = (
        f"GET {path} HTTP/1.1\r\n"
        f"Host: {host_header}\r\n"
        "Upgrade: websocket\r\n"
        "Connection: Upgrade\r\n"
        f"Sec-WebSocket-Key: {key}\r\n"
        "Sec-WebSocket-Version: 13\r\n"
        f"User-Agent: {user_agent}\r\n"
        "\r\n"
    ).encode("ascii")
    try:
        sock.sendall(request)
        raw_headers, initial_bytes = _read_upgrade_response(sock)
        _validate_upgrade_response(raw_headers, key)
        return WebSocketClient(
            sock,
            initial_bytes=initial_bytes,
            max_message_bytes=max_message_bytes,
        )
    except (OSError, WebSocketProtocolError):
        sock.close()
        raise


__all__ = [
    "WebSocketClient",
    "WebSocketClosed",
    "WebSocketError",
    "WebSocketProtocolError",
    "connect",
]
