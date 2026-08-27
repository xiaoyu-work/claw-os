"""Bidirectional Discord gateway for the ClawOS system agent.

The foreground ``start`` command connects to Discord Gateway v10, routes
allowlisted messages to ``cos agent ask``, and replies in the source channel.
DMs keep one agent session per user; guild channels and threads keep one
session per channel. The implementation is stdlib-only.

One-time setup::

    cos credential store discord_bot_token <token>
    cos app gateway-discord configure \
      "users=123456789 guilds=987654321 require_mention=true"
    cos app gateway-discord start

``users`` is mandatory and fail-closed. Guild messages additionally require
an allowed guild or channel. Environment variables can override persisted
configuration; see ``docs/external-communications.md``.
"""

from __future__ import annotations

import ctypes
import errno
import json
import os
import queue
import random
import re
import select
import shlex
import signal
import subprocess
import sys
import threading
import time
import urllib.error
import urllib.parse
from typing import Any


sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

from _shared import (  # noqa: E402
    atomic,
    gateway_memory,
    inbound,
    safe_egress,
    safe_subprocess,
    websocket,
)

try:
    from cos_runtime import policy as _cos_policy  # type: ignore[import-not-found]
except ImportError:  # pragma: no cover - runtime is present in ClawOS
    _cos_policy = None

_POLICY_ERRORS = (_cos_policy.PolicyError,) if _cos_policy is not None else ()


PLATFORM = "discord"
VERSION = "0.3.0"
DISCORD_API = "https://discord.com/api/v10"
DISCORD_GATEWAY_URL = "wss://gateway.discord.gg/?v=10&encoding=json"
USER_AGENT = "DiscordBot (https://github.com/xiaoyu-work/claw-os, 0.3.0)"

MAX_MESSAGE_LEN = 2000
MAX_INBOUND_QUEUE = 32
MAX_ATTACHMENTS = 10
AGENT_TIMEOUT_S = 120
AGENT_SERVER_TIMEOUT_S = 110
DEFAULT_RPM = 5
BACKOFF_SCHEDULE_S = [1, 2, 5, 10, 30, 60]

# GUILDS | GUILD_MESSAGES | DIRECT_MESSAGES | MESSAGE_CONTENT
GATEWAY_INTENTS = (1 << 0) | (1 << 9) | (1 << 12) | (1 << 15)
FATAL_CLOSE_CODES = {
    4004: "authentication failed; replace discord_bot_token",
    4010: "invalid shard configuration",
    4011: "sharding is required for this bot",
    4012: "invalid Discord Gateway API version",
    4013: "invalid gateway intents",
    4014: "Message Content Intent is disabled in the Discord Developer Portal",
}
NON_RESUMABLE_CLOSE_CODES = {4007, 4009}

ENV_TOKEN = "COS_DISCORD_TOKEN"
ENV_ALLOWED_USERS = "COS_DISCORD_ALLOWED_USERS"
ENV_ALLOWED_GUILDS = "COS_DISCORD_ALLOWED_GUILDS"
ENV_ALLOWED_CHANNELS = "COS_DISCORD_ALLOWED_CHANNELS"
ENV_REQUIRE_MENTION = "COS_DISCORD_REQUIRE_MENTION"
ENV_RPM = "COS_DISCORD_RPM"

_ID_RE = re.compile(r"^\d{1,20}$")
_STATE_LOCK = threading.RLock()
_RATE_LIMITER: inbound.TokenBucket | None = None


class DiscordApiError(Exception):
    """Discord REST API returned an error response."""


class GatewayReconnect(Exception):
    """Reconnect the Gateway WebSocket, preserving resumable state."""


class FatalGatewayError(Exception):
    """Gateway cannot recover without operator action."""


def _state_dir() -> str:
    base = os.environ.get("COS_DATA_DIR")
    if not base:
        if sys.platform == "win32":
            base = os.environ.get("LOCALAPPDATA") or os.path.expanduser("~")
            base = os.path.join(base, "cos")
        else:
            base = "/var/lib/cos"
    path = os.path.join(base, "apps", "gateway-discord")
    os.makedirs(path, exist_ok=True)
    return path


def _state_path() -> str:
    return os.path.join(_state_dir(), "state.json")


def _config_path() -> str:
    return os.path.join(_state_dir(), "config.json")


def _pid_path() -> str:
    return os.path.join(_state_dir(), "gateway.pid")


def _stop_path() -> str:
    return os.path.join(_state_dir(), "stop.request")


def _read_state() -> dict[str, Any]:
    try:
        with open(_state_path(), "r", encoding="utf-8") as state_file:
            state = json.load(state_file)
    except (FileNotFoundError, json.JSONDecodeError):
        return {}
    return state if isinstance(state, dict) else {}


def _write_state(state: dict[str, Any]) -> None:
    with _STATE_LOCK:
        atomic.atomic_write_json(_state_path(), state)


def _update_state(state: dict[str, Any], **changes: Any) -> None:
    with _STATE_LOCK:
        state.update(changes)
        atomic.atomic_write_json(_state_path(), state)


def _clear_resume_state(state: dict[str, Any]) -> None:
    with _STATE_LOCK:
        state.pop("gateway_session_id", None)
        state.pop("sequence", None)
        state.pop("resume_gateway_url", None)
        atomic.atomic_write_json(_state_path(), state)


def _read_stored_config() -> dict[str, Any]:
    try:
        with open(_config_path(), "r", encoding="utf-8") as config_file:
            config = json.load(config_file)
    except FileNotFoundError:
        return {}
    except json.JSONDecodeError as exc:
        raise ValueError(f"invalid Discord config JSON: {exc}") from exc
    if not isinstance(config, dict):
        raise ValueError("Discord config must be a JSON object")
    return config


def _parse_bool(value: Any, field: str) -> bool:
    if isinstance(value, bool):
        return value
    normalized = str(value).strip().lower()
    if normalized in {"1", "true", "yes", "on"}:
        return True
    if normalized in {"0", "false", "no", "off"}:
        return False
    raise ValueError(f"{field} must be true or false")


def _parse_ids(value: Any, field: str) -> list[str]:
    if value is None:
        return []
    if isinstance(value, str):
        values = value.split(",")
    elif isinstance(value, (list, tuple, set)):
        values = value
    else:
        raise ValueError(f"{field} must be a comma-separated string or list")
    out: list[str] = []
    for item in values:
        item = str(item).strip()
        if not item:
            continue
        if item != "*" and not _ID_RE.fullmatch(item):
            raise ValueError(f"{field} contains an invalid Discord id: {item!r}")
        if item not in out:
            out.append(item)
    return out


def _validate_config(raw: dict[str, Any]) -> dict[str, Any]:
    try:
        rpm = int(raw.get("requests_per_minute", DEFAULT_RPM))
    except (TypeError, ValueError):
        raise ValueError("requests_per_minute must be an integer") from None
    if not 1 <= rpm <= 60:
        raise ValueError("requests_per_minute must be between 1 and 60")
    return {
        "allowed_users": _parse_ids(raw.get("allowed_users"), "allowed_users"),
        "allowed_guilds": _parse_ids(raw.get("allowed_guilds"), "allowed_guilds"),
        "allowed_channels": _parse_ids(
            raw.get("allowed_channels"), "allowed_channels"
        ),
        "require_mention": _parse_bool(
            raw.get("require_mention", True), "require_mention"
        ),
        "requests_per_minute": rpm,
    }


def _effective_config() -> dict[str, Any]:
    raw = _read_stored_config()
    env_overrides = {
        ENV_ALLOWED_USERS: "allowed_users",
        ENV_ALLOWED_GUILDS: "allowed_guilds",
        ENV_ALLOWED_CHANNELS: "allowed_channels",
        ENV_REQUIRE_MENTION: "require_mention",
        ENV_RPM: "requests_per_minute",
    }
    for env_name, key in env_overrides.items():
        if env_name in os.environ:
            raw[key] = os.environ[env_name]
    return _validate_config(raw)


def _configure(args: Any) -> dict[str, Any]:
    aliases = {
        "users": "allowed_users",
        "allowed_users": "allowed_users",
        "guilds": "allowed_guilds",
        "allowed_guilds": "allowed_guilds",
        "channels": "allowed_channels",
        "allowed_channels": "allowed_channels",
        "require_mention": "require_mention",
        "rpm": "requests_per_minute",
        "requests_per_minute": "requests_per_minute",
    }
    updates: dict[str, Any] = {}
    if isinstance(args, dict):
        if set(args) == {"settings"}:
            return _configure([args["settings"]])
        for key, value in args.items():
            canonical = aliases.get(str(key))
            if canonical is None:
                return {"ok": False, "error": f"unknown setting: {key}"}
            updates[canonical] = value
    elif isinstance(args, list):
        try:
            tokens = shlex.split(" ".join(str(arg) for arg in args))
        except ValueError as exc:
            return {"ok": False, "error": f"invalid settings: {exc}"}
        for token in tokens:
            if "=" not in token:
                return {
                    "ok": False,
                    "error": f"expected key=value setting, got {token!r}",
                }
            key, value = token.split("=", 1)
            canonical = aliases.get(key.strip())
            if canonical is None:
                return {"ok": False, "error": f"unknown setting: {key}"}
            updates[canonical] = value
    else:
        return {"ok": False, "error": "settings must be key=value text or an object"}

    try:
        stored = _read_stored_config()
        stored.update(updates)
        config = _validate_config(stored)
    except ValueError as exc:
        return {"ok": False, "error": str(exc)}
    if not config["allowed_users"]:
        return {
            "ok": False,
            "error": "allowed_users is required; refusing an open inbound gateway",
        }
    atomic.atomic_write_json(_config_path(), config)
    return {"ok": True, "platform": PLATFORM, "configured": config}


def _read_pid() -> int | None:
    try:
        with open(_pid_path(), "r", encoding="utf-8") as pid_file:
            return int(pid_file.read().strip())
    except (FileNotFoundError, ValueError):
        return None


def _write_pid(pid: int) -> None:
    atomic.atomic_write_text(_pid_path(), str(pid), mode=0o644)


def _clear_pid() -> None:
    try:
        os.remove(_pid_path())
    except FileNotFoundError:
        pass


def _clear_stop_request() -> None:
    try:
        os.remove(_stop_path())
    except FileNotFoundError:
        pass


def _watch_stop_request(stop_event: threading.Event, pid: int) -> None:
    while not stop_event.wait(0.25):
        try:
            with open(_stop_path(), "r", encoding="utf-8") as stop_file:
                requested_pid = int(stop_file.read().strip())
        except (FileNotFoundError, ValueError, OSError):
            continue
        if requested_pid == pid:
            stop_event.set()
            return


def _pid_alive(pid: int) -> bool:
    if pid <= 0:
        return False
    if sys.platform == "win32":
        process_query = 0x1000
        still_active = 259
        kernel32 = ctypes.windll.kernel32
        handle = kernel32.OpenProcess(process_query, False, pid)
        if not handle:
            return False
        try:
            exit_code = ctypes.c_ulong()
            if not kernel32.GetExitCodeProcess(handle, ctypes.byref(exit_code)):
                return False
            return exit_code.value == still_active
        finally:
            kernel32.CloseHandle(handle)
    try:
        os.kill(pid, 0)
        return True
    except ProcessLookupError:
        return False
    except PermissionError:
        return True
    except OSError as exc:
        return exc.errno != errno.ESRCH


def log_event(level: str, message: str, **fields: Any) -> None:
    record = {
        "ts": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "lvl": level,
        "msg": message,
    }
    record.update(fields)
    sys.stderr.write(json.dumps(record, ensure_ascii=True) + "\n")
    sys.stderr.flush()


def _cos_bin() -> str:
    return os.environ.get("COS_BIN", "cos")


def _load_token() -> str:
    env_token = os.environ.get(ENV_TOKEN)
    if env_token:
        return env_token.strip()
    token, error = safe_subprocess.safe_credential_load(
        "discord_bot_token",
        timeout=10.0,
        cos_bin=_cos_bin(),
    )
    if token is None:
        raise RuntimeError(
            error
            or (
                "discord_bot_token is not configured; run "
                "`cos credential store discord_bot_token <token>`"
            )
        )
    return token


def _api_call(
    token: str,
    method: str,
    path: str,
    payload: dict[str, Any] | None = None,
    *,
    timeout: float = 15.0,
    retry_rate_limit: bool = True,
) -> dict[str, Any]:
    body = None
    headers = {
        "Authorization": f"Bot {token}",
        "User-Agent": USER_AGENT,
    }
    if payload is not None:
        body = json.dumps(payload, ensure_ascii=False).encode("utf-8")
        headers["Content-Type"] = "application/json"
    try:
        _, _, raw = safe_egress.safe_urlopen(
            method,
            f"{DISCORD_API}{path}",
            headers=headers,
            body=body,
            timeout=timeout,
            verb_id="net.dial",
        )
    except urllib.error.HTTPError as exc:
        raw_error = exc.read().decode("utf-8", errors="replace")
        if exc.code == 429 and retry_rate_limit:
            try:
                error_payload = json.loads(raw_error)
                retry_after = float(error_payload.get("retry_after", 1))
            except (json.JSONDecodeError, TypeError, ValueError):
                retry_after = 1.0
            time.sleep(min(max(retry_after, 0.1), 10.0))
            return _api_call(
                token,
                method,
                path,
                payload,
                timeout=timeout,
                retry_rate_limit=False,
            )
        try:
            error_payload = json.loads(raw_error)
            detail = error_payload.get("message", "unknown error")
        except json.JSONDecodeError:
            detail = raw_error[:300] or str(exc.reason)
        raise DiscordApiError(f"Discord HTTP {exc.code}: {detail}") from exc

    if not raw:
        return {}
    try:
        response = json.loads(raw.decode("utf-8"))
    except json.JSONDecodeError as exc:
        raise DiscordApiError("Discord returned a non-JSON response") from exc
    if not isinstance(response, dict):
        raise DiscordApiError("Discord returned an invalid JSON response")
    return response


def _current_bot_user(token: str) -> dict[str, Any]:
    user = _api_call(token, "GET", "/users/@me")
    user_id = str(user.get("id", ""))
    if not _ID_RE.fullmatch(user_id):
        raise DiscordApiError("Discord /users/@me response has no valid bot id")
    return user


def _normalize_gateway_url(value: Any) -> str:
    try:
        parsed = urllib.parse.urlsplit(str(value or ""))
        host = (parsed.hostname or "").rstrip(".").lower()
        port = parsed.port or 443
    except ValueError as exc:
        raise ValueError(f"invalid Discord Gateway URL: {exc}") from None
    if (
        parsed.scheme.lower() != "wss"
        or not host
        or parsed.username is not None
        or parsed.password is not None
        or parsed.fragment
        or port != 443
    ):
        raise ValueError("Discord Gateway URL must be a standard wss:// endpoint")
    if host != "gateway.discord.gg" and not host.endswith(".discord.gg"):
        raise ValueError("Discord Gateway URL is outside discord.gg")
    return urllib.parse.urlunsplit(
        ("wss", host, parsed.path or "/", "v=10&encoding=json", "")
    )


def _gateway_bot_info(token: str) -> dict[str, Any]:
    info = _api_call(token, "GET", "/gateway/bot")
    info["url"] = _normalize_gateway_url(info.get("url"))
    return info


def _validate_snowflake(value: Any, field: str) -> str:
    value = str(value or "").strip()
    if not _ID_RE.fullmatch(value):
        raise ValueError(f"{field} must be a Discord snowflake id")
    return value


def _split_message(text: str) -> list[str]:
    text = str(text or "").strip()
    if not text:
        return ["[agent returned an empty response]"]
    chunks: list[str] = []
    while len(text) > MAX_MESSAGE_LEN:
        cut = text.rfind("\n", 0, MAX_MESSAGE_LEN + 1)
        if cut < MAX_MESSAGE_LEN // 2:
            cut = text.rfind(" ", 0, MAX_MESSAGE_LEN + 1)
        if cut < MAX_MESSAGE_LEN // 2:
            cut = MAX_MESSAGE_LEN
        chunks.append(text[:cut].rstrip())
        text = text[cut:].lstrip()
    if text:
        chunks.append(text)
    return chunks


def _send_messages(
    token: str,
    channel_id: Any,
    text: str,
    *,
    reply_to: Any = None,
) -> dict[str, Any]:
    channel_id = _validate_snowflake(channel_id, "channel_id")
    reply_id = None
    if reply_to is not None:
        reply_id = _validate_snowflake(reply_to, "reply_to")
    message_ids: list[str] = []
    for index, chunk in enumerate(_split_message(text)):
        payload: dict[str, Any] = {
            "content": chunk,
            "allowed_mentions": {"parse": [], "replied_user": False},
        }
        if index == 0 and reply_id is not None:
            payload["message_reference"] = {
                "message_id": reply_id,
                "channel_id": channel_id,
                "fail_if_not_exists": False,
            }
        response = _api_call(
            token,
            "POST",
            f"/channels/{channel_id}/messages",
            payload,
        )
        message_id = response.get("id")
        if isinstance(message_id, str):
            message_ids.append(message_id)
    return {
        "ok": True,
        "platform": PLATFORM,
        "channel_id": channel_id,
        "message_id": message_ids[0] if message_ids else None,
        "message_ids": message_ids,
        "chunks": len(message_ids),
    }


def _send_typing(token: str, channel_id: str) -> None:
    _api_call(token, "POST", f"/channels/{channel_id}/typing", timeout=10.0)


def _rate_limiter(rpm: int) -> inbound.TokenBucket:
    global _RATE_LIMITER
    if _RATE_LIMITER is None or _RATE_LIMITER.capacity != rpm:
        _RATE_LIMITER = inbound.TokenBucket(capacity=rpm, refill_seconds=60.0)
    return _RATE_LIMITER


def _reset_rate_limiter_for_tests() -> None:
    global _RATE_LIMITER
    _RATE_LIMITER = None


def _extract_agent_response(stdout: str) -> tuple[str, str | None]:
    try:
        payload = json.loads(stdout)
    except json.JSONDecodeError:
        return stdout.strip() or "[agent returned an empty response]", None
    if not isinstance(payload, dict):
        return stdout.strip() or "[agent returned an invalid response]", None
    answer = payload.get("answer")
    if not isinstance(answer, str) or not answer.strip():
        for key in ("response", "text", "content", "message"):
            candidate = payload.get(key)
            if isinstance(candidate, str) and candidate.strip():
                answer = candidate
                break
    if not isinstance(answer, str) or not answer.strip():
        answer = "[agent returned an empty response]"
    session_id = payload.get("session_id")
    return answer.strip(), session_id if isinstance(session_id, str) else None


def _ask_agent(
    sender_id: str,
    prompt: str,
    *,
    allowed_users: list[str],
    rpm: int,
    session_id: str | None,
) -> tuple[str, str | None]:
    inbound.verify_sender(
        sender_id,
        ENV_ALLOWED_USERS,
        extra_allowlist=allowed_users,
    )
    if _cos_policy is not None:
        _cos_policy.require("data.inbox.write", name=sender_id)
    if not _rate_limiter(rpm).try_consume(sender_id):
        raise inbound.RateLimited(
            f"Discord user {sender_id} exceeded {rpm} requests per minute"
        )

    argv = [
        _cos_bin(),
        "agent",
        "ask",
        "--full",
        "--timeout-secs",
        str(AGENT_SERVER_TIMEOUT_S),
    ]
    if session_id:
        argv.extend(["--session", session_id])
    argv.append(prompt)
    try:
        proc = safe_subprocess.safe_subprocess(argv, timeout=AGENT_TIMEOUT_S)
    except subprocess.TimeoutExpired:
        return f"[agent timeout after {AGENT_TIMEOUT_S}s]", session_id
    except FileNotFoundError:
        return "[agent unavailable: cos binary not on PATH]", session_id
    if proc.returncode != 0:
        detail = (proc.stderr or proc.stdout or "non-zero exit").strip()[:300]
        return f"[agent error] {detail}", session_id
    return _extract_agent_response(proc.stdout.strip())


def _contains_id(allowed: list[str], value: str) -> bool:
    return "*" in allowed or value in allowed


def _strip_bot_mention(content: str, bot_user_id: str) -> str:
    return re.sub(rf"<@!?{re.escape(bot_user_id)}>", "", content).strip()


def _attachment_lines(message: dict[str, Any]) -> list[str]:
    attachments = message.get("attachments")
    if not isinstance(attachments, list):
        return []
    lines: list[str] = []
    for attachment in attachments[:MAX_ATTACHMENTS]:
        if not isinstance(attachment, dict):
            continue
        url = attachment.get("url")
        if not isinstance(url, str) or not url.startswith("https://"):
            continue
        filename = str(attachment.get("filename") or "attachment")
        filename = "".join(char for char in filename if char >= " ")[:200]
        size = attachment.get("size")
        size_label = f", {size} bytes" if isinstance(size, int) else ""
        lines.append(f"- {filename}{size_label}: {url}")
    return lines


def _prepare_message(
    message: dict[str, Any],
    *,
    bot_user_id: str,
    config: dict[str, Any],
) -> dict[str, str] | None:
    author = message.get("author")
    if not isinstance(author, dict) or author.get("bot") is True:
        return None
    sender_id = str(author.get("id") or "")
    channel_id = str(message.get("channel_id") or "")
    message_id = str(message.get("id") or "")
    if not all(_ID_RE.fullmatch(value) for value in (sender_id, channel_id, message_id)):
        return None
    if sender_id == bot_user_id:
        return None
    try:
        inbound.verify_sender(
            sender_id,
            ENV_ALLOWED_USERS,
            extra_allowlist=config["allowed_users"],
        )
    except inbound.SenderNotAllowed:
        log_event("warn", "Discord sender not allowed", sender_id=sender_id)
        return None

    guild_id_raw = message.get("guild_id")
    guild_id = str(guild_id_raw) if guild_id_raw is not None else ""
    is_guild = bool(guild_id)
    if is_guild:
        if not _ID_RE.fullmatch(guild_id):
            return None
        scope_allowed = _contains_id(config["allowed_guilds"], guild_id) or _contains_id(
            config["allowed_channels"], channel_id
        )
        if not scope_allowed:
            log_event(
                "warn",
                "Discord guild/channel not allowed",
                guild_id=guild_id,
                channel_id=channel_id,
            )
            return None
        if config["require_mention"]:
            mentions = message.get("mentions")
            mentioned_ids = {
                str(item.get("id"))
                for item in (mentions if isinstance(mentions, list) else [])
                if isinstance(item, dict)
            }
            if bot_user_id not in mentioned_ids:
                return None

    content = str(message.get("content") or "")
    content = _strip_bot_mention(content, bot_user_id)
    attachments = _attachment_lines(message)
    if not content and not attachments:
        return None
    context = (
        f"Discord inbound message (sender_id={sender_id}, "
        f"channel_id={channel_id}"
    )
    if guild_id:
        context += f", guild_id={guild_id}"
    context += "). Reply for delivery back to Discord.\n\n"
    prompt = context + content
    if attachments:
        prompt += "\n\nAttachments (untrusted URLs; do not fetch unless needed):\n"
        prompt += "\n".join(attachments)
    session_key = (
        f"discord:channel:{channel_id}"
        if is_guild
        else f"discord:dm:{sender_id}"
    )
    return {
        "sender_id": sender_id,
        "channel_id": channel_id,
        "message_id": message_id,
        "session_key": session_key,
        "prompt": prompt,
    }


def _worker_loop(
    work_queue: queue.Queue,
    stop_event: threading.Event,
    token: str,
    state: dict[str, Any],
    config: dict[str, Any],
) -> None:
    while not stop_event.is_set():
        try:
            item = work_queue.get(timeout=0.5)
        except queue.Empty:
            continue
        try:
            with _STATE_LOCK:
                sessions = state.setdefault("agent_sessions", {})
                session_id = (
                    sessions.get(item["session_key"])
                    if isinstance(sessions, dict)
                    else None
                )
            try:
                _send_typing(token, item["channel_id"])
            except (
                DiscordApiError,
                safe_egress.EgressBlocked,
                urllib.error.URLError,
            ) as exc:
                log_event(
                    "warn",
                    "Discord typing indicator failed",
                    channel_id=item["channel_id"],
                    error=str(exc),
                )
            except _POLICY_ERRORS as exc:
                log_event(
                    "error",
                    "Discord typing indicator denied",
                    channel_id=item["channel_id"],
                    kernel=getattr(exc, "denial", None),
                )
            try:
                reply, new_session_id = _ask_agent(
                    item["sender_id"],
                    item["prompt"],
                    allowed_users=config["allowed_users"],
                    rpm=config["requests_per_minute"],
                    session_id=session_id,
                )
            except inbound.RateLimited:
                reply = (
                    "Rate limit reached. Please wait before sending another request."
                )
                new_session_id = session_id
            except inbound.SenderNotAllowed:
                log_event(
                    "warn",
                    "Discord sender rejected before agent dispatch",
                    sender_id=item["sender_id"],
                )
                continue
            except _POLICY_ERRORS as exc:
                denial = getattr(exc, "denial", None)
                log_event(
                    "error",
                    "Discord agent dispatch failed",
                    sender_id=item["sender_id"],
                    error=str(exc),
                    kernel=denial,
                )
                reply = "Internal error while handling that message."
                new_session_id = session_id

            if new_session_id:
                with _STATE_LOCK:
                    sessions = state.setdefault("agent_sessions", {})
                    if isinstance(sessions, dict):
                        sessions[item["session_key"]] = new_session_id
                    atomic.atomic_write_json(_state_path(), state)
            try:
                result = _send_messages(
                    token,
                    item["channel_id"],
                    reply,
                    reply_to=item["message_id"],
                )
                gateway_memory.remember_send(
                    PLATFORM,
                    result,
                    channel_id=item["channel_id"],
                    text=reply,
                    extra_tags=["reply"],
                )
            except (
                DiscordApiError,
                safe_egress.EgressBlocked,
                urllib.error.URLError,
                ValueError,
            ) as exc:
                log_event(
                    "error",
                    "Discord reply failed",
                    channel_id=item["channel_id"],
                    error=str(exc),
                )
            except _POLICY_ERRORS as exc:
                log_event(
                    "error",
                    "Discord reply denied",
                    channel_id=item["channel_id"],
                    kernel=getattr(exc, "denial", None),
                )
        finally:
            work_queue.task_done()


def _worker_entry(
    work_queue: queue.Queue,
    stop_event: threading.Event,
    token: str,
    state: dict[str, Any],
    config: dict[str, Any],
) -> None:
    # This is the crash boundary for the sole sequential worker. Unexpected
    # failures stop the whole gateway instead of leaving a live socket that
    # keeps accepting messages with no consumer.
    _update_state(state, worker_alive=True, worker_error=None)
    try:
        _worker_loop(work_queue, stop_event, token, state, config)
    except Exception as exc:
        log_event(
            "error",
            "Discord agent worker crashed",
            error=f"{type(exc).__name__}: {exc}",
        )
        try:
            _update_state(
                state,
                worker_alive=False,
                worker_error=f"{type(exc).__name__}: {exc}",
            )
        except OSError as state_error:
            log_event(
                "error",
                "Could not persist Discord worker failure",
                error=str(state_error),
            )
        stop_event.set()
    else:
        _update_state(state, worker_alive=False)


def _enqueue_message(
    work_queue: queue.Queue,
    token: str,
    item: dict[str, str],
) -> None:
    try:
        work_queue.put_nowait(item)
    except queue.Full:
        log_event(
            "warn",
            "Discord inbound queue full",
            channel_id=item["channel_id"],
        )
        try:
            _send_messages(
                token,
                item["channel_id"],
                "The agent is busy. Please try again shortly.",
                reply_to=item["message_id"],
            )
        except (
            DiscordApiError,
            safe_egress.EgressBlocked,
            urllib.error.URLError,
            ValueError,
        ) as exc:
            log_event(
                "error",
                "Discord busy reply failed",
                channel_id=item["channel_id"],
                error=str(exc),
            )
        except _POLICY_ERRORS as exc:
            log_event(
                "error",
                "Discord busy reply denied",
                channel_id=item["channel_id"],
                kernel=getattr(exc, "denial", None),
            )


def _gateway_connection(
    token: str,
    state: dict[str, Any],
    config: dict[str, Any],
    work_queue: queue.Queue,
    stop_event: threading.Event,
) -> None:
    with _STATE_LOCK:
        gateway_session_id = state.get("gateway_session_id")
        sequence = state.get("sequence")
        base_url = state.get("gateway_url") or DISCORD_GATEWAY_URL
        resume_url = state.get("resume_gateway_url")
    should_resume = isinstance(gateway_session_id, str) and isinstance(sequence, int)
    try:
        connect_url = _normalize_gateway_url(
            resume_url if should_resume and resume_url else base_url
        )
    except ValueError:
        _clear_resume_state(state)
        should_resume = False
        connect_url = _normalize_gateway_url(DISCORD_GATEWAY_URL)
    client = websocket.connect(
        connect_url,
        user_agent=USER_AGENT,
        timeout=15.0,
        max_message_bytes=1024 * 1024,
    )
    client.settimeout(15.0)
    try:
        hello = client.recv_json()
        if hello.get("op") != 10 or not isinstance(hello.get("d"), dict):
            raise websocket.WebSocketProtocolError("expected Discord Hello payload")
        heartbeat_ms = hello["d"].get("heartbeat_interval")
        if not isinstance(heartbeat_ms, (int, float)) or heartbeat_ms <= 0:
            raise websocket.WebSocketProtocolError("invalid Discord heartbeat interval")
        heartbeat_interval = float(heartbeat_ms) / 1000.0

        if should_resume:
            client.send_json(
                {
                    "op": 6,
                    "d": {
                        "token": token,
                        "session_id": gateway_session_id,
                        "seq": sequence,
                    },
                }
            )
            log_event("info", "Discord gateway resume requested", sequence=sequence)
        else:
            client.send_json(
                {
                    "op": 2,
                    "d": {
                        "token": token,
                        "intents": GATEWAY_INTENTS,
                        "properties": {
                            "os": sys.platform,
                            "browser": "claw-os",
                            "device": "claw-os",
                        },
                    },
                }
            )
            log_event("info", "Discord gateway identify sent")

        next_heartbeat = time.monotonic() + random.random() * heartbeat_interval
        heartbeat_acked = True
        while not stop_event.is_set():
            now = time.monotonic()
            if now >= next_heartbeat:
                if not heartbeat_acked:
                    raise GatewayReconnect("Discord heartbeat was not acknowledged")
                with _STATE_LOCK:
                    sequence = state.get("sequence")
                client.send_json({"op": 1, "d": sequence})
                heartbeat_acked = False
                next_heartbeat = now + heartbeat_interval

            wait = min(max(next_heartbeat - time.monotonic(), 0.0), 1.0)
            readable, _, _ = select.select([client.fileno()], [], [], wait)
            if not readable:
                continue
            payload = client.recv_json()
            op = payload.get("op")
            sequence_value = payload.get("s")
            if isinstance(sequence_value, int):
                _update_state(state, sequence=sequence_value)
            if op == 11:
                heartbeat_acked = True
                continue
            if op == 1:
                with _STATE_LOCK:
                    sequence = state.get("sequence")
                client.send_json({"op": 1, "d": sequence})
                heartbeat_acked = False
                continue
            if op == 7:
                raise GatewayReconnect("Discord requested reconnect")
            if op == 9:
                resumable = payload.get("d") is True
                if not resumable:
                    _clear_resume_state(state)
                raise GatewayReconnect(
                    "Discord invalidated the gateway session"
                )
            if op != 0:
                continue

            event_type = payload.get("t")
            event = payload.get("d")
            if not isinstance(event, dict):
                continue
            if event_type == "READY":
                session_id = event.get("session_id")
                resume_gateway_url = event.get("resume_gateway_url")
                user = event.get("user")
                bot_user_id = (
                    str(user.get("id"))
                    if isinstance(user, dict) and user.get("id") is not None
                    else state.get("bot_user_id")
                )
                changes: dict[str, Any] = {"last_ready_at": int(time.time())}
                if isinstance(session_id, str):
                    changes["gateway_session_id"] = session_id
                try:
                    changes["resume_gateway_url"] = _normalize_gateway_url(
                        resume_gateway_url
                    )
                except ValueError:
                    log_event(
                        "warn",
                        "Discord returned an invalid resume gateway URL",
                    )
                    changes["resume_gateway_url"] = _normalize_gateway_url(
                        state.get("gateway_url") or DISCORD_GATEWAY_URL
                    )
                if isinstance(bot_user_id, str) and _ID_RE.fullmatch(bot_user_id):
                    changes["bot_user_id"] = bot_user_id
                _update_state(state, **changes)
                log_event("info", "Discord gateway ready", bot_user_id=bot_user_id)
            elif event_type == "RESUMED":
                log_event("info", "Discord gateway resumed")
            elif event_type == "MESSAGE_CREATE":
                with _STATE_LOCK:
                    bot_user_id = str(state.get("bot_user_id") or "")
                if not _ID_RE.fullmatch(bot_user_id):
                    continue
                item = _prepare_message(
                    event,
                    bot_user_id=bot_user_id,
                    config=config,
                )
                if item is not None:
                    _enqueue_message(work_queue, token, item)
    finally:
        client.close()


def _run_gateway(
    token: str,
    state: dict[str, Any],
    config: dict[str, Any],
    work_queue: queue.Queue,
    stop_event: threading.Event,
) -> None:
    backoff_index = 0
    while not stop_event.is_set():
        try:
            _gateway_connection(token, state, config, work_queue, stop_event)
            backoff_index = 0
        except websocket.WebSocketClosed as exc:
            if exc.code in FATAL_CLOSE_CODES:
                raise FatalGatewayError(FATAL_CLOSE_CODES[exc.code]) from exc
            if exc.code in NON_RESUMABLE_CLOSE_CODES:
                _clear_resume_state(state)
            log_event(
                "warn",
                "Discord gateway closed",
                code=exc.code,
                reason=exc.reason,
            )
        except GatewayReconnect as exc:
            log_event("warn", "Discord gateway reconnecting", reason=str(exc))
        except safe_egress.EgressBlocked as exc:
            raise FatalGatewayError(f"Discord egress blocked: {exc}") from exc
        except (
            websocket.WebSocketProtocolError,
            urllib.error.URLError,
            OSError,
        ) as exc:
            denial = getattr(exc, "denial", None)
            if denial is not None:
                raise FatalGatewayError("Discord gateway permission denied") from exc
            log_event("warn", "Discord gateway transport error", error=str(exc))
        except _POLICY_ERRORS as exc:
            raise FatalGatewayError("Discord gateway permission denied") from exc

        if stop_event.is_set():
            return
        wait = BACKOFF_SCHEDULE_S[
            min(backoff_index, len(BACKOFF_SCHEDULE_S) - 1)
        ]
        backoff_index += 1
        stop_event.wait(wait)


def _start_loop() -> dict[str, Any]:
    existing = _read_pid()
    if existing is not None and _pid_alive(existing):
        return {
            "ok": False,
            "platform": PLATFORM,
            "error": f"gateway already running (pid {existing})",
        }
    try:
        config = _effective_config()
    except ValueError as exc:
        return {"ok": False, "platform": PLATFORM, "error": str(exc)}
    if not config["allowed_users"]:
        return {
            "ok": False,
            "platform": PLATFORM,
            "error": (
                "no Discord users are allowed; run `cos app gateway-discord "
                "configure \"users=<discord-user-id>\"`"
            ),
        }
    try:
        token = _load_token()
        bot_user = _current_bot_user(token)
        gateway_info = _gateway_bot_info(token)
    except (
        RuntimeError,
        DiscordApiError,
        safe_egress.EgressBlocked,
        urllib.error.URLError,
        ValueError,
    ) as exc:
        return {"ok": False, "platform": PLATFORM, "error": str(exc)}
    except _POLICY_ERRORS as exc:
        return {
            "ok": False,
            "platform": PLATFORM,
            "error": "Discord gateway permission denied",
            "denial": getattr(exc, "denial", None),
        }

    state = _read_state()
    _clear_stop_request()
    _update_state(
        state,
        bot_user_id=str(bot_user["id"]),
        bot_username=str(bot_user.get("username") or ""),
        gateway_url=gateway_info["url"],
        last_error=None,
    )
    _write_pid(os.getpid())
    stop_event = threading.Event()
    work_queue: queue.Queue = queue.Queue(MAX_INBOUND_QUEUE)
    worker = threading.Thread(
        target=_worker_entry,
        args=(work_queue, stop_event, token, state, config),
        name="discord-agent-worker",
        daemon=True,
    )
    worker.start()
    stop_watcher = threading.Thread(
        target=_watch_stop_request,
        args=(stop_event, os.getpid()),
        name="discord-stop-watcher",
        daemon=True,
    )
    stop_watcher.start()

    def on_signal(_signum, _frame):
        stop_event.set()

    for sig in (signal.SIGINT, signal.SIGTERM):
        try:
            signal.signal(sig, on_signal)
        except (ValueError, OSError):
            pass

    log_event(
        "info",
        "Discord gateway started",
        pid=os.getpid(),
        bot_user_id=bot_user["id"],
        require_mention=config["require_mention"],
    )
    error: str | None = None
    try:
        _run_gateway(token, state, config, work_queue, stop_event)
    except FatalGatewayError as exc:
        error = str(exc)
        _update_state(state, last_error=error)
        log_event("error", "Discord gateway stopped fatally", error=error)
    finally:
        stop_event.set()
        try:
            _update_state(state, worker_alive=False)
        except OSError as exc:
            log_event(
                "error",
                "Could not persist Discord worker shutdown",
                error=str(exc),
            )
        _clear_pid()
        _clear_stop_request()
        log_event("info", "Discord gateway stopped")
    if error is None:
        with _STATE_LOCK:
            worker_error = state.get("worker_error")
        if isinstance(worker_error, str) and worker_error:
            error = f"Discord agent worker stopped: {worker_error}"
    if error:
        return {"ok": False, "platform": PLATFORM, "error": error}
    return {"ok": True, "platform": PLATFORM, "stopped": True}


def _stop() -> dict[str, Any]:
    pid = _read_pid()
    if pid is None:
        return {"ok": True, "platform": PLATFORM, "running": False}
    if not _pid_alive(pid):
        _clear_pid()
        _clear_stop_request()
        return {
            "ok": True,
            "platform": PLATFORM,
            "running": False,
            "stale_pid": pid,
        }
    atomic.atomic_write_text(_stop_path(), str(pid))
    deadline = time.monotonic() + 5.0
    while time.monotonic() < deadline and _pid_alive(pid):
        time.sleep(0.1)
    stopped = not _pid_alive(pid)
    return {
        "ok": True,
        "platform": PLATFORM,
        "stopped_pid": pid if stopped else None,
        "stop_requested_pid": pid,
    }


def _status() -> dict[str, Any]:
    pid = _read_pid()
    state = _read_state()
    try:
        config = _effective_config()
        config_error = None
    except ValueError as exc:
        config = None
        config_error = str(exc)
    sessions = state.get("agent_sessions")
    return {
        "ok": config_error is None,
        "platform": PLATFORM,
        "running": pid is not None and _pid_alive(pid),
        "pid": pid,
        "state_dir": _state_dir(),
        "bot_user_id": state.get("bot_user_id"),
        "bot_username": state.get("bot_username"),
        "gateway_session_resumable": bool(state.get("gateway_session_id")),
        "sequence": state.get("sequence"),
        "agent_sessions": len(sessions) if isinstance(sessions, dict) else 0,
        "worker_alive": state.get("worker_alive"),
        "worker_error": state.get("worker_error"),
        "configured": config,
        "config_error": config_error,
        "token_env_configured": bool(os.environ.get(ENV_TOKEN, "").strip()),
        "last_error": state.get("last_error"),
    }


def _send(args: Any) -> dict[str, Any]:
    if isinstance(args, dict):
        channel_id = args.get("channel_id")
        text = args.get("text")
    elif isinstance(args, list):
        channel_id = args[0] if args else None
        text = " ".join(str(value) for value in args[1:]) if len(args) > 1 else None
    else:
        return {"ok": False, "platform": PLATFORM, "error": "invalid args"}
    if not text or not str(text).strip():
        return {"ok": False, "platform": PLATFORM, "error": "text is required"}
    try:
        token = _load_token()
        return _send_messages(token, channel_id, str(text))
    except (
        RuntimeError,
        DiscordApiError,
        safe_egress.EgressBlocked,
        urllib.error.URLError,
        ValueError,
    ) as exc:
        return {"ok": False, "platform": PLATFORM, "error": str(exc)}
    except _POLICY_ERRORS as exc:
        return {
            "ok": False,
            "platform": PLATFORM,
            "error": "Discord send permission denied",
            "denial": getattr(exc, "denial", None),
        }


def run(command: str, args: Any) -> dict[str, Any]:
    from canonical_argv import normalize_canonical_argv
    if isinstance(args, list):
        args = normalize_canonical_argv(args)
    if command == "configure":
        return _configure(args)
    if command == "start":
        return _start_loop()
    if command == "stop":
        return _stop()
    if command == "status":
        return _status()
    if command == "send":
        result = _send(args)
        if isinstance(args, dict):
            channel_id = str(args.get("channel_id") or "")
            text = str(args.get("text") or "")
        elif isinstance(args, list):
            channel_id = str(args[0]) if args else ""
            text = " ".join(str(value) for value in args[1:])
        else:
            channel_id = ""
            text = ""
        gateway_memory.remember_send(
            PLATFORM,
            result,
            channel_id=channel_id,
            text=text,
        )
        return result
    return {"ok": False, "error": f"unknown command: {command}"}


if __name__ == "__main__":
    cmd = os.environ.get("COS_COMMAND") or (
        sys.argv[1] if len(sys.argv) > 1 else ""
    )
    raw_args = os.environ.get("COS_ARGS_JSON")
    parsed_args = json.loads(raw_args) if raw_args else sys.argv[2:]
    print(json.dumps(run(cmd, parsed_args), ensure_ascii=False))
