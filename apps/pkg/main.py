"""pkg — Declarative capability management for Agent OS.

Say what you need, not how to install it.
"""

import shutil
import subprocess


def _dpkg_check(package):
    """Return True if a package is installed according to dpkg."""
    try:
        result = subprocess.run(
            ["dpkg", "-s", package],
            capture_output=True, text=True
        )
        return result.returncode == 0
    except FileNotFoundError:
        return False


def _apt_install(packages):
    """Install packages via apt-get. Returns (installed, failed) lists."""
    installed = []
    failed = []
    for pkg in packages:
        try:
            result = subprocess.run(
                ["apt-get", "install", "-y", pkg],
                capture_output=True, text=True
            )
            if result.returncode == 0:
                installed.append(pkg)
            else:
                failed.append(pkg)
        except PermissionError:
            failed.append(pkg)
        except FileNotFoundError:
            failed.append(pkg)
    return installed, failed


def cmd_need(args):
    """Ensure packages are installed, only installing what's missing."""
    if not args:
        return {"error": "need requires at least one package name"}

    already_present = []
    to_install = []

    for pkg in args:
        if _dpkg_check(pkg):
            already_present.append(pkg)
        else:
            to_install.append(pkg)

    installed = []
    failed = []
    if to_install:
        installed, failed = _apt_install(to_install)

    return {
        "installed": installed,
        "already_present": already_present,
        "failed": failed,
    }


def cmd_has(args):
    """Check if a capability is available."""
    if not args:
        return {"error": "has requires a name argument"}

    name = args[0]

    # Check dpkg first
    if _dpkg_check(name):
        return {
            "name": name,
            "available": True,
            "type": "system",
            "details": "installed via dpkg",
        }

    # Fall back to which
    path = shutil.which(name)
    if path:
        return {
            "name": name,
            "available": True,
            "type": "command",
            "details": f"found at {path}",
        }

    return {
        "name": name,
        "available": False,
        "type": "",
        "details": "not found",
    }


def cmd_list(args):
    """List installed packages via dpkg."""
    try:
        result = subprocess.run(
            ["dpkg", "--get-selections"],
            capture_output=True, text=True
        )
        if result.returncode != 0:
            return {"packages": [], "error": result.stderr.strip()}

        packages = []
        for line in result.stdout.strip().splitlines():
            parts = line.split()
            if parts:
                packages.append(parts[0])
        return {"packages": packages}
    except FileNotFoundError:
        return {"packages": [], "error": "dpkg not found"}


def run(command, args):
    """Entry point called by the aos router."""
    commands = {
        "need": cmd_need,
        "has": cmd_has,
        "list": cmd_list,
    }
    handler = commands.get(command)
    if handler is None:
        return {"error": f"unknown command: {command}"}
    return handler(args)
