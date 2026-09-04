"""Find, inspect, and broker launches for installed desktop applications."""

from __future__ import annotations

import fcntl
import json
import os
import pathlib
import re
import shlex
import shutil
import subprocess
import sys
import unicodedata
from collections.abc import Callable
from datetime import datetime, timezone
from typing import TypeVar

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from _shared.atomic import atomic_write_bytes  # noqa: E402
from _shared.env_scrub import scrub_env  # noqa: E402

from cos_runtime import policy  # noqa: E402


DesktopEntry = dict[str, object]
_T = TypeVar("_T")

DATA_DIR = os.environ.get("COS_DATA_DIR", "/var/lib/cos")
LAUNCHER_DIR = os.path.join(DATA_DIR, "launcher")
RECENT_PATH = os.path.join(LAUNCHER_DIR, "recent.jsonl")
RECENT_ROTATE_BYTES = 1_000_000
RECENT_KEEP_LINES = 500
DEFAULT_FIND_LIMIT = 10
DEFAULT_RECENT_LIMIT = 20
MAX_URI_COUNT = 32
MAX_URI_BYTES = 4096
BROKER_TIMEOUT_SECS = int(os.environ.get("CLAW_LAUNCHER_BROKER_TIMEOUT", "120"))
APP_ID_RE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._-]{0,254}$")
URI_SCHEME_RE = re.compile(r"^[A-Za-z][A-Za-z0-9+.-]*:")


def _xdg_data_dirs() -> list[str]:
    """Return application directories in freedesktop shadowing order."""
    dirs: list[str] = []
    owner_home = os.environ.get("COS_OWNER_HOME")
    data_home = os.environ.get("XDG_DATA_HOME")
    if not data_home:
        data_home = os.path.join(
            owner_home or os.path.expanduser("~"),
            ".local",
            "share",
        )
    dirs.append(os.path.join(data_home, "applications"))

    raw = os.environ.get("XDG_DATA_DIRS") or "/usr/local/share:/usr/share"
    for directory in raw.split(":"):
        directory = directory.strip()
        if directory:
            dirs.append(os.path.join(directory, "applications"))

    seen: set[str] = set()
    deduped: list[str] = []
    for directory in dirs:
        if directory not in seen:
            seen.add(directory)
            deduped.append(directory)
    return deduped


def _locale_chain() -> list[str]:
    """Return the locale fallback chain used for localized desktop fields."""
    lang = (
        os.environ.get("LC_MESSAGES")
        or os.environ.get("LC_ALL")
        or os.environ.get("LANG")
        or ""
    )
    primary = lang.split(".")[0]
    if primary in ("", "C", "POSIX"):
        return []
    chain = [primary]
    if "_" in primary:
        chain.append(primary.split("_", 1)[0])
    return chain


def _parse_desktop_file(path: str) -> dict[str, str] | None:
    """Read one `[Desktop Entry]` section."""
    try:
        with open(path, "r", encoding="utf-8", errors="replace") as file:
            content = file.read()
    except OSError:
        return None

    entries: dict[str, str] = {}
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


def _localized(
    entries: dict[str, str],
    key: str,
    locale_chain: list[str],
) -> str:
    for locale in locale_chain:
        value = entries.get(f"{key}[{locale}]")
        if value:
            return value
    return entries.get(key, "")


def _passes_visibility(
    entries: dict[str, str],
    current_desktops: set[str],
    include_hidden: bool,
    include_no_display: bool,
) -> bool:
    if not include_hidden and entries.get("Hidden", "").lower() == "true":
        return False
    if not include_no_display and entries.get("NoDisplay", "").lower() == "true":
        return False
    only = entries.get("OnlyShowIn", "")
    if only:
        wanted = {item for item in only.rstrip(";").split(";") if item}
        if wanted and not (wanted & current_desktops):
            return False
    not_show = entries.get("NotShowIn", "")
    if not_show:
        unwanted = {item for item in not_show.rstrip(";").split(";") if item}
        if unwanted & current_desktops:
            return False
    return True


def _app_id_from_relpath(relative: str) -> str:
    if relative.endswith(".desktop"):
        relative = relative[: -len(".desktop")]
    return relative.replace(os.sep, "-").replace("/", "-")


def _exec_binary(entries: dict[str, str]) -> str:
    exec_line = (entries.get("TryExec") or entries.get("Exec", "")).strip()
    if not exec_line:
        return ""
    try:
        tokens = shlex.split(exec_line)
    except ValueError:
        tokens = exec_line.split()
    return os.path.basename(tokens[0]) if tokens else ""


def _build_entry(
    applications_root: str,
    desktop_path: str,
    entries: dict[str, str],
    locale_chain: list[str],
) -> DesktopEntry:
    relative = os.path.relpath(desktop_path, applications_root)
    return {
        "app_id": _app_id_from_relpath(relative),
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


def _scan_apps(
    include_hidden: bool = False,
    include_no_display: bool = False,
    gate: bool = True,
) -> dict[str, DesktopEntry]:
    locale_chain = _locale_chain()
    current_desktop_raw = os.environ.get("XDG_CURRENT_DESKTOP") or "COSMIC"
    current_desktops = {
        item for item in current_desktop_raw.split(":") if item
    }

    apps: dict[str, DesktopEntry] = {}
    for root in _xdg_data_dirs():
        if not os.path.isdir(root):
            continue
        if gate:
            policy.require("fs.read", path=root + "/")
        for dirpath, _dirs, files in os.walk(root):
            for filename in files:
                if not filename.endswith(".desktop"):
                    continue
                desktop_path = os.path.join(dirpath, filename)
                parsed = _parse_desktop_file(desktop_path)
                if parsed is None or not _passes_visibility(
                    parsed,
                    current_desktops,
                    include_hidden,
                    include_no_display,
                ):
                    continue
                entry = _build_entry(root, desktop_path, parsed, locale_chain)
                app_id = _entry_text(entry, "app_id")
                if app_id and app_id not in apps:
                    apps[app_id] = entry
    return apps


def _find_entry(app_id: str) -> DesktopEntry | None:
    return _scan_apps(include_hidden=True, include_no_display=True).get(app_id)


def _entry_text(entry: DesktopEntry, key: str) -> str:
    value = entry.get(key)
    return value if isinstance(value, str) else ""


def _tokenize(value: str) -> set[str]:
    return {
        token
        for token in value.lower().replace("-", " ").replace("_", " ").split()
        if token
    }


def _score(query: str, entry: DesktopEntry) -> int:
    normalized = query.lower().strip()
    if not normalized:
        return 0
    query_tokens = _tokenize(normalized)
    fields = [
        (_entry_text(entry, "name"), 10),
        (_entry_text(entry, "app_id"), 8),
        (_entry_text(entry, "generic_name"), 7),
        (_entry_text(entry, "keywords"), 5),
        (_entry_text(entry, "exec_binary"), 4),
        (_entry_text(entry, "comment"), 3),
    ]
    score = 0
    for text, weight in fields:
        candidate = text.lower()
        if not candidate:
            continue
        if candidate == normalized:
            score += weight * 5
        elif candidate.startswith(normalized):
            score += weight * 3
        elif normalized in candidate:
            score += weight * 2
        else:
            overlap = len(query_tokens & _tokenize(candidate))
            if overlap:
                score += weight * overlap
    return score


def _contains_control(value: str) -> bool:
    return any(unicodedata.category(character) == "Cc" for character in value)


def _validate_bool(value: object, name: str) -> bool:
    if type(value) is not bool:
        raise ValueError(f"{name} must be a boolean")
    return value


def _validate_limit(value: object, name: str) -> int:
    if type(value) is not int:
        raise ValueError(f"{name} must be an integer")
    return max(value, 0)


def _validate_query(value: object) -> str:
    if type(value) is not str or not value.strip() or _contains_control(value):
        raise ValueError("query must be non-empty text without control characters")
    return value


def _validate_app_id(value: object) -> str:
    if type(value) is not str or APP_ID_RE.fullmatch(value) is None:
        raise ValueError("app_id must be an exact desktop AppID")
    return value


def _validate_string_list(value: object | None, name: str) -> list[str]:
    if value is None:
        return []
    if type(value) is not list:
        raise ValueError(f"{name} must be a list")
    values: list[str] = []
    for item in value:
        if (
            type(item) is not str
            or not item
            or len(item.encode("utf-8")) > MAX_URI_BYTES
            or _contains_control(item)
        ):
            raise ValueError(
                f"{name} values must be non-empty text of at most "
                f"{MAX_URI_BYTES} bytes without control characters"
            )
        values.append(item)
    return values


def _validate_launch_targets(
    uri: object | None,
    path: object | None,
) -> tuple[list[str], list[str]]:
    uris = _validate_string_list(uri, "uri")
    paths = _validate_string_list(path, "path")
    if len(uris) + len(paths) > MAX_URI_COUNT:
        raise ValueError(
            f"uri and path accept at most {MAX_URI_COUNT} values in total"
        )
    for value in uris:
        if URI_SCHEME_RE.match(value) is None:
            raise ValueError("uri values must be absolute URIs")
        if value.split(":", 1)[0].lower() == "file":
            raise ValueError("file URIs are not accepted; use path")

    canonical_paths: list[str] = []
    for value in paths:
        if not os.path.isabs(value):
            raise ValueError("path values must be absolute")
        normalized = os.path.normpath(value)
        if normalized != value:
            raise ValueError("path values must already be canonical")
        canonical = os.path.realpath(normalized)
        if canonical != normalized:
            raise ValueError("path values must not contain symbolic links")
        if not os.path.isfile(canonical):
            raise ValueError("path values must name existing regular files")
        file_uri = pathlib.Path(canonical).as_uri()
        if len(file_uri.encode("utf-8")) > MAX_URI_BYTES:
            raise ValueError(
                f"path values must encode as URIs of at most {MAX_URI_BYTES} bytes"
            )
        canonical_paths.append(canonical)

    uris.extend(pathlib.Path(value).as_uri() for value in canonical_paths)
    return uris, canonical_paths


def _cos_binary() -> str:
    cos_binary = os.environ.get("COS_BIN") or shutil.which("cos")
    if not cos_binary:
        raise FileNotFoundError("cos binary not found; Launcher broker unavailable")
    return cos_binary


def _parse_broker_payload(payload_text: str) -> dict[str, object]:
    if not payload_text:
        raise RuntimeError("Launcher broker returned no JSON result")
    try:
        payload = json.loads(payload_text)
    except json.JSONDecodeError as exc:
        raise RuntimeError("Launcher broker returned invalid JSON") from exc
    if not isinstance(payload, dict):
        raise RuntimeError("Launcher broker returned a non-object result")
    return payload


def _broker_launch(app_id: str, uris: list[str]) -> dict[str, object]:
    argv = [_cos_binary(), "__desktop", "launch", "--app-id", app_id]
    for uri in uris:
        argv.extend(["--uri", uri])
    try:
        completed = subprocess.run(
            argv,
            capture_output=True,
            text=True,
            timeout=BROKER_TIMEOUT_SECS,
            stdin=subprocess.DEVNULL,
            env=scrub_env(),
            check=False,
        )
    except FileNotFoundError as exc:
        raise FileNotFoundError(
            f"Launcher broker executable not found: {argv[0]}"
        ) from exc
    except PermissionError as exc:
        raise PermissionError(
            f"permission denied launching Launcher broker: {argv[0]}"
        ) from exc
    except subprocess.TimeoutExpired as exc:
        raise TimeoutError(
            f"Launcher broker exceeded {BROKER_TIMEOUT_SECS}s"
        ) from exc

    payload_text = (completed.stdout or "").strip()
    if not payload_text:
        payload_text = (completed.stderr or "").strip()
    payload = _parse_broker_payload(payload_text)
    if "error" in payload:
        error = payload["error"]
        if not isinstance(error, str) or not error:
            raise RuntimeError("Launcher broker returned an invalid error payload")
        raise RuntimeError(error)
    if completed.returncode != 0:
        raise RuntimeError(f"Launcher broker exited {completed.returncode}")
    if set(payload) != {"launched", "app_id", "launcher"}:
        raise RuntimeError("Launcher broker returned an invalid launch result")
    if payload["launched"] is not True:
        raise RuntimeError("Launcher broker did not confirm the launch")
    if payload["app_id"] != app_id:
        raise RuntimeError("Launcher broker returned the wrong app_id")
    if not isinstance(payload["launcher"], str) or not payload["launcher"]:
        raise RuntimeError("Launcher broker returned an invalid launcher")
    return payload


def _now_iso() -> str:
    return datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def _with_recent_lock(function: Callable[[], _T]) -> _T:
    os.makedirs(LAUNCHER_DIR, exist_ok=True)
    lock_path = RECENT_PATH + ".lock"
    with open(lock_path, "w", encoding="utf-8") as lock_file:
        fcntl.flock(lock_file, fcntl.LOCK_EX)
        try:
            return function()
        finally:
            fcntl.flock(lock_file, fcntl.LOCK_UN)


def _read_recent_lines_unlocked() -> list[str]:
    try:
        with open(RECENT_PATH, "r", encoding="utf-8") as file:
            return file.readlines()
    except FileNotFoundError:
        return []


def _decode_recent_lines(lines: list[str]) -> list[dict[str, object]]:
    records: list[dict[str, object]] = []
    for line_number, raw in enumerate(lines, start=1):
        line = raw.strip()
        if not line:
            continue
        try:
            record = json.loads(line)
        except json.JSONDecodeError as exc:
            raise ValueError(
                f"recent state is corrupt at line {line_number}: invalid JSON"
            ) from exc
        if not isinstance(record, dict):
            raise ValueError(
                f"recent state is corrupt at line {line_number}: record is not an object"
            )
        app_id = record.get("app_id")
        timestamp = record.get("ts")
        name = record.get("name")
        if (
            type(app_id) is not str
            or APP_ID_RE.fullmatch(app_id) is None
            or type(timestamp) is not str
            or type(name) is not str
        ):
            raise ValueError(
                f"recent state is corrupt at line {line_number}: invalid record"
            )
        records.append(record)
    return records


def _append_recent(record: dict[str, object]) -> None:
    def append() -> None:
        lines = _read_recent_lines_unlocked()
        _decode_recent_lines(lines)
        size = sum(len(line.encode("utf-8")) for line in lines)
        if size > RECENT_ROTATE_BYTES:
            lines = lines[-RECENT_KEEP_LINES:]
        existing = "".join(lines)
        if existing and not existing.endswith("\n"):
            existing += "\n"
        payload = existing + json.dumps(record, ensure_ascii=False) + "\n"
        atomic_write_bytes(RECENT_PATH, payload.encode("utf-8"))

    _with_recent_lock(append)


def _read_recent(limit: int) -> list[dict[str, object]]:
    def read() -> list[dict[str, object]]:
        records = _decode_recent_lines(_read_recent_lines_unlocked())
        seen: dict[str, dict[str, object]] = {}
        for record in reversed(records):
            app_id = record["app_id"]
            assert isinstance(app_id, str)
            if app_id not in seen:
                seen[app_id] = {
                    "app_id": app_id,
                    "name": record["name"],
                    "last_launched_at": record["ts"],
                    "count": 1,
                }
                if len(seen) >= limit and limit > 0:
                    break
            else:
                count = seen[app_id]["count"]
                assert isinstance(count, int)
                seen[app_id]["count"] = count + 1
        return list(seen.values()) if limit > 0 else []

    return _with_recent_lock(read)


def _read_comm(pid: int) -> str | None:
    try:
        with open(
            f"/proc/{pid}/comm",
            "r",
            encoding="utf-8",
            errors="replace",
        ) as file:
            return file.read().strip()
    except OSError:
        return None


def _read_exe(pid: int) -> str | None:
    try:
        return os.readlink(f"/proc/{pid}/exe")
    except OSError:
        return None


def _pids_matching(binary_basename: str) -> list[int]:
    if not binary_basename:
        return []
    try:
        entries = os.listdir("/proc")
    except OSError:
        return []
    truncated = binary_basename[:15]
    found: list[int] = []
    for name in entries:
        if not name.isdigit():
            continue
        pid = int(name)
        comm = _read_comm(pid)
        if comm and (comm == binary_basename or comm == truncated):
            found.append(pid)
            continue
        executable = _read_exe(pid)
        if executable and os.path.basename(executable) == binary_basename:
            found.append(pid)
    return found


def list_apps(
    include_no_display: bool = False,
    include_hidden: bool = False,
) -> dict[str, object]:
    include_no_display = _validate_bool(include_no_display, "include_no_display")
    include_hidden = _validate_bool(include_hidden, "include_hidden")
    apps = _scan_apps(
        include_hidden=include_hidden,
        include_no_display=include_hidden or include_no_display,
    )
    entries = sorted(
        apps.values(),
        key=lambda entry: _entry_text(entry, "name").lower()
        or _entry_text(entry, "app_id"),
    )
    return {"count": len(entries), "apps": entries}


def find(query: str, limit: int = DEFAULT_FIND_LIMIT) -> dict[str, object]:
    query = _validate_query(query)
    limit = _validate_limit(limit, "limit")
    apps = _scan_apps()
    scored: list[tuple[int, DesktopEntry]] = []
    for entry in apps.values():
        score = _score(query, entry)
        if score > 0:
            scored.append((score, entry))
    scored.sort(key=lambda item: (-item[0], _entry_text(item[1], "name").lower()))
    matches = [
        {**entry, "score": score}
        for score, entry in scored[:limit]
    ]
    return {"query": query, "count": len(matches), "matches": matches}


def open_app(
    app_id: str,
    uri: list[str] | None = None,
    path: list[str] | None = None,
) -> dict[str, object]:
    app_id = _validate_app_id(app_id)
    uris, paths = _validate_launch_targets(uri, path)

    entry = _find_entry(app_id)
    if entry is None:
        raise FileNotFoundError(f"no installed app with AppID `{app_id}`")

    for local_path in paths:
        policy.require("fs.read", path=local_path)
    policy.require("desktop.launch", name=app_id)
    launch = _broker_launch(app_id, uris)
    launched_at = _now_iso()
    record: dict[str, object] = {
        "ts": launched_at,
        "app_id": app_id,
        "name": _entry_text(entry, "name"),
        "extras": uris,
    }
    _append_recent(record)
    return {
        **launch,
        "name": record["name"],
        "launched_at": launched_at,
        "exec_binary": _entry_text(entry, "exec_binary"),
    }


def recent(limit: int = DEFAULT_RECENT_LIMIT) -> dict[str, object]:
    limit = _validate_limit(limit, "limit")
    return {"recent": _read_recent(limit)}


def is_running(app_id: str) -> dict[str, object]:
    app_id = _validate_app_id(app_id)
    policy.require("proc.observe", wild=True)

    entry = _find_entry(app_id)
    if entry is None:
        raise FileNotFoundError(f"no installed app with AppID `{app_id}`")
    binary = _entry_text(entry, "exec_binary")
    pids = _pids_matching(binary) if binary else []
    return {
        "app_id": app_id,
        "exec_binary": binary,
        "running": bool(pids),
        "pids": pids,
    }
