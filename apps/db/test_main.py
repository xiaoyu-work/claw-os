"""Tests for db app query row limits."""

import os
import sqlite3
import tempfile
import unittest

import sys
sys.path.insert(0, os.path.dirname(__file__))

# Override DATA_DIR before importing
_tmpdir = tempfile.mkdtemp()
os.environ["COS_DATA_DIR"] = _tmpdir

import main as db_main
from main import cmd_query, MAX_ROWS


class TestCmdQueryTruncation(unittest.TestCase):
    def setUp(self):
        self.db_name = "testdb"
        path = db_main._db_path(self.db_name)
        conn = sqlite3.connect(path)
        conn.execute("CREATE TABLE IF NOT EXISTS items (id INTEGER PRIMARY KEY, val TEXT)")
        conn.execute("DELETE FROM items")
        # Insert MAX_ROWS + 100 rows
        for i in range(MAX_ROWS + 100):
            conn.execute("INSERT INTO items (id, val) VALUES (?, ?)", (i, f"row_{i}"))
        conn.commit()
        conn.close()

    def test_query_truncated(self):
        result = cmd_query([self.db_name, "SELECT * FROM items"])
        self.assertEqual(result["count"], MAX_ROWS)
        self.assertTrue(result["truncated"])
        self.assertEqual(result["total_rows"], MAX_ROWS + 100)
        self.assertEqual(len(result["rows"]), MAX_ROWS)

    def test_query_not_truncated(self):
        result = cmd_query([self.db_name, "SELECT * FROM items LIMIT 10"])
        self.assertEqual(result["count"], 10)
        self.assertNotIn("truncated", result)
        self.assertNotIn("total_rows", result)

    def test_query_exact_limit(self):
        result = cmd_query([self.db_name, f"SELECT * FROM items LIMIT {MAX_ROWS}"])
        self.assertEqual(result["count"], MAX_ROWS)
        self.assertNotIn("truncated", result)

    def test_query_error(self):
        result = cmd_query([self.db_name, "SELECT * FROM nonexistent"])
        self.assertIn("error", result)

    def test_query_missing_args(self):
        result = cmd_query(["onlydb"])
        self.assertIn("error", result)


if __name__ == "__main__":
    unittest.main()
