#!/usr/bin/env python3
"""Exercise the Claw-owned Agent TUI through a Linux PTY."""

import argparse
import contextlib
from collections import Counter
import errno
import fcntl
import json
import os
from pathlib import Path
import pty
import pwd
import re
import select
import signal
import socket
import struct
import subprocess
import tempfile
import termios
import threading
import time
import uuid


SESSION_ID = "ses_001953abcdef0_123456789abc"
FORK_SESSION_ID = "ses_001953abcdef1_abcdef123456"
PRESENTATION_ID = "01234567-89ab-8cde-8123-456789abcdef"
ANSWER = "CLAW_TUI_PTY_COMPLETED_9F43"
RUNNING = "CLAW_TUI_PTY_RUNNING_3A91"
PENDING_APPROVAL_ID = "approval-center-pending"
RECENT_APPROVAL_ID = "approval-center-recent"
ACTIVITY_ID = "00000000-0000-4000-8000-000000000001"


class FixtureBroker:
    def __init__(self, path, case):
        self.path = path
        self.case = case
        self.requests = []
        self.errors = []
        self.cancelled = threading.Event()
        self.stopped = threading.Event()
        self.task_id = str(uuid.uuid4())
        self.history_running_id = "task-history-running"
        self.history_failed_id = "task-history-failed"
        self.history_retry_id = "task-history-retry"
        self.history_cancelled = False
        self.notification_state = "unread"
        self.notification_preferences = {
            "web_enabled": True,
            "desktop_enabled": True,
            "ntfy_enabled": False,
            "web_min_severity": "info",
            "desktop_min_severity": "info",
            "ntfy_min_severity": "warning",
            "muted_kinds": [],
            "dnd_start_minute_utc": None,
            "dnd_end_minute_utc": None,
            "critical_bypasses_dnd": True,
            "retention_days": 30,
            "ntfy_server": "https://ntfy.sh",
            "ntfy_topic": None,
        }
        self.activity_state = "active"
        self.activity_completion_note = None
        self.activity_task_id = "task-activity-fixture"
        self.cursor = 0
        self.requested_model = None
        self.session_id = SESSION_ID
        self.title = "Terminal integration fixture"
        self.archived = False
        self.listener = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.listener.bind(str(path))
        self.listener.listen(8)
        self.listener.settimeout(0.2)
        self.thread = threading.Thread(target=self.serve)

    def conversation(self):
        return {
            "id": self.session_id,
            "presentation_id": PRESENTATION_ID,
            "title": self.title,
            "created_at": "2026-01-01T00:00:00Z",
            "updated_at": "2026-01-01T00:00:00Z",
            "archived": self.archived,
            "deleted": False,
            "parent_id": None,
            "messages": [],
            "message_count": 0,
            "messages_truncated": False,
            "jobs": [],
            "job_count": 0,
            "jobs_truncated": False,
        }

    def job(self, status):
        return {
            "id": self.task_id,
            "session_id": self.session_id,
            "prompt": (
                "First line\nSecond line"
                if self.case == "multiline"
                else "Run the terminal integration fixture"
            ),
            "status": status,
            "created_at": "2026-01-01T00:00:00Z",
            "started_at": "2026-01-01T00:00:00Z",
            "finished_at": None if status == "running" else "2026-01-01T00:00:01Z",
            "provider": "ollama",
            "model": "tui-fixture",
            "requested_model": self.requested_model,
            "turns_used": 2,
            "response": ANSWER if status == "ok" else None,
            "error": None,
            "waiting_on": [],
        }

    def history_job(self, task_id, status):
        return {
            "id": task_id,
            "session_id": self.session_id,
            "prompt": f"Historical {status} task",
            "title": f"Historical {status} task",
            "status": status,
            "created_at": "2025-12-31T23:59:00Z",
            "started_at": "2025-12-31T23:59:01Z",
            "finished_at": (
                None
                if status in ("pending", "running", "waiting_approval")
                else "2025-12-31T23:59:02Z"
            ),
            "provider": "ollama",
            "model": "tui-fixture",
            "requested_model": "tui-fixture",
            "turns_used": 1,
            "response": None,
            "error": "fixture failure" if status == "error" else None,
            "waiting_on": [],
            "cancel_requested": False,
        }

    def notification(self):
        return {
            "schema": 1,
            "sequence": 1,
            "id": "notification-inbox-fixture",
            "owner_uid": os.geteuid(),
            "source": "agent",
            "kind": "task.completed",
            "severity": "info",
            "title": "Fixture task completed",
            "body": "The durable fixture task completed.",
            "delivery_policy": "immediate",
            "task_id": self.history_failed_id,
            "session_id": self.session_id,
            "state": self.notification_state,
            "occurrences": 1,
            "created_at_ms": 1767225600000,
            "updated_at_ms": 1767225600000,
            "actions": [{
                "id": "open-task",
                "label": "Open task",
                "uri": f"clawos://task/{self.history_failed_id}",
            }],
            "deliveries": [{
                "channel": "web",
                "state": "delivered",
                "attempts": 1,
            }],
        }

    def activity(self):
        return {
            "id": ACTIVITY_ID,
            "owner_uid": os.geteuid(),
            "title": "Fixture Activity",
            "goal": "Complete the fixture goal",
            "completion_criteria": "",
            "boundaries": "",
            "resources": [],
            "state": self.activity_state,
            "completion_note": self.activity_completion_note,
            "created_at": "2026-01-01T00:00:00Z",
            "updated_at": "2026-01-01T00:00:01Z",
        }

    def activity_job(self):
        value = self.history_job(self.activity_task_id, "pending")
        value["activity_id"] = ACTIVITY_ID
        return value

    def dispatch(self, method, params):
        if method == "daemon.status":
            return {"daemon": "clawd", "status": "running"}
        if method == "agent.conversation.create":
            return {"conversation": self.conversation()}
        if method == "agent.conversation.get":
            if params["id"] not in (self.session_id, PRESENTATION_ID):
                raise AssertionError("terminal did not preserve the native session identity")
            return {"conversation": self.conversation()}
        if method == "agent.conversation.list":
            return {
                "conversations": [self.conversation()],
                "conversation_count": 1,
                "conversations_truncated": False,
            }
        if method == "agent.conversation.update":
            if params["id"] != self.session_id:
                raise AssertionError("conversation update addressed another session")
            if "title" in params:
                self.title = params["title"]
            if "archived" in params:
                self.archived = params["archived"]
            return {"conversation": self.conversation()}
        if method == "agent.conversation.fork":
            if params["id"] != self.session_id:
                raise AssertionError("conversation fork addressed another session")
            self.session_id = FORK_SESSION_ID
            self.title = "Forked terminal fixture"
            return {"conversation": self.conversation()}
        if method == "agent.conversation.revert":
            if params["id"] != self.session_id or params.get("user_turns") != 1:
                raise AssertionError("conversation rewind changed identity or count")
            return {"conversation": self.conversation()}
        if method == "memory.sessions":
            return {"n": 0, "sessions": []}
        if method == "memory.history":
            return {"session_id": SESSION_ID, "n": 0, "messages": []}
        if method == "permission.pending" and self.case == "approval-center":
            return {
                "requests": [{
                    "id": PENDING_APPROVAL_ID,
                    "verb": "fs.write",
                    "scope": {"kind": "path", "value": "/home/claw/report.md"},
                    "session": self.session_id,
                    "reason": "Write the requested report",
                    "requested_at": 1767225600,
                    "risk": "high",
                    "requester": "Claw Agent",
                }]
            }
        if method == "permission.recent" and self.case == "approval-center":
            return {
                "requests": [{
                    "id": RECENT_APPROVAL_ID,
                    "verb": "net.dial",
                    "scope": {"kind": "host", "value": "example.com"},
                    "session": self.session_id,
                    "reason": "Fetch public release metadata",
                    "requested_at": 1767225500,
                    "risk": "medium",
                    "requester": "Claw Agent",
                    "decision": {
                        "outcome": "approved",
                        "decided_at": 1767225550,
                        "duration": "once",
                    },
                }]
            }
        if method in ("permission.pending", "permission.recent"):
            return {"requests": []}
        if method == "permission.status":
            return {
                "statuses": [
                    {
                        "id": approval_id,
                        "status": (
                            "pending"
                            if approval_id == PENDING_APPROVAL_ID
                            else "consumed"
                        ),
                    }
        if method == "notification.list":
                    return {
                        "schema": 1,
                        "cursor": 1,
                        "unread": 1 if self.notification_state == "unread" else 0,
                        "notifications": [self.notification()],
                    }
        if method in (
                    "notification.read",
                    "notification.acknowledge",
                    "notification.dismiss",
        ):
                    if params["id"] != "notification-inbox-fixture":
                        raise AssertionError("notification mutation addressed another record")
                    self.notification_state = {
                        "notification.read": "read",
                        "notification.acknowledge": "acknowledged",
                        "notification.dismiss": "dismissed",
                    }[method]
                    return self.notification()
        if method == "notification.preferences.get":
                    return self.notification_preferences
        if method == "notification.preferences.set":
                    self.notification_preferences = dict(params)
                    return self.notification_preferences
        if method == "activity.create":
                    if (
                        params.get("title") != "Fixture Activity"
                        or params.get("goal") != "Complete the fixture goal"
                    ):
                        raise AssertionError("Activity create changed its title or goal")
                    self.activity_state = "active"
                    self.activity_completion_note = None
                    return self.activity()
        if method == "activity.list":
                    return {"schema": 1, "activities": [self.activity()]}
        if method == "activity.get":
                    if params["id"] != ACTIVITY_ID:
                        raise AssertionError("Activity get addressed another goal")
                    return {
                        "schema": 1,
                        "activity": self.activity(),
                        "jobs": [],
                        "sessions": [],
                        "job_limit": 100,
                    }
        if method == "activity.transition":
                    if params["id"] != ACTIVITY_ID:
                        raise AssertionError("Activity transition addressed another goal")
                    self.activity_state = params["state"]
                    self.activity_completion_note = params.get("completion_note")
                    return self.activity()
        if method == "activity.run":
                    if params["id"] != ACTIVITY_ID:
                        raise AssertionError("Activity run addressed another goal")
                    return self.activity_job()
        if method == "activity.attention":
                    if params["id"] != ACTIVITY_ID:
                        raise AssertionError("Activity attention addressed another goal")
                    return {
                        "schema": 1,
                        "activity_id": ACTIVITY_ID,
                        "activity_state": self.activity_state,
                        "limit": 100,
                        "counts": {
                            "queued": 1,
                            "running": 0,
                            "waiting": 0,
                            "completed": 0,
                            "failed": 0,
                            "cancelled": 0,
                            "indeterminate": 0,
                            "pending_decisions": 0,
                            "unavailable_decisions": 0,
                            "unread_notifications": 0,
                        },
                        "decisions": [],
                        "issues": [],
                        "totals": {"decisions": 0, "issues": 0, "notifications": 0},
                        "has_more": {
                            "decisions": False,
                            "issues": False,
                            "notifications": False,
                        },
                        "notifications": [],
                    }
                    for approval_id in params["ids"]
                ]
            }
        if method == "task.list":
            if self.case == "task-center":
                return {
                    "jobs": [
                        self.history_job(
                            self.history_running_id,
                            "cancelled" if self.history_cancelled else "running",
                        ),
                        self.history_job(self.history_failed_id, "error"),
                    ]
                }
            return {"jobs": []}
        if method == "task.submit":
            if params.get("session_id") != self.session_id:
                raise AssertionError("task was not submitted under the canonical conversation")
            expected_prompt = (
                "First line\nSecond line"
                if self.case == "multiline"
                else "Run the terminal integration fixture"
            )
            if params.get("prompt") != expected_prompt:
                raise AssertionError("terminal input was changed or dropped")
            self.requested_model = params.get("model")
            return self.job("running")
        if method == "task.cancel":
            if params["id"] == self.history_running_id:
                self.history_cancelled = True
                value = self.history_job(self.history_running_id, "cancelled")
                value["cancelled"] = True
                return value
            if params["id"] == self.task_id:
                self.cancelled.set()
                value = self.job("cancelled")
                value["cancelled"] = True
                return value
            raise AssertionError("cancellation addressed the wrong task")
        if method in ("task.get", "task.status"):
            if params["id"] == self.history_running_id:
                return self.history_job(
                    self.history_running_id,
                    "cancelled" if self.history_cancelled else "running",
                )
            if params["id"] == self.history_failed_id:
                return self.history_job(self.history_failed_id, "error")
            if params["id"] == self.history_retry_id:
                return self.history_job(self.history_retry_id, "pending")
            return self.job("cancelled" if self.cancelled.is_set() else "running")
        if method == "task.retry":
            if params["id"] != self.history_failed_id:
                raise AssertionError("retry addressed the wrong task")
            return self.history_job(self.history_retry_id, "pending")
        if method == "task.stream":
            if params["id"] != self.task_id:
                raise AssertionError("stream addressed the wrong task")
            events = []
            if params.get("cursor", 0) == 0:
                events = [
                    {"event": {"kind": "text_delta", "text": RUNNING + "\n"}},
                    {"event": {"kind": "tool_use_start", "id": "tool-1", "name": "cos_sysinfo"}},
                    {"event": {
                        "kind": "tool_use", "id": "tool-1",
                        "name": "cos_sysinfo", "input": None,
                    }},
                    {"event": {
                        "kind": "done", "finish": "tool_use",
                        "usage": {
                            "input_tokens": 12, "output_tokens": 4,
                            "cache_read_tokens": 0, "cache_write_tokens": 0,
                        },
                    }},
                    {"progress": {"kind": "tool_start", "id": "tool-1", "name": "cos_sysinfo"}},
                ]
                if self.case in (
                    "complete",
                    "resume",
                    "commands",
                    "confirmations",
                    "multiline",
                    "approval-center",
                    "activity-lifecycle",
                    "notification-inbox",
                    "task-center",
                ):
                    events.extend([
                        {"progress": {
                            "kind": "tool_result", "id": "tool-1", "name": "cos_sysinfo",
                            "ok": True, "latency_ms": 42,
                        }},
                        {"event": {"kind": "text_delta", "text": "\n## Result\n\n" + ANSWER}},
                        {"event": {
                            "kind": "done", "finish": "stop",
                            "usage": {
                                "input_tokens": 20, "output_tokens": 10,
                                "cache_read_tokens": 0, "cache_write_tokens": 0,
                            },
                        }},
                    ])
                self.cursor = len(events)
            terminal = self.case in (
                "complete",
                "resume",
                "commands",
                "confirmations",
                "multiline",
                "approval-center",
                "activity-lifecycle",
                "notification-inbox",
                "task-center",
            ) or self.cancelled.is_set()
            if not terminal:
                time.sleep(0.05)
            status = "cancelled" if self.cancelled.is_set() else "ok" if terminal else "running"
            return {
                "cursor": self.cursor,
                "events": events,
                "terminal": terminal,
                "job": self.job(status),
            }
        raise AssertionError(f"unexpected broker method: {method}")

    @staticmethod
    def read_exact(connection, length):
        chunks = bytearray()
        while len(chunks) < length:
            chunk = connection.recv(length - len(chunks))
            if not chunk:
                raise EOFError("truncated broker request")
            chunks.extend(chunk)
        return bytes(chunks)

    def serve(self):
        while not self.stopped.is_set():
            try:
                connection, _ = self.listener.accept()
            except socket.timeout:
                continue
            with connection:
                connection.settimeout(3)
                try:
                    header = self.read_exact(connection, 10)
                    magic, kind, reserved, length = struct.unpack(">4sBBI", header)
                    if (magic, kind, reserved) != (b"CBK1", 1, 0) or length > 1024 * 1024:
                        raise AssertionError("invalid broker framing")
                    request = json.loads(self.read_exact(connection, length))
                    self.requests.append(request)
                    try:
                        result = self.dispatch(request["command"], request.get("params", {}))
                        response = {
                            "v": 2, "id": request["id"], "ok": True,
                            "result": result, "error": None,
                        }
                    except (AssertionError, KeyError) as error:
                        self.errors.append(str(error))
                        response = {
                            "v": 2, "id": request["id"], "ok": False, "result": None,
                            "error": {"code": "execution_failed", "message": str(error)},
                        }
                    encoded = json.dumps(response).encode()
                    connection.sendall(struct.pack(">4sBBI", b"CBK1", 2, 0, len(encoded)) + encoded)
                except (OSError, EOFError, ValueError, AssertionError) as error:
                    self.errors.append(str(error))

    def close(self):
        self.stopped.set()
        self.thread.join(timeout=5)
        self.listener.close()
        if self.thread.is_alive():
            raise AssertionError("fixture broker did not stop")


def read_terminal(master, output, deadline, predicate, exit_process=None):
    pending = b""
    while time.monotonic() < deadline:
        if predicate(output):
            return
        readable, _, _ = select.select([master], [], [], 0.1)
        if not readable:
            continue
        try:
            chunk = os.read(master, 65536)
        except OSError as error:
            if error.errno == errno.EIO:
                if exit_process is not None:
                    exit_process.wait(timeout=max(0.1, deadline - time.monotonic()))
                break
            raise
        if not chunk:
            if exit_process is not None:
                exit_process.wait(timeout=max(0.1, deadline - time.monotonic()))
            break
        output.extend(chunk)
        pending += chunk
        for query, response in (
            (b"\x1b[6n", b"\x1b[1;1R"),
            (b"\x1b[c", b"\x1b[?1;2c"),
            (b"\x1b[>c", b"\x1b[>0;0;0c"),
            (b"\x1b[?u", b"\x1b[?0u"),
            (b"\x1b]10;?\x1b\\", b"\x1b]10;rgb:ffff/ffff/ffff\x1b\\"),
            (b"\x1b]11;?\x1b\\", b"\x1b]11;rgb:0000/0000/0000\x1b\\"),
        ):
            if query in pending:
                os.write(master, response)
                pending = pending.replace(query, b"")
        pending = pending[-16:]
    if not predicate(output):
        errors = re.findall(r"Error: ([^\r\n]+)", output.decode(errors="replace"))
        detail = errors[-1] if errors else "no completed interaction before timeout"
        raise AssertionError(f"terminal interaction did not reach its expected state: {detail}")


def terminal_child():
    os.setsid()
    fcntl.ioctl(0, termios.TIOCSCTTY, 0)


def send_prompt(master, output, text):
    os.write(master, text.encode())
    settled = time.monotonic() + 0.4
    read_terminal(master, output, settled + 2, lambda _data: time.monotonic() >= settled)
    os.write(master, b"\r")


def process_diagnostics(pid):
    records = []
    pending = [pid]
    while pending and len(records) < 16:
        current = pending.pop()
        root = Path("/proc") / str(current)
        try:
            status = root.joinpath("status").read_text().splitlines()
            records.append({
                "pid": current,
                "comm": root.joinpath("comm").read_text().strip(),
                "status": [line for line in status if line.startswith(("State:", "PPid:", "Threads:"))],
                "wait": root.joinpath("wchan").read_text().strip(),
            })
            children = root.joinpath("task", str(current), "children").read_text()
            pending.extend(int(child) for child in children.split())
        except OSError as error:
            records.append({"pid": current, "inspection_error": str(error)})
    return records


def run(cos, case, transcript, original_namespace, trace):
    if os.geteuid() == 0:
        raise RuntimeError("run this fixture as a non-root account, like ordinary Agent chat")
    output = bytearray()
    with tempfile.TemporaryDirectory(prefix="claw-tui-pty-") as temporary, contextlib.ExitStack() as cleanup:
        root = Path(temporary)
        if (
            not re.fullmatch(r"mnt:\[\d+\]", original_namespace)
            or os.readlink("/proc/self/ns/mnt") == original_namespace
        ):
            raise RuntimeError("launch through unshare --user --map-current-user --keep-caps --mount --net")
        home = Path(pwd.getpwuid(os.geteuid()).pw_dir)
        shadow_home = root / "home"
        shadow_home.mkdir(mode=0o700)
        subprocess.run(["mount", "--bind", str(shadow_home), str(home)], check=True)
        cleanup.callback(subprocess.run, ["umount", str(home)], check=True)
        config = root / "config.json"
        config.write_text(json.dumps({
            "agent": {
                "provider": "ollama", "model": "tui-fixture",
                "base_url": "http://127.0.0.1:1",
            },
        }))
        broker = FixtureBroker(root / "clawd.sock", case)
        master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 36, 110, 0, 0))
        environment = {
            **os.environ,
            "TERM": "xterm-256color",
            "CLAWD_SOCKET": str(broker.path),
            "COS_CONFIG_PATH": str(config),
            "COS_DATA_DIR": str(root / "data"),
            "COS_USER_DATA_DIR": str(home / ".local" / "share" / "cos"),
        }
        command = [str(cos), "agent", "chat", "--tui"]
        if case == "plain":
            command = [str(cos), "agent", "chat", "--plain", "--no-memory"]
        elif case == "resume":
            command.extend(["--session", PRESENTATION_ID])
        if trace:
            command = ["strace", "-f", "-tt", "-o", str(trace), *command]
        process = subprocess.Popen(
            command,
            stdin=slave, stdout=slave, stderr=slave, cwd=root, env=environment,
            preexec_fn=terminal_child,
        )
        os.close(slave)
        broker.thread.start()
        try:
            if case == "plain":
                read_terminal(
                    master, output, time.monotonic() + 30,
                    lambda data: b"you> " in data,
                )
                send_prompt(master, output, "/quit")
                read_terminal(
                    master, output, time.monotonic() + 15,
                    lambda _data: process.poll() is not None,
                    exit_process=process,
                )
                if process.wait(timeout=5) != 0 or broker.requests:
                    raise AssertionError("plain chat did not preserve its independent line interface")
                print(json.dumps({"case": case, "task_submissions": 0, "completed": True}))
                return
            ready_at = None

            def ready(data):
                nonlocal ready_at
                opened = any(
                    request["command"] in (
                        "agent.conversation.create",
                        "agent.conversation.get",
                    )
                    for request in broker.requests
                )
                if opened and b"CLAW" in data:
                    if ready_at is None:
                        ready_at = time.monotonic()
                    return time.monotonic() - ready_at >= 0.5
                return False

            read_terminal(
                master, output, time.monotonic() + 45,
                ready,
            )
            if case == "commands":
                send_prompt(master, output, "/resume")
                read_terminal(
                    master,
                    output,
                    time.monotonic() + 15,
                    lambda _data: any(
                        request["command"] == "agent.conversation.list"
                        for request in broker.requests
                    ),
                )
                os.write(master, b"\r")
                read_terminal(
                    master,
                    output,
                    time.monotonic() + 15,
                    lambda _data: any(
                        request["command"] == "agent.conversation.get"
                        for request in broker.requests
                    ),
                )
                send_prompt(master, output, "/rename Command fixture")
                read_terminal(
                    master,
                    output,
                    time.monotonic() + 15,
                    lambda _data: any(
                        request["command"] == "agent.conversation.update"
                        and request["params"].get("title") == "Command fixture"
                        for request in broker.requests
                    ),
                )
                send_prompt(master, output, "/fork")
                read_terminal(
                    master,
                    output,
                    time.monotonic() + 15,
                    lambda _data: any(
                        request["command"] == "agent.conversation.fork"
                        for request in broker.requests
                    ),
                )
            if case == "confirmations":
                send_prompt(master, output, "/archive")
                settled = time.monotonic() + 0.5
                read_terminal(
                    master,
                    output,
                    settled + 2,
                    lambda _data: time.monotonic() >= settled,
                )
                if any(
                    request["command"] == "agent.conversation.update"
                    for request in broker.requests
                ):
                    raise AssertionError("archive mutated before confirmation")
                os.write(master, b"n")
                time.sleep(0.2)
                send_prompt(master, output, "/archive")
                time.sleep(0.5)
                os.write(master, b"y")
                read_terminal(
                    master,
                    output,
                    time.monotonic() + 15,
                    lambda _data: any(
                        request["command"] == "agent.conversation.update"
                        and request["params"].get("archived") is True
                        for request in broker.requests
                    ),
                )
                send_prompt(master, output, "/unarchive")
                read_terminal(
                    master,
                    output,
                    time.monotonic() + 15,
                    lambda _data: any(
                        request["command"] == "agent.conversation.update"
                        and request["params"].get("archived") is False
                        for request in broker.requests
                    ),
                )
                send_prompt(master, output, "/rewind 1")
                time.sleep(0.5)
                os.write(master, b"y")
                read_terminal(
                    master,
                    output,
                    time.monotonic() + 15,
                    lambda _data: any(
                        request["command"] == "agent.conversation.revert"
                        for request in broker.requests
                    ),
                )
            if case == "task-center":
                send_prompt(master, output, "/tasks")
                read_terminal(
                    master,
                    output,
                    time.monotonic() + 15,
                    lambda _data: any(
                        request["command"] == "task.list"
                        for request in broker.requests
                    ),
                )
                os.write(master, b"\r")
                read_terminal(
                    master,
                    output,
                    time.monotonic() + 15,
                    lambda _data: any(
                        request["command"] == "task.get"
                        and request["params"].get("id") == broker.history_running_id
                        for request in broker.requests
                    ),
                )
                os.write(master, b"c")
                read_terminal(
                    master,
                    output,
                    time.monotonic() + 15,
                    lambda _data: any(
                        request["command"] == "task.cancel"
                        and request["params"].get("id") == broker.history_running_id
                        for request in broker.requests
                    ),
                )
                time.sleep(0.2)
                os.write(master, b"\x1b")
            if case == "approval-center":
                send_prompt(master, output, "/approvals")
                read_terminal(
                    master,
                    output,
                    time.monotonic() + 15,
                    lambda _data: all(
                        any(request["command"] == command for request in broker.requests)
                        for command in (
                            "permission.pending",
                            "permission.recent",
                            "permission.status",
                        )
                    ),
                )
                os.write(master, b"\r")
                time.sleep(0.2)
                os.write(master, b"\x1b")
                send_prompt(master, output, "/approvals")
                read_terminal(
                    master,
                    output,
                    time.monotonic() + 15,
                    lambda _data: sum(
                        request["command"] == "permission.recent"
                        for request in broker.requests
                    )
                    >= 2,
                )
                os.write(master, b"\x1b[B\r")
                time.sleep(0.2)
                os.write(master, b"a")
                time.sleep(0.2)
                os.write(master, b"\x1b")
            if case == "notification-inbox":
                send_prompt(master, output, "/inbox")
                read_terminal(
                    master,
                    output,
                    time.monotonic() + 15,
                    lambda _data: any(
                        request["command"] == "notification.list"
                        for request in broker.requests
                    ),
                )
                os.write(master, b"\r")
                time.sleep(0.2)
                for key, command in [
                    (b"m", "notification.read"),
                    (b"a", "notification.acknowledge"),
                    (b"d", "notification.dismiss"),
                ]:
                    os.write(master, key)
                    read_terminal(
                        master,
                        output,
                        time.monotonic() + 15,
                        lambda _data, expected=command: any(
                            request["command"] == expected
                            for request in broker.requests
                        ),
                    )
                os.write(master, b"\x1b")
            if case == "activity-lifecycle":
                    send_prompt(master, output, "/activities")
                    read_terminal(
                        master,
                        output,
                        time.monotonic() + 15,
                        lambda _data: any(
                            request["command"] == "activity.list"
                            for request in broker.requests
                        ),
                    )
                    os.write(master, b"\r")
                    read_terminal(
                        master,
                        output,
                        time.monotonic() + 15,
                        lambda _data: any(
                            request["command"] == "activity.get"
                            for request in broker.requests
                        ),
                    )
                    os.write(master, b"\x1b")
                    send_prompt(
                        master,
                        output,
                        "/activity-create Fixture Activity | Complete the fixture goal",
                    )
                    read_terminal(
                        master,
                        output,
                        time.monotonic() + 15,
                        lambda _data: any(
                            request["command"] == "activity.create"
                            for request in broker.requests
                        )
                        and sum(
                            request["command"] == "activity.get"
                            for request in broker.requests
                        )
                        >= 2,
                    )
                    os.write(master, b"p")
                    read_terminal(
                        master,
                        output,
                        time.monotonic() + 15,
                        lambda _data: any(
                            request["command"] == "activity.transition"
                            and request["params"].get("state") == "paused"
                            for request in broker.requests
                        ),
                    )
                    os.write(master, b"u")
                    read_terminal(
                        master,
                        output,
                        time.monotonic() + 15,
                        lambda _data: any(
                            request["command"] == "activity.transition"
                            and request["params"].get("state") == "active"
                            for request in broker.requests
                        ),
                    )
                    os.write(master, b"r")
                    read_terminal(
                        master,
                        output,
                        time.monotonic() + 15,
                        lambda _data: any(
                            request["command"] == "activity.run"
                            for request in broker.requests
                        ),
                    )
                    os.write(master, b"\x1b")
                    send_prompt(master, output, f"/activity {ACTIVITY_ID}")
                    read_terminal(
                        master,
                        output,
                        time.monotonic() + 15,
                        lambda _data: sum(
                            request["command"] == "activity.get"
                            for request in broker.requests
                        )
                        >= 2,
                    )
                    os.write(master, b"a")
                    read_terminal(
                        master,
                        output,
                        time.monotonic() + 15,
                        lambda _data: any(
                            request["command"] == "activity.attention"
                            for request in broker.requests
                        ),
                    )
                    os.write(master, b"\x1b")
                    send_prompt(
                        master,
                        output,
                        f"/activity-complete {ACTIVITY_ID} | User confirmed completion",
                    )
                    time.sleep(0.2)
                    os.write(master, b"y")
                    read_terminal(
                        master,
                        output,
                        time.monotonic() + 15,
                        lambda _data: any(
                            request["command"] == "activity.transition"
                            and request["params"].get("state") == "completed"
                            and request["params"].get("completion_note")
                            == "User confirmed completion"
                            for request in broker.requests
                        ),
                    )
                    send_prompt(master, output, f"/activity-resume {ACTIVITY_ID}")
                    read_terminal(
                        master,
                        output,
                        time.monotonic() + 15,
                        lambda _data: sum(
                            request["command"] == "activity.transition"
                            and request["params"].get("state") == "active"
                            for request in broker.requests
                        )
                        >= 2,
                    )
                    send_prompt(master, output, f"/activity-cancel {ACTIVITY_ID}")
                    time.sleep(0.2)
                    os.write(master, b"y")
                    read_terminal(
                        master,
                        output,
                        time.monotonic() + 15,
                        lambda _data: any(
                            request["command"] == "activity.transition"
                            and request["params"].get("state") == "cancelled"
                            for request in broker.requests
                        ),
                    )
                send_prompt(master, output, "/notify-settings")
                read_terminal(
                    master,
                    output,
                    time.monotonic() + 15,
                    lambda _data: any(
                        request["command"] == "notification.preferences.get"
                        for request in broker.requests
                    ),
                )
                os.write(master, b"w")
                read_terminal(
                    master,
                    output,
                    time.monotonic() + 15,
                    lambda _data: any(
                        request["command"] == "notification.preferences.set"
                        and request["params"].get("web_enabled") is False
                        for request in broker.requests
                    ),
                )
                os.write(master, b"q")
                read_terminal(
                    master,
                    output,
                    time.monotonic() + 15,
                    lambda _data: any(
                        request["command"] == "notification.preferences.set"
                        and request["params"].get("dnd_start_minute_utc") == 0
                        and request["params"].get("dnd_end_minute_utc") == 0
                        for request in broker.requests
                    ),
                )
                os.write(master, b"\x1b")
                send_prompt(master, output, "/tasks")
                read_terminal(
                    master,
                    output,
                    time.monotonic() + 15,
                    lambda _data: sum(
                        request["command"] == "task.list"
                        for request in broker.requests
                    )
                    >= 2,
                )
                os.write(master, b"\x1b[B\r")
                read_terminal(
                    master,
                    output,
                    time.monotonic() + 15,
                    lambda _data: any(
                        request["command"] == "task.get"
                        and request["params"].get("id") == broker.history_failed_id
                        for request in broker.requests
                    ),
                )
                os.write(master, b"r")
                read_terminal(
                    master,
                    output,
                    time.monotonic() + 15,
                    lambda _data: any(
                        request["command"] == "task.retry"
                        and request["params"].get("id") == broker.history_failed_id
                        for request in broker.requests
                    ),
                )
                time.sleep(0.2)
                os.write(master, b"\x1b")
            if case == "multiline":
                os.write(master, b"\x1b[200~First line\nSecond line\x1b[201~")
                settled = time.monotonic() + 0.4
                read_terminal(
                    master,
                    output,
                    settled + 2,
                    lambda _data: time.monotonic() >= settled,
                )
                os.write(master, b"\r")
            else:
                send_prompt(master, output, "Run the terminal integration fixture")
            marker = ANSWER if case == "complete" else RUNNING
            read_terminal(
                master, output, time.monotonic() + 30,
                lambda data: marker.encode() in data,
            )
            if case == "cancel":
                os.write(master, b"\x1b")
                read_terminal(
                    master, output, time.monotonic() + 15,
                    lambda _data: broker.cancelled.is_set(),
                )
            send_prompt(master, output, "/quit")
            read_terminal(
                master, output, time.monotonic() + 15,
                lambda _data: process.poll() is not None,
                exit_process=process,
            )
            if process.wait(timeout=5) != 0:
                raise AssertionError(f"TUI exited with {process.returncode}")
            if broker.errors:
                raise AssertionError("; ".join(broker.errors))
            submissions = sum(request["command"] == "task.submit" for request in broker.requests)
            if submissions != 1:
                raise AssertionError(f"expected one actual task submission, got {submissions}")
            if case == "resume" and any(
                request["command"] == "agent.conversation.create"
                for request in broker.requests
            ):
                raise AssertionError("resume created a replacement conversation")
            print(json.dumps({"case": case, "task_submissions": submissions, "completed": True}))
        except AssertionError:
            if transcript:
                transcript.with_suffix(".diagnostics.json").write_text(json.dumps({
                    "requests": list(dict.fromkeys(request["command"] for request in broker.requests)),
                    "request_counts": dict(Counter(request["command"] for request in broker.requests)),
                    "broker_errors": broker.errors,
                    "processes": process_diagnostics(process.pid),
                }, indent=2))
            raise
        finally:
            if process.poll() is None:
                os.killpg(process.pid, signal.SIGTERM)
                try:
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    os.killpg(process.pid, signal.SIGKILL)
                    process.wait(timeout=5)
            os.close(master)
            broker.close()
            if transcript:
                transcript.write_bytes(output)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--cos", type=Path, required=True)
    parser.add_argument(
        "--case",
        choices=(
            "complete",
            "cancel",
            "commands",
            "confirmations",
            "multiline",
            "approval-center",
            "activity-lifecycle",
            "notification-inbox",
            "plain",
            "resume",
            "task-center",
        ),
        default="complete",
    )
    parser.add_argument("--transcript", type=Path)
    parser.add_argument("--original-mount-namespace", required=True)
    parser.add_argument("--trace", type=Path)
    arguments = parser.parse_args()
    run(
        arguments.cos.resolve(strict=True),
        arguments.case,
        arguments.transcript,
        arguments.original_mount_namespace,
        arguments.trace.resolve() if arguments.trace else None,
    )
