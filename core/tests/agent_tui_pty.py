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


class FixtureBroker:
    def __init__(self, path, case):
        self.path = path
        self.case = case
        self.requests = []
        self.errors = []
        self.cancelled = threading.Event()
        self.stopped = threading.Event()
        self.task_id = str(uuid.uuid4())
        self.cursor = 0
        self.requested_model = None
        self.session_id = SESSION_ID
        self.title = "Terminal integration fixture"
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
            "archived": False,
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
            "prompt": "Run the terminal integration fixture",
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
            return {"conversation": self.conversation()}
        if method == "agent.conversation.fork":
            if params["id"] != self.session_id:
                raise AssertionError("conversation fork addressed another session")
            self.session_id = FORK_SESSION_ID
            self.title = "Forked terminal fixture"
            return {"conversation": self.conversation()}
        if method == "memory.sessions":
            return {"n": 0, "sessions": []}
        if method == "memory.history":
            return {"session_id": SESSION_ID, "n": 0, "messages": []}
        if method in ("permission.pending", "permission.recent"):
            return {"requests": []}
        if method == "task.list":
            return {"jobs": []}
        if method == "task.submit":
            if params.get("session_id") != self.session_id:
                raise AssertionError("task was not submitted under the canonical conversation")
            if params.get("prompt") != "Run the terminal integration fixture":
                raise AssertionError("terminal input was changed or dropped")
            self.requested_model = params.get("model")
            return self.job("running")
        if method == "task.cancel":
            if params["id"] != self.task_id:
                raise AssertionError("cancellation addressed the wrong task")
            self.cancelled.set()
            return self.job("cancelled")
        if method in ("task.get", "task.status"):
            return self.job("cancelled" if self.cancelled.is_set() else "running")
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
                if self.case in ("complete", "resume", "commands"):
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
            terminal = self.case in ("complete", "resume", "commands") or self.cancelled.is_set()
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
        choices=("complete", "cancel", "commands", "plain", "resume"),
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
