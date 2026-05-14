"""Agent — open the ClawOS Agent desktop window.

This app is the GUI face of the system-level Agent kernel
(`cos agent`). It prefers the native libcosmic UI binary
(`cos-agent-ui`) when present, falling back to a windowed
Chromium pointed at the local cos-agent-bridge HTTP server
when the native binary isn't installed yet.

The bridge is normally auto-started by the cos-agent-bridge.service
user-scoped systemd unit at login. If the port file is missing we
also try to start the unit on-demand before giving up.
"""

import json
import os
import shutil
import subprocess
import sys
import time


def _runtime_dir():
    """Directory where the bridge publishes its bound port."""
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
    """Try to start the user systemd unit. Best effort, no error if it fails."""
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


def _find_browser():
    """Return (name, path) of a usable GUI browser binary, or (None, None)."""
    for cand in ("chromium", "chromium-browser", "google-chrome", "firefox"):
        path = shutil.which(cand)
        if path:
            return cand, path
    return None, None


def _find_native_ui():
    """Return path to `cos-agent-ui` if installed, else None.

    The native libcosmic UI is the preferred surface: same brand, same
    SSE protocol, no chromium dependency, full GPU rendering through
    the COSMIC compositor. When it's missing (overlay-only rootfs,
    pre-cutover images), we fall back to launching chromium against
    the bridge's static React export.
    """
    return shutil.which("cos-agent-ui")


def _ensure_port():
    """Get the bridge port, starting the bridge on demand if needed."""
    port = _read_port(timeout=0.5)
    if port is not None:
        return port
    _start_bridge()
    return _read_port(timeout=5.0)


def _cmd_url(_args):
    """Print http://127.0.0.1:PORT/. Useful for the overlay + scripting."""
    port = _ensure_port()
    if port is None:
        return {"error": "cos-agent-bridge is not running"}
    return {"url": f"http://127.0.0.1:{port}/", "port": port}


def _exec_native(extra_args):
    """Replace this process with the native libcosmic UI binary."""
    native = _find_native_ui()
    if not native:
        return None
    try:
        os.execv(native, [native, *extra_args])
    except OSError as exc:
        return {"error": f"failed to launch cos-agent-ui: {exc}"}
    return {"error": "execv returned"}  # unreachable on success


def _cmd_open(_args):
    """Open the Agent window.

    Prefers `cos-agent-ui` (native libcosmic). Falls back to the
    chromium-backed React app served by the bridge.
    """
    # Native path: doesn't need the bridge port up-front — the binary
    # reads it from the port file itself and shows an in-app error if
    # the bridge isn't up. Avoids the 5s systemd wait on the happy path.
    if _find_native_ui():
        return _exec_native([])

    port = _ensure_port()
    if port is None:
        return {
            "error": "cos-agent-bridge is not running and could not be started",
            "hint": "systemctl --user start cos-agent-bridge.service",
        }

    url = f"http://127.0.0.1:{port}/"
    name, browser = _find_browser()
    if not browser:
        return {
            "error": "no supported windowed browser found",
            "hint": "apt-get install chromium  (or install cos-agent-ui)",
            "url": url,
        }

    if name in ("chromium", "chromium-browser", "google-chrome"):
        # Site-specific browser window: no tabs, no address bar. A
        # dedicated user-data-dir keeps cookies/cache isolated from
        # the user's regular browsing profile, and --class lets the
        # compositor group multiple Agent windows under one icon.
        profile = os.path.join(_runtime_dir(), "cos-agent-chrome-profile")
        cmd = [
            browser,
            f"--app={url}",
            "--class=ClawOS-Agent",
            f"--user-data-dir={profile}",
        ]
    else:
        cmd = [browser, "--new-window", url]

    # execv collapses the cos→python→browser chain into a single PID
    # so the .desktop launcher's lifecycle tracks the actual window.
    try:
        os.execv(browser, cmd)
    except OSError as exc:
        return {"error": f"failed to launch {browser}: {exc}"}
    return {"error": "execv returned"}  # unreachable on success


def _cmd_overlay(args):
    """Open the Spotlight-style quick-summon overlay.

    Native path: spawn `cos-agent-ui --overlay`. The window is its own
    compact Esc-to-close surface; the libcosmic compositor handles the
    re-summon (focusing an existing instance) for us.

    Chromium fallback: site-specific browser window pointed at the
    bridge's `/?overlay=1` URL. Uses a distinct --user-data-dir so the
    React UI's `overlay=1` query param triggers the compact layout.

    Optional `--voice` arms the mic on open (used by Super+Shift+A).
    The native UI accepts `--voice` directly; the chromium fallback
    threads it through as a `voice=1` query param the React composer
    auto-reads.
    """
    voice = "--voice" in args

    if _find_native_ui():
        argv = ["--overlay"]
        if voice:
            argv.append("--voice")
        return _exec_native(argv)

    port = _ensure_port()
    if port is None:
        return {
            "error": "cos-agent-bridge is not running and could not be started",
            "hint": "systemctl --user start cos-agent-bridge.service",
        }

    query = "overlay=1"
    if voice:
        query += "&voice=1"
    url = f"http://127.0.0.1:{port}/?{query}"

    name, browser = _find_browser()
    if not browser:
        return {
            "error": "no supported windowed browser found",
            "hint": "apt-get install chromium  (or install cos-agent-ui)",
            "url": url,
        }

    if name in ("chromium", "chromium-browser", "google-chrome"):
        profile = os.path.join(_runtime_dir(), "cos-agent-overlay-profile")
        # 640×420 centered horizontally near the top of the screen
        # gives the Spotlight feel without requiring a real layer-shell
        # window manager hook.
        cmd = [
            browser,
            f"--app={url}",
            "--class=ClawOS-Agent-Overlay",
            f"--user-data-dir={profile}",
            "--window-size=640,420",
            "--window-position=center,top",
        ]
    else:
        cmd = [browser, "--new-window", url]

    try:
        os.execv(browser, cmd)
    except OSError as exc:
        return {"error": f"failed to launch {browser}: {exc}"}
    return {"error": "execv returned"}


def _schema():
    return {
        "open": {
            "description": "Open the ClawOS Agent window (native cos-agent-ui, chromium fallback)",
            "parameters": [],
            "example": "cos app agent open",
        },
        "overlay": {
            "description": "Open the Spotlight-style Super+A quick-summon overlay",
            "parameters": [
                {"name": "--voice", "type": "boolean", "required": False,
                 "description": "Auto-arm the microphone on open (chromium fallback only for now)",
                 "kind": "flag", "default": False},
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
