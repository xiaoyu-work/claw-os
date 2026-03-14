#!/usr/bin/env python3
"""Agent OS HTTP API server.

Wraps the aos CLI functionality as a JSON HTTP API so that AI agents
can interact with Agent OS over HTTP instead of forking processes.

Usage:
    python3 runtime/server.py [--port PORT]
"""

import argparse
import json
import os
import platform
import shutil
import subprocess
import sys
import urllib.error
import urllib.request
from http.server import HTTPServer, BaseHTTPRequestHandler

VERSION = "0.2.0"
DEFAULT_HOST = "0.0.0.0"
DEFAULT_PORT = 8080


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

def json_response(handler, data, status=200):
    """Write a JSON response to the client."""
    body = json.dumps(data).encode("utf-8")
    handler.send_response(status)
    handler.send_header("Content-Type", "application/json")
    handler.send_header("Content-Length", str(len(body)))
    handler.end_headers()
    handler.wfile.write(body)


def json_error(handler, message, status=400):
    """Write a JSON error response to the client."""
    json_response(handler, {"error": message}, status=status)


def read_json_body(handler):
    """Read and parse the JSON request body. Returns {} when body is empty."""
    length = int(handler.headers.get("Content-Length", 0))
    if length == 0:
        return {}
    raw = handler.rfile.read(length)
    try:
        return json.loads(raw)
    except json.JSONDecodeError as exc:
        raise ValueError(f"invalid JSON: {exc}") from exc


# ---------------------------------------------------------------------------
# Route handlers — each returns (data_dict, status_code)
# ---------------------------------------------------------------------------

def handle_health(_body):
    return {"status": "ok"}, 200


# --- fs --------------------------------------------------------------------

def handle_fs_ls(body):
    target = body.get("path", ".")
    try:
        entries = sorted(os.listdir(target))
    except OSError as exc:
        return {"error": str(exc)}, 400
    return {"path": os.path.abspath(target), "files": entries}, 200


def handle_fs_pwd(_body):
    return {"cwd": os.getcwd()}, 200


def handle_fs_read(body):
    path = body.get("path")
    if not path:
        return {"error": "missing required field: path"}, 400
    try:
        with open(path, "r") as fh:
            content = fh.read()
    except OSError as exc:
        return {"error": str(exc)}, 400
    return {"path": os.path.abspath(path), "content": content}, 200


def handle_fs_write(body):
    path = body.get("path")
    content = body.get("content", "")
    if not path:
        return {"error": "missing required field: path"}, 400
    try:
        os.makedirs(os.path.dirname(path) or ".", exist_ok=True)
        with open(path, "w") as fh:
            fh.write(content)
    except OSError as exc:
        return {"error": str(exc)}, 400
    return {"path": os.path.abspath(path), "bytes": len(content)}, 200


def handle_fs_stat(body):
    path = body.get("path")
    if not path:
        return {"error": "missing required field: path"}, 400
    try:
        st = os.stat(path)
    except OSError as exc:
        return {"error": str(exc)}, 400
    return {
        "path": os.path.abspath(path),
        "size": st.st_size,
        "mode": oct(st.st_mode),
        "uid": st.st_uid,
        "gid": st.st_gid,
        "is_dir": os.path.isdir(path),
        "is_file": os.path.isfile(path),
    }, 200


def handle_fs_rm(body):
    path = body.get("path")
    if not path:
        return {"error": "missing required field: path"}, 400
    try:
        if os.path.isdir(path):
            shutil.rmtree(path)
        else:
            os.remove(path)
    except OSError as exc:
        return {"error": str(exc)}, 400
    return {"removed": os.path.abspath(path)}, 200


def handle_fs_mkdir(body):
    path = body.get("path")
    if not path:
        return {"error": "missing required field: path"}, 400
    try:
        os.makedirs(path, exist_ok=True)
    except OSError as exc:
        return {"error": str(exc)}, 400
    return {"created": os.path.abspath(path)}, 200


# --- exec ------------------------------------------------------------------

def handle_exec_run(body):
    command = body.get("command")
    if not command:
        return {"error": "missing required field: command"}, 400

    use_shell = body.get("shell", False)
    timeout = body.get("timeout", 300)

    try:
        result = subprocess.run(
            command,
            capture_output=True,
            text=True,
            shell=use_shell,
            timeout=timeout,
        )
    except FileNotFoundError:
        cmd_name = command if isinstance(command, str) else command[0]
        return {"error": f"command not found: {cmd_name}"}, 400
    except subprocess.TimeoutExpired:
        cmd_str = command if isinstance(command, str) else " ".join(command)
        return {"error": f"timeout after {timeout}s: {cmd_str}"}, 408

    return {
        "command": command,
        "exit_code": result.returncode,
        "stdout": result.stdout,
        "stderr": result.stderr,
    }, 200


def handle_exec_which(body):
    name = body.get("name")
    if not name:
        return {"error": "missing required field: name"}, 400
    path = shutil.which(name)
    if path is None:
        return {"error": f"not found: {name}"}, 404
    return {"command": name, "path": path}, 200


# --- net -------------------------------------------------------------------

def handle_net_fetch(body):
    url = body.get("url")
    if not url:
        return {"error": "missing required field: url"}, 400

    method = body.get("method", "GET").upper()
    headers = body.get("headers", {})
    headers.setdefault("User-Agent", f"aos/{VERSION}")
    req_body = body.get("body")
    data = req_body.encode("utf-8") if req_body else None

    try:
        req = urllib.request.Request(url, method=method, headers=headers, data=data)
        with urllib.request.urlopen(req, timeout=30) as resp:
            resp_body = resp.read().decode("utf-8", errors="replace")
            return {
                "url": url,
                "status": resp.status,
                "body": resp_body,
            }, 200
    except urllib.error.HTTPError as exc:
        return {"url": url, "status": exc.code, "error": str(exc)}, 502
    except Exception as exc:
        return {"error": f"fetch failed: {exc}"}, 502


# --- sys -------------------------------------------------------------------

def handle_sys_info(_body):
    return {
        "name": "agent-os",
        "version": VERSION,
        "platform": platform.platform(),
        "arch": platform.machine(),
        "python": platform.python_version(),
        "hostname": platform.node(),
        "pid": os.getpid(),
        "uid": os.getuid(),
    }, 200


def handle_sys_env(_body):
    return {"env": dict(os.environ)}, 200


# ---------------------------------------------------------------------------
# Route table
# ---------------------------------------------------------------------------

ROUTES = {
    ("fs", "ls"):      handle_fs_ls,
    ("fs", "pwd"):     handle_fs_pwd,
    ("fs", "read"):    handle_fs_read,
    ("fs", "write"):   handle_fs_write,
    ("fs", "stat"):    handle_fs_stat,
    ("fs", "rm"):      handle_fs_rm,
    ("fs", "mkdir"):   handle_fs_mkdir,
    ("exec", "run"):   handle_exec_run,
    ("exec", "which"): handle_exec_which,
    ("net", "fetch"):  handle_net_fetch,
    ("sys", "info"):   handle_sys_info,
    ("sys", "env"):    handle_sys_env,
}


# ---------------------------------------------------------------------------
# HTTP request handler
# ---------------------------------------------------------------------------

class AosRequestHandler(BaseHTTPRequestHandler):
    """Handles incoming HTTP requests and routes them to handlers."""

    # Silence default stderr log line per request (we do our own logging).
    def log_message(self, fmt, *args):
        sys.stderr.write(f"[aos-api] {self.address_string()} {fmt % args}\n")

    # -- GET ----------------------------------------------------------------

    def do_GET(self):
        if self.path == "/api/v1/health":
            data, status = handle_health({})
            json_response(self, data, status)
        else:
            json_error(self, f"not found: {self.path}", 404)

    # -- POST ---------------------------------------------------------------

    def do_POST(self):
        # Parse /api/v1/{group}/{command}
        parts = self.path.strip("/").split("/")
        if len(parts) != 4 or parts[0] != "api" or parts[1] != "v1":
            json_error(self, f"not found: {self.path}", 404)
            return

        group, command = parts[2], parts[3]
        handler = ROUTES.get((group, command))
        if handler is None:
            json_error(self, f"unknown command: {group} {command}", 404)
            return

        try:
            body = read_json_body(self)
        except ValueError as exc:
            json_error(self, str(exc), 400)
            return

        try:
            data, status = handler(body)
        except Exception as exc:
            json_error(self, f"internal error: {exc}", 500)
            return

        json_response(self, data, status)


# ---------------------------------------------------------------------------
# Entry point
# ---------------------------------------------------------------------------

def main():
    parser = argparse.ArgumentParser(description="Agent OS HTTP API server")
    parser.add_argument("--host", default=DEFAULT_HOST, help="bind address")
    parser.add_argument("--port", type=int, default=DEFAULT_PORT, help="listen port")
    args = parser.parse_args()

    server = HTTPServer((args.host, args.port), AosRequestHandler)
    sys.stderr.write(f"[aos-api] listening on {args.host}:{args.port}\n")
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        sys.stderr.write("\n[aos-api] shutting down\n")
        server.server_close()


if __name__ == "__main__":
    main()
