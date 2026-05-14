"""Tests for the rclone adapter.

The real ``rclone`` binary isn't available in CI, so each test writes a
shell stub to a tmpdir, points ``CLAW_RCLONE_BIN`` at it, and re-imports
``main`` so the module-level ``App()`` is fresh.
"""

from __future__ import annotations

import importlib
import io
import json
import os
import pathlib
import shutil
import stat
import sys
import tempfile
import textwrap
import unittest
from contextlib import contextmanager


_HERE = pathlib.Path(__file__).resolve().parent


_STUB = r"""#!/bin/sh
printf '%s\n' "$@" > "$RCLONE_ARGS_LOG"
case "$1" in
  listremotes)
    echo 'mydrive:'
    echo 's3prod:'
    echo 'photos:'
    exit 0
    ;;
  lsjson)
    echo '[{"Path":"a.txt","Name":"a.txt","Size":12,"IsDir":false},{"Path":"sub","Name":"sub","Size":0,"IsDir":true}]'
    exit 0
    ;;
  size)
    echo '{"count":7,"bytes":12345,"sizeless":0}'
    exit 0
    ;;
  copy)
    echo 'Transferred:    1 / 1, 100%' 1>&2
    exit "${RCLONE_EXIT:-0}"
    ;;
esac
echo "stub: unknown verb $1" 1>&2
exit 99
"""


@contextmanager
def _capture_stdout():
    buf = io.StringIO()
    saved = sys.stdout
    sys.stdout = buf
    try:
        yield buf
    finally:
        sys.stdout = saved


class RcloneAdapterTests(unittest.TestCase):
    def setUp(self):
        self.tmp = pathlib.Path(tempfile.mkdtemp(prefix="rclone-adapter-"))
        self.stub = self.tmp / "rclone"
        self.args_log = self.tmp / "args.log"
        self._write_stub(_STUB)
        os.environ["CLAW_RCLONE_BIN"] = str(self.stub)
        os.environ["RCLONE_ARGS_LOG"] = str(self.args_log)
        os.environ.pop("RCLONE_EXIT", None)
        sys.path.insert(0, str(_HERE))
        sys.path.insert(0, str(_HERE.parent.parent / "apps"))
        if "main" in sys.modules:
            del sys.modules["main"]
        self.main = importlib.import_module("main")

    def tearDown(self):
        shutil.rmtree(self.tmp, ignore_errors=True)
        for var in ("CLAW_RCLONE_BIN", "RCLONE_ARGS_LOG", "RCLONE_EXIT"):
            os.environ.pop(var, None)
        try:
            sys.path.remove(str(_HERE))
        except ValueError:
            pass
        try:
            sys.path.remove(str(_HERE.parent.parent / "apps"))
        except ValueError:
            pass
        sys.modules.pop("main", None)

    def _write_stub(self, script: str):
        self.stub.write_text(textwrap.dedent(script))
        self.stub.chmod(self.stub.stat().st_mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH)

    def _call(self, method: str, params: dict | None = None) -> dict:
        frame = {"jsonrpc": "2.0", "id": 1, "method": method, "params": params or {}}
        with _capture_stdout() as buf:
            self.main.app._handle_line(json.dumps(frame))
        out = buf.getvalue().strip().splitlines()
        self.assertTrue(out, "adapter produced no stdout")
        return json.loads(out[-1])

    def _tool_call(self, name: str, arguments: dict | None = None) -> dict:
        return self._call("tools/call", {"name": name, "arguments": arguments or {}})

    # ---- tools/list ----

    def test_tools_list_reports_four_tools(self):
        resp = self._call("tools/list")
        names = sorted(t["name"] for t in resp["result"]["tools"])
        self.assertEqual(
            names,
            ["cloud.copy", "cloud.listremotes", "cloud.ls", "cloud.size"],
        )

    # ---- cloud.listremotes ----

    def test_listremotes_strips_trailing_colon(self):
        resp = self._tool_call("cloud.listremotes")
        result = resp["result"]
        self.assertFalse(result.get("isError"))
        data = json.loads(result["content"][0]["text"])
        self.assertEqual(data, ["mydrive", "s3prod", "photos"])

    # ---- cloud.ls ----

    def test_ls_returns_parsed_json(self):
        resp = self._tool_call("cloud.ls", {"target": "mydrive:"})
        data = json.loads(resp["result"]["content"][0]["text"])
        self.assertEqual(len(data), 2)
        self.assertEqual(data[0]["Name"], "a.txt")
        self.assertTrue(data[1]["IsDir"])
        args = self.args_log.read_text().splitlines()
        self.assertEqual(args, ["lsjson", "mydrive:"])

    def test_ls_rejects_target_without_colon(self):
        resp = self._tool_call("cloud.ls", {"target": "mydrive"})
        result = resp["result"]
        self.assertTrue(result.get("isError"))
        msg = result["content"][0]["text"]
        self.assertIn("remote:path", msg)

    def test_ls_rejects_bad_remote_name(self):
        resp = self._tool_call("cloud.ls", {"target": "-evil:foo"})
        result = resp["result"]
        self.assertTrue(result.get("isError"))

    def test_ls_surfaces_invalid_json(self):
        self._write_stub("""\
            #!/bin/sh
            echo 'not-json'
            exit 0
        """)
        resp = self._tool_call("cloud.ls", {"target": "mydrive:foo"})
        result = resp["result"]
        self.assertTrue(result.get("isError"))
        self.assertIn("invalid JSON", result["content"][0]["text"])

    # ---- cloud.size ----

    def test_size_returns_parsed_object(self):
        resp = self._tool_call("cloud.size", {"target": "mydrive:bigdir"})
        data = json.loads(resp["result"]["content"][0]["text"])
        self.assertEqual(data, {"count": 7, "bytes": 12345, "sizeless": 0})
        args = self.args_log.read_text().splitlines()
        self.assertEqual(args, ["size", "--json", "mydrive:bigdir"])

    # ---- cloud.copy ----

    def test_copy_builds_argv_and_tails_stderr(self):
        resp = self._tool_call(
            "cloud.copy",
            {"source": "mydrive:photos", "destination": "/home/u/Pictures"},
        )
        data = json.loads(resp["result"]["content"][0]["text"])
        self.assertEqual(data["source"], "mydrive:photos")
        self.assertEqual(data["destination"], "/home/u/Pictures")
        self.assertFalse(data["dry_run"])
        self.assertIn("Transferred", data["stderr_tail"])
        args = self.args_log.read_text().splitlines()
        self.assertEqual(args, ["copy", "mydrive:photos", "/home/u/Pictures"])

    def test_copy_dry_run_adds_flag(self):
        self._tool_call(
            "cloud.copy",
            {
                "source": "/home/u/Pictures",
                "destination": "mydrive:photos",
                "dry_run": True,
            },
        )
        args = self.args_log.read_text().splitlines()
        self.assertEqual(
            args, ["copy", "--dry-run", "/home/u/Pictures", "mydrive:photos"]
        )

    def test_copy_rejects_leading_dash_in_path(self):
        resp = self._tool_call(
            "cloud.copy", {"source": "--delete-after", "destination": "mydrive:dst"}
        )
        result = resp["result"]
        self.assertTrue(result.get("isError"))

    def test_copy_surfaces_nonzero_exit(self):
        os.environ["RCLONE_EXIT"] = "3"
        resp = self._tool_call(
            "cloud.copy",
            {"source": "mydrive:photos", "destination": "/home/u/Pictures"},
        )
        result = resp["result"]
        self.assertTrue(result.get("isError"))
        self.assertIn("exit 3", result["content"][0]["text"])


if __name__ == "__main__":
    unittest.main()
