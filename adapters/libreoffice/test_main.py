"""Unit tests for the LibreOffice headless adapter. A shell stub
stands in for ``soffice`` so the test runs without a real install."""

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


# Stub that produces <stem>.<ext> in --outdir from the input + format flag.
_SOFFICE_STUB = """\
#!/bin/sh
printf '%s\\n' "$@" > "$SOFFICE_ARGS_LOG"
fmt=""
outdir=""
src=""
while [ $# -gt 0 ]; do
  case "$1" in
    --convert-to) fmt="$2"; shift 2 ;;
    --outdir) outdir="$2"; shift 2 ;;
    -env:*|--headless|--norestore|--nologo|--nolockcheck) shift ;;
    *) src="$1"; shift ;;
  esac
done
ext="${fmt%%:*}"
stem="$(basename "$src")"
stem="${stem%.*}"
: > "$outdir/$stem.$ext"
exit "${SOFFICE_EXIT:-0}"
"""


class LibreofficeAdapterTests(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        root = pathlib.Path(self.tmp.name)
        self.root = root
        self.bin_dir = root / "bin"
        self.bin_dir.mkdir()

        self.args_log = root / "soffice.log"
        os.environ["SOFFICE_ARGS_LOG"] = str(self.args_log)
        self.stub = _write_stub(self.bin_dir, "soffice", _SOFFICE_STUB)
        os.environ["CLAW_SOFFICE_BIN"] = str(self.stub)

        self.docx = root / "report.docx"
        self.docx.write_bytes(b"PK\x03\x04 fake docx")

        if "main" in sys.modules:
            del sys.modules["main"]
        import main  # noqa: F401
        self.main = sys.modules["main"]
        self.main.app._initialized = True

    def tearDown(self) -> None:
        for k in ("CLAW_SOFFICE_BIN", "SOFFICE_ARGS_LOG", "SOFFICE_EXIT"):
            os.environ.pop(k, None)
        sys.modules.pop("main", None)

    def test_tools_list_reports_both_tools(self) -> None:
        reply = _rpc(self.main.app, "tools/list")
        names = sorted(t["name"] for t in reply["result"]["tools"])
        self.assertEqual(names, ["office.convert", "office.convert_to_pdf"])

    def test_convert_to_pdf_returns_expected_path(self) -> None:
        outdir = self.root / "out"
        reply = _rpc(
            self.main.app,
            "tools/call",
            {
                "name": "office.convert_to_pdf",
                "arguments": {"input": str(self.docx), "output_dir": str(outdir)},
            },
        )
        self.assertFalse(reply["result"].get("isError", False), reply)
        body = reply["result"]["content"][0]["text"]
        self.assertTrue(body.endswith("report.pdf"), body)
        self.assertTrue((outdir / "report.pdf").exists())

    def test_convert_passes_format_token(self) -> None:
        outdir = self.root / "out"
        reply = _rpc(
            self.main.app,
            "tools/call",
            {
                "name": "office.convert",
                "arguments": {
                    "input": str(self.docx),
                    "output_dir": str(outdir),
                    "target_format": "html",
                },
            },
        )
        self.assertFalse(reply["result"].get("isError", False), reply)
        args = self.args_log.read_text().splitlines()
        self.assertIn("--convert-to", args)
        self.assertIn("html", args)
        self.assertTrue((outdir / "report.html").exists())

    def test_convert_accepts_filter_form(self) -> None:
        outdir = self.root / "out"
        reply = _rpc(
            self.main.app,
            "tools/call",
            {
                "name": "office.convert",
                "arguments": {
                    "input": str(self.docx),
                    "output_dir": str(outdir),
                    "target_format": "pdf:writer_pdf_Export",
                },
            },
        )
        self.assertFalse(reply["result"].get("isError", False), reply)
        self.assertTrue((outdir / "report.pdf").exists())

    def test_convert_rejects_shell_meta_format(self) -> None:
        outdir = self.root / "out"
        reply = _rpc(
            self.main.app,
            "tools/call",
            {
                "name": "office.convert",
                "arguments": {
                    "input": str(self.docx),
                    "output_dir": str(outdir),
                    "target_format": "pdf; rm -rf /",
                },
            },
        )
        self.assertTrue(reply["result"]["isError"])
        self.assertIn("invalid format", reply["result"]["content"][0]["text"].lower())

    def test_convert_rejects_missing_input(self) -> None:
        reply = _rpc(
            self.main.app,
            "tools/call",
            {
                "name": "office.convert_to_pdf",
                "arguments": {"input": "/no/such/file.docx", "output_dir": str(self.root)},
            },
        )
        self.assertTrue(reply["result"]["isError"])

    def test_uses_unique_user_profile_dir(self) -> None:
        outdir = self.root / "out"
        _rpc(
            self.main.app,
            "tools/call",
            {
                "name": "office.convert_to_pdf",
                "arguments": {"input": str(self.docx), "output_dir": str(outdir)},
            },
        )
        args = self.args_log.read_text()
        # Profile flag must include a file:// URL with a tmp dir.
        self.assertIn("-env:UserInstallation=file://", args)

    def test_nonzero_exit_surfaces_as_tool_error(self) -> None:
        os.environ["SOFFICE_EXIT"] = "77"
        # Stub still creates the file on exit≠0, so we need a different stub.
        _write_stub(
            self.bin_dir,
            "soffice",
            """\
            #!/bin/sh
            echo 'bad doc' 1>&2
            exit 77
            """,
        )
        outdir = self.root / "out"
        reply = _rpc(
            self.main.app,
            "tools/call",
            {
                "name": "office.convert_to_pdf",
                "arguments": {"input": str(self.docx), "output_dir": str(outdir)},
            },
        )
        self.assertTrue(reply["result"]["isError"])
        self.assertIn("bad doc", reply["result"]["content"][0]["text"])

    def test_missing_output_surfaces_as_tool_error(self) -> None:
        # Stub that exits 0 but doesn't create the file.
        _write_stub(
            self.bin_dir,
            "soffice",
            """\
            #!/bin/sh
            echo "I didn't actually convert anything"
            exit 0
            """,
        )
        outdir = self.root / "out"
        reply = _rpc(
            self.main.app,
            "tools/call",
            {
                "name": "office.convert_to_pdf",
                "arguments": {"input": str(self.docx), "output_dir": str(outdir)},
            },
        )
        self.assertTrue(reply["result"]["isError"])
        self.assertIn("expected output is missing", reply["result"]["content"][0]["text"])


if __name__ == "__main__":
    unittest.main()
