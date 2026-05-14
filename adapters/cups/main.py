"""CUPS adapter — exposes ``print.printers``, ``print.jobs`` and
``print.submit`` so the system Agent can list printers and queue
print jobs via the CUPS ``lp``/``lpstat`` CLIs.

Upstream: https://www.cups.org/ (Apache-2.0). Adapters look for the
binaries on PATH; on most Linux distros the ``cups-client`` package
provides them.
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


def _bin(env_name: str, default_name: str) -> str:
    explicit = os.environ.get(env_name)
    if explicit:
        return explicit
    found = shutil.which(default_name)
    if found is None:
        raise FileNotFoundError(
            f"{default_name} not found on PATH; install the `cups-client` package"
        )
    return found


def _lp() -> str:
    return _bin("CLAW_LP_BIN", "lp")


def _lpstat() -> str:
    return _bin("CLAW_LPSTAT_BIN", "lpstat")


def _run(cmd: list[str]) -> str:
    proc = subprocess.run(cmd, capture_output=True, text=True, check=False)
    if proc.returncode != 0:
        msg = proc.stderr.strip() or proc.stdout.strip() or f"{cmd[0]} failed"
        raise RuntimeError(f"{pathlib.Path(cmd[0]).name} (exit {proc.returncode}): {msg}")
    return proc.stdout


# Identifier validation: CUPS destination names are restricted to ASCII
# letters/digits/underscore (no whitespace/slashes/etc.). Be strict so
# callers cannot smuggle shell args via the printer name.
_NAME_RE = re.compile(r"^[A-Za-z0-9_][A-Za-z0-9_.-]{0,127}$")
_OPTION_KEY_RE = re.compile(r"^[A-Za-z][A-Za-z0-9_-]*$")
_OPTION_VAL_RE = re.compile(r"^[A-Za-z0-9_.,:=+/-]+$")


def _validate_printer(name: str) -> str:
    if not _NAME_RE.match(name):
        raise ValueError(
            f"invalid printer name '{name}'. Use ASCII letters/digits/_-. only."
        )
    return name


def _validate_options(opts: dict) -> list[str]:
    """Turn an option dict into a list of ['-o', 'key=value', '-o', ...].
    Each key and value is validated against a conservative charset so
    callers cannot inject shell metas or extra flags."""
    out: list[str] = []
    if opts is None:
        return out
    if not isinstance(opts, dict):
        raise ValueError("options must be a JSON object (mapping)")
    for k, v in opts.items():
        if not isinstance(k, str) or not _OPTION_KEY_RE.match(k):
            raise ValueError(f"invalid option key: {k!r}")
        sv = str(v)
        if not _OPTION_VAL_RE.match(sv):
            raise ValueError(f"invalid option value for '{k}': {sv!r}")
        out.extend(["-o", f"{k}={sv}"])
    return out


app = App()


_LPSTAT_P = re.compile(r"^printer\s+(\S+)\s+(.*)$")
_LPSTAT_D = re.compile(r"^system default destination:\s*(\S+)\s*$")


@app.tool(
    "print.printers",
    summary="List CUPS printers and indicate which one is the system default.",
    args={},
)
def print_printers() -> dict:
    out = _run([_lpstat(), "-p", "-d"])
    printers: list[dict] = []
    default: str | None = None
    for line in out.splitlines():
        m = _LPSTAT_P.match(line.strip())
        if m:
            printers.append({"name": m.group(1), "status": m.group(2).rstrip(".")})
            continue
        m = _LPSTAT_D.match(line.strip())
        if m:
            default = m.group(1)
            continue
    return {"default": default, "printers": printers}


@app.tool(
    "print.jobs",
    summary="List currently queued / printing jobs (optionally filtered to one printer).",
    args={
        "printer": {
            "type": "string",
            "description": "Optional CUPS destination name to filter on.",
        },
    },
)
def print_jobs(printer: str = "") -> list:
    cmd = [_lpstat(), "-o"]
    if printer:
        cmd.append(_validate_printer(printer))
    out = _run(cmd)
    jobs: list[dict] = []
    for line in out.splitlines():
        # Typical format: "<dest>-<id> <user> <size> <date>"
        parts = line.split(maxsplit=3)
        if len(parts) < 3:
            continue
        job_id, user, size = parts[0], parts[1], parts[2]
        when = parts[3] if len(parts) > 3 else ""
        jobs.append({"id": job_id, "user": user, "size_bytes": size, "submitted": when})
    return jobs


@app.tool(
    "print.submit",
    summary="Submit a file to a CUPS printer. Returns the job id reported by lp.",
    args={
        "path": {
            "type": "string",
            "description": "Absolute path to the file to print.",
        },
        "printer": {
            "type": "string",
            "description": "Destination printer name. Omit to use the system default.",
        },
        "copies": {
            "type": "integer",
            "description": "Number of copies (≥1). Default 1.",
        },
        "double_sided": {
            "type": "boolean",
            "description": "If true, request duplex printing (sides=two-sided-long-edge).",
        },
        "options": {
            "type": "object",
            "description": (
                "Extra CUPS options as a {key:value} map. Keys must match "
                "[A-Za-z][A-Za-z0-9_-]*; values are limited to a safe charset."
            ),
        },
    },
    required=["path"],
)
def print_submit(
    path: str,
    printer: str = "",
    copies: int = 1,
    double_sided: bool = False,
    options: dict | None = None,
) -> str:
    src = pathlib.Path(path)
    if not src.is_file():
        raise FileNotFoundError(f"file not found: {path}")
    if not isinstance(copies, int) or copies < 1:
        raise ValueError("copies must be a positive integer")

    cmd = [_lp()]
    if printer:
        cmd.extend(["-d", _validate_printer(printer)])
    if copies > 1:
        cmd.extend(["-n", str(copies)])
    if double_sided:
        cmd.extend(["-o", "sides=two-sided-long-edge"])
    cmd.extend(_validate_options(options or {}))
    cmd.append(str(src))

    out = _run(cmd)
    # `lp` prints e.g. "request id is FooBar-42 (1 file(s))". Surface that.
    return out.strip()


if __name__ == "__main__":
    app.serve()
