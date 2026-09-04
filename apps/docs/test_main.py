"""Tests for the Recoll-backed docs App."""

from __future__ import annotations

import ast
import json
import os
import pathlib
import shutil
import stat
import sys
import tempfile
import unittest
from unittest import mock

from test_support import load_local_module


APP_DIR = pathlib.Path(__file__).resolve().parent
MANIFEST_PATH = APP_DIR / "app.json"
SERVER_PATH = APP_DIR / "server.py"
TOOL_NAMES = [
    "docs.search",
    "docs.index",
    "docs.status",
    "docs.configure",
]

_RECOLLQ_STUB = r"""#!/bin/sh
printf '%s\n' "$@" > "$RECOLLQ_ARGS_LOG"
printf '%s\n' "$HOME" > "$RECOLLQ_HOME_LOG"

QUERY=
for arg in "$@"; do
    QUERY=$arg
done

if [ -n "$RECOLLQ_FAIL" ]; then
    echo "stub recollq forced failure" 1>&2
    exit "${RECOLLQ_EXIT:-1}"
fi
if [ -n "$RECOLLQ_MALFORMED" ]; then
    printf '%s\n' '"unterminated'
    exit 0
fi

case "$QUERY" in
  *empty*)
    exit 0
    ;;
  *)
    cat <<'OUT'
"file:///home/jay/Documents/budget-q3.xlsx" "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet" "1700000000" "Q3 budget proposal: marketing 1.2M, R&D 3.5M"
"file:///home/jay/Documents/notes/budget plan.md" "text/markdown" "1701000000" "Quarterly budget plan v3"
OUT
    ;;
esac
"""

_RECOLLINDEX_STUB = r"""#!/bin/sh
printf '%s\n' "$@" > "$RECOLLINDEX_ARGS_LOG"
printf '%s\n' "$HOME" > "$RECOLLINDEX_HOME_LOG"
echo "Indexing ~/Documents..." 1>&2
exit "${RECOLLINDEX_EXIT:-0}"
"""


def _write_stub(path: pathlib.Path, content: str) -> None:
    path.write_text(content, encoding="utf-8")
    path.chmod(path.stat().st_mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH)


def _server_bindings() -> dict[str, ast.FunctionDef]:
    bindings: dict[str, ast.FunctionDef] = {}
    for node in ast.parse(SERVER_PATH.read_text(encoding="utf-8")).body:
        if not isinstance(node, ast.FunctionDef):
            continue
        for decorator in node.decorator_list:
            if (
                isinstance(decorator, ast.Call)
                and isinstance(decorator.func, ast.Attribute)
                and isinstance(decorator.func.value, ast.Name)
                and decorator.func.value.id == "app"
                and decorator.func.attr == "tool"
                and len(decorator.args) == 1
                and isinstance(decorator.args[0], ast.Constant)
                and isinstance(decorator.args[0].value, str)
            ):
                bindings[decorator.args[0].value] = node
    return bindings


class DocsAppTests(unittest.TestCase):
    def setUp(self):
        self.tmp = pathlib.Path(tempfile.mkdtemp(prefix="docs-app-"))
        self.owner_home = self.tmp / "owner"
        self.private_home = self.tmp / "private"
        self.owner_home.mkdir()
        self.private_home.mkdir()
        (self.owner_home / ".recoll").mkdir()

        self.recollq = self.tmp / "recollq"
        self.recollindex = self.tmp / "recollindex"
        _write_stub(self.recollq, _RECOLLQ_STUB)
        _write_stub(self.recollindex, _RECOLLINDEX_STUB)

        self.q_args = self.tmp / "recollq.args"
        self.q_home = self.tmp / "recollq.home"
        self.index_args = self.tmp / "recollindex.args"
        self.index_home = self.tmp / "recollindex.home"

        self._saved_env: dict[str, str | None] = {}
        environment = {
            "HOME": str(self.private_home),
            "COS_OWNER_HOME": str(self.owner_home),
            "RECOLLQ_ARGS_LOG": str(self.q_args),
            "RECOLLQ_HOME_LOG": str(self.q_home),
            "RECOLLINDEX_ARGS_LOG": str(self.index_args),
            "RECOLLINDEX_HOME_LOG": str(self.index_home),
        }
        for key, value in environment.items():
            self._saved_env[key] = os.environ.get(key)
            os.environ[key] = value
        for variable in (
            "CLAW_RECOLLQ_BIN",
            "CLAW_RECOLLINDEX_BIN",
            "RECOLLQ_FAIL",
            "RECOLLQ_EXIT",
            "RECOLLQ_MALFORMED",
            "RECOLLINDEX_EXIT",
        ):
            self._saved_env[variable] = os.environ.get(variable)
            os.environ.pop(variable, None)

        sys.path.insert(0, str(APP_DIR))
        sys.path.insert(
            0,
            str(APP_DIR.parent.parent / "claw-os-sdk" / "python" / "src"),
        )
        sys.path.insert(
            0,
            str(APP_DIR.parent.parent / "cos-runtime" / "python" / "src"),
        )
        self.module_name = f"claw_test_docs_main_{id(self)}"
        self.main = load_local_module(
            APP_DIR / "main.py",
            self.module_name,
            clear_modules=("_shared",),
        )
        self.main.RECOLLQ_BIN = str(self.recollq)
        self.main.RECOLLINDEX_BIN = str(self.recollindex)
        self.policy_patcher = mock.patch.object(self.main.policy, "require")
        self.policy = self.policy_patcher.start()

    def tearDown(self):
        self.policy_patcher.stop()
        shutil.rmtree(self.tmp, ignore_errors=True)
        for key, value in self._saved_env.items():
            if value is None:
                os.environ.pop(key, None)
            else:
                os.environ[key] = value
        for path in (
            str(APP_DIR),
            str(APP_DIR.parent.parent / "claw-os-sdk" / "python" / "src"),
            str(APP_DIR.parent.parent / "cos-runtime" / "python" / "src"),
        ):
            try:
                sys.path.remove(path)
            except ValueError:
                pass
        sys.modules.pop(self.module_name, None)

    def _create_index(self) -> pathlib.Path:
        index = self.owner_home / ".recoll" / "xapiandb"
        index.mkdir()
        return index

    def test_manifest_and_handlers_are_mcp_only_and_aligned(self):
        manifest = json.loads(MANIFEST_PATH.read_text(encoding="utf-8"))
        self.assertNotIn("operations", manifest)
        self.assertNotIn("ai", manifest)

        tools = manifest["mcp"]["tools"]
        self.assertEqual([tool["name"] for tool in tools], TOOL_NAMES)
        self.assertEqual(
            tools[0]["args"],
            [
                {
                    "name": "query",
                    "kind": "text",
                    "required": True,
                    "binding": "flag",
                },
                {
                    "name": "max_results",
                    "kind": "integer",
                    "required": False,
                    "binding": "flag",
                    "default": 20,
                },
            ],
        )

        source = SERVER_PATH.read_text(encoding="utf-8")
        self.assertIn("from claw_os_sdk.mcp import App", source)
        self.assertNotIn("serve_manifest_operations", source)
        bindings = _server_bindings()
        self.assertEqual(list(bindings), TOOL_NAMES)
        self.assertEqual(
            [argument.arg for argument in bindings["docs.search"].args.args],
            ["query", "max_results"],
        )
        self.assertEqual(
            ast.literal_eval(bindings["docs.search"].args.defaults[0]),
            20,
        )

        main_tree = ast.parse((APP_DIR / "main.py").read_text(encoding="utf-8"))
        main_source = (APP_DIR / "main.py").read_text(encoding="utf-8")
        self.assertIn('RECOLLQ_BIN = "/usr/bin/recollq"', main_source)
        self.assertIn('RECOLLINDEX_BIN = "/usr/bin/recollindex"', main_source)
        self.assertNotIn("CLAW_RECOLLQ_BIN", main_source)
        self.assertNotIn("CLAW_RECOLLINDEX_BIN", main_source)
        self.assertFalse(
            any(
                isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef))
                and node.name == "run"
                for node in main_tree.body
            )
        )

    def test_owner_home_is_daemon_supplied_not_private_home(self):
        self.assertEqual(self.main.OWNER_HOME, self.owner_home)
        self.assertEqual(self.main.RECOLL_DIR, self.owner_home / ".recoll")
        self.assertNotEqual(self.main.RECOLL_DIR, self.private_home / ".recoll")

    def test_owner_home_has_no_missing_or_noncanonical_fallback(self):
        with mock.patch.dict(os.environ, {}, clear=False):
            os.environ.pop("COS_OWNER_HOME", None)
            with self.assertRaisesRegex(RuntimeError, "COS_OWNER_HOME is required"):
                self.main._owner_home()
        with mock.patch.dict(
            os.environ,
            {"COS_OWNER_HOME": str(self.owner_home / ".." / "owner")},
        ):
            with self.assertRaisesRegex(RuntimeError, "absolute canonical path"):
                self.main._owner_home()

    def test_configure_creates_default_config_atomically(self):
        result = self.main.configure()

        config = self.owner_home / ".recoll" / "recoll.conf"
        self.assertTrue(result["created"])
        self.assertTrue(config.exists())
        self.assertIn("topdirs", config.read_text(encoding="utf-8"))
        self.assertFalse((self.private_home / ".recoll" / "recoll.conf").exists())
        self.policy.assert_called_once_with(
            "fs.write",
            path=str(self.owner_home / ".recoll"),
        )

    def test_configure_is_idempotent(self):
        self.main.configure()
        result = self.main.configure()

        self.assertFalse(result["created"])
        self.assertIn("left untouched", result["message"])

    def test_status_when_nothing_is_configured(self):
        result = self.main.status()

        self.assertFalse(result["config_exists"])
        self.assertFalse(result["index_exists"])
        self.assertEqual(result["topdirs"], [])
        self.policy.assert_called_once_with(
            "fs.read",
            path=str(self.owner_home / ".recoll"),
        )

    def test_status_reads_config_and_index_metadata(self):
        self.main.configure()
        index = self._create_index()
        (index / "termlist.glass").write_bytes(b"\0" * 16)
        self.policy.reset_mock()

        result = self.main.status()

        self.assertTrue(result["config_exists"])
        self.assertTrue(result["index_exists"])
        self.assertIn("~/Documents", result["topdirs"])
        self.assertEqual(result["index_files"], ["termlist.glass"])
        self.assertIsInstance(result["last_indexed"], int)

    def test_status_surfaces_invalid_config_encoding(self):
        self.main.RECOLL_CONF.write_bytes(b"\xff")

        with self.assertRaises(UnicodeDecodeError):
            self.main.status()

    def test_search_without_index_returns_actionable_hint(self):
        result = self.main.search("budget")

        self.assertEqual(result["count"], 0)
        self.assertIn("cos app docs index", result["hint"])
        self.assertEqual(
            self.policy.call_args_list,
            [
                mock.call("proc.spawn", name="recollq"),
                mock.call("fs.read", path=str(self.owner_home / ".recoll")),
            ],
        )

    def test_search_parses_results_and_uses_owner_config(self):
        self._create_index()

        result = self.main.search("budget", 5)

        self.assertEqual(result["query"], "budget")
        self.assertEqual(result["count"], 2)
        self.assertEqual(
            result["results"][0]["path"],
            "/home/jay/Documents/budget-q3.xlsx",
        )
        self.assertIn("Q3 budget", result["results"][0]["snippet"])
        self.assertEqual(
            result["results"][1]["path"],
            "/home/jay/Documents/notes/budget plan.md",
        )
        arguments = self.q_args.read_text(encoding="utf-8").splitlines()
        self.assertEqual(arguments[:2], ["-c", str(self.owner_home / ".recoll")])
        self.assertIn("5:0", arguments)
        self.assertEqual(arguments[-1], "budget")
        self.assertEqual(
            self.q_home.read_text(encoding="utf-8").strip(),
            str(self.owner_home),
        )

    def test_search_zero_hits(self):
        self._create_index()

        result = self.main.search("empty")

        self.assertEqual(result["results"], [])
        self.assertEqual(result["count"], 0)

    def test_search_rejects_invalid_arguments_before_policy(self):
        for query in ("", "   ", None):
            with self.subTest(query=query):
                with self.assertRaisesRegex(ValueError, "query must"):
                    self.main.search(query)
        for limit in (0, 201, True, "5"):
            with self.subTest(limit=limit):
                with self.assertRaisesRegex(ValueError, "max_results must"):
                    self.main.search("budget", limit)
        self.policy.assert_not_called()

    def test_search_surfaces_recollq_failure(self):
        self._create_index()
        os.environ["RECOLLQ_FAIL"] = "1"
        os.environ["RECOLLQ_EXIT"] = "2"

        with self.assertRaisesRegex(RuntimeError, "recollq exited 2"):
            self.main.search("anything")

    def test_search_rejects_malformed_recoll_output(self):
        self._create_index()
        os.environ["RECOLLQ_MALFORMED"] = "1"

        with self.assertRaisesRegex(RuntimeError, "malformed result line"):
            self.main.search("anything")

    def test_index_requires_config(self):
        with self.assertRaisesRegex(FileNotFoundError, "recoll.conf"):
            self.main.index()

    def test_index_runs_fixed_binary_with_owner_config(self):
        self.main.configure()
        self.policy.reset_mock()

        result = self.main.index()

        self.assertTrue(result["ok"])
        self.assertEqual(result["exit"], 0)
        self.assertGreaterEqual(result["elapsed_secs"], 0.0)
        self.assertEqual(
            self.index_args.read_text(encoding="utf-8").splitlines(),
            ["-c", str(self.owner_home / ".recoll")],
        )
        self.assertEqual(
            self.index_home.read_text(encoding="utf-8").strip(),
            str(self.owner_home),
        )
        self.assertEqual(
            self.policy.call_args_list,
            [
                mock.call("proc.spawn", name="recollindex"),
                mock.call("fs.read", wild=True),
                mock.call("fs.write", path=str(self.owner_home / ".recoll")),
            ],
        )

    def test_index_surfaces_nonzero_exit(self):
        self.main.configure()
        os.environ["RECOLLINDEX_EXIT"] = "5"

        with self.assertRaisesRegex(RuntimeError, "recollindex exited 5"):
            self.main.index()


if __name__ == "__main__":
    unittest.main()
