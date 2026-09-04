"""Unit tests for the libarchive (bsdtar) adapter. A shell stub stands
in for ``bsdtar`` so the test runs on any machine."""

from __future__ import annotations

import io
import json
import os
import pathlib
import stat
import sys
import tempfile
import textwrap
import unittest

from test_support import authenticated_mcp_params, load_local_module


HERE = pathlib.Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))


def _write_stub(dir: pathlib.Path, name: str, script: str) -> pathlib.Path:
    p = dir / name
    p.write_text(textwrap.dedent(script))
    p.chmod(p.stat().st_mode | stat.S_IEXEC | stat.S_IXGRP | stat.S_IXOTH)
    return p


class _Capture:
    def __init__(self) -> None:
        self.buf = io.StringIO()
        self._prev: object = None

    def __enter__(self) -> "io.StringIO":
        self._prev = sys.stdout
        sys.stdout = self.buf
        return self.buf

    def __exit__(self, *exc) -> None:
        sys.stdout = self._prev


def _rpc(app, method: str, params: dict | None = None, msg_id: int = 1) -> dict:
    frame = {"jsonrpc": "2.0", "id": msg_id, "method": method}
    if params is not None:
        if method == "tools/call":
            params = authenticated_mcp_params(params, call_id=f"test-{msg_id}")
        frame["params"] = params
    with _Capture() as out:
        app._handle_line(json.dumps(frame))
    line = out.getvalue().strip().splitlines()[-1]
    return json.loads(line)


_BSDTAR_STUB = """\
#!/bin/sh
printf '%s\\n' "$@" > "$BSDTAR_ARGS_LOG"
mode=""
for a in "$@"; do
  case "$a" in
    -tf) mode="list"; shift; src="$1"; break ;;
    -xf) mode="extract"; shift; src="$1"; break ;;
    -acf) mode="create"; shift; dst="$1"; break ;;
  esac
  shift
done
case "$mode" in
  list)
    echo "a/"
    echo "a/file1.txt"
    echo "a/file2.txt"
    ;;
  extract) : ;;  # would unpack to -C dir
  create) : > "$dst" ;;
esac
exit "${BSDTAR_EXIT:-0}"
"""


class LibarchiveAdapterTests(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        root = pathlib.Path(self.tmp.name)
        self.root = root
        self.bin_dir = root / "bin"
        self.bin_dir.mkdir()

        self.args_log = root / "bsdtar.log"
        os.environ["BSDTAR_ARGS_LOG"] = str(self.args_log)
        self.stub = _write_stub(self.bin_dir, "bsdtar", _BSDTAR_STUB)
        os.environ["CLAW_BSDTAR_BIN"] = str(self.stub)

        self.archive = root / "thing.tar.gz"
        self.archive.write_bytes(b"\x1f\x8b fake gz")
        self.file_a = root / "a.txt"
        self.file_a.write_text("hi")
        self.file_b = root / "b.txt"
        self.file_b.write_text("hi2")

        self.main = load_local_module(
            HERE / "main.py",
            "claw_test_libarchive_adapter_main",
        )
        self.main.app._initialized = True

    def tearDown(self) -> None:
        for k in ("CLAW_BSDTAR_BIN", "BSDTAR_ARGS_LOG", "BSDTAR_EXIT"):
            os.environ.pop(k, None)
        sys.modules.pop("claw_test_libarchive_adapter_main", None)

    def test_tools_list_reports_three_tools(self) -> None:
        reply = _rpc(self.main.app, "tools/list")
        names = sorted(t["name"] for t in reply["result"]["tools"])
        self.assertEqual(names, ["archive.create", "archive.extract", "archive.list"])

    def test_list_returns_entries(self) -> None:
        reply = _rpc(
            self.main.app,
            "tools/call",
            {"name": "archive.list", "arguments": {"path": str(self.archive)}},
        )
        self.assertFalse(reply["result"].get("isError", False), reply)
        body = json.loads(reply["result"]["content"][0]["text"])
        self.assertEqual(body, ["a/", "a/file1.txt", "a/file2.txt"])

    def test_list_rejects_missing_archive(self) -> None:
        reply = _rpc(
            self.main.app,
            "tools/call",
            {"name": "archive.list", "arguments": {"path": "/nope.tar"}},
        )
        self.assertTrue(reply["result"]["isError"])

    def test_extract_passes_no_same_owner_and_creates_dest(self) -> None:
        dst = self.root / "out"
        reply = _rpc(
            self.main.app,
            "tools/call",
            {
                "name": "archive.extract",
                "arguments": {"path": str(self.archive), "destination": str(dst)},
            },
        )
        self.assertFalse(reply["result"].get("isError", False), reply)
        self.assertTrue(dst.is_dir())
        args = self.args_log.read_text().splitlines()
        self.assertIn("--no-same-owner", args)
        self.assertIn("--no-same-permissions", args)

    def test_extract_strip_components_propagates(self) -> None:
        dst = self.root / "out"
        reply = _rpc(
            self.main.app,
            "tools/call",
            {
                "name": "archive.extract",
                "arguments": {
                    "path": str(self.archive),
                    "destination": str(dst),
                    "strip_components": 2,
                },
            },
        )
        self.assertFalse(reply["result"].get("isError", False), reply)
        args = self.args_log.read_text().splitlines()
        self.assertIn("--strip-components", args)
        self.assertIn("2", args)

    def test_extract_rejects_negative_strip(self) -> None:
        dst = self.root / "out"
        reply = _rpc(
            self.main.app,
            "tools/call",
            {
                "name": "archive.extract",
                "arguments": {
                    "path": str(self.archive),
                    "destination": str(dst),
                    "strip_components": -1,
                },
            },
        )
        self.assertTrue(reply["result"]["isError"])

    def test_create_writes_output_and_passes_sources(self) -> None:
        out = self.root / "bundle.tar.zst"
        reply = _rpc(
            self.main.app,
            "tools/call",
            {
                "name": "archive.create",
                "arguments": {
                    "output": str(out),
                    "sources": [str(self.file_a), str(self.file_b)],
                },
            },
        )
        self.assertFalse(reply["result"].get("isError", False), reply)
        self.assertTrue(out.exists())
        args = self.args_log.read_text().splitlines()
        self.assertIn("-acf", args)
        self.assertIn(str(self.file_a), args)
        self.assertIn(str(self.file_b), args)

    def test_create_refuses_clobber(self) -> None:
        out = self.root / "exists.tar"
        out.write_bytes(b"existing")
        reply = _rpc(
            self.main.app,
            "tools/call",
            {
                "name": "archive.create",
                "arguments": {"output": str(out), "sources": [str(self.file_a)]},
            },
        )
        self.assertTrue(reply["result"]["isError"])

    def test_create_rejects_empty_sources(self) -> None:
        out = self.root / "out.tar"
        reply = _rpc(
            self.main.app,
            "tools/call",
            {
                "name": "archive.create",
                "arguments": {"output": str(out), "sources": []},
            },
        )
        self.assertTrue(reply["result"]["isError"])

    def test_nonzero_exit_surfaces_as_tool_error(self) -> None:
        _write_stub(
            self.bin_dir,
            "bsdtar",
            """\
            #!/bin/sh
            echo 'corrupt archive' 1>&2
            exit 1
            """,
        )
        reply = _rpc(
            self.main.app,
            "tools/call",
            {"name": "archive.list", "arguments": {"path": str(self.archive)}},
        )
        self.assertTrue(reply["result"]["isError"])
        self.assertIn("corrupt archive", reply["result"]["content"][0]["text"])


if __name__ == "__main__":
    unittest.main()
