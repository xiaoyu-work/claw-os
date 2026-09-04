"""printer-manager — CUPS discovery, queue, print, and cancel."""

import json
import os
import re
import shutil
import subprocess
import sys

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from _shared.env_scrub import scrub_env  # noqa: E402

from cos_runtime import policy  # noqa: E402


NAME_RE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9_.-]{0,126}$")
JOB_RE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9_.-]{0,126}-[0-9]+$")
MEDIA_RE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9_.-]{0,127}$")
SIDES = frozenset({"one-sided", "two-sided-long-edge", "two-sided-short-edge"})
TIMEOUT_SECS = int(os.environ.get("CLAW_PRINTER_MANAGER_TIMEOUT", "600"))


def _cos_binary() -> str | None:
    return os.environ.get("COS_BIN") or shutil.which("cos")


def _printer_name(raw: object) -> str:
    if not isinstance(raw, str) or NAME_RE.fullmatch(raw) is None:
        raise ValueError("invalid printer name")
    return raw


def _job_id(raw: object) -> str:
    if not isinstance(raw, str) or JOB_RE.fullmatch(raw) is None:
        raise ValueError("invalid print job ID")
    return raw


def _canonical_source(raw: object) -> str:
    if (
        not isinstance(raw, str)
        or not raw
        or "\x00" in raw
        or not os.path.isabs(raw)
    ):
        raise ValueError("print source must be a canonical non-symlink path")
    canonical = os.path.realpath(raw)
    if canonical != raw or os.path.islink(raw):
        raise ValueError("print source must be a canonical non-symlink path")
    return canonical


def _copies(raw: object) -> int:
    if isinstance(raw, bool) or not isinstance(raw, int) or not 1 <= raw <= 100:
        raise ValueError("copies must be an integer from 1 to 100")
    return raw


def _title(raw: object) -> str | None:
    if raw is None:
        return None
    if (
        not isinstance(raw, str)
        or not raw
        or len(raw) > 128
        or any(ord(character) < 32 for character in raw)
    ):
        raise ValueError("invalid print title")
    return raw


def _media(raw: object) -> str | None:
    if raw is None:
        return None
    if not isinstance(raw, str) or MEDIA_RE.fullmatch(raw) is None:
        raise ValueError("invalid media option")
    return raw


def _sides(raw: object) -> str | None:
    if raw is None:
        return None
    if not isinstance(raw, str) or raw not in SIDES:
        raise ValueError("invalid sides option")
    return raw


def _broker(
    action: str,
    *,
    printer: str | None = None,
    source: str | None = None,
    job_id: str | None = None,
    title: str | None = None,
    media: str | None = None,
    sides: str | None = None,
    copies: int | None = None,
    confirm: bool = False,
) -> dict:
    cos_bin = _cos_binary()
    if not cos_bin:
        raise FileNotFoundError(
            "cos binary not found; Printer Manager broker unavailable"
        )
    argv = [cos_bin, "__printer", action]
    values = {
        "printer": printer,
        "source": source,
        "job_id": job_id,
        "title": title,
        "media": media,
        "sides": sides,
        "copies": copies,
    }
    for key in ("printer", "source", "job_id", "title", "media", "sides", "copies"):
        value = values[key]
        if value is not None:
            argv.extend([f"--{key.replace('_', '-')}", str(value)])
    if confirm:
        argv.append("--confirm")
    try:
        result = subprocess.run(
            argv,
            capture_output=True,
            text=True,
            timeout=TIMEOUT_SECS,
            stdin=subprocess.DEVNULL,
            env=scrub_env(),
            check=False,
        )
    except FileNotFoundError as exc:
        raise FileNotFoundError(
            f"Printer Manager broker executable not found: {cos_bin}"
        ) from exc
    except PermissionError as exc:
        raise PermissionError(
            f"permission denied launching Printer Manager broker: {cos_bin}"
        ) from exc
    except subprocess.TimeoutExpired as exc:
        raise TimeoutError(
            f"Printer Manager broker exceeded {TIMEOUT_SECS}s for {action}"
        ) from exc
    payload_text = (result.stdout or "").strip() or (result.stderr or "").strip()
    try:
        payload = json.loads(payload_text) if payload_text else {}
    except json.JSONDecodeError as exc:
        raise RuntimeError("Printer Manager broker returned invalid JSON") from exc
    if not isinstance(payload, dict):
        raise RuntimeError("Printer Manager broker returned a non-object result")
    if "error" in payload:
        error = payload["error"]
        if not isinstance(error, str) or not error:
            raise RuntimeError(
                "Printer Manager broker returned an invalid error payload"
            )
        raise RuntimeError(error)
    if result.returncode != 0:
        raise RuntimeError(f"Printer Manager broker exited {result.returncode}")
    return payload


def status() -> dict:
    policy.require("sys.observe", name="printing")
    return _broker("status")


def capabilities(printer: str) -> dict:
    printer = _printer_name(printer)
    policy.require("sys.observe", name="printing")
    return _broker("capabilities", printer=printer)


def jobs(printer: str | None = None) -> dict:
    if printer is not None:
        printer = _printer_name(printer)
    policy.require("device.printer", name="observe")
    return _broker("jobs", printer=printer)


def print_document(
    printer: str,
    source: str,
    sides: str | None = None,
    copies: int = 1,
    title: str | None = None,
    media: str | None = None,
) -> dict:
    printer = _printer_name(printer)
    source = _canonical_source(source)
    sides = _sides(sides)
    copies = _copies(copies)
    title = _title(title)
    media = _media(media)
    policy.require("device.printer", name="print")
    policy.require("fs.read", path=source)
    return _broker(
        "print",
        printer=printer,
        source=source,
        sides=sides,
        copies=copies,
        title=title,
        media=media,
    )


def cancel(job_id: str, confirm: bool) -> dict:
    job_id = _job_id(job_id)
    if confirm is not True:
        raise ValueError("cancel requires confirmation")
    policy.require("device.printer", name="control")
    return _broker("cancel", job_id=job_id, confirm=True)
