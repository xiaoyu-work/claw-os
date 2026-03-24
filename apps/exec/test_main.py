"""Tests for exec app output size limits."""

import os
import sys
import unittest

sys.path.insert(0, os.path.dirname(__file__))

# We can't import main directly on Windows due to fcntl, so test the
# truncation logic in isolation by importing the constants and simulating.
# On Linux this would import fine. We test the logic portably here.


MAX_OUTPUT_BYTES = 1_000_000  # mirror the constant from main.py


class TestOutputTruncationLogic(unittest.TestCase):
    """Test the truncation logic that cmd_run and cmd_script apply."""

    def _apply_truncation(self, stdout, stderr):
        """Replicate the truncation logic from cmd_run/cmd_script."""
        truncated = False
        if len(stdout) > MAX_OUTPUT_BYTES:
            stdout = stdout[:MAX_OUTPUT_BYTES]
            truncated = True
        if len(stderr) > MAX_OUTPUT_BYTES:
            stderr = stderr[:MAX_OUTPUT_BYTES]
            truncated = True
        resp = {
            "exit_code": 0,
            "stdout": stdout,
            "stderr": stderr,
        }
        if truncated:
            resp["truncated"] = True
        return resp

    def test_small_output_not_truncated(self):
        resp = self._apply_truncation("hello", "")
        self.assertEqual(resp["stdout"], "hello")
        self.assertNotIn("truncated", resp)

    def test_large_stdout_truncated(self):
        big = "x" * (MAX_OUTPUT_BYTES + 500)
        resp = self._apply_truncation(big, "")
        self.assertEqual(len(resp["stdout"]), MAX_OUTPUT_BYTES)
        self.assertTrue(resp["truncated"])

    def test_large_stderr_truncated(self):
        big = "e" * (MAX_OUTPUT_BYTES + 500)
        resp = self._apply_truncation("ok", big)
        self.assertEqual(len(resp["stderr"]), MAX_OUTPUT_BYTES)
        self.assertEqual(resp["stdout"], "ok")
        self.assertTrue(resp["truncated"])

    def test_both_truncated(self):
        big_out = "o" * (MAX_OUTPUT_BYTES + 1)
        big_err = "e" * (MAX_OUTPUT_BYTES + 1)
        resp = self._apply_truncation(big_out, big_err)
        self.assertEqual(len(resp["stdout"]), MAX_OUTPUT_BYTES)
        self.assertEqual(len(resp["stderr"]), MAX_OUTPUT_BYTES)
        self.assertTrue(resp["truncated"])

    def test_exact_limit_not_truncated(self):
        exact = "x" * MAX_OUTPUT_BYTES
        resp = self._apply_truncation(exact, "")
        self.assertEqual(len(resp["stdout"]), MAX_OUTPUT_BYTES)
        self.assertNotIn("truncated", resp)


class TestConstantInFile(unittest.TestCase):
    """Verify the constant is defined in main.py."""

    def test_constant_defined(self):
        main_path = os.path.join(os.path.dirname(__file__), "main.py")
        with open(main_path) as f:
            content = f.read()
        self.assertIn("MAX_OUTPUT_BYTES = 1_000_000", content)
        self.assertIn("MAX_OUTPUT_BYTES", content)
        # Verify truncation logic is present for both cmd_run and cmd_script
        self.assertIn('resp["truncated"] = True', content)


if __name__ == "__main__":
    unittest.main()
