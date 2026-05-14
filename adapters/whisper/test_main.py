"""Unit tests for the whisper.cpp adapter. A shell stub stands in for
``whisper-cli`` so the test never touches a real ggml model."""

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


class WhisperAdapterTests(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        root = pathlib.Path(self.tmp.name)
        self.bin_dir = root / "bin"
        self.bin_dir.mkdir()
        self.models = root / "models"
        self.models.mkdir()
        (self.models / "ggml-base.bin").write_bytes(b"fake")
        (self.models / "ggml-small.bin").write_bytes(b"fake")
        (self.models / "ggml-large-v3.gguf").write_bytes(b"fake")

        self.audio = root / "clip.wav"
        self.audio.write_bytes(b"RIFFfake")

        self.stub = _write_stub(
            self.bin_dir,
            "whisper-cli",
            """\
            #!/bin/sh
            printf '%s\\n' "$@" > "$WHISPER_ARGS_LOG"
            echo "hello world from whisper stub"
            """,
        )
        self.args_log = root / "args.log"
        os.environ["CLAW_WHISPER_BIN"] = str(self.stub)
        os.environ["CLAW_WHISPER_MODELS_DIR"] = str(self.models)
        os.environ["WHISPER_ARGS_LOG"] = str(self.args_log)

        if "main" in sys.modules:
            del sys.modules["main"]
        import main  # noqa: F401
        self.main = sys.modules["main"]
        self.main.app._initialized = True

    def tearDown(self) -> None:
        for k in ("CLAW_WHISPER_BIN", "CLAW_WHISPER_MODELS_DIR", "WHISPER_ARGS_LOG"):
            os.environ.pop(k, None)
        sys.modules.pop("main", None)

    def test_tools_list_reports_both_tools(self) -> None:
        reply = _rpc(self.main.app, "tools/list")
        names = sorted(t["name"] for t in reply["result"]["tools"])
        self.assertEqual(names, ["transcribe.audio", "transcribe.models"])

    def test_transcribe_audio_returns_stub_text_and_passes_lang_flag(self) -> None:
        reply = _rpc(
            self.main.app,
            "tools/call",
            {
                "name": "transcribe.audio",
                "arguments": {"path": str(self.audio), "model": "base", "language": "zh"},
            },
        )
        self.assertFalse(reply["result"].get("isError", False), reply)
        body = reply["result"]["content"][0]["text"]
        self.assertIn("hello world", body)
        args = self.args_log.read_text().splitlines()
        self.assertIn("-l", args)
        self.assertIn("zh", args)
        self.assertTrue(any(a.endswith("ggml-base.bin") for a in args), args)

    def test_transcribe_audio_auto_lang_omits_lang_flag(self) -> None:
        reply = _rpc(
            self.main.app,
            "tools/call",
            {"name": "transcribe.audio", "arguments": {"path": str(self.audio)}},
        )
        self.assertFalse(reply["result"].get("isError", False), reply)
        args = self.args_log.read_text().splitlines()
        self.assertNotIn("-l", args)

    def test_transcribe_audio_translate_flag_propagates(self) -> None:
        reply = _rpc(
            self.main.app,
            "tools/call",
            {
                "name": "transcribe.audio",
                "arguments": {
                    "path": str(self.audio),
                    "language": "zh",
                    "translate_to_english": True,
                },
            },
        )
        self.assertFalse(reply["result"].get("isError", False), reply)
        args = self.args_log.read_text().splitlines()
        self.assertIn("--translate", args)

    def test_transcribe_audio_rejects_missing_file(self) -> None:
        reply = _rpc(
            self.main.app,
            "tools/call",
            {"name": "transcribe.audio", "arguments": {"path": "/no/such/audio.wav"}},
        )
        self.assertTrue(reply["result"]["isError"])

    def test_transcribe_audio_rejects_unknown_model(self) -> None:
        reply = _rpc(
            self.main.app,
            "tools/call",
            {
                "name": "transcribe.audio",
                "arguments": {"path": str(self.audio), "model": "no-such-model"},
            },
        )
        self.assertTrue(reply["result"]["isError"])
        self.assertIn("not found", reply["result"]["content"][0]["text"].lower())

    def test_transcribe_audio_surfaces_nonzero_exit(self) -> None:
        _write_stub(
            self.bin_dir,
            "whisper-cli",
            """\
            #!/bin/sh
            echo 'cuda oom' 1>&2
            exit 2
            """,
        )
        reply = _rpc(
            self.main.app,
            "tools/call",
            {"name": "transcribe.audio", "arguments": {"path": str(self.audio)}},
        )
        self.assertTrue(reply["result"]["isError"])
        self.assertIn("cuda oom", reply["result"]["content"][0]["text"])

    def test_transcribe_models_lists_local_files(self) -> None:
        reply = _rpc(self.main.app, "tools/call", {"name": "transcribe.models", "arguments": {}})
        self.assertFalse(reply["result"].get("isError", False), reply)
        body = json.loads(reply["result"]["content"][0]["text"])
        names = sorted(m["name"] for m in body)
        self.assertEqual(names, ["base", "large-v3", "small"])


if __name__ == "__main__":
    unittest.main()
