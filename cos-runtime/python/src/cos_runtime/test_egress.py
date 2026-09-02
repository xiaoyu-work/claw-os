"""Tests for the brokered egress client."""

from __future__ import annotations

import os
import socket
import threading
import unittest

from cos_runtime import egress


class EnvGuard:
    """Set and restore the egress environment around one test."""

    def __init__(self, **values):
        self._values = values
        self._previous = {}

    def __enter__(self):
        for key, value in self._values.items():
            self._previous[key] = os.environ.get(key)
            if value is None:
                os.environ.pop(key, None)
            else:
                os.environ[key] = value
        return self

    def __exit__(self, *_):
        for key, value in self._previous.items():
            if value is None:
                os.environ.pop(key, None)
            else:
                os.environ[key] = value


class AvailabilityTests(unittest.TestCase):
    def test_absent_outside_a_grant(self):
        with EnvGuard(COS_EGRESS_SOCKET=None):
            self.assertFalse(egress.available())
            self.assertIsNone(egress.socket_path())

    def test_present_inside_a_grant(self):
        with EnvGuard(COS_EGRESS_SOCKET="/run/cos/worker-egress.sock"):
            self.assertTrue(egress.available())

    def test_endpoints_are_parsed_and_normalised(self):
        with EnvGuard(COS_EGRESS_ENDPOINTS="API.Example.com:443, files.example.com:8443,junk"):
            self.assertEqual(
                egress.allowed_endpoints(),
                [("api.example.com", 443), ("files.example.com", 8443)],
            )

    def test_no_broker_means_an_explicit_refusal_not_a_direct_dial(self):
        with EnvGuard(COS_EGRESS_SOCKET=None):
            with self.assertRaises(egress.EgressUnavailable):
                egress.create_connection("example.com", 443, 1)


class TargetTests(unittest.TestCase):
    def test_targets_are_normalised(self):
        self.assertEqual(egress._target("API.Example.COM", 443), "api.example.com:443")
        # A trailing root dot and IPv6 brackets are stripped, so the
        # broker compares the same string the grant named.
        self.assertEqual(egress._target("example.com.", 443), "example.com:443")
        self.assertEqual(egress._target("[2606:4700::1111]", 443), "2606:4700::1111:443")

    def test_invalid_targets_are_refused(self):
        for host, port in (
            ("", 443),
            ("example.com", 0),
            ("example.com", 70000),
            ("exa mple.com", 443),
            ("user@example.com", 443),
            ("example.com/path", 443),
            ("a" * 400, 443),
        ):
            with self.assertRaises(egress.EgressDenied, msg=f"{host}:{port}"):
                egress._target(host, port)

    def test_an_internationalised_host_is_encoded_not_guessed(self):
        self.assertEqual(egress._target("ex\u00e4mple.com", 443), "xn--exmple-cua.com:443")


class TunnelTests(unittest.TestCase):
    """Exercise the CONNECT exchange against a stub broker."""

    def _serve(self, path, reply, *, capture):
        listener = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        listener.bind(path)
        listener.listen(1)

        def run():
            stream, _ = listener.accept()
            with stream:
                request = b""
                while b"\r\n\r\n" not in request:
                    chunk = stream.recv(256)
                    if not chunk:
                        break
                    request += chunk
                capture.append(request.decode("latin-1"))
                stream.sendall(reply)
            listener.close()

        thread = threading.Thread(target=run, daemon=True)
        thread.start()
        return thread

    def test_a_successful_tunnel_returns_the_stream(self):
        import tempfile

        with tempfile.TemporaryDirectory() as tmp:
            path = os.path.join(tmp, "egress.sock")
            capture = []
            thread = self._serve(
                path,
                b"HTTP/1.1 200 Connection established\r\n\r\npayload",
                capture=capture,
            )
            with EnvGuard(COS_EGRESS_SOCKET=path):
                stream = egress.create_connection("example.com", 443, 5)
            self.assertEqual(stream.recv(7), b"payload")
            stream.close()
            thread.join(timeout=5)
            self.assertIn("CONNECT example.com:443 HTTP/1.1", capture[0])
            self.assertIn("Host: example.com:443", capture[0])

    def test_a_refusal_is_raised_and_never_downgraded(self):
        import tempfile

        with tempfile.TemporaryDirectory() as tmp:
            path = os.path.join(tmp, "egress.sock")
            capture = []
            thread = self._serve(path, b"HTTP/1.1 403 Forbidden\r\n\r\n", capture=capture)
            with EnvGuard(COS_EGRESS_SOCKET=path):
                with self.assertRaises(egress.EgressDenied):
                    egress.create_connection("blocked.example", 443, 5)
            thread.join(timeout=5)

    def test_a_malformed_reply_is_refused(self):
        import tempfile

        with tempfile.TemporaryDirectory() as tmp:
            path = os.path.join(tmp, "egress.sock")
            capture = []
            thread = self._serve(path, b"garbage\r\n\r\n", capture=capture)
            with EnvGuard(COS_EGRESS_SOCKET=path):
                with self.assertRaises(egress.EgressDenied):
                    egress.create_connection("example.com", 443, 5)
            thread.join(timeout=5)

    def test_an_unreachable_broker_is_refused(self):
        with EnvGuard(COS_EGRESS_SOCKET="/nonexistent/egress.sock"):
            with self.assertRaises(egress.EgressUnavailable):
                egress.create_connection("example.com", 443, 1)


if __name__ == "__main__":  # pragma: no cover
    unittest.main()
