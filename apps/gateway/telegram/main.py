"""Telegram gateway — long-poll the Telegram Bot API and forward
inbound messages to `cos agent ask`, replying with the answer.

Phase 9 implementation.

Wiring:

  cos credential store telegram_bot_token <token>     # one-time
  cos app gateway-telegram start                       # foreground loop

Or run under `cos service` so it survives across sessions.

State files live under
``$COS_DATA_DIR/apps/gateway-telegram/`` (or
``$LOCALAPPDATA/cos/apps/gateway-telegram/`` on Windows). The loop
keeps:

  * ``state.json``  — last processed update_id (Telegram offset).
  * ``gateway.pid`` — PID of the running ``start`` loop, used by
    ``status`` and ``stop``.

Message flow per inbound update:

  1. ``message.text`` → run ``cos agent ask <text>`` as a subprocess,
     reading stdout (JSON-or-text agnostic).
  2. ``sendMessage`` back to ``message.chat.id`` with the response.
  3. Persist the new offset (update_id + 1) so we never re-process
     after a restart.

Errors are logged inline (``log_event``) and the loop continues
with exponential backoff on transport failures.

Stdlib only (urllib, json, subprocess, signal, time, os, sys, errno)
— no third-party deps required to install/run the gateway.
"""

from __future__ import annotations

import errno
import json
import os
import signal
import subprocess
import sys
import time
import urllib.error
import urllib.parse
import urllib.request


PLATFORM = "telegram"
TG_API_BASE = "https://api.telegram.org"

# Long-poll timeout (Telegram caps at 50; we use 25 so the socket
# never feels frozen to a watcher).
LONG_POLL_TIMEOUT_S = 25
# Backoff schedule for transport / 5xx failures.
BACKOFF_SCHEDULE_S = [1, 2, 5, 10, 30, 60]
# Truncate replies past Telegram's 4096-char message limit.
TG_MESSAGE_LIMIT = 4096


def _state_dir() -> str:
    base = os.environ.get("COS_DATA_DIR")
    if not base:
        if sys.platform == "win32":
            base = os.environ.get("LOCALAPPDATA") or os.path.expanduser("~")
            base = os.path.join(base, "cos")
        else:
            base = "/var/lib/cos"
    path = os.path.join(base, "apps", "gateway-telegram")
    os.makedirs(path, exist_ok=True)
    return path


def _state_path() -> str:
    return os.path.join(_state_dir(), "state.json")


def _pid_path() -> str:
    return os.path.join(_state_dir(), "gateway.pid")


def _read_state() -> dict:
    try:
        with open(_state_path(), "r", encoding="utf-8") as f:
            return json.load(f)
    except (FileNotFoundError, json.JSONDecodeError):
        return {}


def _write_state(state: dict) -> None:
    tmp = _state_path() + ".tmp"
    with open(tmp, "w", encoding="utf-8") as f:
        json.dump(state, f)
    os.replace(tmp, _state_path())


def _read_pid() -> int | None:
    try:
        with open(_pid_path(), "r", encoding="utf-8") as f:
            return int(f.read().strip())
    except (FileNotFoundError, ValueError):
        return None


def _write_pid(pid: int) -> None:
    with open(_pid_path(), "w", encoding="utf-8") as f:
        f.write(str(pid))


def _clear_pid() -> None:
    try:
        os.remove(_pid_path())
    except FileNotFoundError:
        pass


def _pid_alive(pid: int) -> bool:
    if pid <= 0:
        return False
    if sys.platform == "win32":
        # We can't sniff cheaply on Windows without ctypes; treat
        # any non-zero pidfile entry as "claimed" and let the user
        # call stop or reset the file. Avoids accidental clobber.
        return True
    try:
        os.kill(pid, 0)
        return True
    except ProcessLookupError:
        return False
    except PermissionError:
        # Process exists but isn't ours — still counts as alive.
        return True
    except OSError as e:
        if e.errno == errno.ESRCH:
            return False
        return True


def log_event(level: str, msg: str, **fields) -> None:
    """Single-line JSON log to stderr so cos service can scrape."""
    record = {"ts": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()), "lvl": level, "msg": msg}
    record.update(fields)
    sys.stderr.write(json.dumps(record) + "\n")
    sys.stderr.flush()


def _cos_bin() -> str:
    return os.environ.get("COS_BIN", "cos")


def _load_token() -> str:
    """Pull the bot token from cos credential storage. Falls back to
    the COS_TELEGRAM_TOKEN env var so smoke tests don't need the
    full credential store wired."""
    env_token = os.environ.get("COS_TELEGRAM_TOKEN")
    if env_token:
        return env_token.strip()
    proc = subprocess.run(
        [_cos_bin(), "credential", "load", "telegram_bot_token"],
        capture_output=True,
        text=True,
    )
    if proc.returncode != 0:
        raise RuntimeError(
            "telegram_bot_token not in credential store; "
            "run `cos credential store telegram_bot_token <token>` first"
        )
    try:
        payload = json.loads(proc.stdout)
    except json.JSONDecodeError as e:
        raise RuntimeError(f"credential load returned non-JSON: {e}; raw={proc.stdout!r}")
    if "error" in payload:
        raise RuntimeError(f"credential load failed: {payload['error']}")
    value = payload.get("value")
    if not value:
        raise RuntimeError("credential load returned empty value")
    return value.strip()


def _api_call(token: str, method: str, params: dict, timeout: float) -> dict:
    """POST to https://api.telegram.org/bot<token>/<method>. Returns
    parsed JSON. Raises urllib.error.URLError on transport failure;
    raises RuntimeError on Telegram-side `ok: false`."""
    url = f"{TG_API_BASE}/bot{token}/{method}"
    body = urllib.parse.urlencode(params).encode("utf-8")
    req = urllib.request.Request(
        url,
        data=body,
        headers={"Content-Type": "application/x-www-form-urlencoded"},
    )
    with urllib.request.urlopen(req, timeout=timeout) as resp:
        raw = resp.read().decode("utf-8")
    payload = json.loads(raw)
    if not payload.get("ok"):
        raise RuntimeError(f"telegram api error: {payload.get('description', raw)}")
    return payload


def _ask_agent(text: str) -> str:
    proc = subprocess.run(
        [_cos_bin(), "agent", "ask", text],
        capture_output=True,
        text=True,
    )
    if proc.returncode != 0:
        # Surface a short error so the user gets feedback in chat
        # instead of silent drops.
        return f"[agent error] {proc.stderr.strip() or proc.stdout.strip() or 'non-zero exit'}"
    out = proc.stdout.strip()
    if not out:
        return "[agent returned empty response]"
    # If the agent output is structured JSON with a typical shape,
    # extract the human-readable text; otherwise pass through.
    try:
        payload = json.loads(out)
    except json.JSONDecodeError:
        return out
    for key in ("response", "text", "answer", "content", "message"):
        v = payload.get(key) if isinstance(payload, dict) else None
        if isinstance(v, str) and v.strip():
            return v.strip()
    return out


def _truncate(text: str) -> str:
    if len(text) <= TG_MESSAGE_LIMIT:
        return text
    return text[: TG_MESSAGE_LIMIT - 1] + "…"


def _process_update(token: str, update: dict) -> None:
    msg = update.get("message") or update.get("edited_message")
    if not isinstance(msg, dict):
        return
    chat = msg.get("chat") or {}
    chat_id = chat.get("id")
    text = msg.get("text")
    if chat_id is None or not isinstance(text, str) or not text.strip():
        return
    log_event("info", "inbound", chat_id=chat_id, len=len(text))
    reply = _ask_agent(text.strip())
    try:
        _api_call(
            token,
            "sendMessage",
            {"chat_id": chat_id, "text": _truncate(reply)},
            timeout=15,
        )
    except (urllib.error.URLError, RuntimeError) as e:
        log_event("error", "send failed", chat_id=chat_id, err=str(e))


def _start_loop() -> dict:
    existing = _read_pid()
    if existing is not None and _pid_alive(existing):
        return {
            "ok": False,
            "platform": PLATFORM,
            "error": f"gateway already running (pid {existing}); call stop first",
        }

    try:
        token = _load_token()
    except RuntimeError as e:
        return {"ok": False, "platform": PLATFORM, "error": str(e)}

    _write_pid(os.getpid())
    state = _read_state()
    offset = int(state.get("offset", 0))
    log_event("info", "started", offset=offset, pid=os.getpid())

    stop_flag = {"v": False}

    def _on_signal(_sig, _frame):
        stop_flag["v"] = True
        log_event("info", "signal received, draining")

    for sig in (signal.SIGINT, signal.SIGTERM):
        try:
            signal.signal(sig, _on_signal)
        except (ValueError, OSError):
            # Signal can't be installed (e.g. non-main thread on
            # Windows). Loop will still exit on Ctrl-C via
            # KeyboardInterrupt.
            pass

    backoff_idx = 0
    try:
        while not stop_flag["v"]:
            try:
                payload = _api_call(
                    token,
                    "getUpdates",
                    {"offset": offset, "timeout": LONG_POLL_TIMEOUT_S},
                    timeout=LONG_POLL_TIMEOUT_S + 5,
                )
                backoff_idx = 0
            except urllib.error.URLError as e:
                wait = BACKOFF_SCHEDULE_S[min(backoff_idx, len(BACKOFF_SCHEDULE_S) - 1)]
                backoff_idx += 1
                log_event("warn", "poll failed", err=str(e), backoff_s=wait)
                time.sleep(wait)
                continue
            except RuntimeError as e:
                # Telegram-side error (bad token / rate-limit). Bail
                # so the user can fix it instead of looping.
                log_event("error", "telegram api error", err=str(e))
                break

            for update in payload.get("result", []):
                update_id = update.get("update_id")
                if isinstance(update_id, int):
                    offset = update_id + 1
                _process_update(token, update)
                _write_state({"offset": offset})
    except KeyboardInterrupt:
        pass
    finally:
        _clear_pid()
        log_event("info", "stopped", offset=offset)

    return {"ok": True, "platform": PLATFORM, "stopped": True, "offset": offset}


def _stop() -> dict:
    pid = _read_pid()
    if pid is None:
        return {"ok": True, "platform": PLATFORM, "running": False}
    if not _pid_alive(pid):
        _clear_pid()
        return {"ok": True, "platform": PLATFORM, "running": False, "stale_pid": pid}
    if sys.platform == "win32":
        # Best-effort terminate via taskkill; cos service will
        # generally manage process lifetime instead.
        subprocess.run(["taskkill", "/PID", str(pid), "/F"], capture_output=True)
    else:
        try:
            os.kill(pid, signal.SIGTERM)
        except ProcessLookupError:
            pass
    return {"ok": True, "platform": PLATFORM, "stopped_pid": pid}


def _status() -> dict:
    pid = _read_pid()
    state = _read_state()
    running = pid is not None and _pid_alive(pid)
    return {
        "ok": True,
        "platform": PLATFORM,
        "running": running,
        "pid": pid,
        "offset": state.get("offset", 0),
        "state_dir": _state_dir(),
    }


def _send(args) -> dict:
    if len(args) < 2:
        return {"ok": False, "error": "usage: send <chat_id> <text>"}
    chat_id, text = args[0], " ".join(args[1:])
    try:
        token = _load_token()
    except RuntimeError as e:
        return {"ok": False, "error": str(e)}
    try:
        _api_call(
            token,
            "sendMessage",
            {"chat_id": chat_id, "text": _truncate(text)},
            timeout=15,
        )
    except (urllib.error.URLError, RuntimeError) as e:
        return {"ok": False, "error": str(e)}
    return {"ok": True, "platform": PLATFORM, "sent_to": chat_id, "len": len(text)}


def _schema():
    return {
        "name": f"gateway-{PLATFORM}",
        "version": "0.2.0",
        "description": "Telegram bot gateway — long-poll, forward to `cos agent ask`",
        "commands": {
            "start": {
                "description": "Start the long-poll loop (foreground; use under `cos service` for daemonization)",
                "parameters": [],
                "example": "cos app gateway-telegram start",
            },
            "stop": {
                "description": "Stop the running gateway loop",
                "parameters": [],
                "example": "cos app gateway-telegram stop",
            },
            "status": {
                "description": "Show running state + last update_id offset",
                "parameters": [],
                "example": "cos app gateway-telegram status",
            },
            "send": {
                "description": "Send a one-shot message to a chat id",
                "parameters": [
                    {"name": "chat_id", "type": "string", "required": True, "kind": "positional"},
                    {"name": "text", "type": "string", "required": True, "kind": "positional"},
                ],
                "example": "cos app gateway-telegram send 12345 'hello'",
            },
        },
    }


def run(command, args):
    if command == "__schema__":
        return _schema()
    if command == "start":
        return _start_loop()
    if command == "stop":
        return _stop()
    if command == "status":
        return _status()
    if command == "send":
        return _send(args)
    return {"error": f"unknown command: {command}"}


if __name__ == "__main__":
    cmd = os.environ.get("COS_COMMAND") or (sys.argv[1] if len(sys.argv) > 1 else "")
    raw_args = os.environ.get("COS_ARGS_JSON")
    if raw_args:
        cli_args = json.loads(raw_args)
    else:
        cli_args = sys.argv[2:]
    print(json.dumps(run(cmd, cli_args)))
