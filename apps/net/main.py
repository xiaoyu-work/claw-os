"""net — HTTP client for API calls."""

import argparse
import json
import os
import tempfile
import urllib.error
import urllib.request
import sys

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

from cos_runtime import policy
from _shared.safe_http import open_url

USER_AGENT = "cos/" + os.environ.get("COS_VERSION", "0.1.0")
DEFAULT_TIMEOUT = int(os.environ.get("COS_NET_TIMEOUT", "30"))
MAX_RESPONSE_BYTES = 5_000_000  # 5 MB response body limit for fetch
MAX_DOWNLOAD_BYTES = int(os.environ.get("COS_NET_DOWNLOAD_MAX", str(512 * 1024 * 1024)))
_READ_CHUNK = 64 * 1024


class _DownloadLimitExceeded(Exception):
    pass


def _read_bounded(resp, limit):
    """Read at most ``limit`` bytes from ``resp`` and report whether
    the response was truncated.

    ``resp.read()`` without an argument will happily read multi-GiB
    bodies into memory; this helper streams in 64 KiB chunks and stops
    as soon as the cap is hit, so a malicious / misconfigured server
    can't OOM the agent.
    """
    chunks: list[bytes] = []
    total = 0
    truncated = False
    while True:
        # +1 so we can detect that *more* bytes were available beyond
        # the limit (and therefore the response was truncated).
        want = min(_READ_CHUNK, limit + 1 - total)
        if want <= 0:
            truncated = True
            break
        chunk = resp.read(want)
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


def _build_fetch_parser():
    p = argparse.ArgumentParser(prog="cos net fetch", add_help=False)
    p.add_argument("url")
    p.add_argument("--method", default="GET", choices=["GET", "POST", "PUT", "DELETE"])
    p.add_argument("--data", default=None)
    p.add_argument("--header", action="append", default=[])
    p.add_argument("--timeout", type=int, default=DEFAULT_TIMEOUT)
    return p


def _build_download_parser():
    p = argparse.ArgumentParser(prog="cos net download", add_help=False)
    p.add_argument("url")
    p.add_argument("output", nargs="?")
    p.add_argument("--output", dest="output_option", default=None)
    return p


def _parse_header(header_str):
    """Parse 'Key: Value' into (key, value)."""
    key, _, value = header_str.partition(":")
    return key.strip(), value.strip()


def cmd_fetch(args):
    parser = _build_fetch_parser()
    opts = parser.parse_args(args)

    headers = {"User-Agent": USER_AGENT}
    for h in opts.header:
        k, v = _parse_header(h)
        headers[k] = v

    data = None
    if opts.data is not None:
        data = opts.data.encode("utf-8")
        if "Content-Type" not in headers:
            headers["Content-Type"] = "application/json"

    req = urllib.request.Request(
        opts.url,
        data=data,
        headers=headers,
        method=opts.method,
    )

    try:
        with open_url(req, timeout=opts.timeout)[0] as resp:
            raw, truncated = _read_bounded(resp, MAX_RESPONSE_BYTES)
            resp_headers = dict(resp.getheaders())
            body = raw.decode("utf-8", errors="replace")
            result = {
                "url": opts.url,
                "status": resp.status,
                "headers": resp_headers,
                "body": body,
            }
            if truncated:
                result["truncated"] = True
            return result
    except urllib.error.HTTPError as e:
        body = ""
        try:
            body = e.read().decode("utf-8", errors="replace")
        except Exception:
            pass
        return {"error": body or str(e), "status": e.code}
    except urllib.error.URLError as e:
        return {"error": str(e.reason)}
    except policy.PolicyError:
        raise
    except Exception as e:
        return {"error": str(e)}


def cmd_download(args):
    parser = _build_download_parser()
    opts = parser.parse_args(args)

    output_path = opts.output_option if opts.output_option is not None else opts.output
    if output_path is None:
        raise ValueError("download output default was not bound by the app bridge")
    # ``realpath`` so the kernel's fs.write check sees the actual
    # destination after symlink resolution; ``abspath`` alone would
    # let a symlink in the output dir redirect the write to a path
    # the caller doesn't have fs.write on.
    output_path = os.path.realpath(output_path)

    policy.require("fs.write", path=output_path)

    headers = {"User-Agent": USER_AGENT}
    req = urllib.request.Request(opts.url, headers=headers)

    temp_fd = None
    temp_path = None
    try:
        with open_url(req, timeout=DEFAULT_TIMEOUT)[0] as resp:
            parent = os.path.dirname(output_path)
            if parent:
                os.makedirs(parent, exist_ok=True)
            temp_parent = parent or "."
            # mkstemp creates the file with mode 0o600. Keeping it beside the
            # destination also guarantees that os.replace stays on one filesystem.
            temp_fd, temp_path = tempfile.mkstemp(
                dir=temp_parent,
                prefix=f".{os.path.basename(output_path)}.",
                suffix=".tmp",
            )
            temp_file = os.fdopen(temp_fd, "wb", closefd=True)
            temp_fd = None
            total = 0
            with temp_file as f:
                while True:
                    remaining = MAX_DOWNLOAD_BYTES - total
                    chunk = resp.read(min(_READ_CHUNK, remaining + 1))
                    if not chunk:
                        break
                    if len(chunk) > remaining:
                        raise _DownloadLimitExceeded
                    f.write(chunk)
                    total += len(chunk)
                f.flush()
                os.fsync(f.fileno())
        os.replace(temp_path, output_path)
        temp_path = None
        return {"url": opts.url, "path": output_path, "bytes": total}
    except _DownloadLimitExceeded:
        return {
            "error": f"download exceeds size limit of {MAX_DOWNLOAD_BYTES} bytes",
            "limit": MAX_DOWNLOAD_BYTES,
        }
    except urllib.error.HTTPError as e:
        return {"error": str(e), "status": e.code}
    except urllib.error.URLError as e:
        return {"error": str(e.reason)}
    except policy.PolicyError:
        raise
    except Exception as e:
        return {"error": str(e)}
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


def _schema():
    return {
        "fetch": {
            "description": "Make an HTTP request and return the response",
            "parameters": [
                {"name": "url", "type": "string", "required": True, "description": "URL to fetch", "kind": "positional"},
                {"name": "--method", "type": "string", "required": False, "description": "HTTP method: GET, POST, PUT, DELETE", "kind": "flag", "default": "GET"},
                {"name": "--data", "type": "string", "required": False, "description": "Request body data", "kind": "flag"},
                {"name": "--header", "type": "string", "required": False, "description": "Request header in 'Key: Value' format (can be repeated)", "kind": "flag"},
                {"name": "--timeout", "type": "integer", "required": False, "description": "Request timeout in seconds", "kind": "flag", "default": 30},
            ],
            "example": "cos app net fetch https://api.example.com/data --method POST --data '{\"key\": \"value\"}' --header 'Authorization: Bearer token'",
        },
        "download": {
            "description": "Download a file from a URL",
            "parameters": [
                {"name": "url", "type": "string", "required": True, "description": "URL to download from", "kind": "positional"},
                {"name": "--output", "type": "string", "required": False, "description": "Output file path (defaults to $COS_HOME/<filename>)", "kind": "flag"},
            ],
            "example": "cos app net download https://example.com/file.zip --output /workspace/file.zip",
        },
    }


def run(command, args):
    """Entry point called by cos."""
    if command == "__schema__":
        return _schema()
    handlers = {
        "fetch": cmd_fetch,
        "download": cmd_download,
    }
    handler = handlers.get(command)
    if handler is None:
        return {"error": f"unknown command: {command}"}
    try:
        return handler(args)
    except policy.PermissionDenied as denied:
        return {"error": str(denied), "denial": denied.denial}
    except policy.PolicyUnavailable as exc:
        return {"error": f"capability check failed: {exc}"}
