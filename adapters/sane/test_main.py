"""Unit tests for the SANE adapter. scanimage is replaced with a
shell stub so the test runs without a real scanner."""

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


_STUB = """\
#!/bin/sh
printf '%s\\n' "$@" > "$SCANIMAGE_ARGS_LOG"
out=""
case "$1" in
  -L)
    echo "device \\`epson2:libusb:001:002' is a Epson Perfection V370 flatbed scanner"
    echo "device \\`plustek:libusb:003:004' is a Canon CanoScan LiDE 220 flatbed scanner"
    exit 0
    ;;
esac
# Look for -o <path>
while [ $# -gt 0 ]; do
  case "$1" in
    -o) out="$2"; shift 2 ;;
    *) shift ;;
  esac
done
if [ -n "$out" ]; then
  : > "$out"
fi
exit "${SCANIMAGE_EXIT:-0}"
"""


class SaneAdapterTests(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        root = pathlib.Path(self.tmp.name)
        self.root = root
        self.bin_dir = root / "bin"
        self.bin_dir.mkdir()

        self.args_log = root / "scanimage.log"
        os.environ["SCANIMAGE_ARGS_LOG"] = str(self.args_log)
        self.stub = _write_stub(self.bin_dir, "scanimage", _STUB)
        os.environ["CLAW_SCANIMAGE_BIN"] = str(self.stub)

        if "main" in sys.modules:
            del sys.modules["main"]
        import main  # noqa: F401
        self.main = sys.modules["main"]
        self.main.app._initialized = True

    def tearDown(self) -> None:
        for k in ("CLAW_SCANIMAGE_BIN", "SCANIMAGE_ARGS_LOG", "SCANIMAGE_EXIT"):
            os.environ.pop(k, None)
        sys.modules.pop("main", None)

    def test_tools_list_reports_two_tools(self) -> None:
        reply = _rpc(self.main.app, "tools/list")
        names = sorted(t["name"] for t in reply["result"]["tools"])
        self.assertEqual(names, ["scan.devices", "scan.image"])

    def test_devices_parses_scanimage_L(self) -> None:
        reply = _rpc(self.main.app, "tools/call", {"name": "scan.devices", "arguments": {}})
        self.assertFalse(reply["result"].get("isError", False), reply)
        body = json.loads(reply["result"]["content"][0]["text"])
        names = sorted(d["name"] for d in body)
        self.assertEqual(names, ["epson2:libusb:001:002", "plustek:libusb:003:004"])

    def test_image_creates_output_and_passes_flags(self) -> None:
        out = self.root / "scan.png"
        reply = _rpc(
            self.main.app,
            "tools/call",
            {
                "name": "scan.image",
                "arguments": {
                    "output": str(out),
                    "device": "epson2:libusb:001:002",
                    "resolution": 600,
                    "mode": "Gray",
                    "format": "png",
                },
            },
        )
        self.assertFalse(reply["result"].get("isError", False), reply)
        self.assertTrue(out.exists())
        args = self.args_log.read_text().splitlines()
        self.assertIn("--format=png", args)
        self.assertIn("--resolution", args)
        self.assertIn("600", args)
        self.assertIn("--mode", args)
        self.assertIn("Gray", args)
        self.assertIn("-d", args)
        self.assertIn("epson2:libusb:001:002", args)

    def test_image_omits_device_when_blank(self) -> None:
        out = self.root / "scan.png"
        reply = _rpc(
            self.main.app,
            "tools/call",
            {"name": "scan.image", "arguments": {"output": str(out)}},
        )
        self.assertFalse(reply["result"].get("isError", False), reply)
        args = self.args_log.read_text().splitlines()
        self.assertNotIn("-d", args)

    def test_image_rejects_invalid_resolution(self) -> None:
        out = self.root / "scan.png"
        reply = _rpc(
            self.main.app,
            "tools/call",
            {"name": "scan.image", "arguments": {"output": str(out), "resolution": 10}},
        )
        self.assertTrue(reply["result"]["isError"])

    def test_image_rejects_invalid_mode(self) -> None:
        out = self.root / "scan.png"
        reply = _rpc(
            self.main.app,
            "tools/call",
            {"name": "scan.image", "arguments": {"output": str(out), "mode": "Banana"}},
        )
        self.assertTrue(reply["result"]["isError"])

    def test_image_rejects_invalid_format(self) -> None:
        out = self.root / "scan.exe"
        reply = _rpc(
            self.main.app,
            "tools/call",
            {"name": "scan.image", "arguments": {"output": str(out), "format": "exe"}},
        )
        self.assertTrue(reply["result"]["isError"])

    def test_image_rejects_shell_meta_device(self) -> None:
        out = self.root / "scan.png"
        reply = _rpc(
            self.main.app,
            "tools/call",
            {
                "name": "scan.image",
                "arguments": {"output": str(out), "device": "; rm -rf /"},
            },
        )
        self.assertTrue(reply["result"]["isError"])

    def test_image_refuses_clobber(self) -> None:
        out = self.root / "scan.png"
        out.write_bytes(b"existing")
        reply = _rpc(
            self.main.app,
            "tools/call",
            {"name": "scan.image", "arguments": {"output": str(out)}},
        )
        self.assertTrue(reply["result"]["isError"])

    def test_image_rejects_missing_parent_dir(self) -> None:
        reply = _rpc(
            self.main.app,
            "tools/call",
            {"name": "scan.image", "arguments": {"output": "/no/such/dir/scan.png"}},
        )
        self.assertTrue(reply["result"]["isError"])

    def test_scanimage_nonzero_exit_surfaces(self) -> None:
        _write_stub(
            self.bin_dir,
            "scanimage",
            """\
            #!/bin/sh
            echo 'device busy' 1>&2
            exit 7
            """,
        )
        reply = _rpc(self.main.app, "tools/call", {"name": "scan.devices", "arguments": {}})
        self.assertTrue(reply["result"]["isError"])
        self.assertIn("device busy", reply["result"]["content"][0]["text"])


if __name__ == "__main__":
    unittest.main()
