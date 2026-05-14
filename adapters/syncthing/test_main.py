"""Unit tests for the Syncthing adapter. The syncthing binary is
replaced with a shell stub so the test runs without a daemon."""

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


_STUB = r"""#!/bin/sh
printf '%s\n' "$@" > "$SYNCTHING_ARGS_LOG"
# match against the last three positional args
last3="$(printf '%s\n' "$@" | tail -n 3 | tr '\n' ' ')"
case "$last3" in
  *"config folders list"*)
    echo '[{"id":"default","label":"Default Folder","path":"/home/jay/Sync"},{"id":"docs","label":"Docs","path":"/home/jay/Documents"}]'
    exit 0
    ;;
  *"config devices list"*)
    echo '[{"deviceID":"ABC-123","name":"laptop"},{"deviceID":"DEF-456","name":"phone"}]'
    exit 0
    ;;
esac
# operations rescan --folder <id>
case " $* " in
  *" operations rescan --folder "*)
    exit "${SYNCTHING_EXIT:-0}"
    ;;
esac
echo 'unknown command' 1>&2
exit 99
"""


class SyncthingAdapterTests(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        root = pathlib.Path(self.tmp.name)
        self.bin_dir = root / "bin"
        self.bin_dir.mkdir()

        self.args_log = root / "syncthing.log"
        os.environ["SYNCTHING_ARGS_LOG"] = str(self.args_log)
        self.stub = _write_stub(self.bin_dir, "syncthing", _STUB)
        os.environ["CLAW_SYNCTHING_BIN"] = str(self.stub)

        if "main" in sys.modules:
            del sys.modules["main"]
        import main  # noqa: F401
        self.main = sys.modules["main"]
        self.main.app._initialized = True

    def tearDown(self) -> None:
        for k in ("CLAW_SYNCTHING_BIN", "SYNCTHING_ARGS_LOG", "SYNCTHING_EXIT"):
            os.environ.pop(k, None)
        sys.modules.pop("main", None)

    def test_tools_list_reports_three_tools(self) -> None:
        reply = _rpc(self.main.app, "tools/list")
        names = sorted(t["name"] for t in reply["result"]["tools"])
        self.assertEqual(names, ["sync.devices", "sync.folders", "sync.rescan"])

    def test_folders_returns_parsed_json(self) -> None:
        reply = _rpc(self.main.app, "tools/call", {"name": "sync.folders", "arguments": {}})
        self.assertFalse(reply["result"].get("isError", False), reply)
        body = json.loads(reply["result"]["content"][0]["text"])
        ids = sorted(f["id"] for f in body)
        self.assertEqual(ids, ["default", "docs"])

    def test_devices_returns_parsed_json(self) -> None:
        reply = _rpc(self.main.app, "tools/call", {"name": "sync.devices", "arguments": {}})
        self.assertFalse(reply["result"].get("isError", False), reply)
        body = json.loads(reply["result"]["content"][0]["text"])
        ids = sorted(d["deviceID"] for d in body)
        self.assertEqual(ids, ["ABC-123", "DEF-456"])

    def test_rescan_validates_folder_id_and_passes_through(self) -> None:
        reply = _rpc(
            self.main.app,
            "tools/call",
            {"name": "sync.rescan", "arguments": {"folder_id": "default"}},
        )
        self.assertFalse(reply["result"].get("isError", False), reply)
        body = json.loads(reply["result"]["content"][0]["text"])
        self.assertEqual(body, {"rescanned": "default"})
        args = self.args_log.read_text().splitlines()
        self.assertIn("rescan", args)
        self.assertIn("--folder", args)
        self.assertIn("default", args)

    def test_rescan_rejects_shell_meta_folder_id(self) -> None:
        reply = _rpc(
            self.main.app,
            "tools/call",
            {"name": "sync.rescan", "arguments": {"folder_id": "; rm -rf /"}},
        )
        self.assertTrue(reply["result"]["isError"])
        self.assertIn("invalid folder id", reply["result"]["content"][0]["text"].lower())

    def test_folders_surfaces_invalid_json(self) -> None:
        _write_stub(
            self.bin_dir,
            "syncthing",
            """\
            #!/bin/sh
            echo 'not json at all'
            exit 0
            """,
        )
        reply = _rpc(self.main.app, "tools/call", {"name": "sync.folders", "arguments": {}})
        self.assertTrue(reply["result"]["isError"])
        self.assertIn("invalid json", reply["result"]["content"][0]["text"].lower())

    def test_nonzero_exit_surfaces_as_tool_error(self) -> None:
        _write_stub(
            self.bin_dir,
            "syncthing",
            """\
            #!/bin/sh
            echo 'daemon not running' 1>&2
            exit 2
            """,
        )
        reply = _rpc(self.main.app, "tools/call", {"name": "sync.devices", "arguments": {}})
        self.assertTrue(reply["result"]["isError"])
        self.assertIn("daemon not running", reply["result"]["content"][0]["text"])


if __name__ == "__main__":
    unittest.main()
