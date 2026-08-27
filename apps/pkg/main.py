"""pkg — Declarative capability management for Claw OS.

Say what you need, not how to install it.
"""

import json
import os
import re
import shutil
import subprocess
import sys

# Pull in scrub_env so the apt-* / dpkg children we shell out to don't
# inherit OPENAI_API_KEY / GITHUB_TOKEN / etc.
sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from _shared.env_scrub import scrub_env  # noqa: E402

from cos_runtime import policy  # noqa: E402


DEFAULT_SEARCH_LIMIT = 25
MAX_SEARCH_LIMIT = 100

# Per-command upper bounds for spawned apt/dpkg children. ``apt-get
# install`` can be genuinely slow (mirror, large download); the read
# helpers should be near-instant.
QUERY_TIMEOUT_SECS = 60
INSTALL_TIMEOUT_SECS = int(os.environ.get("CLAW_PKG_INSTALL_TIMEOUT", "900"))
PACKAGE_NAME_RE = re.compile(
    r"^[a-z0-9][a-z0-9+.-]*(?::[a-z0-9][a-z0-9-]*)?$"
)


def _valid_package_name(package):
    return (
        isinstance(package, str)
        and len(package) <= 255
        and PACKAGE_NAME_RE.fullmatch(package) is not None
    )


def _cos_binary():
    explicit = os.environ.get("COS_BIN")
    if explicit:
        return explicit
    return shutil.which("cos")


def _dpkg_check(package):
    """Return True if a package is installed according to dpkg."""
    try:
        result = subprocess.run(
            ["dpkg", "-s", package],
            capture_output=True,
            text=True,
            timeout=QUERY_TIMEOUT_SECS,
            stdin=subprocess.DEVNULL,
            env=scrub_env(),
            check=False,
        )
        return result.returncode == 0
    except (FileNotFoundError, subprocess.TimeoutExpired):
        return False


def _package_broker(action, package=None, version=None):
    """Run one privileged package action through clawd."""
    cos_bin = _cos_binary()
    if not cos_bin:
        return {"error": "cos binary not found; privileged package broker unavailable"}
    argv = [cos_bin, "__package", action]
    if package is not None:
        argv.append(package)
    if version is not None:
        argv.append(version)
    try:
        result = subprocess.run(
            argv,
            capture_output=True,
            text=True,
            timeout=INSTALL_TIMEOUT_SECS,
            stdin=subprocess.DEVNULL,
            env=scrub_env(),
            check=False,
        )
    except PermissionError as exc:
        return {"error": str(exc)}
    except FileNotFoundError as exc:
        return {"error": str(exc)}
    except subprocess.TimeoutExpired:
        return {"error": f"package broker exceeded {INSTALL_TIMEOUT_SECS}s"}

    payload_text = (result.stdout or "").strip() or (result.stderr or "").strip()
    try:
        payload = json.loads(payload_text) if payload_text else {}
    except json.JSONDecodeError:
        return {"error": "package broker returned invalid JSON"}
    if result.returncode != 0 and "error" not in payload:
        payload["error"] = (
            payload.get("stderr_tail")
            or f"package broker exited {result.returncode}"
        )
    return payload


def _apt_install(packages):
    """Install packages through clawd. Returns installed, failed, errors."""
    installed = []
    failed = []
    errors = {}
    for pkg in packages:
        if not _valid_package_name(pkg):
            failed.append(pkg)
            errors[pkg] = "invalid Debian package name"
            continue
        payload = _package_broker("install", pkg)
        if payload.get("error"):
            failed.append(pkg)
            errors[pkg] = payload["error"]
        elif payload.get("after", {}).get("installed") is True:
            installed.append(pkg)
        else:
            failed.append(pkg)
            errors[pkg] = payload.get("stderr_tail") or "package was not installed"
    return installed, failed, errors


def cmd_need(args):
    """Ensure packages are installed, only installing what's missing."""
    if not args:
        return {"error": "need requires at least one package name"}

    for pkg in args:
        if not _valid_package_name(pkg):
            return {"error": f"invalid Debian package name: {pkg!r}"}
        policy.require("sys.package", name=pkg)

    already_present = []
    to_install = []

    for pkg in args:
        if _dpkg_check(pkg):
            already_present.append(pkg)
        else:
            to_install.append(pkg)

    installed = []
    failed = []
    errors = {}
    if to_install:
        installed, failed, errors = _apt_install(to_install)

    response = {
        "installed": installed,
        "already_present": already_present,
        "failed": failed,
    }
    if errors:
        response["errors"] = errors
    return response


def cmd_has(args):
    """Check if a capability is available."""
    if not args:
        return {"error": "has requires a name argument"}

    name = args[0]
    if not name or name.startswith("-") or "/" in name or "\x00" in name:
        return {"error": f"invalid package or command name: {name!r}"}
    policy.require("sys.observe", name=name)

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


def _parse_search_args(args):
    """Pull out --limit / -n and return (query, limit)."""
    from canonical_argv import parse_canonical_argv
    query_parts, options = parse_canonical_argv(args, value_flags={"limit"})
    limit = int(options.get("limit", DEFAULT_SEARCH_LIMIT))
    if limit <= 0:
        raise ValueError("--limit must be positive")
    if limit > MAX_SEARCH_LIMIT:
        limit = MAX_SEARCH_LIMIT
    return " ".join(query_parts).strip(), limit


def cmd_search(args):
    """Search the apt catalog for packages whose name or description
    matches the query. Read-only browse over the catalog the system
    already has indexed — use this when the user asks "what can I
    install to do X?" Pair with `show` for details and `need` to
    install.
    """
    if not args:
        return {"error": "search requires a query"}

    try:
        query, limit = _parse_search_args(args)
    except ValueError as exc:
        return {"error": str(exc)}

    if not query:
        return {"error": "search requires a query"}

    policy.require("sys.observe", name="packages")

    try:
        result = subprocess.run(
            ["apt-cache", "search", "--names-only", query],
            capture_output=True,
            text=True,
            timeout=QUERY_TIMEOUT_SECS,
            stdin=subprocess.DEVNULL,
            env=scrub_env(),
            check=False,
        )
    except FileNotFoundError:
        return {"results": [], "error": "apt-cache not found"}
    except subprocess.TimeoutExpired:
        return {"results": [], "error": f"apt-cache search exceeded {QUERY_TIMEOUT_SECS}s"}

    if result.returncode != 0:
        return {"results": [], "error": result.stderr.strip() or "apt-cache search failed"}

    results = []
    for line in result.stdout.splitlines():
        line = line.rstrip()
        if not line:
            continue
        name, sep, summary = line.partition(" - ")
        if not sep:
            results.append({"name": line.strip(), "summary": ""})
        else:
            results.append({"name": name.strip(), "summary": summary.strip()})

    truncated = len(results) > limit
    if truncated:
        results = results[:limit]

    response = {"query": query, "results": results, "count": len(results)}
    if truncated:
        response["truncated"] = True
        response["hint"] = f"showing first {limit} results; pass --limit N (max {MAX_SEARCH_LIMIT}) to widen"
    return response


def _parse_apt_show(stdout):
    """Parse the first stanza of `apt-cache show` output (RFC822-ish).

    apt-cache prints one stanza per available version separated by
    blank lines; we surface only the first since that's what the
    apt resolver would pick by default.
    """
    fields = {}
    current_key = None
    for raw in stdout.splitlines():
        if not raw.strip():
            if fields:
                break
            continue
        if raw[0] in (" ", "\t"):
            if current_key:
                fields[current_key] = (fields[current_key] + "\n" + raw.strip()).strip()
            continue
        key, sep, value = raw.partition(":")
        if not sep:
            continue
        current_key = key.strip()
        fields[current_key] = value.strip()
    return fields


def cmd_show(args):
    """Show detailed metadata for a single package from the apt
    catalog: version, summary, full description, homepage,
    section, size, depends. Use after `search` to vet a candidate
    before `need`.
    """
    if not args:
        return {"error": "show requires a package name"}

    name = args[0]
    if not _valid_package_name(name):
        return {"name": name, "found": False, "error": "invalid Debian package name"}
    policy.require("sys.observe", name=name)

    try:
        result = subprocess.run(
            ["apt-cache", "show", name],
            capture_output=True,
            text=True,
            timeout=QUERY_TIMEOUT_SECS,
            stdin=subprocess.DEVNULL,
            env=scrub_env(),
            check=False,
        )
    except FileNotFoundError:
        return {"name": name, "found": False, "error": "apt-cache not found"}
    except subprocess.TimeoutExpired:
        return {"name": name, "found": False, "error": f"apt-cache show exceeded {QUERY_TIMEOUT_SECS}s"}

    if result.returncode != 0 or not result.stdout.strip():
        return {
            "name": name,
            "found": False,
            "error": result.stderr.strip() or "no package found",
        }

    fields = _parse_apt_show(result.stdout)
    if not fields:
        return {"name": name, "found": False, "error": "no package found"}

    description = fields.get("Description", "")
    summary = description.splitlines()[0] if description else ""
    long_description = "\n".join(description.splitlines()[1:]).strip() if description else ""

    return {
        "name": fields.get("Package", name),
        "found": True,
        "version": fields.get("Version", ""),
        "summary": summary,
        "description": long_description,
        "section": fields.get("Section", ""),
        "homepage": fields.get("Homepage", ""),
        "depends": fields.get("Depends", ""),
        "installed_size": fields.get("Installed-Size", ""),
        "maintainer": fields.get("Maintainer", ""),
    }


def cmd_list(args):
    """List installed packages via dpkg."""
    policy.require("sys.observe", name="packages")
    try:
        result = subprocess.run(
            ["dpkg", "--get-selections"],
            capture_output=True,
            text=True,
            timeout=QUERY_TIMEOUT_SECS,
            stdin=subprocess.DEVNULL,
            env=scrub_env(),
            check=False,
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
    except subprocess.TimeoutExpired:
        return {"packages": [], "error": f"dpkg --get-selections exceeded {QUERY_TIMEOUT_SECS}s"}


def _single_package_action(command, args):
    if len(args) != 1 or not _valid_package_name(args[0]):
        return {"error": f"{command} requires one valid Debian package name"}
    package = args[0]
    policy.require("sys.package", name=package)
    return _package_broker(command, package)


def cmd_install_version(args):
    if len(args) != 2 or not _valid_package_name(args[0]):
        return {"error": "install-version requires <package> <version>"}
    package, version = args
    if not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9.:+~-]{0,254}", version):
        return {"error": f"invalid Debian package version: {version!r}"}
    policy.require("sys.package", name=package)
    return _package_broker("install-version", package, version)


def cmd_update(args):
    if args:
        return {"error": "update takes no arguments"}
    policy.require("sys.package", wild=True)
    return _package_broker("update-index")


def cmd_upgrade_all(args):
    if args:
        return {"error": "upgrade-all takes no arguments"}
    policy.require("sys.package", wild=True)
    return _package_broker("upgrade-all")


def run(command, args):
    """Entry point called by the cos router."""
    if command != "search":
        from canonical_argv import parse_canonical_argv
        try:
            args, _ = parse_canonical_argv(args)
        except ValueError as error:
            return {"error": str(error)}
    commands = {
        "need": cmd_need,
        "has": cmd_has,
        "list": cmd_list,
        "search": cmd_search,
        "show": cmd_show,
        "remove": lambda values: _single_package_action("remove", values),
        "purge": lambda values: _single_package_action("purge", values),
        "upgrade": lambda values: _single_package_action("upgrade", values),
        "hold": lambda values: _single_package_action("hold", values),
        "unhold": lambda values: _single_package_action("unhold", values),
        "install-version": cmd_install_version,
        "update": cmd_update,
        "upgrade-all": cmd_upgrade_all,
    }
    handler = commands.get(command)
    if handler is None:
        return {"error": f"unknown command: {command}"}
    try:
        return handler(args)
    except policy.PermissionDenied as denied:
        return {"error": str(denied), "denial": denied.denial}
    except policy.PolicyUnavailable as exc:
        return {"error": f"capability check failed: {exc}"}
