"""Tests for the bidirectional Discord gateway."""

from __future__ import annotations

import base64
import hashlib
import json
import os
import struct
import sys
import tempfile
import unittest

from test_support import load_local_module


sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
sys.path.insert(
    0,
    os.path.join(
        os.path.dirname(__file__),
        os.pardir,
        os.pardir,
        os.pardir,
        "cos-runtime",
        "python",
        "src",
    ),
)


def _load_main():
    path = os.path.join(os.path.dirname(__file__), "main.py")
    return load_local_module(
        path,
        "gateway_discord_main",
        clear_modules=("_shared",),
    )


main = _load_main()
from _shared import inbound, safe_subprocess, websocket  # noqa: E402


class _FakeProc:
    def __init__(self, stdout: str, stderr: str = "", returncode: int = 0):
        self.stdout = stdout
        self.stderr = stderr
        self.returncode = returncode


class DiscordConfigTests(unittest.TestCase):
    def setUp(self):
        self.temp_dir = tempfile.TemporaryDirectory()
        self.old_data_dir = os.environ.get("COS_DATA_DIR")
        os.environ["COS_DATA_DIR"] = self.temp_dir.name
        for name in (
            main.ENV_ALLOWED_USERS,
            main.ENV_ALLOWED_GUILDS,
            main.ENV_ALLOWED_CHANNELS,
            main.ENV_REQUIRE_MENTION,
            main.ENV_RPM,
        ):
            os.environ.pop(name, None)

    def tearDown(self):
        if self.old_data_dir is None:
            os.environ.pop("COS_DATA_DIR", None)
        else:
            os.environ["COS_DATA_DIR"] = self.old_data_dir
        self.temp_dir.cleanup()

    def test_configure_persists_fail_closed_policy(self):
        result = main._configure(
            {
                "settings": (
                    "users=42,43 guilds=99 channels=100 "
                    "require_mention=false rpm=7"
                )
            }
        )
        self.assertTrue(result["ok"])
        config = main._effective_config()
        self.assertEqual(config["allowed_users"], ["42", "43"])
        self.assertEqual(config["allowed_guilds"], ["99"])
        self.assertEqual(config["allowed_channels"], ["100"])
        self.assertFalse(config["require_mention"])
        self.assertEqual(config["requests_per_minute"], 7)

    def test_configure_rejects_empty_user_allowlist(self):
        result = main._configure(["guilds=99"])
        self.assertFalse(result["ok"])
        self.assertIn("allowed_users", result["error"])

    def test_environment_overrides_persisted_scope(self):
        self.assertTrue(main._configure(["users=42 guilds=99"])["ok"])
        os.environ[main.ENV_ALLOWED_GUILDS] = "101"
        config = main._effective_config()
        self.assertEqual(config["allowed_guilds"], ["101"])

    def test_worker_failure_stops_gateway_and_is_persisted(self):
        original_worker = main._worker_loop

        def crashing_worker(*_args):
            raise PermissionError("cos is not executable")

        main._worker_loop = crashing_worker
        stop_event = main.threading.Event()
        state = {}
        try:
            main._worker_entry(
                main.queue.Queue(),
                stop_event,
                "token",
                state,
                {},
            )
        finally:
            main._worker_loop = original_worker
        self.assertTrue(stop_event.is_set())
        self.assertFalse(state["worker_alive"])
        self.assertIn("PermissionError", state["worker_error"])

    def test_stop_request_wakes_matching_gateway_pid(self):
        stop_event = main.threading.Event()
        main.atomic.atomic_write_text(main._stop_path(), "123")
        watcher = main.threading.Thread(
            target=main._watch_stop_request,
            args=(stop_event, 123),
        )
        watcher.start()
        watcher.join(timeout=1.0)
        self.assertTrue(stop_event.is_set())


class DiscordInboundRoutingTests(unittest.TestCase):
    def setUp(self):
        self.old_users = os.environ.get(main.ENV_ALLOWED_USERS)
        os.environ.pop(main.ENV_ALLOWED_USERS, None)
        self.config = {
            "allowed_users": ["42"],
            "allowed_guilds": ["99"],
            "allowed_channels": [],
            "require_mention": True,
            "requests_per_minute": 5,
        }

    def tearDown(self):
        if self.old_users is None:
            os.environ.pop(main.ENV_ALLOWED_USERS, None)
        else:
            os.environ[main.ENV_ALLOWED_USERS] = self.old_users

    def _message(self, **changes):
        message = {
            "id": "1001",
            "channel_id": "2001",
            "guild_id": "99",
            "content": "<@9001> diagnose the network",
            "author": {"id": "42", "bot": False},
            "mentions": [{"id": "9001"}],
            "attachments": [],
        }
        message.update(changes)
        return message

    def test_guild_mention_routes_to_channel_session(self):
        item = main._prepare_message(
            self._message(),
            bot_user_id="9001",
            config=self.config,
        )
        self.assertIsNotNone(item)
        self.assertEqual(item["session_key"], "discord:channel:2001")
        self.assertIn("diagnose the network", item["prompt"])
        self.assertNotIn("<@9001>", item["prompt"])

    def test_guild_message_without_mention_is_ignored(self):
        item = main._prepare_message(
            self._message(content="hello", mentions=[]),
            bot_user_id="9001",
            config=self.config,
        )
        self.assertIsNone(item)

    def test_dm_routes_to_sender_session_without_mention(self):
        item = main._prepare_message(
            self._message(guild_id=None, content="hello", mentions=[]),
            bot_user_id="9001",
            config=self.config,
        )
        self.assertIsNotNone(item)
        self.assertEqual(item["session_key"], "discord:dm:42")

    def test_disallowed_sender_and_bot_messages_are_ignored(self):
        disallowed = self._message(author={"id": "77", "bot": False})
        self.assertIsNone(
            main._prepare_message(
                disallowed,
                bot_user_id="9001",
                config=self.config,
            )
        )
        bot_message = self._message(author={"id": "42", "bot": True})
        self.assertIsNone(
            main._prepare_message(
                bot_message,
                bot_user_id="9001",
                config=self.config,
            )
        )

    def test_regional_resume_url_is_allowed_but_lookalike_is_rejected(self):
        self.assertEqual(
            main._normalize_gateway_url("wss://gateway-us-east1-b.discord.gg"),
            "wss://gateway-us-east1-b.discord.gg/?v=10&encoding=json",
        )
        with self.assertRaises(ValueError):
            main._normalize_gateway_url("wss://discord.gg.attacker.example")


class DiscordAgentDispatchTests(unittest.TestCase):
    def setUp(self):
        self.old_users = os.environ.get(main.ENV_ALLOWED_USERS)
        os.environ.pop(main.ENV_ALLOWED_USERS, None)
        main._reset_rate_limiter_for_tests()
        self.original_subprocess = safe_subprocess.safe_subprocess
        self.original_require = (
            main._cos_policy.require if main._cos_policy is not None else None
        )
        if main._cos_policy is not None:
            main._cos_policy.require = lambda *args, **kwargs: None

    def tearDown(self):
        safe_subprocess.safe_subprocess = self.original_subprocess
        if main._cos_policy is not None and self.original_require is not None:
            main._cos_policy.require = self.original_require
        main._reset_rate_limiter_for_tests()
        if self.old_users is None:
            os.environ.pop(main.ENV_ALLOWED_USERS, None)
        else:
            os.environ[main.ENV_ALLOWED_USERS] = self.old_users

    def test_existing_channel_session_is_forwarded_to_agent(self):
        calls = []

        def fake_subprocess(argv, **kwargs):
            calls.append((argv, kwargs))
            return _FakeProc(
                json.dumps({"answer": "done", "session_id": "session-1"})
            )

        safe_subprocess.safe_subprocess = fake_subprocess
        answer, session_id = main._ask_agent(
            "42",
            "hello",
            allowed_users=["42"],
            rpm=5,
            session_id="session-1",
        )
        self.assertEqual(answer, "done")
        self.assertEqual(session_id, "session-1")
        self.assertEqual(
            calls[0][0][1:],
            [
                "agent",
                "ask",
                "--full",
                "--timeout-secs",
                str(main.AGENT_SERVER_TIMEOUT_S),
                "--session",
                "session-1",
                "hello",
            ],
        )
        self.assertEqual(calls[0][1]["timeout"], main.AGENT_TIMEOUT_S)

    def test_disallowed_sender_never_spawns_agent(self):
        safe_subprocess.safe_subprocess = lambda *args, **kwargs: self.fail(
            "subprocess should not run"
        )
        with self.assertRaises(inbound.SenderNotAllowed):
            main._ask_agent(
                "77",
                "hello",
                allowed_users=["42"],
                rpm=5,
                session_id=None,
            )

    def test_long_replies_are_split_to_discord_limit(self):
        chunks = main._split_message("word " * 1200)
        self.assertGreater(len(chunks), 1)
        self.assertTrue(all(0 < len(chunk) <= main.MAX_MESSAGE_LEN for chunk in chunks))


class _FakeSocket:
    def __init__(self, incoming: bytes):
        self.incoming = bytearray(incoming)
        self.sent = bytearray()

    def recv(self, size: int) -> bytes:
        if not self.incoming:
            return b""
        out = bytes(self.incoming[:size])
        del self.incoming[:size]
        return out

    def sendall(self, payload: bytes) -> None:
        self.sent.extend(payload)

    def shutdown(self, _how):
        return None

    def close(self):
        return None

    def fileno(self):
        return 0

    def settimeout(self, _timeout):
        return None


def _server_frame(opcode: int, payload: bytes, *, fin: bool = True) -> bytes:
    first = (0x80 if fin else 0) | opcode
    if len(payload) < 126:
        return bytes([first, len(payload)]) + payload
    return bytes([first, 126]) + struct.pack("!H", len(payload)) + payload


class WebSocketTransportTests(unittest.TestCase):
    def test_fragmented_text_and_ping_are_handled(self):
        incoming = (
            _server_frame(0x1, b'{"op":', fin=False)
            + _server_frame(0x9, b"x")
            + _server_frame(0x0, b"11}")
        )
        sock = _FakeSocket(incoming)
        client = websocket.WebSocketClient(sock)
        self.assertEqual(client.recv_json(), {"op": 11})
        self.assertEqual(sock.sent[0] & 0x0F, 0x0A)
        self.assertTrue(sock.sent[1] & 0x80, "client pong must be masked")

    def test_upgrade_accept_validation_uses_rfc_guid(self):
        key = "dGhlIHNhbXBsZSBub25jZQ=="
        accept = base64.b64encode(
            hashlib.sha1(
                (
                    key
                    + "258EAFA5-E914-47DA-95CA-C5AB0DC85B11"
                ).encode("ascii")
            ).digest()
        ).decode("ascii")
        headers = (
            "HTTP/1.1 101 Switching Protocols\r\n"
            "Upgrade: websocket\r\n"
            "Connection: Upgrade\r\n"
            f"Sec-WebSocket-Accept: {accept}"
        ).encode("ascii")
        websocket._validate_upgrade_response(headers, key)

    def test_close_frame_surfaces_discord_code(self):
        sock = _FakeSocket(_server_frame(0x8, struct.pack("!H", 4014) + b"intent"))
        client = websocket.WebSocketClient(sock)
        with self.assertRaises(websocket.WebSocketClosed) as raised:
            client.recv_message()
        self.assertEqual(raised.exception.code, 4014)


if __name__ == "__main__":
    unittest.main()
