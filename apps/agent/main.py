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
import stat
import subprocess
import sys
import time


def _runtime_dir():
    base = os.environ.get("XDG_RUNTIME_DIR")
    if not base or not os.path.isabs(base):
        return None
    return os.path.join(base, "cos-agent-bridge")


def _endpoint_file():
    runtime_dir = _runtime_dir()
    return os.path.join(runtime_dir, "endpoint.json") if runtime_dir else None


def _read_endpoint(timeout=0.5):
    """Read the bridge's private endpoint, optionally polling until timeout."""
    path = _endpoint_file()
    if path is None:
        return None
    deadline = time.monotonic() + timeout
    while True:
        try:
            runtime_metadata = os.stat(os.path.dirname(path), follow_symlinks=False)
            if (
                not stat.S_ISDIR(runtime_metadata.st_mode)
                or runtime_metadata.st_uid != os.geteuid()
                or stat.S_IMODE(runtime_metadata.st_mode) & 0o077
            ):
                return None
            flags = (
                os.O_RDONLY
                | getattr(os, "O_CLOEXEC", 0)
                | getattr(os, "O_NOFOLLOW", 0)
            )
            fd = os.open(path, flags)
            try:
                metadata = os.fstat(fd)
                if (
                    not stat.S_ISREG(metadata.st_mode)
                    or metadata.st_uid != os.geteuid()
                    or stat.S_IMODE(metadata.st_mode) & 0o077
                    or metadata.st_size <= 0
                    or metadata.st_size > 4096
                ):
                    return None
                with os.fdopen(fd, "r", encoding="utf-8") as endpoint_file:
                    fd = -1
                    endpoint = json.load(endpoint_file)
            finally:
                if fd >= 0:
                    os.close(fd)
            port = endpoint.get("port")
            token = endpoint.get("token")
            if (
                isinstance(port, int)
                and 1 <= port <= 65535
                and isinstance(token, str)
                and 32 <= len(token) <= 256
                and all(char.isascii() and (char.isalnum() or char in "-_") for char in token)
            ):
                return endpoint
            return None
        except (FileNotFoundError, OSError, ValueError, TypeError, json.JSONDecodeError):
            if time.monotonic() >= deadline:
                return None
            time.sleep(0.1)


def _start_bridge():
    """Try to start the user systemd unit. Best effort."""
    systemctl = shutil.which("systemctl")
    if not systemctl:
        return False
    try:
        # ``stdin=DEVNULL`` so a confused systemctl can't block on a
        # password prompt; we'd rather fail fast and report it.
        subprocess.run(
            [systemctl, "--user", "start", "cos-agent-bridge.service"],
            timeout=5,
            check=False,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
        return True
    except (subprocess.TimeoutExpired, OSError):
        return False


def _ensure_endpoint():
    """Return the bridge endpoint, starting the unit on demand if needed."""
    endpoint = _read_endpoint(timeout=0.5)
    if endpoint is not None:
        return endpoint
    _start_bridge()
    return _read_endpoint(timeout=5.0)


def _find_native_ui():
    """Return path to `cos-agent-ui` if installed, else None."""
    return shutil.which("cos-agent-ui")


def _exec_native(extra_args):
    """Replace this process with the native UI binary.

    execv collapses cos → python → cos-agent-ui into one PID so the
    .desktop launcher's lifecycle tracks the actual window. Ensure the
    user service has published an endpoint first; the native UI then
    performs its own health check and reconnect loop.
    """
    native = _find_native_ui()
    if not native:
        return {
            "error": "cos-agent-ui is not installed",
            "hint": "apt-get install --reinstall claw-os-desktop",
        }
    if _ensure_endpoint() is None:
        return {
            "error": "cos-agent-bridge is not ready",
            "hint": "systemctl --user restart cos-agent-bridge.service",
        }
    try:
        os.execv(native, [native, *extra_args])
    except OSError as exc:
        return {"error": f"failed to launch cos-agent-ui: {exc}"}
    return {"error": "execv returned"}  # unreachable on success


def _cmd_url(_args):
    """Return the authenticated local endpoint for scripting and debugging."""
    endpoint = _ensure_endpoint()
    if endpoint is None:
        return {"error": "cos-agent-bridge is not running"}
    port = endpoint["port"]
    return {
        "url": f"http://127.0.0.1:{port}/api/",
        "port": port,
        "authorization": "Bearer " + endpoint["token"],
    }


def _cmd_open(_args):
    """Open the Agent window via the native cos-agent-ui."""
    return _exec_native([])


def _cmd_overlay(args):
    """Open the Spotlight-style quick-summon overlay.

    Spawns `cos-agent-ui --overlay`. With `--voice`, the native UI
    auto-arms the microphone on launch (used by the Super+Shift+A
    global hotkey). With `--query <text>`, the prompt is pre-filled
    and submitted immediately (used by the launcher's "Ask Claw AI").
    """
    argv = ["--overlay"]
    i = 0
    while i < len(args):
        a = args[i]
        if a == "--voice":
            argv.append("--voice")
        elif a == "--query" and i + 1 < len(args):
            argv.append("--query")
            argv.append(args[i + 1])
            i += 1
        elif a == "--context" and i + 1 < len(args):
            argv.append("--context")
            argv.append(args[i + 1])
            i += 1
        i += 1
    return _exec_native(argv)


def run(command, args):
    """Entry point called by cos."""
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
    cmd, rest = argv[0], argv[1:]
    result = run(cmd, rest)
    # _cmd_open / _cmd_overlay exec on the happy path and never return.
    print(json.dumps(result, indent=2, ensure_ascii=False))
    if isinstance(result, dict) and "error" in result:
        sys.exit(1)


if __name__ == "__main__":
    main()
