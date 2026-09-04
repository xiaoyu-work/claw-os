import json
import os
import unittest
from unittest import mock

from cos_runtime import network_diagnostics


def _process(stdout: bytes, *, returncode: int = 0) -> mock.Mock:
    process = mock.Mock(returncode=returncode)
    process.communicate.return_value = (stdout, b"")
    return process


class NetworkDiagnosticsBridgeTests(unittest.TestCase):
    def test_request_uses_private_stdin_bridge(self) -> None:
        captured: dict = {}

        def fake_popen(command, **kwargs):  # type: ignore[no-untyped-def]
            captured["command"] = list(command)
            process = _process(
                json.dumps(
                    {
                        "ok": True,
                        "wire_version": 1,
                        "data": {"resolved": True},
                    }
                ).encode()
            )

            def communicate(input_bytes, **_options):  # type: ignore[no-untyped-def]
                captured["input"] = input_bytes
                return process.communicate.return_value

            process.communicate.side_effect = communicate
            return process

        with mock.patch.dict(os.environ, {"CLAW_COS_BIN": "/usr/local/bin/cos"}):
            with mock.patch("subprocess.Popen", side_effect=fake_popen):
                result = network_diagnostics.request(
                    "tcp",
                    target="example.com:443",
                    attempts=3,
                    timeout_ms=5000,
                )

        self.assertEqual(result, {"resolved": True})
        self.assertEqual(
            captured["command"],
            [
                "/usr/local/bin/cos",
                "--wire=1",
                "__netdiag",
                "request",
                "--request-stdin",
            ],
        )
        self.assertEqual(
            json.loads(captured["input"]),
            {
                "action": "tcp",
                "target": "example.com:443",
                "attempts": 3,
                "timeout_ms": 5000,
            },
        )

    def test_permission_denial_is_explicit(self) -> None:
        process = _process(
            json.dumps(
                {
                    "ok": False,
                    "wire_version": 1,
                    "code": "PERMISSION_DENIED",
                    "error": "net.dial denied",
                }
            ).encode(),
            returncode=1,
        )
        with mock.patch.dict(os.environ, {"CLAW_COS_BIN": "/runtime/cos"}):
            with mock.patch("subprocess.Popen", return_value=process):
                with self.assertRaises(network_diagnostics.PermissionDenied):
                    network_diagnostics.request("tcp", target="example.com:443")

    def test_unknown_action_is_rejected_before_process_creation(self) -> None:
        with mock.patch("subprocess.Popen") as popen:
            with self.assertRaises(network_diagnostics.NetworkDiagnosticsError):
                network_diagnostics.request("shell")
        popen.assert_not_called()


if __name__ == "__main__":
    unittest.main()
