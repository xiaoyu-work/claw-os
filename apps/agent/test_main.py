"""Tests for the native Agent launcher app."""

import os
import sys
import unittest
import json
from unittest import mock

sys.path.insert(0, os.path.dirname(__file__))

import main


class AgentLauncherTests(unittest.TestCase):
    def test_missing_native_ui_points_to_desktop_package(self):
        with mock.patch.object(main, "_find_native_ui", return_value=None):
            result = main._exec_native([])
        self.assertEqual(result["error"], "cos-agent-ui is not installed")
        self.assertIn("claw-os-desktop", result["hint"])

    def test_overlay_forwards_all_supported_arguments(self):
        with mock.patch.object(main, "_exec_native", return_value={"ok": True}) as execute:
            result = main._cmd_overlay(
                [
                    "--voice",
                    "--query",
                    "hello",
                    "--context",
                    '{"app":"files"}',
                ]
            )
        self.assertEqual(result, {"ok": True})
        execute.assert_called_once_with(
            [
                "--overlay",
                "--voice",
                "--query",
                "hello",
                "--context",
                '{"app":"files"}',
            ]
        )

    def test_native_launch_requires_ready_bridge(self):
        with (
            mock.patch.object(main, "_find_native_ui", return_value="/usr/bin/cos-agent-ui"),
            mock.patch.object(main, "_ensure_endpoint", return_value=None),
            mock.patch.object(os, "execv") as execv,
        ):
            result = main._exec_native(["--overlay"])
        self.assertEqual(result["error"], "cos-agent-bridge is not ready")
        execv.assert_not_called()

    def test_schema_documents_context_argument(self):
        parameters = main._schema()["overlay"]["parameters"]
        self.assertIn("--context", [parameter["name"] for parameter in parameters])
        with open(os.path.join(os.path.dirname(__file__), "app.json"), encoding="utf-8") as file:
            manifest = json.load(file)
        args = manifest["operations"]["overlay"]["args"]
        self.assertEqual(
            [arg["name"] for arg in args],
            ["voice", "query", "context"],
        )

if __name__ == "__main__":
    unittest.main()
