"""net — HTTP client for API calls."""

from __future__ import annotations

import os
import re
import sys
import tempfile
import unicodedata
import urllib.request

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

from _shared import safe_http  # noqa: E402
from cos_runtime import policy  # noqa: E402


USER_AGENT = "cos/" + os.environ.get("COS_VERSION", "0.1.0")
DEFAULT_TIMEOUT = 30
MAX_REQUEST_DATA_BYTES = 1_000_000
MAX_RESPONSE_BYTES = 5_000_000
MAX_DOWNLOAD_BYTES = int(
    os.environ.get("COS_NET_DOWNLOAD_MAX", str(512 * 1024 * 1024))
)
MAX_HEADER_COUNT = 100
MAX_HEADER_LINE_BYTES = 8 * 1024
MAX_HEADER_BYTES = 64 * 1024
MAX_OUTPUT_PATH_CHARS = 4096
_READ_CHUNK = 64 * 1024
_METHODS = frozenset({"GET", "POST", "PUT", "DELETE"})
_HEADER_NAME_RE = re.compile(r"^[!#$%&'*+\-.^_`|~0-9A-Za-z]+$")


class DownloadLimitExceeded(RuntimeError):
    """The response body exceeded the configured download hard limit."""


def _has_control_or_newline(value: str) -> bool:
    return any(
        unicodedata.category(character) in {"Cc", "Zl", "Zp"}
        for character in value
    )


def _validate_url(value: object) -> str:
    if type(value) is not str or not value:
        raise ValueError("url must be a non-empty string")
    safe_http.parse_url(value)
    return value


def _validate_method(value: object) -> str:
    if type(value) is not str or value not in _METHODS:
        raise ValueError("method must be one of GET, POST, PUT, DELETE")
    return value


def _encode_data(value: object | None) -> bytes | None:
    if value is None:
        return None
    if type(value) is not str:
        raise ValueError("data must be a string")
    encoded = value.encode("utf-8")
    if len(encoded) > MAX_REQUEST_DATA_BYTES:
        raise ValueError(
            f"data exceeds size limit of {MAX_REQUEST_DATA_BYTES} bytes"
        )
    return encoded


def _build_headers(values: object | None) -> dict[str, str]:
    headers = {"User-Agent": USER_AGENT}
    if values is None:
        return headers
    if type(values) is not list:
        raise ValueError("header must be a list of 'Name: value' strings")
    if len(values) > MAX_HEADER_COUNT:
        raise ValueError(f"header count exceeds limit of {MAX_HEADER_COUNT}")

    total_bytes = 0
    for raw in values:
        if type(raw) is not str:
            raise ValueError("header entries must be strings")
        if _has_control_or_newline(raw):
            raise ValueError("header entries must not contain controls or newlines")
        try:
            line_bytes = len(raw.encode("latin-1"))
        except UnicodeEncodeError:
            raise ValueError(
                "header entries must contain only Latin-1 characters"
            ) from None
        if line_bytes > MAX_HEADER_LINE_BYTES:
            raise ValueError(
                f"header entry exceeds size limit of {MAX_HEADER_LINE_BYTES} bytes"
            )
        total_bytes += line_bytes
        if total_bytes > MAX_HEADER_BYTES:
            raise ValueError(
                f"headers exceed total size limit of {MAX_HEADER_BYTES} bytes"
            )

        name, separator, value = raw.partition(":")
        if not separator or _HEADER_NAME_RE.fullmatch(name) is None:
            raise ValueError("header entries must use a valid 'Name: value' form")
        headers[name] = value.strip()
    return headers


def _validate_timeout(value: object) -> int:
    if type(value) is not int:
        raise ValueError("timeout must be an integer")
    if not 1 <= value <= 300:
        raise ValueError("timeout must be 1..300 seconds")
    return value


def _validate_output(value: object) -> str:
    if (
        type(value) is not str
        or not value
        or len(value) > MAX_OUTPUT_PATH_CHARS
        or not os.path.isabs(value)
        or _has_control_or_newline(value)
    ):
        raise ValueError(
            "output must be a non-empty absolute canonical path without controls"
        )

    basename = os.path.basename(value)
    if not basename or basename in {".", ".."}:
        raise ValueError("output must name a file")
    exists = os.path.lexists(value)
    if exists and os.path.islink(value):
        raise ValueError("output symlinks are not allowed")

    canonical = os.path.join(
        os.path.realpath(os.path.dirname(value)),
        basename,
    )
    if exists:
        canonical = os.path.realpath(value)
    if canonical != value:
        raise ValueError(f"use the canonical output path: {canonical}")
    return canonical


def _read_bounded(response, limit: int) -> tuple[bytes, bool]:
    chunks: list[bytes] = []
    total = 0
    truncated = False
    while True:
        want = min(_READ_CHUNK, limit + 1 - total)
        if want <= 0:
            truncated = True
            break
        chunk = response.read(want)
        if not chunk:
            break
        chunks.append(chunk)
        total += len(chunk)
        if total > limit:
            truncated = True
            break
    raw = b"".join(chunks)
    if truncated:
        raw = raw[:limit]
    return raw, truncated


def fetch(
    url: str,
    method: str = "GET",
    data: str | None = None,
    header: list[str] | None = None,
    timeout: int = 30,
) -> dict[str, object]:
    url = _validate_url(url)
    method = _validate_method(method)
    encoded_data = _encode_data(data)
    headers = _build_headers(header)
    timeout = _validate_timeout(timeout)
    if encoded_data is not None and not any(
        name.lower() == "content-type" for name in headers
    ):
        headers["Content-Type"] = "application/json"

    request = urllib.request.Request(
        url,
        data=encoded_data,
        headers=headers,
        method=method,
    )
    with safe_http.open_url(request, timeout=timeout)[0] as response:
        raw, truncated = _read_bounded(response, MAX_RESPONSE_BYTES)
        result: dict[str, object] = {
            "url": url,
            "status": response.status,
            "headers": dict(response.getheaders()),
            "body": raw.decode("utf-8", errors="replace"),
        }
        if truncated:
            result["truncated"] = True
        return result


def download(url: str, output: str) -> dict[str, object]:
    url = _validate_url(url)
    output_path = _validate_output(output)
    request = urllib.request.Request(url, headers={"User-Agent": USER_AGENT})
    policy.require("fs.write", path=output_path)
    temp_fd: int | None = None
    temp_path: str | None = None
    total = 0
    try:
        with safe_http.open_url(request, timeout=DEFAULT_TIMEOUT)[0] as response:
            parent = os.path.dirname(output_path)
            os.makedirs(parent, exist_ok=True)
            temp_fd, temp_path = tempfile.mkstemp(
                dir=parent,
                prefix=f".{os.path.basename(output_path)}.",
                suffix=".tmp",
            )
            os.fchmod(temp_fd, 0o600)
            temp_file = os.fdopen(temp_fd, "wb", closefd=True)
            temp_fd = None
            with temp_file as file:
                while True:
                    remaining = MAX_DOWNLOAD_BYTES - total
                    chunk = response.read(min(_READ_CHUNK, remaining + 1))
                    if not chunk:
                        break
                    if len(chunk) > remaining:
                        raise DownloadLimitExceeded(
                            "download exceeds size limit of "
                            f"{MAX_DOWNLOAD_BYTES} bytes"
                        )
                    file.write(chunk)
                    total += len(chunk)
                file.flush()
                os.fsync(file.fileno())
        os.replace(temp_path, output_path)
        temp_path = None
        return {"url": url, "path": output_path, "bytes": total}
    finally:
        if temp_fd is not None:
            try:
                os.close(temp_fd)
            except OSError:
                pass
        if temp_path is not None:
            try:
                os.unlink(temp_path)
            except OSError:
                pass
