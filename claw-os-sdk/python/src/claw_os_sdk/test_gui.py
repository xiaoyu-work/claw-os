"""Unit tests for the desktop GUI bootstrap in `claw_os_sdk.gui`."""

import os
import sys
import unittest

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
        self.assertTrue(gui.is_gui_launch("anything"))

    def test_command_fallback(self) -> None:
        os.environ.pop("COS_APP_GUI", None)
        self.assertTrue(gui.is_gui_launch("--gui"))
        self.assertFalse(gui.is_gui_launch("create"))
        self.assertFalse(gui.is_gui_launch(None))


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

    def test_missing_app_id_defaults_to_unknown(self) -> None:
        os.environ.pop("COS_APP_ID", None)
        os.environ.pop("COS_ARGS_JSON", None)
        ctx = gui.context()
        self.assertEqual(ctx.app_id, "unknown")
        self.assertEqual(ctx.files, [])

    def test_malformed_args_json_is_ignored(self) -> None:
        os.environ["COS_APP_ID"] = "notes"
        os.environ["COS_ARGS_JSON"] = "not-json"
        ctx = gui.context()
        self.assertEqual(ctx.files, [])


class OverlayTests(unittest.TestCase):
    def test_missing_binary_raises(self) -> None:
        ctx = gui.GuiContext(app_id="notes")
        os.environ["COS_AGENT_UI_BIN"] = "/nonexistent/attacker"
        try:
            with self.assertRaises(FileNotFoundError):
                ctx.open_agent_overlay()
        finally:
            os.environ.pop("COS_AGENT_UI_BIN", None)


if __name__ == "__main__":
    unittest.main()
