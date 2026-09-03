"""Strict wire transport tests for ``cos_runtime.policy``."""

from __future__ import annotations

import json
import os
import subprocess
import unittest
from unittest import mock

from cos_runtime import policy


def _wire_success(data: dict) -> str:
    return json.dumps({"ok": True, "wire_version": 1, "data": data})


class PolicyTransportTests(unittest.TestCase):
    def test_check_uses_wire_v1_and_returns_decision(self) -> None:
        completed = subprocess.CompletedProcess(
            ["cos"],
            0,
            _wire_success({"decision": "allow", "verb": "fs.read"}),
            "",
        )
        with mock.patch.dict(
            os.environ, {"CLAW_COS_BIN": "/usr/bin/cos"}
        ), mock.patch("subprocess.run", return_value=completed) as run:
            decision = policy.check("fs.read", path="/tmp/file")

        self.assertEqual(decision["decision"], "allow")
        self.assertEqual(
            run.call_args.args[0][:4],
            ["/usr/bin/cos", "--wire=1", "__policy", "check"],
        )

    def test_require_raises_for_a_denied_decision(self) -> None:
        denial = {
            "decision": "deny",
            "verb": "fs.read",
            "summary": "not granted",
        }
        completed = subprocess.CompletedProcess(
            ["cos"], 0, _wire_success(denial), ""
        )
        with mock.patch.dict(
            os.environ, {"CLAW_COS_BIN": "/usr/bin/cos"}
        ), mock.patch("subprocess.run", return_value=completed):
            with self.assertRaises(policy.PermissionDenied) as raised:
                policy.require("fs.read", path="/tmp/file")
        self.assertEqual(raised.exception.denial, denial)

    def test_stderr_json_is_not_a_wire_response(self) -> None:
        completed = subprocess.CompletedProcess(
            ["cos"],
            1,
            "",
            json.dumps(
                {
                    "ok": False,
                    "wire_version": 1,
                    "error": "denied",
                    "code": "PERMISSION_DENIED",
                }
            ),
        )
        with mock.patch.dict(
            os.environ, {"CLAW_COS_BIN": "/usr/bin/cos"}
        ), mock.patch("subprocess.run", return_value=completed):
            with self.assertRaises(policy.PolicyUnavailable):
                policy.check("fs.read", path="/tmp/file")


if __name__ == "__main__":
    unittest.main()
