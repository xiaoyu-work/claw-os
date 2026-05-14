"""Agent — open the ClawOS Agent desktop window.

GUI face of the system-level Agent kernel (`cos agent`). Spawns the
native libcosmic UI (`cos-agent-ui`) and points it at the local
`cos-agent-bridge` HTTP+SSE daemon (which is normally auto-started by
the cos-agent-bridge.service user-scoped systemd unit at login).

There is no fallback path. The chromium+React fallback was retired
when the native UI shipped — both surfaces speak the same `/api/*`
contract, so a missing `cos-agent-ui` binary is a packaging bug, not
a degradation we silently absorb.
"""

import json
import os
import shutil
import subprocess
import sys
import time


def _runtime_dir():
    return os.environ.get("XDG_RUNTIME_DIR") or "/tmp"


def _port_file():
    return os.path.join(_runtime_dir(), "cos-agent-bridge.port")


def _read_port(timeout=0.5):
    """Read the bridge's bound port, optionally polling until timeout."""
    path = _port_file()
    deadline = time.monotonic() + timeout
    while True:
        try:
            with open(path, "r") as f:
                return int(f.read().strip())
        except (FileNotFoundError, ValueError):
            if time.monotonic() >= deadline:
                return None
            time.sleep(0.1)


def _start_bridge():
    """Try to start the user systemd unit. Best effort."""
    systemctl = shutil.which("systemctl")
    if not systemctl:
        return False
    try:
        subprocess.run(
            [systemctl, "--user", "start", "cos-agent-bridge.service"],
            timeout=5,
            check=False,
        )
        return True
    except Exception:
        return False


def _ensure_port():
    """Return the bridge port, starting the unit on demand if needed."""
    port = _read_port(timeout=0.5)
    if port is not None:
        return port
    _start_bridge()
    return _read_port(timeout=5.0)


def _find_native_ui():
    """Return path to `cos-agent-ui` if installed, else None."""
    return shutil.which("cos-agent-ui")


def _exec_native(extra_args):
    """Replace this process with the native UI binary.

    execv collapses cos → python → cos-agent-ui into one PID so the
    .desktop launcher's lifecycle tracks the actual window. The
    native binary reads the bridge port from the port file itself, so
    we don't need _ensure_port() on the happy path.
    """
    native = _find_native_ui()
    if not native:
        return {
            "error": "cos-agent-ui is not installed",
            "hint": "apt-get install --reinstall claw-os-base",
        }
    try:
        os.execv(native, [native, *extra_args])
    except OSError as exc:
        return {"error": f"failed to launch cos-agent-ui: {exc}"}
    return {"error": "execv returned"}  # unreachable on success


def _cmd_url(_args):
    """Print http://127.0.0.1:PORT/. Useful for scripting + debugging."""
    port = _ensure_port()
    if port is None:
        return {"error": "cos-agent-bridge is not running"}
    return {"url": f"http://127.0.0.1:{port}/", "port": port}


def _cmd_open(_args):
    """Open the Agent window via the native cos-agent-ui."""
    return _exec_native([])


def _cmd_overlay(args):
    """Open the Spotlight-style quick-summon overlay.

    Spawns `cos-agent-ui --overlay`. With `--voice`, the native UI
    auto-arms the microphone on launch (used by the Super+Shift+A
    global hotkey).
    """
    argv = ["--overlay"]
    if "--voice" in args:
        argv.append("--voice")
    return _exec_native(argv)


def _schema():
    return {
        "open": {
            "description": "Open the ClawOS Agent window (native libcosmic UI)",
            "parameters": [],
            "example": "cos app agent open",
        },
        "overlay": {
            "description": "Open the Spotlight-style Super+A quick-summon overlay",
            "parameters": [
                {"name": "--voice", "type": "boolean", "required": False,
                 "description": "Auto-arm the microphone on open", "kind": "flag",
                 "default": False},
            ],
            "example": "cos app agent overlay --voice",
        },
        "url": {
            "description": "Print the URL of the local cos-agent-bridge",
            "parameters": [],
            "example": "cos app agent url",
        },
    }


def run(command, args):
    """Entry point called by cos."""
    if command == "__schema__":
        return _schema()
    handlers = {
        "open": _cmd_open,
        "overlay": _cmd_overlay,
        "url": _cmd_url,
    }
    handler = handlers.get(command)
    if handler is None:
        return {"error": f"unknown command: {command}"}
    return handler(args)


def main():
    argv = sys.argv[1:]
    if not argv:
        print(json.dumps({
            "error": "usage: cos app agent <open|overlay|url>",
            "commands": ["open", "overlay", "url"],
        }))
        return
    if argv[0] in ("--schema", "-h", "--help"):
        print(json.dumps(_schema(), indent=2))
        return
    cmd, rest = argv[0], argv[1:]
    result = run(cmd, rest)
    # _cmd_open / _cmd_overlay exec on the happy path and never return.
    print(json.dumps(result, indent=2, ensure_ascii=False))
    if isinstance(result, dict) and "error" in result:
        sys.exit(1)


if __name__ == "__main__":
    main()
