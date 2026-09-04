"""libarchive (bsdtar) adapter — exposes ``archive.list``,
``archive.extract`` and ``archive.create`` so the system Agent can
work with tar/zip/7z archives without learning bsdtar's flags.

Upstream: https://www.libarchive.org/ (BSD-2-Clause).

We use ``bsdtar`` rather than GNU tar because libarchive's bsdtar
auto-detects formats (tar, zip, 7z, cpio, iso, …) and is consistent
across Linux/BSD/macOS.
"""

from __future__ import annotations

import os
import pathlib
import shutil
import subprocess

from claw_os_sdk.mcp import App


_HERE = pathlib.Path(__file__).resolve().parent


def _bsdtar_bin() -> str:
    explicit = os.environ.get("CLAW_BSDTAR_BIN")
    if explicit:
        return explicit
    found = shutil.which("bsdtar")
    if found is None:
        raise FileNotFoundError(
            "bsdtar not found on PATH; install the `libarchive-tools` "
            "package (provides bsdtar)"
        )
    return found


def _run(cmd: list[str]) -> subprocess.CompletedProcess:
    proc = subprocess.run(cmd, capture_output=True, text=True, check=False)
    if proc.returncode != 0:
        msg = proc.stderr.strip() or proc.stdout.strip() or "bsdtar failed"
        raise RuntimeError(f"bsdtar (exit {proc.returncode}): {msg}")
    return proc


app = App.from_manifest(_HERE / "app.json")


@app.tool("archive.list")
def archive_list(path: str) -> list:
    src = pathlib.Path(path)
    if not src.is_file():
        raise FileNotFoundError(f"archive not found: {path}")
    proc = _run([_bsdtar_bin(), "-tf", str(src)])
    return [ln for ln in proc.stdout.splitlines() if ln.strip()]


@app.tool("archive.extract")
def archive_extract(path: str, destination: str, strip_components: int = 0) -> dict:
    src = pathlib.Path(path)
    if not src.is_file():
        raise FileNotFoundError(f"archive not found: {path}")
    dst = pathlib.Path(destination)
    dst.mkdir(parents=True, exist_ok=True)
    if not isinstance(strip_components, int) or strip_components < 0:
        raise ValueError("strip_components must be a non-negative integer")
    cmd = [
        _bsdtar_bin(),
        "-xf",
        str(src),
        "-C",
        str(dst),
        "--no-same-owner",
        "--no-same-permissions",
    ]
    if strip_components > 0:
        cmd.extend(["--strip-components", str(strip_components)])
    _run(cmd)
    return {"destination": str(dst.resolve())}


@app.tool("archive.create")
def archive_create(output: str, sources: list, overwrite: bool = False) -> str:
    if not isinstance(sources, list) or not sources:
        raise ValueError("sources must be a non-empty list of paths")
    src_paths: list[pathlib.Path] = []
    for s in sources:
        sp = pathlib.Path(s)
        if not sp.exists():
            raise FileNotFoundError(f"source not found: {s}")
        src_paths.append(sp)
    dst = pathlib.Path(output)
    if dst.exists() and not overwrite:
        raise RuntimeError(f"refusing to overwrite existing archive (pass overwrite=true): {output}")
    cmd = [_bsdtar_bin(), "-acf", str(dst)] + [str(p) for p in src_paths]
    _run(cmd)
    return str(dst)


if __name__ == "__main__":
    app.serve()
