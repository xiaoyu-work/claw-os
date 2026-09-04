import json
import os
import subprocess
import unittest
from unittest import mock

from cos_runtime import browser_bridge


def _wire(data: object) -> bytes:
    return json.dumps({"ok": True, "wire_version": 1, "data": data}).encode()


def _process(stdout: bytes, *, returncode: int = 0, stderr: bytes = b"") -> mock.Mock:
    process = mock.Mock(returncode=returncode)
    process.communicate.return_value = (stdout, stderr)
    return process


class BrowserBridgeTests(unittest.TestCase):
    def test_sensitive_values_travel_only_over_stdin(self) -> None:
        captured: dict = {}

        def fake_popen(command, **kwargs):  # type: ignore[no-untyped-def]
            captured["command"] = list(command)
            process = _process(_wire({"ok": True}))

            def communicate(input_bytes, **_options):  # type: ignore[no-untyped-def]
                captured["input"] = input_bytes
                return process.communicate.return_value

            process.communicate.side_effect = communicate
            return process

        with mock.patch.dict(os.environ, {"CLAW_COS_BIN": "/usr/local/bin/cos"}):
            with mock.patch("subprocess.Popen", side_effect=fake_popen):
                result = browser_bridge.request(
                    "dom.fill_secret",
                    tab_id=7,
                    page_url="https://example.com/login",
                    reference="field-1",
                    value="correct horse battery staple",
                )

        self.assertEqual(result, {"ok": True})
        self.assertEqual(
            captured["command"],
            [
                "/usr/local/bin/cos",
                "--wire=1",
                "__browser",
                "request",
                "--request-stdin",
            ],
        )
        self.assertNotIn("correct horse battery staple", " ".join(captured["command"]))
        self.assertEqual(
            json.loads(captured["input"])["value"],
            "correct horse battery staple",
        )

    def test_permission_denial_is_explicit(self) -> None:
        denied = json.dumps(
            {
                "ok": False,
                "wire_version": 1,
                "code": "PERMISSION_DENIED",
                "error": "browser.eval denied",
            }
        ).encode()
        process = _process(denied, returncode=1)
        with mock.patch.dict(os.environ, {"CLAW_COS_BIN": "/runtime/cos"}):
            with mock.patch("subprocess.Popen", return_value=process):
                with self.assertRaises(browser_bridge.PermissionDenied):
                    browser_bridge.request("eval", tab_id=3, expr="document.title")

    def test_invalid_success_data_is_rejected(self) -> None:
        process = _process(_wire(["not", "an", "object"]))
        with mock.patch.dict(os.environ, {"CLAW_COS_BIN": "/runtime/cos"}):
            with mock.patch("subprocess.Popen", return_value=process):
                with self.assertRaises(browser_bridge.BrowserActionIndeterminate):
                    browser_bridge.request("tabs.list")

    def test_subprocess_timeout_is_indeterminate(self) -> None:
        process = _process(b"")
        process.poll.return_value = None
        process.communicate.side_effect = [
            subprocess.TimeoutExpired("/runtime/cos", 60),
            (b"", b""),
        ]
        with mock.patch.dict(os.environ, {"CLAW_COS_BIN": "/runtime/cos"}):
            with mock.patch("subprocess.Popen", return_value=process):
                with self.assertRaises(browser_bridge.BrowserActionIndeterminate):
                    browser_bridge.request("dom.click", tab_id=3)
        process.kill.assert_called_once_with()

    def test_post_launch_pipe_failure_is_indeterminate(self) -> None:
        process = _process(b"")
        process.poll.return_value = None
        process.communicate.side_effect = [OSError("pipe failed"), (b"", b"")]
        with mock.patch.dict(os.environ, {"CLAW_COS_BIN": "/runtime/cos"}):
            with mock.patch("subprocess.Popen", return_value=process):
                with self.assertRaises(browser_bridge.BrowserActionIndeterminate):
                    browser_bridge.request("dom.click", tab_id=3)
        process.kill.assert_called_once_with()

    def test_wrong_wire_version_is_indeterminate(self) -> None:
        wrong_version = json.dumps(
            {"ok": True, "wire_version": 2, "data": {}}
        ).encode()
        process = _process(wrong_version)
        with mock.patch.dict(os.environ, {"CLAW_COS_BIN": "/runtime/cos"}):
            with mock.patch("subprocess.Popen", return_value=process):
                with self.assertRaises(browser_bridge.BrowserActionIndeterminate):
                    browser_bridge.request("tabs.list")

    def test_boolean_wire_version_is_not_accepted_as_v1(self) -> None:
        wrong_version = json.dumps(
            {"ok": True, "wire_version": True, "data": {}}
        ).encode()
        process = _process(wrong_version)
        with mock.patch.dict(os.environ, {"CLAW_COS_BIN": "/runtime/cos"}):
            with mock.patch("subprocess.Popen", return_value=process):
                with self.assertRaises(browser_bridge.BrowserActionIndeterminate):
                    browser_bridge.request("tabs.list")

    def test_process_creation_failure_is_unavailable(self) -> None:
        with mock.patch.dict(os.environ, {"CLAW_COS_BIN": "/runtime/cos"}):
            with mock.patch("subprocess.Popen", side_effect=OSError("missing")):
                with self.assertRaises(browser_bridge.BrowserUnavailable):
                    browser_bridge.request("tabs.list")

    def test_indeterminate_action_is_not_reported_as_a_retryable_outage(self) -> None:
        failed = json.dumps(
            {
                "ok": False,
                "wire_version": 1,
                "code": "INDETERMINATE",
                "error": "response lost after dispatch",
            }
        ).encode()
        process = _process(failed, returncode=1)
        with mock.patch.dict(os.environ, {"CLAW_COS_BIN": "/runtime/cos"}):
            with mock.patch("subprocess.Popen", return_value=process):
                with self.assertRaises(browser_bridge.BrowserActionIndeterminate):
                    browser_bridge.request("dom.click", tab_id=3)

    def test_explicit_provider_rejection_is_an_action_failure(self) -> None:
        failed = json.dumps(
            {
                "ok": False,
                "wire_version": 1,
                "code": "EXECUTION_FAILED",
                "error": "browser action failed",
            }
        ).encode()
        process = _process(failed, returncode=1)
        with mock.patch.dict(os.environ, {"CLAW_COS_BIN": "/runtime/cos"}):
            with mock.patch("subprocess.Popen", return_value=process):
                with self.assertRaises(browser_bridge.BrowserActionFailed):
                    browser_bridge.request("eval", tab_id=3)

    def test_runtime_binary_path_is_required(self) -> None:
        with mock.patch.dict(os.environ, {}, clear=True):
            with self.assertRaises(browser_bridge.BrowserUnavailable):
                browser_bridge.request("tabs.list")


if __name__ == "__main__":
    unittest.main()
