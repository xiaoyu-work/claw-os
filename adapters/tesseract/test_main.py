"""Unit tests for the tesseract adapter. The real ``tesseract``
binary is replaced with a tiny shell stub on PATH so the test runs in
under a second and on any machine.

The tests drive the MCP server in-process via ``serve._handle_line``
rather than spawning a subprocess, since `App.serve()` reads from
stdin and would block.
"""

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


HERE = pathlib.Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))  # so `import main` works


def _write_stub(dir: pathlib.Path, name: str, script: str) -> pathlib.Path:
    """Drop an executable shell stub at ``dir/name`` and return its path."""
    p = dir / name
    p.write_text(textwrap.dedent(script))
    p.chmod(p.stat().st_mode | stat.S_IEXEC | stat.S_IXGRP | stat.S_IXOTH)
    return p


class _Capture:
    """Swap stdout for a buffer so we can capture JSON-RPC replies."""

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
    """Push one JSON-RPC frame at the App's stdin handler, return the parsed reply."""
    frame = {"jsonrpc": "2.0", "id": msg_id, "method": method}
    if params is not None:
        frame["params"] = params
    with _Capture() as out:
        app._handle_line(json.dumps(frame))
    line = out.getvalue().strip().splitlines()[-1]
    return json.loads(line)


class TesseractAdapterTests(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory()
        self.bin_dir = pathlib.Path(self.tmp.name) / "bin"
        self.bin_dir.mkdir()
        self.addCleanup(self.tmp.cleanup)
        # Each test customises the stub; default is "always succeed".
        self.stub = _write_stub(
            self.bin_dir,
            "tesseract",
            """\
            #!/bin/sh
            # Args: <input> <output|-> -l <lang> --psm <n> | --list-langs
            for a in "$@"; do
              if [ "$a" = "--list-langs" ]; then
                echo "List of available languages (3):"
                echo "eng"
                echo "chi_sim"
                echo "deu"
                exit 0
              fi
            done
            echo "hello from stub"
            """,
        )
        os.environ["CLAW_TESSERACT_BIN"] = str(self.stub)
        # Re-import main fresh so module-level state (the App instance) is
        # rebuilt for every test.
        if "main" in sys.modules:
            del sys.modules["main"]
        import main  # noqa: F401
        self.main = sys.modules["main"]
        # Pretend we already saw `initialize`.
        self.main.app._initialized = True

    def tearDown(self) -> None:
        os.environ.pop("CLAW_TESSERACT_BIN", None)
        sys.modules.pop("main", None)

    def test_tools_list_reports_both_tools(self) -> None:
        reply = _rpc(self.main.app, "tools/list")
        names = sorted(t["name"] for t in reply["result"]["tools"])
        self.assertEqual(names, ["ocr.languages", "ocr.run"])

    def test_ocr_run_returns_stub_output(self) -> None:
        img = pathlib.Path(self.tmp.name) / "x.png"
        img.write_bytes(b"\x89PNG\r\n\x1a\nfake")
        reply = _rpc(
            self.main.app,
            "tools/call",
            {"name": "ocr.run", "arguments": {"path": str(img)}},
        )
        self.assertNotIn("error", reply, reply)
        text_items = [c["text"] for c in reply["result"]["content"]]
        self.assertIn("hello from stub", text_items[0])
        self.assertFalse(reply["result"].get("isError", False))

    def test_ocr_run_rejects_missing_file(self) -> None:
        reply = _rpc(
            self.main.app,
            "tools/call",
            {"name": "ocr.run", "arguments": {"path": "/no/such/file.png"}},
        )
        # Tool errors are returned as result + isError=true (per MCP spec
        # and the `_lib.serve` convention).
        self.assertTrue(reply["result"]["isError"])
        body = reply["result"]["content"][0]["text"]
        self.assertIn("not found", body.lower())

    def test_ocr_run_surfaces_nonzero_exit(self) -> None:
        # Replace the stub with one that always fails.
        _write_stub(
            self.bin_dir,
            "tesseract",
            """\
            #!/bin/sh
            echo 'cannot read input' 1>&2
            exit 1
            """,
        )
        img = pathlib.Path(self.tmp.name) / "x.png"
        img.write_bytes(b"\x89PNG\r\n\x1a\nfake")
        reply = _rpc(
            self.main.app,
            "tools/call",
            {"name": "ocr.run", "arguments": {"path": str(img)}},
        )
        self.assertTrue(reply["result"]["isError"])
        body = reply["result"]["content"][0]["text"]
        self.assertIn("cannot read input", body)

    def test_languages_strips_header(self) -> None:
        reply = _rpc(self.main.app, "tools/call", {"name": "ocr.languages", "arguments": {}})
        self.assertFalse(reply["result"].get("isError", False))
        body = reply["result"]["content"][0]["text"]
        # `_lib.serve` JSON-encodes lists, so parse it back to assert.
        langs = json.loads(body)
        self.assertEqual(langs, ["eng", "chi_sim", "deu"])

    def test_missing_binary_reports_helpful_error(self) -> None:
        os.environ["CLAW_TESSERACT_BIN"] = "/no/such/tesseract"
        reply = _rpc(self.main.app, "tools/call", {"name": "ocr.languages", "arguments": {}})
        self.assertTrue(reply["result"]["isError"])


if __name__ == "__main__":
    unittest.main()
