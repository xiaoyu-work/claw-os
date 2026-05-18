"""Tests for the docs (Recoll) app.

Stubs ``recollq`` and ``recollindex`` by pointing
``CLAW_RECOLLQ_BIN`` / ``CLAW_RECOLLINDEX_BIN`` at shell scripts in a
tmpdir. Each test gets its own fake ``~/.recoll``.
"""

from __future__ import annotations

import importlib
import os
import pathlib
import shutil
import stat
import sys
import tempfile
import textwrap
import unittest


_HERE = pathlib.Path(__file__).resolve().parent


_RECOLLQ_STUB = r"""#!/bin/sh
# Args observed by the test
printf '%s\n' "$@" > "$RECOLLQ_ARGS_LOG"

# Detect the query: with our caller we pass `-t -n N:0 -F "url mtype mtime abstract" <query>`
# The last positional arg is the query.
QUERY="${@: -1}"

# Configurable failure
if [ -n "$RECOLLQ_FAIL" ]; then
    echo "stub recollq forced failure" 1>&2
    exit "${RECOLLQ_EXIT:-1}"
fi

case "$QUERY" in
  *empty*)
    # produce zero hits
    exit 0
    ;;
  *)
    cat <<'OUT'
"file:///home/jay/Documents/budget-q3.xlsx" "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet" "1700000000" "Q3 budget proposal: marketing 1.2M, R&D 3.5M"
"file:///home/jay/Documents/notes/budget plan.md" "text/markdown" "1701000000" "Quarterly budget plan v3"
OUT
    exit 0
    ;;
esac
"""


_RECOLLINDEX_STUB = r"""#!/bin/sh
printf '%s\n' "$@" > "$RECOLLINDEX_ARGS_LOG"
echo "Indexing ~/Documents..." 1>&2
exit "${RECOLLINDEX_EXIT:-0}"
"""


_COS_STUB = r"""#!/bin/sh
# Permissive stub used in tests — every policy check returns allow.
case "$2" in
  check)
    echo '{"decision":"allow"}'
    exit 0
    ;;
esac
echo "stub cos: unsupported subcommand: $@" 1>&2
exit 99
"""


def _write_stub(path: pathlib.Path, content: str):
    path.write_text(content)
    path.chmod(path.stat().st_mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH)


class DocsAppTests(unittest.TestCase):
    def setUp(self):
        self.tmp = pathlib.Path(tempfile.mkdtemp(prefix="docs-app-"))
        self.fake_home = self.tmp / "home"
        self.fake_home.mkdir()
        (self.fake_home / ".recoll").mkdir()

        # stub bins
        self.recollq = self.tmp / "recollq"
        self.recollindex = self.tmp / "recollindex"
        self.cos = self.tmp / "cos"
        _write_stub(self.recollq, _RECOLLQ_STUB)
        _write_stub(self.recollindex, _RECOLLINDEX_STUB)
        _write_stub(self.cos, _COS_STUB)

        self.q_args = self.tmp / "recollq.args"
        self.idx_args = self.tmp / "recollindex.args"

        # Environment for the app
        self._saved_env = {}
        for k, v in {
            "HOME": str(self.fake_home),
            "CLAW_RECOLLQ_BIN": str(self.recollq),
            "CLAW_RECOLLINDEX_BIN": str(self.recollindex),
            "RECOLLQ_ARGS_LOG": str(self.q_args),
            "RECOLLINDEX_ARGS_LOG": str(self.idx_args),
            "COS_BIN": str(self.cos),
        }.items():
            self._saved_env[k] = os.environ.get(k)
            os.environ[k] = v
        for var in ("RECOLLQ_FAIL", "RECOLLQ_EXIT", "RECOLLINDEX_EXIT"):
            self._saved_env[var] = os.environ.get(var)
            os.environ.pop(var, None)

        # Fresh import so module-level HOME picks up our fake $HOME
        sys.path.insert(0, str(_HERE))
        sys.path.insert(
            0,
            str(_HERE.parent.parent / "claw-os-sdk" / "python" / "src"),
        )  # for `from claw_os_sdk import …`
        sys.path.insert(
            0,
            str(_HERE.parent.parent / "cos-runtime" / "python" / "src"),
        )  # for `from cos_runtime import …`
        if "main" in sys.modules:
            del sys.modules["main"]
        self.main = importlib.import_module("main")

    def tearDown(self):
        shutil.rmtree(self.tmp, ignore_errors=True)
        for k, v in self._saved_env.items():
            if v is None:
                os.environ.pop(k, None)
            else:
                os.environ[k] = v
        for p in (
            str(_HERE),
            str(_HERE.parent.parent / "claw-os-sdk" / "python" / "src"),
            str(_HERE.parent.parent / "cos-runtime" / "python" / "src"),
        ):
            try:
                sys.path.remove(p)
            except ValueError:
                pass
        sys.modules.pop("main", None)

    # ---- configure ----

    def test_configure_creates_default_config(self):
        result = self.main.run("configure", [])
        self.assertTrue(result["created"])
        conf = self.fake_home / ".recoll" / "recoll.conf"
        self.assertTrue(conf.exists())
        text = conf.read_text()
        self.assertIn("topdirs", text)
        self.assertIn("~/Documents", text)

    def test_configure_is_idempotent(self):
        self.main.run("configure", [])
        result = self.main.run("configure", [])
        self.assertFalse(result["created"])
        self.assertIn("already exists", result["message"])

    def test_configure_rejects_args(self):
        result = self.main.run("configure", ["--surprise"])
        self.assertIn("error", result)

    # ---- status ----

    def test_status_when_nothing_configured(self):
        result = self.main.run("status", [])
        self.assertFalse(result["config_exists"])
        self.assertFalse(result["index_exists"])
        self.assertEqual(result["topdirs"], [])

    def test_status_reads_topdirs_from_config(self):
        self.main.run("configure", [])
        result = self.main.run("status", [])
        self.assertTrue(result["config_exists"])
        self.assertIn("~/Documents", result["topdirs"])

    def test_status_reports_index_presence(self):
        self.main.run("configure", [])
        # Simulate a built index
        idx = self.fake_home / ".recoll" / "xapiandb"
        idx.mkdir()
        (idx / "termlist.glass").write_bytes(b"\0" * 16)
        result = self.main.run("status", [])
        self.assertTrue(result["index_exists"])
        self.assertIn("termlist.glass", result["index_files"])
        self.assertIsInstance(result["last_indexed"], int)

    # ---- search ----

    def test_search_requires_index(self):
        # No xapiandb yet
        result = self.main.run("search", ["--query", "budget"])
        self.assertEqual(result["count"], 0)
        self.assertIn("hint", result)

    def test_search_parses_recollq_output(self):
        # Pretend an index exists
        (self.fake_home / ".recoll" / "xapiandb").mkdir()
        result = self.main.run("search", ["--query", "budget", "--max-results", "5"])
        self.assertEqual(result["query"], "budget")
        self.assertEqual(result["count"], 2)
        self.assertEqual(
            result["results"][0]["path"], "/home/jay/Documents/budget-q3.xlsx"
        )
        self.assertEqual(result["results"][0]["mime"].split("/")[0], "application")
        self.assertIn("Q3 budget", result["results"][0]["snippet"])
        # Path with embedded space is preserved
        self.assertEqual(
            result["results"][1]["path"], "/home/jay/Documents/notes/budget plan.md"
        )
        # Recollq received the right argv
        args = self.q_args.read_text().splitlines()
        self.assertIn("-t", args)
        self.assertIn("-n", args)
        self.assertIn("5:0", args)
        self.assertIn("budget", args)

    def test_search_clamps_max_results(self):
        (self.fake_home / ".recoll" / "xapiandb").mkdir()
        self.main.run("search", ["--query", "budget", "--max-results", "999999"])
        args = self.q_args.read_text().splitlines()
        # MAX_MAX_RESULTS is 200
        self.assertIn("200:0", args)

    def test_search_zero_hits(self):
        (self.fake_home / ".recoll" / "xapiandb").mkdir()
        result = self.main.run("search", ["--query", "empty"])
        self.assertEqual(result["count"], 0)
        self.assertEqual(result["results"], [])

    def test_search_empty_query_rejected(self):
        (self.fake_home / ".recoll" / "xapiandb").mkdir()
        result = self.main.run("search", ["--query", "   "])
        self.assertIn("error", result)

    def test_search_surfaces_recollq_failure(self):
        (self.fake_home / ".recoll" / "xapiandb").mkdir()
        os.environ["RECOLLQ_FAIL"] = "1"
        os.environ["RECOLLQ_EXIT"] = "2"
        result = self.main.run("search", ["--query", "anything"])
        self.assertIn("error", result)
        self.assertIn("exit 2", result["error"])

    # ---- index ----

    def test_index_requires_config(self):
        result = self.main.run("index", [])
        self.assertIn("error", result)
        self.assertIn("recoll.conf", result["error"])

    def test_index_runs_recollindex_when_configured(self):
        self.main.run("configure", [])
        result = self.main.run("index", [])
        self.assertTrue(result["ok"])
        self.assertEqual(result["exit"], 0)
        self.assertGreaterEqual(result["elapsed_secs"], 0.0)

    def test_index_surfaces_nonzero_exit(self):
        self.main.run("configure", [])
        os.environ["RECOLLINDEX_EXIT"] = "5"
        result = self.main.run("index", [])
        self.assertFalse(result["ok"])
        self.assertEqual(result["exit"], 5)

    def test_index_rejects_args(self):
        self.main.run("configure", [])
        result = self.main.run("index", ["junk"])
        self.assertIn("error", result)

    # ---- dispatcher ----

    def test_unknown_command(self):
        result = self.main.run("nope", [])
        self.assertIn("error", result)

    def test_schema_dispatch(self):
        schema = self.main.run("__schema__", [])
        self.assertIn("search", schema)
        self.assertIn("index", schema)
        self.assertIn("status", schema)
        self.assertIn("configure", schema)


if __name__ == "__main__":
    unittest.main()
