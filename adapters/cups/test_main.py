"""Unit tests for the CUPS adapter. lp and lpstat are replaced with
shell stubs so the test runs without a real CUPS install."""

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


_LPSTAT_STUB = """\
#!/bin/sh
printf '%s\\n' "$@" > "$LPSTAT_ARGS_LOG"
case "$1" in
  -p)
    echo "printer Office is idle.  enabled since Mon May 12 09:00:00 2025"
    echo "printer Lab is processing job 41."
    if [ "$2" = "-d" ]; then
      echo "system default destination: Office"
    fi
    ;;
  -o)
    if [ -n "$2" ]; then
      echo "$2-77 jay 1024 Tue May 13 10:00:00 2025"
    else
      echo "Office-42 alice 2048 Tue May 13 10:00:00 2025"
      echo "Lab-41 bob 8192 Tue May 13 11:00:00 2025"
    fi
    ;;
esac
exit "${LPSTAT_EXIT:-0}"
"""

_LP_STUB = """\
#!/bin/sh
printf '%s\\n' "$@" > "$LP_ARGS_LOG"
echo "request id is Office-99 (1 file(s))"
exit "${LP_EXIT:-0}"
"""


class CupsAdapterTests(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        root = pathlib.Path(self.tmp.name)
        self.root = root
        self.bin_dir = root / "bin"
        self.bin_dir.mkdir()

        self.lpstat_log = root / "lpstat.log"
        self.lp_log = root / "lp.log"
        os.environ["LPSTAT_ARGS_LOG"] = str(self.lpstat_log)
        os.environ["LP_ARGS_LOG"] = str(self.lp_log)
        _write_stub(self.bin_dir, "lpstat", _LPSTAT_STUB)
        _write_stub(self.bin_dir, "lp", _LP_STUB)
        os.environ["CLAW_LPSTAT_BIN"] = str(self.bin_dir / "lpstat")
        os.environ["CLAW_LP_BIN"] = str(self.bin_dir / "lp")

        self.file = root / "doc.pdf"
        self.file.write_bytes(b"%PDF-1.7\n")

        if "main" in sys.modules:
            del sys.modules["main"]
        import main  # noqa: F401
        self.main = sys.modules["main"]
        self.main.app._initialized = True

    def tearDown(self) -> None:
        for k in (
            "CLAW_LPSTAT_BIN",
            "CLAW_LP_BIN",
            "LPSTAT_ARGS_LOG",
            "LP_ARGS_LOG",
            "LPSTAT_EXIT",
            "LP_EXIT",
        ):
            os.environ.pop(k, None)
        sys.modules.pop("main", None)

    def test_tools_list_reports_three_tools(self) -> None:
        reply = _rpc(self.main.app, "tools/list")
        names = sorted(t["name"] for t in reply["result"]["tools"])
        self.assertEqual(names, ["print.jobs", "print.printers", "print.submit"])

    def test_printers_parses_lpstat_output(self) -> None:
        reply = _rpc(self.main.app, "tools/call", {"name": "print.printers", "arguments": {}})
        self.assertFalse(reply["result"].get("isError", False), reply)
        body = json.loads(reply["result"]["content"][0]["text"])
        self.assertEqual(body["default"], "Office")
        names = sorted(p["name"] for p in body["printers"])
        self.assertEqual(names, ["Lab", "Office"])

    def test_jobs_no_filter_returns_all(self) -> None:
        reply = _rpc(self.main.app, "tools/call", {"name": "print.jobs", "arguments": {}})
        self.assertFalse(reply["result"].get("isError", False), reply)
        body = json.loads(reply["result"]["content"][0]["text"])
        ids = sorted(j["id"] for j in body)
        self.assertEqual(ids, ["Lab-41", "Office-42"])

    def test_jobs_filter_propagates_printer_name(self) -> None:
        reply = _rpc(
            self.main.app,
            "tools/call",
            {"name": "print.jobs", "arguments": {"printer": "Office"}},
        )
        self.assertFalse(reply["result"].get("isError", False), reply)
        args = self.lpstat_log.read_text().splitlines()
        self.assertIn("Office", args)

    def test_jobs_rejects_shell_meta_printer(self) -> None:
        reply = _rpc(
            self.main.app,
            "tools/call",
            {"name": "print.jobs", "arguments": {"printer": "x; rm -rf /"}},
        )
        self.assertTrue(reply["result"]["isError"])

    def test_submit_uses_default_when_printer_omitted(self) -> None:
        reply = _rpc(
            self.main.app,
            "tools/call",
            {"name": "print.submit", "arguments": {"path": str(self.file)}},
        )
        self.assertFalse(reply["result"].get("isError", False), reply)
        body = reply["result"]["content"][0]["text"]
        self.assertIn("Office-99", body)
        args = self.lp_log.read_text().splitlines()
        self.assertNotIn("-d", args)

    def test_submit_passes_copies_duplex_and_options(self) -> None:
        reply = _rpc(
            self.main.app,
            "tools/call",
            {
                "name": "print.submit",
                "arguments": {
                    "path": str(self.file),
                    "printer": "Office",
                    "copies": 3,
                    "double_sided": True,
                    "options": {"media": "A4", "fit-to-page": "true"},
                },
            },
        )
        self.assertFalse(reply["result"].get("isError", False), reply)
        args = self.lp_log.read_text().splitlines()
        self.assertIn("-d", args)
        self.assertIn("Office", args)
        self.assertIn("-n", args)
        self.assertIn("3", args)
        self.assertIn("sides=two-sided-long-edge", args)
        self.assertIn("media=A4", args)
        self.assertIn("fit-to-page=true", args)

    def test_submit_rejects_invalid_copies(self) -> None:
        reply = _rpc(
            self.main.app,
            "tools/call",
            {"name": "print.submit", "arguments": {"path": str(self.file), "copies": 0}},
        )
        self.assertTrue(reply["result"]["isError"])

    def test_submit_rejects_shell_meta_option_value(self) -> None:
        reply = _rpc(
            self.main.app,
            "tools/call",
            {
                "name": "print.submit",
                "arguments": {
                    "path": str(self.file),
                    "options": {"media": "A4; rm"},
                },
            },
        )
        self.assertTrue(reply["result"]["isError"])

    def test_submit_rejects_missing_file(self) -> None:
        reply = _rpc(
            self.main.app,
            "tools/call",
            {"name": "print.submit", "arguments": {"path": "/no/such/file.pdf"}},
        )
        self.assertTrue(reply["result"]["isError"])

    def test_lp_nonzero_exit_surfaces(self) -> None:
        os.environ["LP_EXIT"] = "1"
        _write_stub(
            self.bin_dir,
            "lp",
            """\
            #!/bin/sh
            echo 'no printers found' 1>&2
            exit 1
            """,
        )
        reply = _rpc(
            self.main.app,
            "tools/call",
            {"name": "print.submit", "arguments": {"path": str(self.file)}},
        )
        self.assertTrue(reply["result"]["isError"])
        self.assertIn("no printers found", reply["result"]["content"][0]["text"])


if __name__ == "__main__":
    unittest.main()
