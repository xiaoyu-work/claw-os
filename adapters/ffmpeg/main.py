"""ffmpeg adapter — exposes ``media.probe``, ``media.convert``,
``media.thumbnail`` and ``media.extract_audio`` so the system Agent
can inspect and transcode media without learning the upstream CLI.

Upstream: https://ffmpeg.org (LGPL-2.1-or-later).
"""

from __future__ import annotations

import json as _json
import os
import pathlib
import shutil
import subprocess

from claw_os_sdk.mcp import App


_HERE = pathlib.Path(__file__).resolve().parent


def _bin(env_name: str, default_name: str) -> str:
    explicit = os.environ.get(env_name)
    if explicit:
        return explicit
    found = shutil.which(default_name)
    if found is None:
        raise FileNotFoundError(
            f"{default_name} not found on PATH; install the `ffmpeg` package"
        )
    return found


def _ffmpeg() -> str:
    return _bin("CLAW_FFMPEG_BIN", "ffmpeg")


def _ffprobe() -> str:
    return _bin("CLAW_FFPROBE_BIN", "ffprobe")


def _run(cmd: list[str]) -> str:
    proc = subprocess.run(cmd, capture_output=True, text=True, check=False)
    if proc.returncode != 0:
        msg = proc.stderr.strip() or proc.stdout.strip() or "ffmpeg failed"
        raise RuntimeError(f"{cmd[0]} (exit {proc.returncode}): {msg}")
    return proc.stdout


app = App.from_manifest(_HERE / "app.json")


@app.tool("media.probe")
def media_probe(path: str) -> dict:
    src = pathlib.Path(path)
    if not src.is_file():
        raise FileNotFoundError(f"media file not found: {path}")
    out = _run(
        [
            _ffprobe(),
            "-v",
            "error",
            "-print_format",
            "json",
            "-show_format",
            "-show_streams",
            str(src),
        ]
    )
    try:
        return _json.loads(out)
    except _json.JSONDecodeError as e:
        raise RuntimeError(f"ffprobe produced invalid JSON: {e}")


@app.tool("media.convert")
def media_convert(
    input: str,
    output: str,
    video_codec: str = "copy",
    audio_codec: str = "copy",
    overwrite: bool = False,
) -> str:
    src = pathlib.Path(input)
    if not src.is_file():
        raise FileNotFoundError(f"input not found: {input}")
    dst = pathlib.Path(output)
    if dst.exists() and not overwrite:
        raise RuntimeError(f"refusing to overwrite existing output (pass overwrite=true): {output}")
    cmd = [_ffmpeg(), "-y" if overwrite else "-n", "-i", str(src), "-c:v", video_codec, "-c:a", audio_codec, str(dst)]
    _run(cmd)
    return str(dst)


@app.tool("media.thumbnail")
def media_thumbnail(input: str, output: str, at: str = "00:00:01", overwrite: bool = False) -> str:
    src = pathlib.Path(input)
    if not src.is_file():
        raise FileNotFoundError(f"input not found: {input}")
    dst = pathlib.Path(output)
    if dst.exists() and not overwrite:
        raise RuntimeError(f"refusing to overwrite existing output (pass overwrite=true): {output}")
    cmd = [
        _ffmpeg(),
        "-y" if overwrite else "-n",
        "-ss",
        at,
        "-i",
        str(src),
        "-frames:v",
        "1",
        "-q:v",
        "2",
        str(dst),
    ]
    _run(cmd)
    return str(dst)


@app.tool("media.extract_audio")
def media_extract_audio(input: str, output: str, audio_codec: str = "copy", overwrite: bool = False) -> str:
    src = pathlib.Path(input)
    if not src.is_file():
        raise FileNotFoundError(f"input not found: {input}")
    dst = pathlib.Path(output)
    if dst.exists() and not overwrite:
        raise RuntimeError(f"refusing to overwrite existing output (pass overwrite=true): {output}")
    cmd = [
        _ffmpeg(),
        "-y" if overwrite else "-n",
        "-i",
        str(src),
        "-vn",
        "-c:a",
        audio_codec,
        str(dst),
    ]
    _run(cmd)
    return str(dst)


if __name__ == "__main__":
    app.serve()
