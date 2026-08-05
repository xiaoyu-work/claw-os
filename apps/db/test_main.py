"""Tests for db app query row limits."""

import os
import pathlib
import sqlite3
import tempfile
import unittest

import sys
sys.path.insert(0, os.path.dirname(__file__))
sys.path.insert(
    0,
    os.path.join(
        os.path.dirname(__file__), os.pardir, os.pardir,
        "claw-os-sdk", "python", "src",
    ),
)  # for `from claw_os_sdk import …`
sys.path.insert(
    0,
    os.path.join(
        os.path.dirname(__file__), os.pardir, os.pardir,
        "cos-runtime", "python", "src",
    ),
)  # for `from cos_runtime import …`

# Override DATA_DIR before importing
_tmpdir = tempfile.mkdtemp()
os.environ["COS_DATA_DIR"] = _tmpdir

from test_support import load_local_module

db_main = load_local_module(
    pathlib.Path(__file__).with_name("main.py"),
    "claw_test_db_main",
    clear_modules=("_shared",),
)
cmd_query = db_main.cmd_query
MAX_ROWS = db_main.MAX_ROWS


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


class TestNameTraversal(unittest.TestCase):
    """Regression coverage for CR-3 (db name path-traversal).

    Database names used to be plumbed straight through
    ``os.path.join(DB_DIR, f"{name}.db")``. A name like
    ``"../../../etc/passwd"`` therefore happily walked above
    ``DB_DIR`` — and because the verbs only ever opened the file
    via sqlite, the agent could ``cos db tables ../../../etc/foo``
    to ``stat`` arbitrary fs paths.

    The fix validates the name against a strict character set
    (``[A-Za-z0-9_.-]``), refuses names with ``..`` / leading
    ``.`` / path separators, and verifies the resolved path's
    parent equals ``realpath(DB_DIR)``.
    """

    def test_parent_directory_traversal_rejected(self):
        for bad in (
            "../etc/passwd",
            "../../etc/passwd",
            "../shadow",
        ):
            result = cmd_query([bad, "SELECT 1"])
            self.assertIn(
                "error",
                result,
                f"CR-3 regression: traversal name {bad!r} was accepted",
            )

    def test_absolute_path_rejected(self):
        for bad in ("/etc/passwd", "/tmp/x", "/"):
            result = cmd_query([bad, "SELECT 1"])
            self.assertIn("error", result, f"absolute name {bad!r} was accepted")

    def test_separator_in_name_rejected(self):
        for bad in ("foo/bar", "foo\\bar", "sub/dir/db"):
            result = cmd_query([bad, "SELECT 1"])
            self.assertIn("error", result, f"name with separator {bad!r} was accepted")

    def test_leading_dot_rejected(self):
        for bad in (".hidden", "..parent", "."):
            result = cmd_query([bad, "SELECT 1"])
            self.assertIn("error", result, f"hidden-style name {bad!r} was accepted")

    def test_null_byte_rejected(self):
        result = cmd_query(["foo\x00bar", "SELECT 1"])
        self.assertIn("error", result)

    def test_empty_name_rejected(self):
        result = cmd_query(["", "SELECT 1"])
        self.assertIn("error", result)

    def test_valid_names_resolve_under_db_dir(self):
        """Sanity: a legal name still works after validation."""
        path = db_main._db_path("safe_name-1.test")
        real_parent = os.path.realpath(os.path.dirname(path))
        real_db_dir = os.path.realpath(db_main.DB_DIR)
        self.assertEqual(real_parent, real_db_dir)


if __name__ == "__main__":
    unittest.main()
