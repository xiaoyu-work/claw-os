"""Internal atomic replacement transport; never writes a target directly.

The broker spends exact read/write capabilities and owns the durable mutation
bracket. A transport failure can hide a committed replacement; callers must
keep their own applying bracket unresolved rather than automatically retrying.
"""

from __future__ import annotations

import base64
import hashlib
import json
import re
import subprocess

from .policy import PolicyError, _cos_binary

_MAX_CONTENT = 65_536
_MAX_JSON = 128 * 1024
_TIMEOUT = 60
_STATE_KEYS = {"sha256", "size", "device", "inode", "mode", "modified_ns", "changed_ns"}
_RESULT_KEYS = {"path", "bytes", "sha256", "changed"}


class FileChangeError(Exception):
    """Replacement failed or its outcome is unknown.

    ``indeterminate=False`` is used only for validation/discovery failures
    before invoking cos. A relay may flatten provider errors, so an ordinary
    error code from a subprocess is not evidence that the target is unchanged.
    """

    def __init__(self, message: str, *, code: str | None = None, indeterminate: bool = True):
        self.code = code
        self.indeterminate = indeterminate
        super().__init__(message)


def _invalid(message: str) -> None:
    raise FileChangeError(message, code="invalid_request", indeterminate=False)


def validate_path(path: str) -> None:
    """Validate a literal broker target without I/O or an authority check."""
    if not isinstance(path, str):
        _invalid("replacement path must be a string")
    try:
        path_size = len(path.encode("utf-8"))
    except UnicodeError as exc:
        raise FileChangeError("replacement path is not UTF-8", code="invalid_request", indeterminate=False) from exc
    if (
        not path.startswith("/")
        or path_size > 4096
        or path == "/"
        or any(not char.isprintable() or char in "*?[]{}<>|;&$`\\" for char in path)
        or any(part in {"", ".", ".."} for part in path.split("/")[1:])
    ):
        _invalid("replacement path must be absolute and literal, without traversal or redirection")


def _validate(path: str, expected: dict | None, content: bytes) -> dict | None:
    validate_path(path)
    if not isinstance(content, bytes) or len(content) > _MAX_CONTENT:
        _invalid("replacement content must be bytes no larger than 64 KiB")
    if expected is None:
        return None
    if not isinstance(expected, dict) or set(expected) != _STATE_KEYS:
        _invalid("replacement expected state must have exactly the file fingerprint fields")
    expected = dict(expected)
    if not isinstance(expected["sha256"], str) or not re.fullmatch(r"sha256:[0-9a-f]{64}", expected["sha256"]):
        _invalid("replacement expected SHA-256 is not canonical")
    for key in _STATE_KEYS - {"sha256"}:
        if type(expected[key]) is not int:
            _invalid("replacement expected metadata must contain integers")
        lower, upper = (-(1 << 63), (1 << 63) - 1) if key in {"modified_ns", "changed_ns"} else (0, (1 << 64) - 1)
        if key == "mode":
            upper = (1 << 32) - 1
        elif key == "size":
            upper = _MAX_CONTENT
        if not lower <= expected[key] <= upper:
            _invalid("replacement expected metadata is out of bounds")
    return expected


def replace_file(path: str, expected: dict | None, content: bytes) -> dict:
    """Replace one bounded file through ``cos __file replace``.

    ``expected`` is required: ``None`` means the target must be absent.
    Existing-file fingerprints use full ``st_mode`` and nanosecond timestamps,
    without namespace-dependent uid/gid fields. Content is sent only on stdin.
    """
    expected = _validate(path, expected, content)
    payload = json.dumps(
        {"path": path, "expected": expected, "content_base64": base64.b64encode(content).decode("ascii")},
        ensure_ascii=False,
        separators=(",", ":"),
    )
    if len(payload.encode("utf-8")) > _MAX_JSON:
        _invalid("replacement JSON exceeds 128 KiB")
    try:
        binary = _cos_binary()
    except PolicyError as exc:
        raise FileChangeError("cos is unavailable for file replacement", code="unavailable", indeterminate=False) from exc
    try:
        process = subprocess.run(
            [binary, "__file", "replace"],
            input=payload,
            capture_output=True,
            text=True,
            encoding="utf-8",
            check=False,
            timeout=_TIMEOUT,
        )
    except (OSError, subprocess.TimeoutExpired, UnicodeError) as exc:
        raise FileChangeError("file replacement transport failed; outcome is unknown") from exc

    stdout = process.stdout or ""
    try:
        if len(stdout.encode("utf-8")) > _MAX_JSON:
            raise FileChangeError("file replacement response exceeds its bound")
        result = json.loads(stdout)
    except (ValueError, UnicodeError, RecursionError) as exc:
        raise FileChangeError("file replacement returned an invalid response") from exc
    if process.returncode != 0 or not isinstance(result, dict) or "error" in result:
        code = result.get("code") if isinstance(result, dict) else None
        if not isinstance(code, str) or not re.fullmatch(r"[a-z0-9_]{1,64}", code):
            code = None
        message = result.get("error") if isinstance(result, dict) else None
        if not isinstance(message, str) or not message:
            message = "file replacement was refused or its outcome is unknown"
        raise FileChangeError(message[:1024], code=code)

    digest = "sha256:" + hashlib.sha256(content).hexdigest()
    if (
        set(result) != _RESULT_KEYS
        or result["path"] != path
        or type(result["bytes"]) is not int
        or result["bytes"] != len(content)
        or result["sha256"] != digest
        or type(result["changed"]) is not bool
        or (
            not result["changed"]
            and (expected is None or expected["sha256"] != digest or expected["size"] != len(content))
        )
    ):
        raise FileChangeError("file replacement returned an inconsistent result")
    return result
