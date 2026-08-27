"""Telegram gateway — long-poll the Telegram Bot API and forward
inbound messages to `cos agent ask`, replying with the answer.

Phase 9 implementation, hardened in the post-incident sweep:

* Every inbound message goes through an explicit sender allowlist
  (env ``COS_TELEGRAM_ALLOWED_CHATS``, comma-separated chat IDs).
  Anything else is dropped with a polite "not authorised" reply, so
  the public side of the bot can't drive ``cos agent ask`` for free.

* Per-chat token-bucket rate limit (5 calls / 60s by default; tunable
  via ``COS_TELEGRAM_RPM``). Bursts get a "rate-limited, try again"
  reply instead of being silently dropped or queued.

* The ``cos agent ask`` subprocess is now timed out and run with a
  scrubbed environment (``stdin=DEVNULL``), so a hung agent can never
  pin the long-poll loop forever or inherit ambient secrets.

* All outbound HTTP funnels through
  :func:`apps.gateway._shared.safe_egress.safe_urlopen` which enforces
  ``policy.require("net.dial", host="api.telegram.org")``
  and refuses to follow 30x redirects.

Wiring:

  cos credential store telegram_bot_token <token>      # one-time
  export COS_TELEGRAM_ALLOWED_CHATS=12345,67890         # required
  cos app gateway-telegram start                        # foreground loop

Or run under `cos service` so it survives across sessions.

State files live under
``$COS_DATA_DIR/apps/gateway-telegram/`` (or
``$LOCALAPPDATA/cos/apps/gateway-telegram/`` on Windows). The loop
keeps:

  * ``state.json``  — last processed update_id (Telegram offset).
  * ``gateway.pid`` — PID of the running ``start`` loop, used by
    ``status`` and ``stop``.

Stdlib only.
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


# Sibling ``_shared`` package import (script-mode invocation).
sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

from _shared import atomic, gateway_memory, inbound, safe_egress, safe_subprocess  # noqa: E402


PLATFORM = "telegram"
TG_API_BASE = "https://api.telegram.org"
TG_API_HOST = "api.telegram.org"

# Long-poll timeout (Telegram caps at 50; we use 25 so the socket
# never feels frozen to a watcher).
LONG_POLL_TIMEOUT_S = 25
# Backoff schedule for transport / 5xx failures.
BACKOFF_SCHEDULE_S = [1, 2, 5, 10, 30, 60]
# Truncate replies past Telegram's 4096-char message limit.
TG_MESSAGE_LIMIT = 4096
# Default rate-limit budget per chat. Override via COS_TELEGRAM_RPM.
DEFAULT_RPM = 5
# Hard cap on how long a `cos agent ask` invocation may run before
# the gateway gives up and tells the user.
AGENT_TIMEOUT_S = 60

# Environment variable names. Kept as module constants so tests can
# poke them out of band.
ENV_ALLOWED_CHATS = "COS_TELEGRAM_ALLOWED_CHATS"
ENV_RPM = "COS_TELEGRAM_RPM"


# In-process rate limiter shared across all inbound calls. Build it
# lazily so test harnesses can reset it.
_RATE_LIMITER: inbound.TokenBucket | None = None


def _rate_limiter() -> inbound.TokenBucket:
    global _RATE_LIMITER
    if _RATE_LIMITER is None:
        try:
            rpm = int(os.environ.get(ENV_RPM, str(DEFAULT_RPM)))
            if rpm <= 0:
                rpm = DEFAULT_RPM
        except ValueError:
            rpm = DEFAULT_RPM
        _RATE_LIMITER = inbound.TokenBucket(capacity=rpm, refill_seconds=60.0)
    return _RATE_LIMITER


def _reset_rate_limiter_for_tests() -> None:
    """Test hook — re-reads env on next ``_rate_limiter()`` call."""
    global _RATE_LIMITER
    _RATE_LIMITER = None


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
    # Atomic via tmp + fsync + replace + dir-fsync so a crash mid-write
    # can't leave a half-written offset that drops or replays updates
    # on the next run.
    atomic.atomic_write_json(_state_path(), state)


def _read_pid() -> int | None:
    try:
        with open(_pid_path(), "r", encoding="utf-8") as f:
            return int(f.read().strip())
    except (FileNotFoundError, ValueError):
        return None


def _write_pid(pid: int) -> None:
    atomic.atomic_write_text(_pid_path(), str(pid), mode=0o644)


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
    val, err = safe_subprocess.safe_credential_load(
        "telegram_bot_token", timeout=10.0, cos_bin=_cos_bin()
    )
    if val is None:
        raise RuntimeError(
            err
            or (
                "telegram_bot_token not in credential store; "
                "run `cos credential store telegram_bot_token <token>` first"
            )
        )
    return val


def _api_call(token: str, method: str, params: dict, timeout: float) -> dict:
    """POST to https://api.telegram.org/bot<token>/<method>. Returns
    parsed JSON. Raises urllib.error.URLError on transport failure;
    raises RuntimeError on Telegram-side `ok: false`."""
    url = f"{TG_API_BASE}/bot{token}/{method}"
    body = urllib.parse.urlencode(params).encode("utf-8")
    headers = {"Content-Type": "application/x-www-form-urlencoded"}
    _, _, raw = safe_egress.safe_urlopen(
        "POST",
        url,
        headers=headers,
        body=body,
        timeout=timeout,
        verb_id="net.dial",
    )
    payload = json.loads(raw.decode("utf-8"))
    if not payload.get("ok"):
        raise RuntimeError(
            f"telegram api error: {payload.get('description', 'unknown')}"
        )
    return payload


def _ask_agent(chat_id: object, text: str) -> str:
    """Run ``cos agent ask <text>`` with the kernel having authorised
    the inbound first. Bounded timeout, scrubbed env, no inherited
    stdin.

    Raises:
        inbound.SenderNotAllowed: ``chat_id`` not in
            ``COS_TELEGRAM_ALLOWED_CHATS``.
        inbound.RateLimited:      Per-chat budget exhausted.
    """
    inbound.verify_sender(chat_id, ENV_ALLOWED_CHATS)

    # Kernel-side gate as well — even an allowlisted chat must clear
    # the policy verb. The kernel can deny per-session.
    try:
        from cos_runtime import policy as _policy  # type: ignore
    except Exception:  # pragma: no cover - missing only outside kernel
        _policy = None
    if _policy is not None:
        _policy.require("data.inbox.write", name=str(chat_id))

    if not _rate_limiter().try_consume(str(chat_id)):
        raise inbound.RateLimited(
            f"chat {chat_id} exceeded {DEFAULT_RPM} req/min budget"
        )

    try:
        proc = safe_subprocess.safe_subprocess(
            [_cos_bin(), "agent", "ask", text],
            timeout=AGENT_TIMEOUT_S,
        )
    except subprocess.TimeoutExpired:
        return f"[agent timeout after {AGENT_TIMEOUT_S}s]"
    except FileNotFoundError:
        return "[agent unavailable: cos binary not on PATH]"

    if proc.returncode != 0:
        # Surface a short error so the user gets feedback in chat
        # instead of silent drops.
        stderr_preview = (proc.stderr or "").strip()[:200]
        stdout_preview = (proc.stdout or "").strip()[:200]
        return f"[agent error] {stderr_preview or stdout_preview or 'non-zero exit'}"
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


def _send_reply(token: str, chat_id: object, text: str) -> None:
    try:
        _api_call(
            token,
            "sendMessage",
            {"chat_id": chat_id, "text": _truncate(text)},
            timeout=15,
        )
    except (urllib.error.URLError, RuntimeError, safe_egress.EgressBlocked) as e:
        log_event("error", "send failed", chat_id=chat_id, err=str(e))


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

    try:
        reply = _ask_agent(chat_id, text.strip())
    except inbound.SenderNotAllowed as e:
        log_event("warn", "sender not allowed", chat_id=chat_id, err=str(e))
        _send_reply(
            token,
            chat_id,
            "Sorry — this gateway is not configured to take requests from this chat.",
        )
        return
    except inbound.RateLimited as e:
        log_event("warn", "rate limited", chat_id=chat_id, err=str(e))
        _send_reply(
            token,
            chat_id,
            "Rate-limited (max ~5 requests / minute per chat). Please slow down.",
        )
        return
    except Exception as e:
        # PermissionDenied from the kernel and anything else we did
        # not anticipate. Don't surface implementation details to the
        # chat (could include token fragments in pathological cases).
        denial = getattr(e, "denial", None)
        log_event(
            "error",
            "agent dispatch failed",
            chat_id=chat_id,
            err=str(e),
            kernel=denial,
        )
        _send_reply(token, chat_id, "Internal error handling that message.")
        return

    _send_reply(token, chat_id, reply)


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
            except (urllib.error.URLError, safe_egress.EgressBlocked) as e:
                # Transient transport failure or local egress
                # rejection (DNS flap, IMDS-shaped redirect). Back
                # off exponentially up to 60s before retrying so we
                # don't hot-loop against a misbehaving network.
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
        # generally manage process lifetime instead. Bounded timeout
        # so a wedged taskkill can't hang ``stop`` forever.
        try:
            safe_subprocess.safe_subprocess(
                ["taskkill", "/PID", str(pid), "/F"], timeout=10.0
            )
        except (FileNotFoundError, subprocess.TimeoutExpired):
            pass
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
        "allowlist_env": ENV_ALLOWED_CHATS,
        "allowlist_configured": bool(os.environ.get(ENV_ALLOWED_CHATS, "").strip()),
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
    except (urllib.error.URLError, RuntimeError, safe_egress.EgressBlocked) as e:
        return {"ok": False, "error": str(e)}
    return {"ok": True, "platform": PLATFORM, "sent_to": chat_id, "len": len(text)}


def run(command, args):
    from canonical_argv import normalize_canonical_argv
    if isinstance(args, list):
        args = normalize_canonical_argv(args)
    if command == "start":
        return _start_loop()
    if command == "stop":
        return _stop()
    if command == "status":
        return _status()
    if command == "send":
        result = _send(args)
        chat_id = ""
        text = ""
        if isinstance(args, list) and len(args) >= 2:
            chat_id = str(args[0])
            text = " ".join(str(a) for a in args[1:])
        elif isinstance(args, dict):
            chat_id = str(args.get("chat_id", "") or "")
            text = str(args.get("text", "") or "")
        gateway_memory.remember_send(PLATFORM, result, channel_id=chat_id, text=text)
        return result
    return {"error": f"unknown command: {command}"}


if __name__ == "__main__":
    cmd = os.environ.get("COS_COMMAND") or (sys.argv[1] if len(sys.argv) > 1 else "")
    raw_args = os.environ.get("COS_ARGS_JSON")
    if raw_args:
        cli_args = json.loads(raw_args)
    else:
        cli_args = sys.argv[2:]
    print(json.dumps(run(cmd, cli_args)))
