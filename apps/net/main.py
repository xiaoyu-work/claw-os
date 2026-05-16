"""net — HTTP client for API calls."""

import argparse
import json
import os
import urllib.error
import urllib.parse
import urllib.request

from cos_runtime import policy

USER_AGENT = "cos/" + os.environ.get("COS_VERSION", "0.1.0")
DEFAULT_TIMEOUT = int(os.environ.get("COS_NET_TIMEOUT", "30"))
MAX_RESPONSE_BYTES = 5_000_000  # 5 MB response body limit for fetch
MAX_DOWNLOAD_BYTES = int(os.environ.get("COS_NET_DOWNLOAD_MAX", str(512 * 1024 * 1024)))
_READ_CHUNK = 64 * 1024


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
    p.add_argument("--output", default=None)
    return p


def _parse_header(header_str):
    """Parse 'Key: Value' into (key, value)."""
    key, _, value = header_str.partition(":")
    return key.strip(), value.strip()


def _host_from_url(url):
    """Extract the host[:port] from a URL for capability scoping."""
    try:
        parsed = urllib.parse.urlparse(url)
    except ValueError:
        return None
    if not parsed.hostname:
        return None
    if parsed.port:
        return f"{parsed.hostname}:{parsed.port}"
    return parsed.hostname


def cmd_fetch(args):
    parser = _build_fetch_parser()
    opts = parser.parse_args(args)

    host = _host_from_url(opts.url)
    if host is None:
        return {"error": f"invalid URL: {opts.url}"}
    policy.require("net.dial", host=host)

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
        with urllib.request.urlopen(req, timeout=opts.timeout) as resp:
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
    except Exception as e:
        return {"error": str(e)}


def cmd_download(args):
    parser = _build_download_parser()
    opts = parser.parse_args(args)

    host = _host_from_url(opts.url)
    if host is None:
        return {"error": f"invalid URL: {opts.url}"}

    output_path = opts.output
    if output_path is None:
        filename = os.path.basename(urllib.parse.urlparse(opts.url).path) or "download"
        output_path = os.path.join(os.environ.get("COS_HOME") or os.environ.get("HOME") or "/root", filename)
    # ``realpath`` so the kernel's fs.write check sees the actual
    # destination after symlink resolution; ``abspath`` alone would
    # let a symlink in the output dir redirect the write to a path
    # the caller doesn't have fs.write on.
    output_path = os.path.realpath(output_path)

    policy.require("net.dial", host=host)
    policy.require("fs.write", path=output_path)

    headers = {"User-Agent": USER_AGENT}
    req = urllib.request.Request(opts.url, headers=headers)

    try:
        with urllib.request.urlopen(req, timeout=DEFAULT_TIMEOUT) as resp:
            parent = os.path.dirname(output_path)
            if parent:
                os.makedirs(parent, exist_ok=True)
            # Bounded streaming copy so an unbounded-length response
            # can't fill the disk. ``COS_NET_DOWNLOAD_MAX`` overrides
            # the default 512 MiB limit.
            total = 0
            truncated = False
            with open(output_path, "wb") as f:
                while True:
                    chunk = resp.read(_READ_CHUNK)
                    if not chunk:
                        break
                    if total + len(chunk) > MAX_DOWNLOAD_BYTES:
                        f.write(chunk[: MAX_DOWNLOAD_BYTES - total])
                        total = MAX_DOWNLOAD_BYTES
                        truncated = True
                        break
                    f.write(chunk)
                    total += len(chunk)
            result = {"url": opts.url, "path": output_path, "bytes": total}
            if truncated:
                result["truncated"] = True
                result["limit"] = MAX_DOWNLOAD_BYTES
            return result
    except urllib.error.HTTPError as e:
        return {"error": str(e), "status": e.code}
    except urllib.error.URLError as e:
        return {"error": str(e.reason)}
    except Exception as e:
        return {"error": str(e)}


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
