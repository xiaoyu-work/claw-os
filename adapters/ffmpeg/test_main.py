"""Unit tests for the ffmpeg adapter. ffmpeg and ffprobe are replaced
with shell stubs so the test runs without the real binaries."""

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

from test_support import load_local_module


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


class FfmpegAdapterTests(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        root = pathlib.Path(self.tmp.name)
        self.root = root
        self.bin_dir = root / "bin"
        self.bin_dir.mkdir()

        self.input = root / "in.mp4"
        self.input.write_bytes(b"fake mp4")

        # ffprobe stub emits a minimal but valid JSON document.
        _write_stub(
            self.bin_dir,
            "ffprobe",
            """\
            #!/bin/sh
            printf '%s\\n' "$@" > "$FFPROBE_ARGS_LOG"
            cat <<'JSON'
            {"streams":[{"codec_type":"video","codec_name":"h264"}],"format":{"duration":"42.0"}}
            JSON
            """,
        )
        # ffmpeg stub creates the output file with given last-arg path.
        _write_stub(
            self.bin_dir,
            "ffmpeg",
            """\
            #!/bin/sh
            printf '%s\\n' "$@" > "$FFMPEG_ARGS_LOG"
            # last arg is the destination path
            for a in "$@"; do dst="$a"; done
            : > "$dst"
            """,
        )
        self.ffprobe_args = root / "ffprobe.log"
        self.ffmpeg_args = root / "ffmpeg.log"
        os.environ["CLAW_FFMPEG_BIN"] = str(self.bin_dir / "ffmpeg")
        os.environ["CLAW_FFPROBE_BIN"] = str(self.bin_dir / "ffprobe")
        os.environ["FFMPEG_ARGS_LOG"] = str(self.ffmpeg_args)
        os.environ["FFPROBE_ARGS_LOG"] = str(self.ffprobe_args)

        self.main = load_local_module(
            HERE / "main.py",
            "claw_test_ffmpeg_adapter_main",
        )
        self.main.app._initialized = True

    def tearDown(self) -> None:
        for k in (
            "CLAW_FFMPEG_BIN",
            "CLAW_FFPROBE_BIN",
            "FFMPEG_ARGS_LOG",
            "FFPROBE_ARGS_LOG",
        ):
            os.environ.pop(k, None)
        sys.modules.pop("claw_test_ffmpeg_adapter_main", None)

    def test_tools_list_reports_four_tools(self) -> None:
        reply = _rpc(self.main.app, "tools/list")
        names = sorted(t["name"] for t in reply["result"]["tools"])
        self.assertEqual(
            names,
            ["media.convert", "media.extract_audio", "media.probe", "media.thumbnail"],
        )

    def test_probe_parses_ffprobe_json(self) -> None:
        reply = _rpc(
            self.main.app,
            "tools/call",
            {"name": "media.probe", "arguments": {"path": str(self.input)}},
        )
        self.assertFalse(reply["result"].get("isError", False), reply)
        body = json.loads(reply["result"]["content"][0]["text"])
        self.assertEqual(body["format"]["duration"], "42.0")
        self.assertEqual(body["streams"][0]["codec_name"], "h264")

    def test_probe_rejects_missing_input(self) -> None:
        reply = _rpc(
            self.main.app,
            "tools/call",
            {"name": "media.probe", "arguments": {"path": "/no/such/file.mp4"}},
        )
        self.assertTrue(reply["result"]["isError"])

    def test_convert_creates_output_and_passes_codecs(self) -> None:
        dst = self.root / "out.webm"
        reply = _rpc(
            self.main.app,
            "tools/call",
            {
                "name": "media.convert",
                "arguments": {
                    "input": str(self.input),
                    "output": str(dst),
                    "video_codec": "libvpx-vp9",
                    "audio_codec": "libopus",
                },
            },
        )
        self.assertFalse(reply["result"].get("isError", False), reply)
        self.assertTrue(dst.exists())
        args = self.ffmpeg_args.read_text().splitlines()
        self.assertIn("libvpx-vp9", args)
        self.assertIn("libopus", args)
        self.assertIn("-n", args)  # default no-overwrite

    def test_convert_refuses_to_clobber_without_overwrite(self) -> None:
        dst = self.root / "out.mkv"
        dst.write_bytes(b"existing")
        reply = _rpc(
            self.main.app,
            "tools/call",
            {
                "name": "media.convert",
                "arguments": {"input": str(self.input), "output": str(dst)},
            },
        )
        self.assertTrue(reply["result"]["isError"])
        self.assertIn("overwrite", reply["result"]["content"][0]["text"].lower())

    def test_convert_overwrite_propagates_minus_y(self) -> None:
        dst = self.root / "out.mkv"
        dst.write_bytes(b"existing")
        reply = _rpc(
            self.main.app,
            "tools/call",
            {
                "name": "media.convert",
                "arguments": {
                    "input": str(self.input),
                    "output": str(dst),
                    "overwrite": True,
                },
            },
        )
        self.assertFalse(reply["result"].get("isError", False), reply)
        args = self.ffmpeg_args.read_text().splitlines()
        self.assertIn("-y", args)

    def test_thumbnail_passes_timestamp(self) -> None:
        dst = self.root / "thumb.png"
        reply = _rpc(
            self.main.app,
            "tools/call",
            {
                "name": "media.thumbnail",
                "arguments": {"input": str(self.input), "output": str(dst), "at": "00:00:05"},
            },
        )
        self.assertFalse(reply["result"].get("isError", False), reply)
        args = self.ffmpeg_args.read_text().splitlines()
        self.assertIn("-ss", args)
        self.assertIn("00:00:05", args)
        self.assertIn("-frames:v", args)
        self.assertIn("1", args)

    def test_extract_audio_passes_vn_and_codec(self) -> None:
        dst = self.root / "out.mp3"
        reply = _rpc(
            self.main.app,
            "tools/call",
            {
                "name": "media.extract_audio",
                "arguments": {
                    "input": str(self.input),
                    "output": str(dst),
                    "audio_codec": "libmp3lame",
                },
            },
        )
        self.assertFalse(reply["result"].get("isError", False), reply)
        args = self.ffmpeg_args.read_text().splitlines()
        self.assertIn("-vn", args)
        self.assertIn("libmp3lame", args)

    def test_nonzero_exit_surfaces_as_tool_error(self) -> None:
        _write_stub(
            self.bin_dir,
            "ffmpeg",
            """\
            #!/bin/sh
            echo 'unsupported codec' 1>&2
            exit 1
            """,
        )
        dst = self.root / "out.mp4"
        reply = _rpc(
            self.main.app,
            "tools/call",
            {
                "name": "media.convert",
                "arguments": {"input": str(self.input), "output": str(dst)},
            },
        )
        self.assertTrue(reply["result"]["isError"])
        self.assertIn("unsupported codec", reply["result"]["content"][0]["text"])


if __name__ == "__main__":
    unittest.main()
