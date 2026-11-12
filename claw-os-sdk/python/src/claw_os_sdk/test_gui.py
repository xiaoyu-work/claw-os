"""Unit tests for the desktop GUI bootstrap in `claw_os_sdk.gui`."""

import os
import json
import stat
import sys
import unittest
from types import SimpleNamespace
from unittest.mock import patch

_THIS_DIR = os.path.dirname(__file__)
sys.path.insert(0, os.path.dirname(_THIS_DIR))  # so `from claw_os_sdk import gui` works

from claw_os_sdk import ai, gui, tools  # noqa: E402


class IsGuiLaunchTests(unittest.TestCase):
    def setUp(self) -> None:
        self._saved = os.environ.pop("COS_APP_GUI", None)

    def tearDown(self) -> None:
        if self._saved is not None:
            os.environ["COS_APP_GUI"] = self._saved
        else:
            os.environ.pop("COS_APP_GUI", None)

    def test_env_flag_detected(self) -> None:
        os.environ["COS_APP_GUI"] = "1"
        self.assertTrue(gui.is_gui_launch())

    def test_command_does_not_replace_host_context(self) -> None:
        os.environ.pop("COS_APP_GUI", None)
        self.assertFalse(gui.is_gui_launch())


class ContextTests(unittest.TestCase):
    def setUp(self) -> None:
        self._saved = {
            k: os.environ.get(k) for k in ("COS_APP_ID", "COS_ARGS_JSON")
        }

    def tearDown(self) -> None:
        for k, v in self._saved.items():
            if v is None:
                os.environ.pop(k, None)
            else:
                os.environ[k] = v

    def test_app_id_and_explicit_files(self) -> None:
        os.environ["COS_APP_ID"] = "notes"
        ctx = gui.context(files=["/tmp/a.md"])
        self.assertEqual(ctx.app_id, "notes")
        self.assertEqual(ctx.files, ["/tmp/a.md"])
        self.assertIs(ctx.ai, ai)
        self.assertIs(ctx.tools, tools)

    def test_files_decoded_from_env(self) -> None:
        os.environ["COS_APP_ID"] = "notes"
        os.environ["COS_ARGS_JSON"] = '["/tmp/x.md", "/tmp/y.md"]'
        ctx = gui.context()
        self.assertEqual(ctx.files, ["/tmp/x.md", "/tmp/y.md"])

    def test_missing_app_id_is_rejected(self) -> None:
        os.environ.pop("COS_APP_ID", None)
        os.environ.pop("COS_ARGS_JSON", None)
        with self.assertRaisesRegex(ValueError, "COS_APP_ID"):
            gui.context()

    def test_malformed_args_json_is_rejected(self) -> None:
        os.environ["COS_APP_ID"] = "notes"
        os.environ["COS_ARGS_JSON"] = "not-json"
        with self.assertRaisesRegex(ValueError, "valid JSON"):
            gui.context()

    def test_non_string_file_is_rejected(self) -> None:
        os.environ["COS_APP_ID"] = "notes"
        os.environ["COS_ARGS_JSON"] = '["/tmp/x.md", 7]'
        with self.assertRaisesRegex(ValueError, "array of strings"):
            gui.context()


class OverlayTests(unittest.TestCase):
    def test_missing_binary_raises(self) -> None:
        ctx = gui.GuiContext(app_id="notes")
        os.environ["COS_AGENT_UI_BIN"] = "/nonexistent/attacker"
        def trusted_parents_missing_launcher(path):
            if path == gui.ASK_CLAW_LAUNCHER:
                raise FileNotFoundError(path)
            return SimpleNamespace(st_mode=stat.S_IFDIR | 0o755, st_uid=0)
        try:
            with patch.object(gui.os, "lstat", side_effect=trusted_parents_missing_launcher) as inspect:
                with self.assertRaises(FileNotFoundError):
                    ctx.open_agent_overlay()
            self.assertEqual(inspect.call_args_list[-1].args, (gui.ASK_CLAW_LAUNCHER,))
        finally:
            os.environ.pop("COS_AGENT_UI_BIN", None)

class SelectedReadTests(unittest.TestCase):
    def test_request_status_use_only_node_and_root_status_reference(self) -> None:
        selection = "sr-" + "a" * 32
        pending = {"version": 1, "selection_id": selection, "node": 7,
                   "access": "read", "state": "pending", "approval_request_id": "ap-private"}
        selected = {**pending, "state": "selected", "expires_in_ms": 120000}
        with patch.object(gui, "call_json_with_stdin_binary", side_effect=[pending, selected]) as call:
            self.assertEqual(gui.request_file_read(7)["state"], "pending")
            self.assertEqual(gui.file_read_status(selection, 7)["state"], "selected")
        self.assertEqual(call.call_args_list[0].args[1], ["__gui-file-read", "request", "--request-stdin"])
        self.assertEqual(json.loads(call.call_args_list[0].args[2]), {"node": 7})
        self.assertEqual(json.loads(call.call_args_list[1].args[2]), {"selection_id": selection, "node": 7})

    def test_invalid_claims_are_refused_without_transport(self) -> None:
        with patch.object(gui, "call_json_with_stdin_binary") as call:
            with self.assertRaises(ValueError):
                gui.request_file_read(0)
            with self.assertRaises(ValueError):
                gui.file_read_status("other-window", 7)
            call.assert_not_called()
        reply = {"version": 1, "selection_id": "sr-" + "a" * 32, "node": 7,
                 "access": "read", "state": "selected", "owner_uid": 0}
        with patch.object(gui, "call_json_with_stdin_binary", return_value=reply):
            with self.assertRaises(ValueError):
                gui.request_file_read(7)


class SelectedWriteTests(unittest.TestCase):
    def test_write_and_target_only_save_keep_root_choice_out_of_stdin(self) -> None:
        selection = "sw-" + "b" * 32
        pending = {"version": 1, "selection_id": selection, "node": 9,
                   "access": "write", "state": "pending"}
        selected = {**pending, "state": "selected", "expires_in_ms": 180000}
        save = {"version": 1, "reservation_id": "ss-" + "c" * 32,
                "selection_id": selection, "node": 9, "parent_node": 1,
                "name": ".cos-save-" + "c" * 32, "expires_in_ms": 170000}
        with patch.object(gui, "call_json_with_stdin_binary", side_effect=[pending, selected, save]) as call:
            self.assertEqual(gui.request_file_write(9)["state"], "pending")
            self.assertEqual(gui.file_write_status(selection, 9)["access"], "write")
            self.assertEqual(gui.reserve_file_save(selection, 9)["name"], save["name"])
        for invocation, action in zip(call.call_args_list, ["request", "status", "reserve"]):
            self.assertEqual(invocation.args[1], ["__gui-file-write", action, "--request-stdin"])
        self.assertEqual(json.loads(call.call_args_list[0].args[2]), {"node": 9})
        self.assertEqual(json.loads(call.call_args_list[2].args[2]), {"selection_id": selection, "node": 9})
        with patch.object(gui, "call_json_with_stdin_binary") as call:
            with self.assertRaises(ValueError):
                gui.reserve_file_save("sr-" + "b" * 32, 9)
            call.assert_not_called()
        with patch.object(gui, "call_json_with_stdin_binary", return_value={**save, "name": "../sibling"}):
            with self.assertRaises(ValueError):
                gui.reserve_file_save(selection, 9)


class SelectedCreateTests(unittest.TestCase):
    def test_literal_parent_one_and_slot_do_not_mean_created(self) -> None:
        selection = "sc-" + "d" * 32
        leaf = "报告 ; '$x'.txt"
        pending = {"version": 1, "selection_id": selection, "parent_node": 1,
                   "name": leaf, "access": "create", "state": "pending"}
        slot = {"version": 1, "reservation_id": "ss-" + "e" * 32, "selection_id": selection,
                "parent_node": 1, "name": ".cos-save-" + "e" * 32,
                "target_name": leaf, "expires_in_ms": 190000}
        with patch.object(gui, "call_json_with_stdin_binary",
                          side_effect=[pending, {**pending, "state": "selected"}, slot,
                                       {**pending, "state": "created"}]) as call:
            self.assertEqual(gui.request_file_create(1, leaf)["state"], "pending")
            self.assertEqual(gui.file_create_status(selection, 1)["state"], "selected")
            self.assertEqual(gui.reserve_file_create(selection, 1)["target_name"], leaf)
            self.assertEqual(gui.file_create_status(selection, 1)["state"], "created")
        for invocation, action in zip(call.call_args_list, ["request", "status", "reserve", "status"]):
            self.assertEqual(invocation.args[1], ["__gui-file-create", action, "--request-stdin"])
        self.assertEqual(json.loads(call.call_args_list[0].args[2]), {"parent_node": 1, "name": leaf})
        self.assertEqual(json.loads(call.call_args_list[2].args[2]), {"selection_id": selection, "parent_node": 1})

    def test_create_refuses_nonliteral_or_non_utf8_names_before_transport(self) -> None:
        with patch.object(gui, "call_json_with_stdin_binary") as call:
            for name in ["", ".", "..", "../leaf", "a/b", "a\\b", "a\0b", "\ud800", "é" * 128]:
                with self.subTest(name=repr(name)), self.assertRaises(ValueError):
                    gui.request_file_create(1, name)
            with self.assertRaises(ValueError):
                gui.request_file_create(0, "leaf")
            with self.assertRaises(ValueError):
                gui.reserve_file_create("sw-" + "d" * 32, 1)
            call.assert_not_called()
        forged = {"version": 1, "selection_id": "sc-" + "d" * 32, "parent_node": 1,
                  "name": "leaf", "access": "create", "state": "created", "owner_uid": 0}
        with patch.object(gui, "call_json_with_stdin_binary", return_value=forged):
            with self.assertRaises(ValueError):
                gui.request_file_create(1, "leaf")


if __name__ == "__main__":
    unittest.main()
