"""Tests for fs app output size limits."""

import os
import tempfile
import unittest

# Adjust path so we can import the module
import sys
sys.path.insert(0, os.path.dirname(__file__))

from main import cmd_read, MAX_READ_BYTES


class TestCmdReadTruncation(unittest.TestCase):
    def setUp(self):
        self.tmpdir = tempfile.mkdtemp()

    def tearDown(self):
        import shutil
        shutil.rmtree(self.tmpdir)

    def _write(self, name, content_bytes):
        path = os.path.join(self.tmpdir, name)
        with open(path, "wb") as f:
            f.write(content_bytes)
        return path

    def test_small_file_no_truncation(self):
        path = self._write("small.txt", b"hello world")
        result = cmd_read([path])
        self.assertEqual(result["content"], "hello world")
        self.assertNotIn("truncated", result)
        self.assertNotIn("total_size", result)

    def test_large_file_truncated(self):
        data = b"x" * (MAX_READ_BYTES + 500)
        path = self._write("big.txt", data)
        result = cmd_read([path])
        self.assertEqual(len(result["content"]), MAX_READ_BYTES)
        self.assertTrue(result["truncated"])
        self.assertEqual(result["total_size"], MAX_READ_BYTES + 500)

    def test_exact_limit_no_truncation(self):
        data = b"a" * MAX_READ_BYTES
        path = self._write("exact.txt", data)
        result = cmd_read([path])
        self.assertEqual(len(result["content"]), MAX_READ_BYTES)
        self.assertNotIn("truncated", result)

    def test_offset(self):
        path = self._write("offset.txt", b"0123456789")
        result = cmd_read([path, "--offset", "5"])
        self.assertEqual(result["content"], "56789")
        self.assertEqual(result["offset"], 5)
        self.assertNotIn("truncated", result)

    def test_limit(self):
        path = self._write("limit.txt", b"0123456789")
        result = cmd_read([path, "--limit", "3"])
        self.assertEqual(result["content"], "012")
        self.assertTrue(result["truncated"])

    def test_offset_and_limit(self):
        path = self._write("combo.txt", b"abcdefghij")
        result = cmd_read([path, "--offset", "2", "--limit", "4"])
        self.assertEqual(result["content"], "cdef")
        self.assertTrue(result["truncated"])

    def test_limit_capped_to_max(self):
        """User-specified limit above MAX_READ_BYTES is capped."""
        data = b"y" * (MAX_READ_BYTES + 100)
        path = self._write("capped.txt", data)
        result = cmd_read([path, "--limit", str(MAX_READ_BYTES + 50000)])
        self.assertEqual(len(result["content"]), MAX_READ_BYTES)
        self.assertTrue(result["truncated"])

    def test_file_not_found(self):
        result = cmd_read(["/nonexistent/file.txt"])
        self.assertIn("error", result)

    def test_no_args_raises(self):
        with self.assertRaises(Exception):
            cmd_read([])


if __name__ == "__main__":
    unittest.main()
