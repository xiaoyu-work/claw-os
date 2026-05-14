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
import sys

_HERE = pathlib.Path(__file__).resolve().parent
_CANDIDATES = [
    pathlib.Path(os.environ["CLAW_PYTHON_LIB"]) if os.environ.get("CLAW_PYTHON_LIB") else None,
    _HERE.parent.parent / "apps",
    pathlib.Path("/opt/claw/python"),
    pathlib.Path("/usr/lib/claw/python"),
]
for _cand in _CANDIDATES:
    if _cand and (_cand / "_lib").is_dir():
        sys.path.insert(0, str(_cand))
        break

from _lib.serve import App  # noqa: E402


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


app = App()


@app.tool(
    "media.probe",
    summary="Inspect a media file with ffprobe. Returns parsed JSON with format + streams.",
    args={
        "path": {
            "type": "string",
            "description": "Absolute path to the media file.",
        },
    },
    required=["path"],
)
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


@app.tool(
    "media.convert",
    summary=(
        "Transcode a media file. Caller picks codecs and container by output extension. "
        "For copy-only repackaging pass video_codec='copy' and audio_codec='copy'."
    ),
    args={
        "input": {"type": "string", "description": "Source media file (absolute path)."},
        "output": {"type": "string", "description": "Destination file path. Container inferred from extension."},
        "video_codec": {
            "type": "string",
            "description": "Video codec (e.g. 'libx264', 'libvpx-vp9', 'copy'). Default 'copy'.",
        },
        "audio_codec": {
            "type": "string",
            "description": "Audio codec (e.g. 'aac', 'libopus', 'copy'). Default 'copy'.",
        },
        "overwrite": {
            "type": "boolean",
            "description": "If true, allow overwriting an existing output file (-y). Default false.",
        },
    },
    required=["input", "output"],
)
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


@app.tool(
    "media.thumbnail",
    summary="Extract a single still frame from a video at a given timestamp.",
    args={
        "input": {"type": "string", "description": "Source video file (absolute path)."},
        "output": {"type": "string", "description": "Output image path (.png/.jpg)."},
        "at": {
            "type": "string",
            "description": "Timestamp like '00:00:01.500' or '90' (seconds). Default '00:00:01'.",
        },
        "overwrite": {
            "type": "boolean",
            "description": "Overwrite existing output (-y). Default false.",
        },
    },
    required=["input", "output"],
)
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


@app.tool(
    "media.extract_audio",
    summary="Pull the audio track out of a media file into a standalone audio file.",
    args={
        "input": {"type": "string", "description": "Source media file (absolute path)."},
        "output": {"type": "string", "description": "Destination audio file. Container from extension."},
        "audio_codec": {
            "type": "string",
            "description": "Audio codec to encode with ('aac', 'libmp3lame', 'libopus', 'flac', or 'copy'). Default 'copy'.",
        },
        "overwrite": {
            "type": "boolean",
            "description": "Overwrite existing output. Default false.",
        },
    },
    required=["input", "output"],
)
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
