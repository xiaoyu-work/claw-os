"""Tests for net app response size limits."""

import os
import sys
import unittest

sys.path.insert(0, os.path.dirname(__file__))

from main import MAX_RESPONSE_BYTES


class TestFetchTruncationLogic(unittest.TestCase):
    """Test the truncation logic applied in cmd_fetch."""

    def _apply_truncation(self, raw_bytes):
        """Replicate the truncation logic from cmd_fetch."""
        truncated = len(raw_bytes) > MAX_RESPONSE_BYTES
        if truncated:
            raw_bytes = raw_bytes[:MAX_RESPONSE_BYTES]
        body = raw_bytes.decode("utf-8", errors="replace")
        result = {
            "url": "http://example.com",
            "status": 200,
            "headers": {},
            "body": body,
        }
        if truncated:
            result["truncated"] = True
        return result

    def test_small_response_not_truncated(self):
        result = self._apply_truncation(b"hello")
        self.assertEqual(result["body"], "hello")
        self.assertNotIn("truncated", result)

    def test_large_response_truncated(self):
        big = b"x" * (MAX_RESPONSE_BYTES + 1000)
        result = self._apply_truncation(big)
        self.assertEqual(len(result["body"]), MAX_RESPONSE_BYTES)
        self.assertTrue(result["truncated"])

    def test_exact_limit_not_truncated(self):
        exact = b"a" * MAX_RESPONSE_BYTES
        result = self._apply_truncation(exact)
        self.assertEqual(len(result["body"]), MAX_RESPONSE_BYTES)
        self.assertNotIn("truncated", result)


class TestConstantInFile(unittest.TestCase):
    def test_constant_value(self):
        self.assertEqual(MAX_RESPONSE_BYTES, 5_000_000)

    def test_constant_defined_in_source(self):
        main_path = os.path.join(os.path.dirname(__file__), "main.py")
        with open(main_path) as f:
            content = f.read()
        self.assertIn("MAX_RESPONSE_BYTES = 5_000_000", content)
        self.assertIn('result["truncated"] = True', content)


if __name__ == "__main__":
    unittest.main()
