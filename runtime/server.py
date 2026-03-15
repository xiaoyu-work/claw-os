#!/usr/bin/env python3
"""Agent OS HTTP API server.

Exposes the aos app system as a JSON HTTP API so that AI agents
can interact with Agent OS over HTTP instead of forking processes.

Routes are discovered dynamically from installed apps — no hardcoded
handlers.  Every app command available via ``aos <app> <cmd>`` is also
available via ``POST /api/v1/<app>/<cmd>``.

Usage:
    python3 runtime/server.py [--port PORT] [--apps-dir DIR]
"""

import argparse
import importlib.util
import json
import os
import platform
import sys
from http.server import HTTPServer, BaseHTTPRequestHandler

VERSION = "0.3.0"
DEFAULT_HOST = "0.0.0.0"
DEFAULT_PORT = 8080
APPS_DIR = os.environ.get("AOS_APPS_DIR", "/usr/lib/aos/apps")


# ---------------------------------------------------------------------------
# App discovery (mirrors the logic in the aos CLI router)
# ---------------------------------------------------------------------------

def discover_apps(apps_dir):
    """Scan apps directory and return {name: manifest} for each valid app."""
    apps = {}
    if not os.path.isdir(apps_dir):
        return apps
    for name in sorted(os.listdir(apps_dir)):
        manifest_path = os.path.join(apps_dir, name, "app.json")
        if os.path.isfile(manifest_path):
            try:
                with open(manifest_path) as f:
                    apps[name] = json.load(f)
            except (json.JSONDecodeError, OSError):
                pass
    return apps


def load_app_module(apps_dir, app_name):
    """Load an app's main.py as a module and return it."""
    main_path = os.path.join(apps_dir, app_name, "main.py")
    if not os.path.isfile(main_path):
        return None
    spec = importlib.util.spec_from_file_location(f"aos_app_{app_name}", main_path)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


# ---------------------------------------------------------------------------
# JSON body → CLI args conversion
#
# Apps expect a list of strings (CLI-style).  The HTTP body is a JSON
# object.  We convert using a lightweight convention:
#   { "path": "/tmp/x", "content": "hello" }
#   → ["--path", "/tmp/x", "--content", "hello"]
#
# Positional args can be passed via the special "args" key:
#   { "args": ["/tmp/x"] }            → ["/tmp/x"]
#   { "path": "/tmp/x" }              → ["/tmp/x"]  (single-value shortcut)
#
# Booleans become flags:
#   { "shell": true }                 → ["--shell"]
#   { "urgent": true }                → ["--urgent"]
#
# For commands that take a single positional argument (most common case),
# we detect well-known field names and pass them positionally.
# ---------------------------------------------------------------------------

# Fields that should be passed as positional args (not --key value).
_POSITIONAL_FIELDS = {"path", "url", "key", "name", "query", "command"}


def body_to_args(body):
    """Convert a JSON request body into a CLI-style args list."""
    if not body:
        return []

    # Explicit args list takes precedence
    if "args" in body:
        raw = body["args"]
        return [str(a) for a in raw] if isinstance(raw, list) else [str(raw)]

    positional = []
    flags = []

    for key, value in body.items():
        if value is None:
            continue
        if isinstance(value, bool):
            if value:
                flags.append(f"--{key}")
        elif key in _POSITIONAL_FIELDS:
            positional.append(str(value))
        elif isinstance(value, list):
            for item in value:
                flags.extend([f"--{key}", str(item)])
        else:
            flags.extend([f"--{key}", str(value)])

    return positional + flags


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
# Built-in handlers (not backed by an app)
# ---------------------------------------------------------------------------

def handle_health(_body):
    return {"status": "ok"}, 200


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


BUILTIN_ROUTES = {
    ("sys", "info"): handle_sys_info,
    ("sys", "env"):  handle_sys_env,
}


# ---------------------------------------------------------------------------
# HTTP request handler
# ---------------------------------------------------------------------------

class AosRequestHandler(BaseHTTPRequestHandler):
    """Routes HTTP requests to aos apps or built-in handlers."""

    def log_message(self, fmt, *args):
        sys.stderr.write(f"[aos-api] {self.address_string()} {fmt % args}\n")

    # -- GET ----------------------------------------------------------------

    def do_GET(self):
        if self.path == "/api/v1/health":
            data, status = handle_health({})
            json_response(self, data, status)
        elif self.path == "/api/v1/apps":
            apps = discover_apps(self.server.apps_dir)
            app_list = []
            for name, manifest in apps.items():
                app_list.append({
                    "name": name,
                    "description": manifest.get("description", ""),
                    "commands": list(manifest.get("commands", {}).keys()),
                })
            json_response(self, {"apps": app_list})
        else:
            json_error(self, f"not found: {self.path}", 404)

    # -- POST ---------------------------------------------------------------

    def do_POST(self):
        parts = self.path.strip("/").split("/")
        if len(parts) != 4 or parts[0] != "api" or parts[1] != "v1":
            json_error(self, f"not found: {self.path}", 404)
            return

        app_name, command = parts[2], parts[3]

        try:
            body = read_json_body(self)
        except ValueError as exc:
            json_error(self, str(exc), 400)
            return

        # Check built-in routes first
        builtin = BUILTIN_ROUTES.get((app_name, command))
        if builtin is not None:
            try:
                data, status = builtin(body)
            except Exception as exc:
                json_error(self, f"internal error: {exc}", 500)
                return
            json_response(self, data, status)
            return

        # Dynamic app routing
        apps = discover_apps(self.server.apps_dir)
        if app_name not in apps:
            json_error(self, f"unknown app: {app_name}", 404)
            return

        valid_commands = apps[app_name].get("commands", {})
        if command not in valid_commands:
            json_error(self, f"unknown command: {app_name} {command}", 404)
            return

        mod = load_app_module(self.server.apps_dir, app_name)
        if mod is None or not hasattr(mod, "run"):
            json_error(self, f"app '{app_name}' is not runnable", 500)
            return

        args = body_to_args(body)

        try:
            result = mod.run(command, args)
        except SystemExit:
            json_error(self, f"{app_name} {command} failed", 500)
            return
        except Exception as exc:
            json_error(self, f"{app_name} {command}: {exc}", 500)
            return

        if result is None:
            result = {}

        status = 200 if "error" not in result else 400
        json_response(self, result, status)


# ---------------------------------------------------------------------------
# Entry point
# ---------------------------------------------------------------------------

def main():
    parser = argparse.ArgumentParser(description="Agent OS HTTP API server")
    parser.add_argument("--host", default=DEFAULT_HOST, help="bind address")
    parser.add_argument("--port", type=int, default=DEFAULT_PORT, help="listen port")
    parser.add_argument("--apps-dir", default=APPS_DIR, help="apps directory")
    args = parser.parse_args()

    server = HTTPServer((args.host, args.port), AosRequestHandler)
    server.apps_dir = args.apps_dir
    sys.stderr.write(f"[aos-api] v{VERSION} listening on {args.host}:{args.port}\n")
    sys.stderr.write(f"[aos-api] apps dir: {args.apps_dir}\n")
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        sys.stderr.write("\n[aos-api] shutting down\n")
        server.server_close()


if __name__ == "__main__":
    main()
