"""qpdf adapter — exposes ``pdf.info``, ``pdf.split_pages``,
``pdf.merge`` and ``pdf.decrypt`` so the system Agent can manipulate
PDFs without learning the upstream CLI.

Upstream: https://github.com/qpdf/qpdf (Apache-2.0).
"""

from __future__ import annotations

import os
import pathlib
import re
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


def _qpdf_bin() -> str:
    explicit = os.environ.get("CLAW_QPDF_BIN")
    if explicit:
        return explicit
    found = shutil.which("qpdf")
    if found is None:
        raise FileNotFoundError("qpdf not found on PATH; install the `qpdf` package")
    return found


def _run(cmd: list[str]) -> subprocess.CompletedProcess:
    proc = subprocess.run(cmd, capture_output=True, text=True, check=False)
    # qpdf uses exit code 3 for warnings ("processed with warnings");
    # treat anything > 3 as a hard failure and ≤3 as success but keep
    # the original exit code in the message for callers that care.
    if proc.returncode > 3:
        msg = proc.stderr.strip() or proc.stdout.strip() or "qpdf failed"
        raise RuntimeError(f"qpdf (exit {proc.returncode}): {msg}")
    return proc


_PAGE_RANGE_RE = re.compile(r"^[0-9rz,\-]+$")


def _validate_range(spec: str) -> str:
    """Allow qpdf's `1,3-5,7-z` syntax but reject shell-meta characters."""
    s = spec.strip()
    if not s or not _PAGE_RANGE_RE.match(s):
        raise ValueError(
            f"invalid page range '{spec}'. Expected qpdf syntax like '1', '1-3', '1,3-5,7-z'."
        )
    return s


app = App()


@app.tool(
    "pdf.info",
    summary="Return number of pages and encryption status for a PDF.",
    args={
        "path": {"type": "string", "description": "Absolute path to the PDF."},
        "password": {
            "type": "string",
            "description": "Owner or user password if the PDF is encrypted. Empty for unencrypted PDFs.",
        },
    },
    required=["path"],
)
def pdf_info(path: str, password: str = "") -> dict:
    src = pathlib.Path(path)
    if not src.is_file():
        raise FileNotFoundError(f"pdf not found: {path}")
    base = [_qpdf_bin()]
    if password:
        base.extend(["--password=" + password])
    pages_proc = _run(base + ["--show-npages", str(src)])
    pages_line = pages_proc.stdout.strip().splitlines()[-1] if pages_proc.stdout.strip() else "0"
    try:
        pages = int(pages_line)
    except ValueError as e:
        raise RuntimeError(f"qpdf returned non-integer page count: {pages_line!r}") from e
    encrypted_proc = subprocess.run(
        base + ["--is-encrypted", str(src)], capture_output=True, text=True, check=False
    )
    return {
        "pages": pages,
        "encrypted": encrypted_proc.returncode == 0,
        "path": str(src),
    }


@app.tool(
    "pdf.split_pages",
    summary="Extract a subset of pages from a PDF into a new file.",
    args={
        "input": {"type": "string", "description": "Absolute path to the source PDF."},
        "output": {"type": "string", "description": "Destination PDF path."},
        "pages": {
            "type": "string",
            "description": "qpdf page-range syntax (e.g. '1', '1-3', '1,3-5,7-z'). 'z' means last page.",
        },
        "password": {
            "type": "string",
            "description": "Owner/user password if the source is encrypted.",
        },
        "overwrite": {
            "type": "boolean",
            "description": "Overwrite the output file if it already exists. Default false.",
        },
    },
    required=["input", "output", "pages"],
)
def pdf_split_pages(
    input: str, output: str, pages: str, password: str = "", overwrite: bool = False
) -> str:
    src = pathlib.Path(input)
    if not src.is_file():
        raise FileNotFoundError(f"pdf not found: {input}")
    dst = pathlib.Path(output)
    if dst.exists() and not overwrite:
        raise RuntimeError(f"refusing to overwrite existing output (pass overwrite=true): {output}")
    rng = _validate_range(pages)
    cmd = [_qpdf_bin()]
    if password:
        cmd.append("--password=" + password)
    cmd.extend([str(src), "--pages", str(src), rng, "--", str(dst)])
    _run(cmd)
    return str(dst)


@app.tool(
    "pdf.merge",
    summary="Concatenate multiple PDFs into a single output file.",
    args={
        "inputs": {
            "type": "array",
            "items": {"type": "string"},
            "description": "Ordered list of absolute paths to source PDFs (length ≥ 2).",
        },
        "output": {"type": "string", "description": "Destination PDF path."},
        "overwrite": {
            "type": "boolean",
            "description": "Overwrite output if it exists. Default false.",
        },
    },
    required=["inputs", "output"],
)
def pdf_merge(inputs: list, output: str, overwrite: bool = False) -> str:
    if not isinstance(inputs, list) or len(inputs) < 2:
        raise ValueError("inputs must be a list of at least two PDF paths")
    src_paths: list[pathlib.Path] = []
    for p in inputs:
        sp = pathlib.Path(p)
        if not sp.is_file():
            raise FileNotFoundError(f"pdf not found: {p}")
        src_paths.append(sp)
    dst = pathlib.Path(output)
    if dst.exists() and not overwrite:
        raise RuntimeError(f"refusing to overwrite existing output (pass overwrite=true): {output}")
    first, rest = src_paths[0], src_paths[1:]
    cmd = [_qpdf_bin(), "--empty", "--pages", str(first), "1-z"]
    for r in rest:
        cmd.extend([str(r), "1-z"])
    cmd.extend(["--", str(dst)])
    _run(cmd)
    return str(dst)


@app.tool(
    "pdf.decrypt",
    summary="Write a copy of the PDF with all password / DRM removed.",
    args={
        "input": {"type": "string", "description": "Source PDF path."},
        "output": {"type": "string", "description": "Destination PDF path."},
        "password": {
            "type": "string",
            "description": "Owner or user password. Required if the PDF is actually encrypted.",
        },
        "overwrite": {
            "type": "boolean",
            "description": "Overwrite output if it exists. Default false.",
        },
    },
    required=["input", "output"],
)
def pdf_decrypt(input: str, output: str, password: str = "", overwrite: bool = False) -> str:
    src = pathlib.Path(input)
    if not src.is_file():
        raise FileNotFoundError(f"pdf not found: {input}")
    dst = pathlib.Path(output)
    if dst.exists() and not overwrite:
        raise RuntimeError(f"refusing to overwrite existing output (pass overwrite=true): {output}")
    cmd = [_qpdf_bin()]
    if password:
        cmd.append("--password=" + password)
    cmd.extend(["--decrypt", str(src), str(dst)])
    _run(cmd)
    return str(dst)


if __name__ == "__main__":
    app.serve()
