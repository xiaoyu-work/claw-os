"""user-manager — critical local identity management through clawd."""

import json
import os
import re
import shutil
import subprocess
import sys
import unicodedata

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from _shared.env_scrub import scrub_env  # noqa: E402

from cos_runtime import policy  # noqa: E402


NAME_RE = re.compile(r"^[a-z_][a-z0-9_-]{0,31}$")
CREDENTIAL_RE = re.compile(
    r"^(?!-)[A-Za-z0-9_.:-]{1,255}/(?!-)[A-Za-z0-9_.:-]{1,255}$"
)
TOKEN_RE = re.compile(r"^[0-9A-Fa-f]{32}$")
MAX_GROUPS = 64
MAX_FULL_NAME_LENGTH = 128
MAX_SHELL_LENGTH = 255
TIMEOUT_SECS = int(os.environ.get("CLAW_USER_MANAGER_TIMEOUT", "300"))


def _cos_binary() -> str | None:
    return os.environ.get("COS_BIN") or shutil.which("cos")


def _parse_payload(payload_text: str) -> dict:
    try:
        payload = json.loads(payload_text)
    except json.JSONDecodeError as exc:
        raise RuntimeError("User Manager broker returned invalid JSON") from exc
    if not isinstance(payload, dict):
        raise RuntimeError("User Manager broker returned a non-object result")
    if "error" in payload:
        error = payload["error"]
        if not isinstance(error, str) or not error.strip():
            raise RuntimeError(
                "User Manager broker returned an invalid error payload"
            )
        raise RuntimeError(error)
    return payload


def _broker(
    action: str,
    *,
    user: str | None = None,
    group: str | None = None,
    full_name: str | None = None,
    shell: str | None = None,
    groups: str | None = None,
    credential: str | None = None,
    token: str | None = None,
    confirm: bool = False,
) -> dict:
    cos_bin = _cos_binary()
    if not cos_bin:
        raise FileNotFoundError(
            "cos binary not found; User Manager broker unavailable"
        )
    argv = [cos_bin, "__users", action]
    for flag, value in (
        ("--user", user),
        ("--group", group),
        ("--full-name", full_name),
        ("--shell", shell),
        ("--groups", groups),
        ("--credential", credential),
        ("--token", token),
    ):
        if value is not None:
            argv.extend([flag, value])
    if confirm:
        argv.append("--confirm")
    try:
        result = subprocess.run(
            argv,
            capture_output=True,
            text=True,
            timeout=TIMEOUT_SECS,
            stdin=subprocess.DEVNULL,
            env=scrub_env(),
            check=False,
        )
    except FileNotFoundError as exc:
        raise FileNotFoundError(
            f"User Manager broker executable not found: {cos_bin}"
        ) from exc
    except PermissionError as exc:
        raise PermissionError(
            f"permission denied launching User Manager broker: {cos_bin}"
        ) from exc
    except subprocess.TimeoutExpired as exc:
        raise TimeoutError(
            f"User Manager broker exceeded {TIMEOUT_SECS}s for {action}"
        ) from exc

    payloads = []
    for output in (
        (result.stdout or "").strip(),
        (result.stderr or "").strip(),
    ):
        if not output:
            continue
        payloads.append(_parse_payload(output))
        if result.returncode == 0:
            break
    if result.returncode != 0:
        raise RuntimeError(f"User Manager broker exited {result.returncode}")
    if not payloads:
        raise RuntimeError("User Manager broker returned invalid JSON")
    return payloads[0]


def _name(raw: object, kind: str) -> str:
    if not isinstance(raw, str) or NAME_RE.fullmatch(raw) is None:
        raise ValueError(f"invalid {kind} name")
    return raw


def _groups(raw: object | None) -> str | None:
    if raw is None:
        return None
    if not isinstance(raw, str):
        raise ValueError("groups must be a comma-separated string")
    names = [value.strip() for value in raw.split(",")]
    if not 1 <= len(names) <= MAX_GROUPS or any(not value for value in names):
        raise ValueError("groups must contain 1-64 non-empty names")
    normalized = [_name(value, "group") for value in names]
    if len(set(normalized)) != len(normalized):
        raise ValueError("groups must not contain duplicates")
    return ",".join(normalized)


def _full_name(raw: object | None) -> str | None:
    if raw is None:
        return None
    if (
        not isinstance(raw, str)
        or not raw
        or len(raw) > MAX_FULL_NAME_LENGTH
        or ":" in raw
        or any(unicodedata.category(character) == "Cc" for character in raw)
    ):
        raise ValueError(
            "full_name must be 1..128 characters without colons or controls"
        )
    return raw


def _shell(raw: object) -> str:
    if (
        not isinstance(raw, str)
        or not raw
        or len(raw) > MAX_SHELL_LENGTH
        or "\x00" in raw
        or any(unicodedata.category(character) == "Cc" for character in raw)
        or not os.path.isabs(raw)
    ):
        raise ValueError(
            "shell must be an absolute canonical non-symlink path without NUL bytes"
        )
    canonical = os.path.realpath(raw)
    if canonical != raw or os.path.islink(raw):
        raise ValueError(
            "shell must be an absolute canonical non-symlink path without NUL bytes"
        )
    return canonical


def _optional_shell(raw: object | None) -> str | None:
    if raw is None:
        return None
    return _shell(raw)


def _credential(raw: object) -> str:
    if not isinstance(raw, str) or CREDENTIAL_RE.fullmatch(raw) is None:
        raise ValueError("credential must use namespace/name form")
    return raw


def _backup_token(raw: object) -> str:
    if not isinstance(raw, str) or TOKEN_RE.fullmatch(raw) is None:
        raise ValueError("backup_token must be exactly 32 hexadecimal characters")
    return raw.lower()


def _require_confirmation(action: str, confirm: object) -> None:
    if confirm is not True:
        raise ValueError(f"{action} requires confirm=true")


def _manage() -> None:
    policy.require("sys.identity", name="manage")


def status() -> dict:
    policy.require("sys.observe", name="identities")
    return _broker("status")


def create_user(
    user: str,
    groups: str | None = None,
    full_name: str | None = None,
    shell: str | None = None,
) -> dict:
    user = _name(user, "user")
    groups = _groups(groups)
    full_name = _full_name(full_name)
    shell = _optional_shell(shell)
    _manage()
    return _broker(
        "create-user",
        user=user,
        groups=groups,
        full_name=full_name,
        shell=shell,
    )


def delete_user(user: str, confirm: bool) -> dict:
    user = _name(user, "user")
    _require_confirmation("delete-user", confirm)
    _manage()
    return _broker("delete-user", user=user, confirm=True)


def lock_user(user: str) -> dict:
    user = _name(user, "user")
    _manage()
    return _broker("lock-user", user=user)


def unlock_user(user: str) -> dict:
    user = _name(user, "user")
    _manage()
    return _broker("unlock-user", user=user)


def set_shell(user: str, shell: str) -> dict:
    user = _name(user, "user")
    shell = _shell(shell)
    _manage()
    return _broker("set-shell", user=user, shell=shell)


def set_password(user: str, credential: str) -> dict:
    user = _name(user, "user")
    credential = _credential(credential)
    _manage()
    policy.require("secret.read", name=credential)
    return _broker("set-password", user=user, credential=credential)


def create_group(group: str) -> dict:
    group = _name(group, "group")
    _manage()
    return _broker("create-group", group=group)


def delete_group(group: str, confirm: bool) -> dict:
    group = _name(group, "group")
    _require_confirmation("delete-group", confirm)
    _manage()
    return _broker("delete-group", group=group, confirm=True)


def add_to_group(user: str, group: str) -> dict:
    user = _name(user, "user")
    group = _name(group, "group")
    _manage()
    return _broker("add-to-group", user=user, group=group)


def remove_from_group(user: str, group: str) -> dict:
    user = _name(user, "user")
    group = _name(group, "group")
    _manage()
    return _broker("remove-from-group", user=user, group=group)


def restore(backup_token: str, confirm: bool) -> dict:
    backup_token = _backup_token(backup_token)
    _require_confirmation("restore", confirm)
    _manage()
    return _broker("restore", token=backup_token, confirm=True)
