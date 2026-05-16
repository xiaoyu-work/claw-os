"""launcher — find and launch installed desktop (GUI) apps.

This app is the AI-facing surface for "open this application". The agent
never spawns binaries directly; instead it picks a `.desktop` AppID
(`com.clawos.Files`, `org.mozilla.firefox`, …) and calls `launcher.open
<app_id>`. The kernel mediates the launch through `desktop.launch`,
which is scoped to the AppID — granting `desktop.launch *` lets the
agent open anything, while `desktop.launch name:com.clawos.*` is a
narrow first-party grant.

Why a `.desktop` AppID and not a binary path?

- The AppID is the stable identifier users see in approval dialogs and
  the panel. Binary names (`cosmic-files`, `cosmic-files-applet`,
  `cosmic-files-bin`) are too brittle and packaging-specific.
- The freedesktop spec for `Exec=` field codes (`%f`, `%u`, …) means
  argument injection is bounded by what the desktop entry declared.
  Extra args we pass through `gtk-launch` are URI/file substitutions,
  not arbitrary `-e bash -c …` payloads.
- Launches go through `gtk-launch` / `gio launch`, which fork the GUI
  into its own session — so closing the agent doesn't kill the window.
"""

from __future__ import annotations

import fcntl
import json
import os
import shlex
import shutil
import subprocess
from datetime import datetime, timezone

from cos_runtime import policy

DATA_DIR = os.environ.get("COS_DATA_DIR", "/var/lib/cos")
LAUNCHER_DIR = os.path.join(DATA_DIR, "launcher")
RECENT_PATH = os.path.join(LAUNCHER_DIR, "recent.jsonl")
RECENT_ROTATE_BYTES = 1_000_000
RECENT_KEEP_LINES = 500
DEFAULT_FIND_LIMIT = 10
DEFAULT_RECENT_LIMIT = 20


# ---------------------------------------------------------------------------
# XDG paths and locale chain
# ---------------------------------------------------------------------------


def _xdg_data_dirs():
    """Return the ordered list of `<dir>/applications` to scan.

    Order matches freedesktop precedence: user-local entries (highest
    priority) come first; entries with the same AppID later in the list
    are shadowed.
    """
    dirs = []

    data_home = os.environ.get("XDG_DATA_HOME") or os.path.join(
        os.path.expanduser("~"), ".local", "share"
    )
    dirs.append(os.path.join(data_home, "applications"))

    raw = os.environ.get("XDG_DATA_DIRS") or "/usr/local/share:/usr/share"
    for d in raw.split(":"):
        d = d.strip()
        if d:
            dirs.append(os.path.join(d, "applications"))

    seen = set()
    deduped = []
    for d in dirs:
        if d not in seen:
            seen.add(d)
            deduped.append(d)
    return deduped


def _locale_chain():
    """Return the locale fallback chain for `Name[lang]` lookups."""
    lang = (
        os.environ.get("LC_MESSAGES")
        or os.environ.get("LC_ALL")
        or os.environ.get("LANG")
        or ""
    )
    main = lang.split(".")[0]
    if main in ("", "C", "POSIX"):
        return []
    chain = [main]
    if "_" in main:
        chain.append(main.split("_", 1)[0])
    return chain


# ---------------------------------------------------------------------------
# .desktop file parsing
# ---------------------------------------------------------------------------


def _parse_desktop_file(path):
    """Parse a `.desktop` file. Returns the `[Desktop Entry]` key→value
    map, or ``None`` if the file is missing, unreadable, or not an
    Application entry.
    """
    try:
        with open(path, "r", encoding="utf-8", errors="replace") as f:
            content = f.read()
    except OSError:
        return None

    entries = {}
    in_main = False
    for raw in content.splitlines():
        line = raw.strip()
        if not line or line.startswith("#"):
            continue
        if line.startswith("[") and line.endswith("]"):
            in_main = line == "[Desktop Entry]"
            continue
        if not in_main or "=" not in line:
            continue
        key, value = line.split("=", 1)
        entries[key.strip()] = value.strip()

    if entries.get("Type", "Application") != "Application":
        return None
    return entries


def _localized(entries, key, locale_chain):
    """Return the best-matching `<key>[lang]` value, else the bare key."""
    for loc in locale_chain:
        v = entries.get(f"{key}[{loc}]")
        if v:
            return v
    return entries.get(key, "")


def _passes_visibility(entries, current_desktops, include_hidden, include_no_display):
    """Apply Hidden / NoDisplay / OnlyShowIn / NotShowIn filtering."""
    if not include_hidden and entries.get("Hidden", "").lower() == "true":
        return False
    if not include_no_display and entries.get("NoDisplay", "").lower() == "true":
        return False
    only = entries.get("OnlyShowIn", "")
    if only:
        wanted = {x for x in only.rstrip(";").split(";") if x}
        if wanted and not (wanted & current_desktops):
            return False
    not_show = entries.get("NotShowIn", "")
    if not_show:
        unwanted = {x for x in not_show.rstrip(";").split(";") if x}
        if unwanted & current_desktops:
            return False
    return True


def _app_id_from_relpath(rel):
    """freedesktop AppID: relative path under `applications/`, minus the
    `.desktop` suffix, with `/` replaced by `-`."""
    if rel.endswith(".desktop"):
        rel = rel[: -len(".desktop")]
    return rel.replace(os.sep, "-").replace("/", "-")


def _exec_binary(entries):
    """Return the basename of the first token of `Exec=`, or empty."""
    exec_line = entries.get("TryExec") or entries.get("Exec", "")
    exec_line = exec_line.strip()
    if not exec_line:
        return ""
    try:
        tokens = shlex.split(exec_line)
    except ValueError:
        tokens = exec_line.split()
    if not tokens:
        return ""
    return os.path.basename(tokens[0])


def _build_entry(applications_root, desktop_path, entries, locale_chain):
    rel = os.path.relpath(desktop_path, applications_root)
    return {
        "app_id": _app_id_from_relpath(rel),
        "name": _localized(entries, "Name", locale_chain),
        "generic_name": _localized(entries, "GenericName", locale_chain),
        "comment": _localized(entries, "Comment", locale_chain),
        "keywords": _localized(entries, "Keywords", locale_chain),
        "categories": entries.get("Categories", "").rstrip(";"),
        "icon": entries.get("Icon", ""),
        "exec": entries.get("Exec", ""),
        "exec_binary": _exec_binary(entries),
        "no_display": entries.get("NoDisplay", "").lower() == "true",
        "hidden": entries.get("Hidden", "").lower() == "true",
        "path": desktop_path,
        "terminal": entries.get("Terminal", "").lower() == "true",
    }


def _scan_apps(include_hidden=False, include_no_display=False, gate=True):
    """Walk every `applications/` directory and yield resolved app entries.

    Earlier directories (user-local) shadow later ones (system) for the
    same AppID, mirroring freedesktop precedence.
    """
    locale_chain = _locale_chain()
    current_desktop_raw = os.environ.get("XDG_CURRENT_DESKTOP", "")
    current_desktops = (
        {x for x in current_desktop_raw.split(":") if x} if current_desktop_raw else set()
    )

    out = {}
    for root in _xdg_data_dirs():
        if not os.path.isdir(root):
            continue
        if gate:
            try:
                policy.require("fs.read", path=root + "/")
            except policy.PermissionDenied:
                # A scope we declared in the manifest wasn't granted —
                # skip this directory rather than aborting the whole
                # scan, so a partial grant still works.
                continue
        for dirpath, _dirs, files in os.walk(root):
            for fname in files:
                if not fname.endswith(".desktop"):
                    continue
                full = os.path.join(dirpath, fname)
                parsed = _parse_desktop_file(full)
                if parsed is None:
                    continue
                if not _passes_visibility(
                    parsed, current_desktops, include_hidden, include_no_display
                ):
                    continue
                entry = _build_entry(root, full, parsed, locale_chain)
                if entry["app_id"] not in out:
                    out[entry["app_id"]] = entry
    return out


def _find_entry(app_id):
    """Look up a single AppID. Returns the resolved entry or ``None``."""
    apps = _scan_apps(include_hidden=True, include_no_display=True)
    return apps.get(app_id)


# ---------------------------------------------------------------------------
# Fuzzy ranking
# ---------------------------------------------------------------------------


def _tokenize(s):
    return {t for t in s.lower().replace("-", " ").replace("_", " ").split() if t}


def _score(query, entry):
    q = query.lower().strip()
    if not q:
        return 0
    q_tokens = _tokenize(q)
    fields = [
        (entry.get("name", ""), 10),
        (entry.get("app_id", ""), 8),
        (entry.get("generic_name", ""), 7),
        (entry.get("keywords", ""), 5),
        (entry.get("exec_binary", ""), 4),
        (entry.get("comment", ""), 3),
    ]
    score = 0
    for text, weight in fields:
        t = text.lower()
        if not t:
            continue
        if t == q:
            score += weight * 5
        elif t.startswith(q):
            score += weight * 3
        elif q in t:
            score += weight * 2
        else:
            t_tokens = _tokenize(t)
            overlap = len(q_tokens & t_tokens)
            if overlap:
                score += weight * overlap
    return score


# ---------------------------------------------------------------------------
# Launch
# ---------------------------------------------------------------------------


def _expand_exec_line(exec_line, extras):
    """Expand freedesktop `Exec=` field codes into a concrete argv.

    Drops deprecated codes (`%i %c %k %d %D %n %N %v %m`) per spec, and
    substitutes file/URI codes from ``extras``. Pure tokenisation — no
    shell metacharacter evaluation.
    """
    try:
        tokens = shlex.split(exec_line)
    except ValueError:
        tokens = exec_line.split()
    out = []
    for tok in tokens:
        if "%" not in tok:
            out.append(tok)
            continue
        if tok in ("%f", "%u"):
            if extras:
                out.append(extras[0])
            continue
        if tok in ("%F", "%U"):
            out.extend(extras)
            continue
        if tok in ("%i", "%c", "%k", "%d", "%D", "%n", "%N", "%v", "%m"):
            continue
        out.append(tok.replace("%%", "%"))
    return out


def _spawn_detached(argv):
    """Spawn `argv` fully detached from the agent's session.

    Returns ``(pid, error)``. Uses ``start_new_session=True`` so the
    launched GUI keeps running after the agent exits.
    """
    try:
        proc = subprocess.Popen(
            argv,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            start_new_session=True,
            close_fds=True,
        )
    except FileNotFoundError:
        return None, f"launcher binary not found: {argv[0]}"
    except OSError as exc:
        return None, f"spawn failed: {exc}"
    return proc.pid, None


def _launch(app_id, entry, extras):
    """Launch ``entry`` via gtk-launch → gio launch → direct fallback."""
    gtk_launch = shutil.which("gtk-launch")
    if gtk_launch:
        return _spawn_detached([gtk_launch, app_id, *extras])

    gio = shutil.which("gio")
    if gio:
        return _spawn_detached([gio, "launch", entry["path"], *extras])

    exec_line = entry.get("exec") or ""
    if not exec_line:
        return None, "no Exec= and no gtk-launch / gio installed"
    argv = _expand_exec_line(exec_line, extras)
    if not argv:
        return None, "empty argv after Exec= expansion"
    return _spawn_detached(argv)


# ---------------------------------------------------------------------------
# Recent log
# ---------------------------------------------------------------------------


def _now_iso():
    return datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def _with_recent_lock(fn):
    os.makedirs(LAUNCHER_DIR, exist_ok=True)
    lock_path = RECENT_PATH + ".lock"
    with open(lock_path, "w") as lock_fd:
        fcntl.flock(lock_fd, fcntl.LOCK_EX)
        try:
            return fn()
        finally:
            fcntl.flock(lock_fd, fcntl.LOCK_UN)


def _maybe_rotate_recent():
    try:
        size = os.path.getsize(RECENT_PATH)
    except OSError:
        return
    if size <= RECENT_ROTATE_BYTES:
        return
    try:
        with open(RECENT_PATH, "r", encoding="utf-8") as f:
            lines = f.readlines()
    except OSError:
        return
    kept = lines[-RECENT_KEEP_LINES:]
    tmp = RECENT_PATH + ".tmp"
    with open(tmp, "w", encoding="utf-8") as f:
        f.writelines(kept)
    os.replace(tmp, RECENT_PATH)


def _append_recent(record):
    def do_append():
        _maybe_rotate_recent()
        with open(RECENT_PATH, "a", encoding="utf-8") as f:
            f.write(json.dumps(record, ensure_ascii=False) + "\n")

    _with_recent_lock(do_append)


def _read_recent(limit):
    try:
        with open(RECENT_PATH, "r", encoding="utf-8") as f:
            lines = f.readlines()
    except OSError:
        return []
    seen = {}
    for line in reversed(lines):
        line = line.strip()
        if not line:
            continue
        try:
            rec = json.loads(line)
        except (json.JSONDecodeError, ValueError):
            continue
        app_id = rec.get("app_id")
        if not app_id:
            continue
        if app_id not in seen:
            seen[app_id] = {
                "app_id": app_id,
                "name": rec.get("name", ""),
                "last_launched_at": rec.get("ts", ""),
                "count": 1,
            }
            if len(seen) >= limit:
                break
        else:
            seen[app_id]["count"] += 1
    return list(seen.values())


# ---------------------------------------------------------------------------
# Process scanning for `is-running`
# ---------------------------------------------------------------------------


def _read_comm(pid):
    try:
        with open(f"/proc/{pid}/comm", "r", encoding="utf-8", errors="replace") as f:
            return f.read().strip()
    except OSError:
        return None


def _read_exe(pid):
    try:
        return os.readlink(f"/proc/{pid}/exe")
    except OSError:
        return None


def _pids_matching(binary_basename):
    """Return live PIDs whose comm or exe basename matches ``binary_basename``.

    The kernel truncates `/proc/<pid>/comm` at 15 characters, so we match
    against the truncated form too.
    """
    if not binary_basename:
        return []
    try:
        entries = os.listdir("/proc")
    except OSError:
        return []
    truncated = binary_basename[:15]
    found = []
    for name in entries:
        if not name.isdigit():
            continue
        pid = int(name)
        comm = _read_comm(pid)
        if comm and (comm == binary_basename or comm == truncated):
            found.append(pid)
            continue
        exe = _read_exe(pid)
        if exe and os.path.basename(exe) == binary_basename:
            found.append(pid)
    return found


# ---------------------------------------------------------------------------
# CLI argument parsing helpers
# ---------------------------------------------------------------------------


def _parse_flag_int(args, flag, default):
    """Extract ``--flag N`` from ``args``. Returns ``(value, remaining)``
    or ``(None, args)`` on a parse error."""
    remaining = []
    it = iter(args)
    value = default
    for arg in it:
        if arg == flag:
            try:
                value = int(next(it))
            except (StopIteration, ValueError):
                return None, args
        else:
            remaining.append(arg)
    return value, remaining


# ---------------------------------------------------------------------------
# Operations
# ---------------------------------------------------------------------------


def cmd_list(args):
    include_no_display = "--include-hidden" in args or "--include-no-display" in args
    include_hidden = "--include-hidden" in args
    apps = _scan_apps(
        include_hidden=include_hidden, include_no_display=include_no_display
    )
    entries = sorted(apps.values(), key=lambda e: e["name"].lower() or e["app_id"])
    return {"count": len(entries), "apps": entries}


def cmd_find(args):
    if not args:
        return {"error": "missing query"}
    limit, remaining = _parse_flag_int(args, "--limit", DEFAULT_FIND_LIMIT)
    if limit is None:
        return {"error": "invalid --limit value"}
    if not remaining:
        return {"error": "missing query"}
    query = " ".join(remaining)

    apps = _scan_apps(include_hidden=False, include_no_display=False)
    scored = []
    for entry in apps.values():
        s = _score(query, entry)
        if s > 0:
            scored.append((s, entry))
    scored.sort(key=lambda x: (-x[0], x[1]["name"].lower()))
    matches = [
        {**entry, "score": score} for score, entry in scored[: max(limit, 0)]
    ]
    return {"query": query, "count": len(matches), "matches": matches}


def cmd_open(args):
    if not args:
        return {"error": "missing app_id"}
    app_id = args[0]
    extras = list(args[1:])

    policy.require("desktop.launch", name=app_id)

    entry = _find_entry(app_id)
    if entry is None:
        return {
            "error": f"no installed app with AppID `{app_id}`",
            "hint": "use `cos app launcher find <query>` to discover AppIDs",
        }

    pid, err = _launch(app_id, entry, extras)
    if err:
        return {"app_id": app_id, "error": err}

    record = {
        "ts": _now_iso(),
        "app_id": app_id,
        "name": entry.get("name", ""),
        "extras": extras,
    }
    try:
        _append_recent(record)
    except OSError:
        # Recent log is best-effort — never fail the launch over it.
        pass

    return {
        "app_id": app_id,
        "name": entry.get("name", ""),
        "pid": pid,
        "launched_at": record["ts"],
        "exec_binary": entry.get("exec_binary", ""),
    }


def cmd_recent(args):
    limit, _ = _parse_flag_int(args, "--limit", DEFAULT_RECENT_LIMIT)
    if limit is None:
        return {"error": "invalid --limit value"}
    return {"recent": _read_recent(max(limit, 0))}


def cmd_is_running(args):
    if not args:
        return {"error": "missing app_id"}
    app_id = args[0]

    policy.require("proc.observe", wild=True)

    entry = _find_entry(app_id)
    if entry is None:
        return {
            "error": f"no installed app with AppID `{app_id}`",
            "hint": "use `cos app launcher find <query>` to discover AppIDs",
        }
    binary = entry.get("exec_binary", "")
    pids = _pids_matching(binary) if binary else []
    return {
        "app_id": app_id,
        "exec_binary": binary,
        "running": bool(pids),
        "pids": pids,
    }


# ---------------------------------------------------------------------------
# Schema + entry point
# ---------------------------------------------------------------------------


def _schema():
    return {
        "list": {
            "description": "Enumerate every installed desktop app (.desktop entry).",
            "parameters": [
                {
                    "name": "--include-no-display",
                    "type": "boolean",
                    "required": False,
                    "kind": "flag",
                    "default": False,
                    "description": "Include entries marked NoDisplay=true (helpers, autostart entries).",
                },
                {
                    "name": "--include-hidden",
                    "type": "boolean",
                    "required": False,
                    "kind": "flag",
                    "default": False,
                    "description": "Include entries marked Hidden=true (almost never useful).",
                },
            ],
            "example": "cos app launcher list",
        },
        "find": {
            "description": "Fuzzy-search installed apps by name, keyword, or AppID.",
            "parameters": [
                {
                    "name": "query",
                    "type": "string",
                    "required": True,
                    "kind": "positional",
                    "description": "Search term (e.g. `files`, `terminal`, `firefox`).",
                },
                {
                    "name": "--limit",
                    "type": "integer",
                    "required": False,
                    "kind": "flag",
                    "default": DEFAULT_FIND_LIMIT,
                    "description": "Maximum number of matches to return.",
                },
            ],
            "example": "cos app launcher find files",
        },
        "open": {
            "description": "Launch a desktop app by its freedesktop AppID.",
            "parameters": [
                {
                    "name": "app_id",
                    "type": "string",
                    "required": True,
                    "kind": "positional",
                    "description": "The AppID — `.desktop` filename without extension (e.g. com.clawos.Files).",
                },
                {
                    "name": "uri_or_path",
                    "type": "string",
                    "required": False,
                    "kind": "positional",
                    "description": "Optional URIs/paths to pass to the app via Exec= field-code substitution.",
                },
            ],
            "example": "cos app launcher open com.clawos.Files",
        },
        "recent": {
            "description": "List desktop apps the agent has recently launched.",
            "parameters": [
                {
                    "name": "--limit",
                    "type": "integer",
                    "required": False,
                    "kind": "flag",
                    "default": DEFAULT_RECENT_LIMIT,
                    "description": "Maximum number of distinct AppIDs to return.",
                },
            ],
            "example": "cos app launcher recent --limit 5",
        },
        "is-running": {
            "description": "Check whether a desktop app's process is alive.",
            "parameters": [
                {
                    "name": "app_id",
                    "type": "string",
                    "required": True,
                    "kind": "positional",
                    "description": "The AppID to look up.",
                },
            ],
            "example": "cos app launcher is-running com.clawos.Files",
        },
    }


def run(command, args):
    """Entry point called by cos."""
    if command == "__schema__":
        return _schema()
    handlers = {
        "list": cmd_list,
        "find": cmd_find,
        "open": cmd_open,
        "recent": cmd_recent,
        "is-running": cmd_is_running,
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
