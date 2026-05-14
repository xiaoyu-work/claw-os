"""whisper.cpp adapter — exposes ``transcribe.audio`` and
``transcribe.models`` so the system Agent can run local speech-to-text
without knowing the upstream CLI.

Upstream: https://github.com/ggerganov/whisper.cpp (MIT).

Binary discovery order:
1. ``$CLAW_WHISPER_BIN`` (test override).
2. ``whisper-cli`` on PATH (current upstream name since 2024).
3. ``main`` on PATH (legacy name).

Model discovery order (for the ``model`` argument when it is a bare
name without ``/``):
1. ``$CLAW_WHISPER_MODELS_DIR``.
2. ``$XDG_DATA_HOME/whisper.cpp/models`` (default
   ``~/.local/share/whisper.cpp/models``).
3. ``~/.cache/whisper`` (compatibility with OpenAI Python whisper).
"""

from __future__ import annotations

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


_MODEL_EXTS = (".bin", ".gguf")


def _whisper_bin() -> str:
    explicit = os.environ.get("CLAW_WHISPER_BIN")
    if explicit:
        return explicit
    for candidate in ("whisper-cli", "main"):
        found = shutil.which(candidate)
        if found:
            return found
    raise FileNotFoundError(
        "whisper.cpp binary not found on PATH; build whisper.cpp and "
        "ensure `whisper-cli` (or `main`) is installed"
    )


def _model_dirs() -> list[pathlib.Path]:
    out: list[pathlib.Path] = []
    explicit = os.environ.get("CLAW_WHISPER_MODELS_DIR")
    if explicit:
        out.append(pathlib.Path(explicit))
    xdg = os.environ.get("XDG_DATA_HOME") or os.path.expanduser("~/.local/share")
    out.append(pathlib.Path(xdg) / "whisper.cpp" / "models")
    out.append(pathlib.Path(os.path.expanduser("~/.cache/whisper")))
    return out


def _resolve_model(model: str) -> pathlib.Path:
    """If `model` looks like a path, return it directly; otherwise
    search the model dirs for `ggml-<model>.bin` or `<model>.bin`."""
    if "/" in model or model.endswith(_MODEL_EXTS):
        p = pathlib.Path(model).expanduser()
        if not p.is_file():
            raise FileNotFoundError(f"model file not found: {model}")
        return p
    for d in _model_dirs():
        for candidate in (
            f"ggml-{model}.bin",
            f"{model}.bin",
            f"ggml-{model}.gguf",
            f"{model}.gguf",
        ):
            p = d / candidate
            if p.is_file():
                return p
    raise FileNotFoundError(
        f"whisper model '{model}' not found in any of: "
        + ", ".join(str(d) for d in _model_dirs())
    )


app = App()


@app.tool(
    "transcribe.audio",
    summary="Transcribe an audio file with whisper.cpp. Returns plain text.",
    args={
        "path": {
            "type": "string",
            "description": "Absolute path to the audio file (wav/mp3/m4a/flac/ogg).",
        },
        "model": {
            "type": "string",
            "description": (
                "Model name (e.g. 'base', 'small', 'medium', 'large-v3') "
                "or absolute path to a .bin/.gguf model file. Defaults "
                "to 'base'. Bare names are resolved against "
                "$CLAW_WHISPER_MODELS_DIR, $XDG_DATA_HOME/whisper.cpp/"
                "models, and ~/.cache/whisper."
            ),
        },
        "language": {
            "type": "string",
            "description": (
                "Audio language as a 2-letter ISO code (e.g. 'en', 'zh'). "
                "Use 'auto' to let whisper detect. Default 'auto'."
            ),
        },
        "translate_to_english": {
            "type": "boolean",
            "description": "If true, translate non-English audio to English (--translate).",
        },
    },
    required=["path"],
)
def transcribe_audio(
    path: str,
    model: str = "base",
    language: str = "auto",
    translate_to_english: bool = False,
) -> str:
    audio = pathlib.Path(path)
    if not audio.is_file():
        raise FileNotFoundError(f"audio file not found: {path}")
    model_path = _resolve_model(model)
    cmd = [
        _whisper_bin(),
        "-m",
        str(model_path),
        "-f",
        str(audio),
        "-nt",
        "-otxt",
        "-of",
        "-",
    ]
    if language and language != "auto":
        cmd.extend(["-l", language])
    if translate_to_english:
        cmd.append("--translate")
    proc = subprocess.run(cmd, capture_output=True, text=True, check=False)
    if proc.returncode != 0:
        msg = proc.stderr.strip() or proc.stdout.strip() or "whisper failed"
        raise RuntimeError(f"whisper (exit {proc.returncode}): {msg}")
    return proc.stdout


@app.tool(
    "transcribe.models",
    summary="List locally installed whisper.cpp model files.",
    args={},
)
def transcribe_models() -> list:
    seen: set[str] = set()
    out: list[dict] = []
    for d in _model_dirs():
        if not d.is_dir():
            continue
        for entry in sorted(d.iterdir()):
            if entry.suffix.lower() not in _MODEL_EXTS:
                continue
            if entry.name in seen:
                continue
            seen.add(entry.name)
            out.append(
                {
                    "name": entry.stem.removeprefix("ggml-"),
                    "file": entry.name,
                    "path": str(entry),
                    "size_bytes": entry.stat().st_size,
                }
            )
    return out


if __name__ == "__main__":
    app.serve()
