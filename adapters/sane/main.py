"""SANE adapter — exposes ``scan.devices`` and ``scan.image`` so the
system Agent can drive a SANE-compatible scanner via the upstream
``scanimage`` CLI.

Upstream: https://www.sane-project.org/ (GPL-2.0-or-later). On most
Linux distros the ``sane-utils`` package provides scanimage.
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


def _scanimage_bin() -> str:
    explicit = os.environ.get("CLAW_SCANIMAGE_BIN")
    if explicit:
        return explicit
    found = shutil.which("scanimage")
    if found is None:
        raise FileNotFoundError(
            "scanimage not found on PATH; install the `sane-utils` package"
        )
    return found


_VALID_FORMATS = ("png", "jpeg", "tiff", "pnm")
_VALID_MODES = ("Color", "Gray", "Lineart", "Halftone")
_DEVICE_RE = re.compile(r"^[A-Za-z0-9_:.+/\-]{1,256}$")


def _validate_device(name: str) -> str:
    if not _DEVICE_RE.match(name):
        raise ValueError(
            f"invalid SANE device name '{name}'. Expected scanimage device "
            "syntax like 'epson2:libusb:001:002' or 'plustek:libusb:...'"
        )
    return name


app = App()


_LIST_LINE = re.compile(r"^device `([^']+)'\s+is a (.+)$")


@app.tool(
    "scan.devices",
    summary="Discover SANE-compatible scanners attached to this host.",
    args={},
)
def scan_devices() -> list:
    proc = subprocess.run(
        [_scanimage_bin(), "-L"], capture_output=True, text=True, check=False
    )
    if proc.returncode != 0:
        msg = proc.stderr.strip() or proc.stdout.strip() or "scanimage -L failed"
        raise RuntimeError(f"scanimage (exit {proc.returncode}): {msg}")
    devices: list[dict] = []
    for line in proc.stdout.splitlines():
        m = _LIST_LINE.match(line.strip())
        if m:
            devices.append({"name": m.group(1), "description": m.group(2)})
    return devices


@app.tool(
    "scan.image",
    summary=(
        "Acquire one image from a SANE scanner and save it to disk. "
        "Returns the absolute path of the saved file."
    ),
    args={
        "output": {
            "type": "string",
            "description": "Destination image path. Parent dirs must exist.",
        },
        "device": {
            "type": "string",
            "description": (
                "SANE device name from scan.devices (e.g. "
                "'epson2:libusb:001:002'). Omit to let scanimage pick the default."
            ),
        },
        "resolution": {
            "type": "integer",
            "description": "DPI (50-2400). Default 300.",
        },
        "mode": {
            "type": "string",
            "description": "One of: Color, Gray, Lineart, Halftone. Default 'Color'.",
        },
        "format": {
            "type": "string",
            "description": "Output format: png, jpeg, tiff, pnm. Default 'png'.",
        },
        "overwrite": {
            "type": "boolean",
            "description": "Allow overwriting an existing output file. Default false.",
        },
    },
    required=["output"],
)
def scan_image(
    output: str,
    device: str = "",
    resolution: int = 300,
    mode: str = "Color",
    format: str = "png",
    overwrite: bool = False,
) -> str:
    dst = pathlib.Path(output)
    if dst.exists() and not overwrite:
        raise RuntimeError(f"refusing to overwrite existing output (pass overwrite=true): {output}")
    parent = dst.parent
    if not parent.is_dir():
        raise FileNotFoundError(f"output parent directory does not exist: {parent}")

    if not isinstance(resolution, int) or not 50 <= resolution <= 2400:
        raise ValueError("resolution must be an integer in [50, 2400]")
    if mode not in _VALID_MODES:
        raise ValueError(f"mode must be one of {_VALID_MODES}, got {mode!r}")
    if format not in _VALID_FORMATS:
        raise ValueError(f"format must be one of {_VALID_FORMATS}, got {format!r}")

    cmd = [_scanimage_bin(), f"--format={format}", "--resolution", str(resolution), "--mode", mode]
    if device:
        cmd.extend(["-d", _validate_device(device)])
    cmd.extend(["-o", str(dst)])

    proc = subprocess.run(cmd, capture_output=True, text=True, check=False)
    if proc.returncode != 0:
        msg = proc.stderr.strip() or proc.stdout.strip() or "scanimage failed"
        raise RuntimeError(f"scanimage (exit {proc.returncode}): {msg}")
    if not dst.exists():
        raise RuntimeError(
            f"scanimage exited 0 but did not produce {dst}. "
            f"stdout: {proc.stdout.strip()!r}, stderr: {proc.stderr.strip()!r}"
        )
    return str(dst)


if __name__ == "__main__":
    app.serve()
