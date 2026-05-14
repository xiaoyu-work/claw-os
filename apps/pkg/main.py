"""pkg — Declarative capability management for Claw OS.

Say what you need, not how to install it.
"""

import shutil
import subprocess

from _lib import policy


DEFAULT_SEARCH_LIMIT = 25
MAX_SEARCH_LIMIT = 100


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

    for pkg in args:
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
    policy.require("sys.package", name=name)

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
    limit = DEFAULT_SEARCH_LIMIT
    query_parts = []
    i = 0
    while i < len(args):
        tok = args[i]
        if tok in ("--limit", "-n"):
            if i + 1 >= len(args):
                raise ValueError(f"{tok} requires a value")
            try:
                limit = int(args[i + 1])
            except ValueError as exc:
                raise ValueError(f"{tok} expects an integer, got {args[i + 1]!r}") from exc
            i += 2
            continue
        if tok.startswith("--limit="):
            try:
                limit = int(tok.split("=", 1)[1])
            except ValueError as exc:
                raise ValueError(f"--limit expects an integer, got {tok!r}") from exc
            i += 1
            continue
        query_parts.append(tok)
        i += 1
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

    policy.require("sys.package", wild=True)

    try:
        result = subprocess.run(
            ["apt-cache", "search", "--names-only", query],
            capture_output=True, text=True
        )
    except FileNotFoundError:
        return {"results": [], "error": "apt-cache not found"}

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
    policy.require("sys.package", name=name)

    try:
        result = subprocess.run(
            ["apt-cache", "show", name],
            capture_output=True, text=True
        )
    except FileNotFoundError:
        return {"name": name, "found": False, "error": "apt-cache not found"}

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
    policy.require("sys.package", wild=True)
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


def _schema():
    return {
        "need": {
            "description": "Ensure packages are installed, only installing what is missing",
            "parameters": [
                {"name": "packages", "type": "string", "required": True, "description": "One or more package names to ensure are installed", "kind": "positional"},
            ],
            "example": "cos app pkg need curl jq ripgrep",
        },
        "has": {
            "description": "Check if a package or command is available on the system",
            "parameters": [
                {"name": "name", "type": "string", "required": True, "description": "Package or command name to check", "kind": "positional"},
            ],
            "example": "cos app pkg has python3",
        },
        "list": {
            "description": "List all installed system packages via dpkg",
            "parameters": [],
            "example": "cos app pkg list",
        },
        "search": {
            "description": "Search the apt catalog for packages whose name or description matches the query",
            "parameters": [
                {"name": "query", "type": "string", "required": True, "description": "Search term(s) to match against package names and short summaries", "kind": "positional"},
                {"name": "--limit", "type": "int", "required": False, "description": f"Max results to return (default {DEFAULT_SEARCH_LIMIT}, capped at {MAX_SEARCH_LIMIT})", "kind": "flag"},
            ],
            "example": "cos app pkg search image converter --limit 10",
        },
        "show": {
            "description": "Show detailed metadata (version, description, homepage, depends, size) for a single package from the apt catalog",
            "parameters": [
                {"name": "name", "type": "string", "required": True, "description": "Package name to describe", "kind": "positional"},
            ],
            "example": "cos app pkg show imagemagick",
        },
    }


def run(command, args):
    """Entry point called by the cos router."""
    if command == "__schema__":
        return _schema()
    commands = {
        "need": cmd_need,
        "has": cmd_has,
        "list": cmd_list,
        "search": cmd_search,
        "show": cmd_show,
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
