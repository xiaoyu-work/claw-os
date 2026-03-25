"""net — HTTP client for API calls."""

import argparse
import json
import os
import shutil
import urllib.error
import urllib.parse
import urllib.request

USER_AGENT = "cos/" + os.environ.get("COS_VERSION", "0.1.0")
DEFAULT_TIMEOUT = int(os.environ.get("COS_NET_TIMEOUT", "30"))
MAX_RESPONSE_BYTES = 5_000_000  # 5 MB response body limit


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
        with urllib.request.urlopen(req, timeout=opts.timeout) as resp:
            raw = resp.read()
            resp_headers = dict(resp.getheaders())
            truncated = len(raw) > MAX_RESPONSE_BYTES
            if truncated:
                raw = raw[:MAX_RESPONSE_BYTES]
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

    output_path = opts.output
    if output_path is None:
        filename = os.path.basename(urllib.parse.urlparse(opts.url).path) or "download"
        output_path = os.path.join("/den", filename)

    headers = {"User-Agent": USER_AGENT}
    req = urllib.request.Request(opts.url, headers=headers)

    try:
        with urllib.request.urlopen(req, timeout=DEFAULT_TIMEOUT) as resp:
            os.makedirs(os.path.dirname(output_path) or ".", exist_ok=True)
            with open(output_path, "wb") as f:
                shutil.copyfileobj(resp, f)
            size = os.path.getsize(output_path)
            return {"url": opts.url, "path": output_path, "bytes": size}
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
                {"name": "--output", "type": "string", "required": False, "description": "Output file path (defaults to /den/<filename>)", "kind": "flag"},
            ],
            "example": "cos app net download https://example.com/file.zip --output /workspace/file.zip",
        },
    }


def run(command, args):
    """Entry point called by cos."""
    if command == "__schema__":
        return _schema()
    if command == "fetch":
        return cmd_fetch(args)
    elif command == "download":
        return cmd_download(args)
    else:
        return {"error": f"unknown command: {command}"}
