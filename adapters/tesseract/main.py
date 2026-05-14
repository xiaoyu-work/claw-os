"""Tesseract OCR adapter — exposes ``ocr.run`` and ``ocr.languages``
as MCP tools so the system Agent can extract text from images
without knowing the upstream CLI.

Upstream: https://github.com/tesseract-ocr/tesseract (Apache-2.0).
The binary must be on ``$PATH`` as ``tesseract`` (Debian:
``apt-get install tesseract-ocr tesseract-ocr-chi-sim`` for English +
simplified Chinese).
"""

from __future__ import annotations

import os
import pathlib
import shutil
import subprocess
import sys

# ---------------------------------------------------------------------------
# sys.path bootstrap for ``_lib.serve`` — see adapters/README.md for the
# rationale. Same block lives in every adapter; intentional duplication
# to keep adapters self-contained (no shared install-time helper).
# ---------------------------------------------------------------------------
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

from _lib.serve import App  # noqa: E402  — sys.path bootstrap above.


# Override binary path for tests via env var; production resolves on PATH.
def _tesseract_bin() -> str:
    explicit = os.environ.get("CLAW_TESSERACT_BIN")
    if explicit:
        return explicit
    found = shutil.which("tesseract")
    if found is None:
        raise FileNotFoundError(
            "tesseract not found on PATH; install `tesseract-ocr` "
            "(and any language packs you need, e.g. tesseract-ocr-chi-sim)"
        )
    return found


app = App()


@app.tool(
    "ocr.run",
    summary="OCR an image file. Returns the recognised text as a single string.",
    args={
        "path": {
            "type": "string",
            "description": "Absolute path to the image to OCR (PNG/JPEG/TIFF/PDF).",
        },
        "lang": {
            "type": "string",
            "description": (
                "Tesseract language code or '+'-joined list, e.g. 'eng', "
                "'eng+chi_sim'. Defaults to 'eng'. Install language packs "
                "via `apt-get install tesseract-ocr-<lang>`."
            ),
        },
        "psm": {
            "type": "integer",
            "description": (
                "Page segmentation mode (0-13). 3 = automatic with OSD "
                "(default), 6 = single uniform block, 11 = sparse text."
            ),
        },
    },
    required=["path"],
)
def ocr_run(path: str, lang: str = "eng", psm: int = 3) -> str:
    img = pathlib.Path(path)
    if not img.is_file():
        raise FileNotFoundError(f"image not found: {path}")
    cmd = [_tesseract_bin(), str(img), "-", "-l", lang, "--psm", str(psm)]
    proc = subprocess.run(
        cmd,
        capture_output=True,
        text=True,
        check=False,
    )
    if proc.returncode != 0:
        # Surface stderr because that's where tesseract reports
        # missing-language-pack and decode errors.
        msg = proc.stderr.strip() or proc.stdout.strip() or "tesseract failed"
        raise RuntimeError(f"tesseract (exit {proc.returncode}): {msg}")
    return proc.stdout


@app.tool(
    "ocr.languages",
    summary="List the OCR language packs installed on this system.",
    args={},
)
def ocr_languages() -> list:
    proc = subprocess.run(
        [_tesseract_bin(), "--list-langs"],
        capture_output=True,
        text=True,
        check=False,
    )
    if proc.returncode != 0:
        msg = proc.stderr.strip() or "tesseract --list-langs failed"
        raise RuntimeError(f"tesseract (exit {proc.returncode}): {msg}")
    # First stdout line is the header ("List of available languages...");
    # rest are the codes.
    lines = [ln.strip() for ln in proc.stdout.splitlines() if ln.strip()]
    return lines[1:] if lines and lines[0].lower().startswith("list of") else lines


if __name__ == "__main__":
    app.serve()
