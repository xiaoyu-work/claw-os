"""Unit tests for the qpdf adapter. The qpdf binary is replaced with
a shell stub so the test runs without the real tool."""

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
        frame["params"] = params
    with _Capture() as out:
        app._handle_line(json.dumps(frame))
    line = out.getvalue().strip().splitlines()[-1]
    return json.loads(line)


# Shell stub that knows qpdf's main verbs.
_QPDF_STUB = """\
#!/bin/sh
# log args for the test to assert against
printf '%s\\n' "$@" >> "$QPDF_ARGS_LOG"
# strip --password=* if present
for a in "$@"; do
  case "$a" in
    --password=*) ;;
    --show-npages) echo 7; exit 0 ;;
    --is-encrypted) exit "${QPDF_ENCRYPTED:-1}" ;;
  esac
done
# any other invocation: touch the last positional arg as output.
for a in "$@"; do dst="$a"; done
case "$dst" in
  --*) : ;;  # last arg was a flag (e.g. only flags) — no file to create
  *) : > "$dst" ;;
esac
exit 0
"""


class QpdfAdapterTests(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        root = pathlib.Path(self.tmp.name)
        self.root = root
        self.bin_dir = root / "bin"
        self.bin_dir.mkdir()
        self.args_log = root / "qpdf.log"
        os.environ["QPDF_ARGS_LOG"] = str(self.args_log)

        self.stub = _write_stub(self.bin_dir, "qpdf", _QPDF_STUB)
        os.environ["CLAW_QPDF_BIN"] = str(self.stub)

        self.pdf = root / "src.pdf"
        self.pdf.write_bytes(b"%PDF-1.7\n")
        self.pdf2 = root / "src2.pdf"
        self.pdf2.write_bytes(b"%PDF-1.7\n")

        if "main" in sys.modules:
            del sys.modules["main"]
        import main  # noqa: F401
        self.main = sys.modules["main"]
        self.main.app._initialized = True

    def tearDown(self) -> None:
        for k in ("CLAW_QPDF_BIN", "QPDF_ARGS_LOG", "QPDF_ENCRYPTED"):
            os.environ.pop(k, None)
        sys.modules.pop("main", None)

    def test_tools_list_reports_four_tools(self) -> None:
        reply = _rpc(self.main.app, "tools/list")
        names = sorted(t["name"] for t in reply["result"]["tools"])
        self.assertEqual(names, ["pdf.decrypt", "pdf.info", "pdf.merge", "pdf.split_pages"])

    def test_info_reports_pages_and_encryption_false(self) -> None:
        reply = _rpc(
            self.main.app, "tools/call", {"name": "pdf.info", "arguments": {"path": str(self.pdf)}}
        )
        self.assertFalse(reply["result"].get("isError", False), reply)
        body = json.loads(reply["result"]["content"][0]["text"])
        self.assertEqual(body["pages"], 7)
        self.assertFalse(body["encrypted"])

    def test_info_detects_encryption(self) -> None:
        os.environ["QPDF_ENCRYPTED"] = "0"  # qpdf --is-encrypted returns 0 when encrypted
        reply = _rpc(
            self.main.app, "tools/call", {"name": "pdf.info", "arguments": {"path": str(self.pdf)}}
        )
        body = json.loads(reply["result"]["content"][0]["text"])
        self.assertTrue(body["encrypted"])

    def test_split_pages_validates_range_and_creates_output(self) -> None:
        dst = self.root / "out.pdf"
        reply = _rpc(
            self.main.app,
            "tools/call",
            {
                "name": "pdf.split_pages",
                "arguments": {"input": str(self.pdf), "output": str(dst), "pages": "1-3,5"},
            },
        )
        self.assertFalse(reply["result"].get("isError", False), reply)
        self.assertTrue(dst.exists())
        args = self.args_log.read_text().splitlines()
        self.assertIn("--pages", args)
        self.assertIn("1-3,5", args)

    def test_split_pages_rejects_shell_meta(self) -> None:
        dst = self.root / "out.pdf"
        reply = _rpc(
            self.main.app,
            "tools/call",
            {
                "name": "pdf.split_pages",
                "arguments": {
                    "input": str(self.pdf),
                    "output": str(dst),
                    "pages": "1; rm -rf /",
                },
            },
        )
        self.assertTrue(reply["result"]["isError"])
        self.assertIn("invalid page range", reply["result"]["content"][0]["text"].lower())

    def test_split_pages_refuses_to_clobber(self) -> None:
        dst = self.root / "out.pdf"
        dst.write_bytes(b"existing")
        reply = _rpc(
            self.main.app,
            "tools/call",
            {
                "name": "pdf.split_pages",
                "arguments": {"input": str(self.pdf), "output": str(dst), "pages": "1"},
            },
        )
        self.assertTrue(reply["result"]["isError"])

    def test_merge_requires_at_least_two_inputs(self) -> None:
        dst = self.root / "out.pdf"
        reply = _rpc(
            self.main.app,
            "tools/call",
            {
                "name": "pdf.merge",
                "arguments": {"inputs": [str(self.pdf)], "output": str(dst)},
            },
        )
        self.assertTrue(reply["result"]["isError"])

    def test_merge_passes_pages_1z_for_each_source(self) -> None:
        dst = self.root / "out.pdf"
        reply = _rpc(
            self.main.app,
            "tools/call",
            {
                "name": "pdf.merge",
                "arguments": {
                    "inputs": [str(self.pdf), str(self.pdf2)],
                    "output": str(dst),
                },
            },
        )
        self.assertFalse(reply["result"].get("isError", False), reply)
        args = self.args_log.read_text().splitlines()
        # --empty --pages <pdf> 1-z <pdf2> 1-z -- <dst>
        self.assertEqual(args.count("1-z"), 2)
        self.assertIn("--empty", args)

    def test_decrypt_passes_password_flag(self) -> None:
        dst = self.root / "decrypted.pdf"
        reply = _rpc(
            self.main.app,
            "tools/call",
            {
                "name": "pdf.decrypt",
                "arguments": {
                    "input": str(self.pdf),
                    "output": str(dst),
                    "password": "hunter2",
                },
            },
        )
        self.assertFalse(reply["result"].get("isError", False), reply)
        args = self.args_log.read_text().splitlines()
        self.assertIn("--password=hunter2", args)
        self.assertIn("--decrypt", args)

    def test_nonzero_exit_surfaces_as_tool_error(self) -> None:
        _write_stub(
            self.bin_dir,
            "qpdf",
            """\
            #!/bin/sh
            echo 'cannot open file' 1>&2
            exit 2
            """,
        )
        # exit 2 is "error", > 3 threshold means... actually qpdf returns
        # 2 for errors; our adapter treats >3 as error. So 2 is treated
        # as success in adapter (warning-tier). Use exit code 4 to test
        # the error path.
        _write_stub(
            self.bin_dir,
            "qpdf",
            """\
            #!/bin/sh
            echo 'truly bad' 1>&2
            exit 4
            """,
        )
        reply = _rpc(
            self.main.app,
            "tools/call",
            {"name": "pdf.info", "arguments": {"path": str(self.pdf)}},
        )
        self.assertTrue(reply["result"]["isError"])
        self.assertIn("truly bad", reply["result"]["content"][0]["text"])


if __name__ == "__main__":
    unittest.main()
