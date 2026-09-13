"""Generator output-scope checks for standalone SDK integration."""

from __future__ import annotations

import contextlib
import importlib.util
import io
import pathlib
import sys
import unittest
from unittest import mock


class OutputScopeTests(unittest.TestCase):
    def test_sdk_only_check_does_not_require_or_touch_core_output(self) -> None:
        path = pathlib.Path(__file__).resolve().with_name("codegen.py")
        spec = importlib.util.spec_from_file_location("sdk_codegen_scope_test", path)
        self.assertIsNotNone(spec)
        self.assertIsNotNone(spec.loader)
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        missing_core = module.ROOT.parent / "core" / "__sdk_codegen_scope_test_missing__.rs"
        self.assertFalse(missing_core.exists())
        with mock.patch.object(module, "CORE_MCP_OUT", missing_core):
            with mock.patch.object(sys, "argv", [str(path), "--sdk-only", "--check"]):
                with contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()):
                    self.assertEqual(module.main(), 0)
            stderr = io.StringIO()
            with mock.patch.object(sys, "argv", [str(path), "--check"]):
                with contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(stderr):
                    self.assertEqual(module.main(), 1)
            self.assertIn("stale", stderr.getvalue())
        self.assertFalse(missing_core.exists())


if __name__ == "__main__":
    unittest.main()
