"""LibreOffice headless adapter — exposes ``office.convert`` and
``office.convert_to_pdf`` so the system Agent can transcode office
documents without learning soffice's flags.

Upstream: https://www.libreoffice.org/ (MPL-2.0).
Binary discovery order:
1. ``$CLAW_SOFFICE_BIN`` (test override).
2. ``soffice`` on PATH (typical Linux package).
3. ``libreoffice`` on PATH (some distros).

A *unique* user-profile directory is passed via
``-env:UserInstallation`` for each invocation so concurrent calls do
not race on the global lock under ``~/.config/libreoffice``.
"""

from __future__ import annotations

import os
import pathlib
import re
import shutil
import subprocess
import sys
import tempfile

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


def _soffice_bin() -> str:
    explicit = os.environ.get("CLAW_SOFFICE_BIN")
    if explicit:
        return explicit
    for cand in ("soffice", "libreoffice"):
        found = shutil.which(cand)
        if found:
            return found
    raise FileNotFoundError(
        "soffice/libreoffice not found on PATH; install LibreOffice (libreoffice-core)"
    )


_FORMAT_RE = re.compile(r"^[A-Za-z0-9_]+(:[A-Za-z0-9_]+)?$")


def _validate_format(fmt: str) -> str:
    """Accept a soffice format token like ``pdf`` or ``pdf:writer_pdf_Export``
    but reject anything else so callers cannot smuggle shell args."""
    if not _FORMAT_RE.match(fmt):
        raise ValueError(
            f"invalid format '{fmt}'. Expected token like 'pdf' or 'pdf:writer_pdf_Export'."
        )
    return fmt


app = App()


def _run_convert(input: str, output_dir: str, target_format: str) -> pathlib.Path:
    src = pathlib.Path(input)
    if not src.is_file():
        raise FileNotFoundError(f"input not found: {input}")
    out_dir = pathlib.Path(output_dir)
    out_dir.mkdir(parents=True, exist_ok=True)
    fmt = _validate_format(target_format)
    base_ext = fmt.split(":", 1)[0]
    dst = out_dir / f"{src.stem}.{base_ext}"

    profile_dir = tempfile.mkdtemp(prefix="claw-lo-")
    try:
        cmd = [
            _soffice_bin(),
            f"-env:UserInstallation=file://{profile_dir}",
            "--headless",
            "--norestore",
            "--nologo",
            "--nolockcheck",
            "--convert-to",
            fmt,
            "--outdir",
            str(out_dir),
            str(src),
        ]
        proc = subprocess.run(cmd, capture_output=True, text=True, check=False)
        if proc.returncode != 0:
            msg = proc.stderr.strip() or proc.stdout.strip() or "soffice failed"
            raise RuntimeError(f"soffice (exit {proc.returncode}): {msg}")
        if not dst.exists():
            raise RuntimeError(
                f"soffice reported success but expected output is missing: {dst}. "
                f"stdout: {proc.stdout.strip()}"
            )
    finally:
        shutil.rmtree(profile_dir, ignore_errors=True)
    return dst


@app.tool(
    "office.convert",
    summary=(
        "Convert an office document to another format using LibreOffice headless. "
        "Returns the absolute path of the created file."
    ),
    args={
        "input": {
            "type": "string",
            "description": "Absolute path to the source document (docx, xlsx, odt, pptx, csv, …).",
        },
        "output_dir": {
            "type": "string",
            "description": "Directory to write the converted file into. Created if missing.",
        },
        "target_format": {
            "type": "string",
            "description": (
                "soffice format token. Common values: 'pdf', 'docx', 'odt', 'xlsx', "
                "'csv', 'html', 'png'. Filter form 'pdf:writer_pdf_Export' is also allowed."
            ),
        },
    },
    required=["input", "output_dir", "target_format"],
)
def office_convert(input: str, output_dir: str, target_format: str) -> str:
    dst = _run_convert(input, output_dir, target_format)
    return str(dst)


@app.tool(
    "office.convert_to_pdf",
    summary="Convenience wrapper: convert any LibreOffice-supported document to PDF.",
    args={
        "input": {"type": "string", "description": "Absolute path to the source document."},
        "output_dir": {
            "type": "string",
            "description": "Directory to write the PDF into. Created if missing.",
        },
    },
    required=["input", "output_dir"],
)
def office_convert_to_pdf(input: str, output_dir: str) -> str:
    return str(_run_convert(input, output_dir, "pdf"))


if __name__ == "__main__":
    app.serve()
