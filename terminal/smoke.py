#!/usr/bin/env python3
"""Probe the real binary's remote startup, without credentials or model turns.

Run in a Linux network namespace: unshare --user --map-root-user --net
python3 -B terminal/smoke.py. The fixture is not a backend implementation.
"""

import argparse
import base64
from contextlib import contextmanager
import errno
import fcntl
import hashlib
import json
import os
from pathlib import Path
import pty
import re
import select
import shutil
import socket
import struct
import subprocess
import sys
import termios
import threading
import time
import uuid


ROOT = Path(__file__).resolve().parent.parent
STOP = "CLAW_TUI_BUILD_SMOKE_STOP"
FRONTEND_OVERRIDES = (
    'cli_auth_credentials_store="ephemeral"',
    "check_for_update_on_startup=false",
    "analytics.enabled=false",
    "feedback.enabled=false",
    'otel.exporter="none"',
    'otel.trace_exporter="none"',
    'otel.metrics_exporter="none"',
    "otel.log_user_prompt=false",
)


@contextmanager
def fixture_directory(project_tmpfs=False):
    directory = ROOT / "build" / "agent-tui" / ("smoke-" + uuid.uuid4().hex[:8])
    directory.mkdir(parents=True)
    mounted = False
    try:
        if project_tmpfs:
            os.unshare(os.CLONE_NEWNS)
            subprocess.run(["mount", "--make-rprivate", "/"], check=True)
            subprocess.run(
                [
                    "mount", "-t", "tmpfs", "-o", "size=64M,mode=700,nosuid,nodev",
                    "tmpfs", str(directory),
                ],
                check=True,
            )
            mounted = True
        yield directory
    finally:
        if mounted:
            subprocess.run(["umount", str(directory)], check=True)
        shutil.rmtree(directory)


def read_exact(stream, size):
    result = bytearray()
    while len(result) < size:
        chunk = stream.recv(size - len(result))
        if not chunk:
            raise EOFError("WebSocket closed")
        result.extend(chunk)
    return result


class Probe:
    def __init__(self, path, home, *, hold_bootstrap=False):
        self.path = path
        self.home = home
        self.listener = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.listener.settimeout(30)
        self.listener.bind(str(path))
        self.listener.listen(1)
        self.methods = []
        self.errors = []
        self.bootstrap_reached = threading.Event()
        self.hold_bootstrap = hold_bootstrap

    def serve(self):
        try:
            with self.listener.accept()[0] as stream:
                stream.settimeout(30)
                header = bytearray()
                while b"\r\n\r\n" not in header:
                    header.extend(read_exact(stream, 1))
                    if len(header) > 16384:
                        raise AssertionError("Oversized WebSocket upgrade")
                lines = header.decode("ascii").split("\r\n")
                assert lines[0] == "GET /rpc HTTP/1.1", lines[0]
                headers = dict(line.lower().split(":", 1) for line in lines[1:] if ":" in line)
                key = next(line.split(":", 1)[1].strip() for line in lines if line.lower().startswith("sec-websocket-key:"))
                assert headers["upgrade"].strip() == "websocket"
                accept = base64.b64encode(hashlib.sha1((key + "258EAFA5-E914-47DA-95CA-C5AB0DC85B11").encode()).digest())
                stream.sendall(b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: " + accept + b"\r\n\r\n")
                while True:
                    first, second = read_exact(stream, 2)
                    length = second & 127
                    if length in (126, 127):
                        length = int.from_bytes(read_exact(stream, 2 if length == 126 else 8), "big")
                    assert length <= 1024 * 1024, "Oversized smoke-fixture message"
                    assert second & 128, "Client WebSocket frames must be masked"
                    mask = read_exact(stream, 4)
                    payload = bytes(value ^ mask[index % 4] for index, value in enumerate(read_exact(stream, length)))
                    if first & 15 == 8:
                        break
                    assert first == 129, "Expected one complete JSON text frame"
                    request = json.loads(payload)
                    method = request["method"]
                    self.methods.append(method)
                    if method == "initialized":
                        continue
                    if method == "initialize":
                        assert request["params"]["clientInfo"]["name"] == "codex-tui"
                        assert request["params"]["capabilities"]["experimentalApi"] is True
                        result = {
                            "userAgent": "claw-build-smoke/1",
                            "codexHome": str(self.home),
                            "platformFamily": "unix",
                            "platformOs": "linux",
                        }
                    elif method == "account/read":
                        assert not request.get("params", {}).get("refreshToken")
                        result = {"account": None, "requiresOpenaiAuth": False}
                    else:
                        assert method != "account/login/start", "Unexpected OpenAI onboarding"
                        assert method != "turn/start", "Smoke probes must never submit model turns"
                        if self.hold_bootstrap and not self.bootstrap_reached.is_set():
                            time.sleep(2)
                        self.bootstrap_reached.set()
                        self.send(stream, {"id": request["id"], "error": {"code": -32601, "message": STOP}})
                        continue
                    self.send(stream, {"id": request["id"], "result": result})
        except (EOFError, BrokenPipeError, ConnectionResetError):
            if not self.bootstrap_reached.is_set():
                self.errors.append("Connection closed before auth-free bootstrap")
        except (AssertionError, KeyError, ValueError, OSError) as error:
            self.errors.append(repr(error))
        finally:
            self.listener.close()

    @staticmethod
    def send(stream, message):
        data = json.dumps(message).encode()
        header = bytes([129, len(data)]) if len(data) < 126 else bytes([129, 126]) + struct.pack("!H", len(data))
        stream.sendall(header + data)


def terminal_child():
    os.setsid()
    fcntl.ioctl(0, termios.TIOCSCTTY, 0)


def run_tui(binary, endpoint, directory, environment, probe=None, *, check_no_cloud=False):
    master, slave = pty.openpty()
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 40, 120, 0, 0))
    command = [
        str(binary), "--remote", endpoint, "--cd", environment["HOME"],
        "--sandbox", "danger-full-access", "--ask-for-approval", "on-request",
    ]
    for override in FRONTEND_OVERRIDES:
        command.extend(["-c", override])
    trace_path = directory / ("network-" + uuid.uuid4().hex[:8] + ".log")
    if check_no_cloud:
        command = [
            "strace", "-f", "-qq", "--kill-on-exit", "-e", "trace=network",
            "-o", str(trace_path), *command,
        ]
    process = subprocess.Popen(
        command,
        cwd=directory, env=environment, stdin=slave, stdout=slave, stderr=slave,
        preexec_fn=terminal_child,
    )
    os.close(slave)
    thread = None
    if probe is not None:
        thread = threading.Thread(target=probe.serve, daemon=True)
        thread.start()
    output = bytearray()
    pending = b""
    deadline = time.monotonic() + 40
    try:
        while time.monotonic() < deadline:
            if select.select([master], [], [], 0.05)[0]:
                try:
                    data = os.read(master, 65536)
                except OSError as error:
                    if error.errno == errno.EIO:
                        break
                    raise
                if not data:
                    break
                output.extend(data)
                pending += data
                for query, response in (
                    (b"\x1b[6n", b"\x1b[1;1R"),
                    (b"\x1b[c", b"\x1b[?1;2c"),
                    (b"\x1b[>c", b"\x1b[>0;0;0c"),
                    (b"\x1b[?u", b"\x1b[?0u"),
                ):
                    if query in pending:
                        os.write(master, response)
                        pending = pending.replace(query, b"")
                pending = pending[-16:]
            if process.poll() is not None:
                break
            if probe is not None and probe.bootstrap_reached.is_set():
                os.write(master, b"\x03")
        code = process.wait(timeout=3)
        if check_no_cloud:
            attempts = [
                line for line in trace_path.read_text().splitlines()
                if re.search(r"\b(?:connect|sendto|sendmsg|sendmmsg)\(.*\bAF_INET6?\b", line)
            ]
            assert not attempts, f"Automatic remote-startup Internet attempt(s): {len(attempts)}"
        return code, output.decode(errors="replace")
    finally:
        if process.poll() is None:
            process.terminate()
            try:
                process.wait(timeout=3)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait()
        os.close(master)
        if thread is not None:
            thread.join(timeout=35)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=ROOT / "build" / "agent-tui" / "bin" / "codex-tui")
    parser.add_argument(
        "--project-tmpfs", action="store_true",
        help="use a private project-local Linux filesystem for WSL/DrvFs Unix sockets",
    )
    parser.add_argument(
        "--check-no-cloud", action="store_true",
        help="trace startup for Internet attempts, with an inert token to detect local auth bootstrap",
    )
    args = parser.parse_args()
    if args.check_no_cloud and shutil.which("strace") is None:
        parser.error("--check-no-cloud requires strace")
    if args.check_no_cloud:
        os.unshare(os.CLONE_NEWNET)
        # An unrouted documentation-only interface keeps AI_ADDRCONFIG from
        # hiding network attempts simply because an empty namespace has no IP.
        for command in (
            ["ip", "link", "set", "lo", "up"],
            ["ip", "link", "add", "clawprobe0", "type", "veth", "peer", "name", "clawprobe1"],
            ["ip", "address", "add", "192.0.2.1/30", "dev", "clawprobe0"],
            ["ip", "link", "set", "clawprobe0", "up"],
            ["ip", "link", "set", "clawprobe1", "up"],
        ):
            subprocess.run(command, check=True)
    binary = args.binary.resolve(strict=True)
    with fixture_directory(args.project_tmpfs) as directory:
        return probe_binary(binary, directory, check_no_cloud=args.check_no_cloud)


def probe_binary(binary, directory, *, check_no_cloud=False):
    output = ""
    try:
        environment = {
            "PATH": "/usr/local/bin:/usr/bin:/bin",
            "TERM": "xterm-256color",
            "LANG": "C.UTF-8",
            "SHELL": "/bin/bash",
            "COS_TUI_FRONTEND": "1",
        }
        for variable in ("HOME", "CODEX_HOME", "XDG_CONFIG_HOME", "XDG_CACHE_HOME", "XDG_DATA_HOME", "XDG_RUNTIME_DIR", "TMPDIR"):
            path = directory / variable.lower()
            path.mkdir(mode=0o700)
            environment[variable] = str(path)
        home = Path(environment["CODEX_HOME"])
        if check_no_cloud:
            environment["CODEX_ACCESS_TOKEN"] = "claw-tui-startup-regression-invalid-token"
        help_text = subprocess.check_output([str(binary), "--help"], cwd=directory, env=environment, text=True)
        assert "--remote " in help_text and "--remote-auth-token-env" in help_text
        assert "--remote-transport" not in help_text
        version = subprocess.check_output([str(binary), "--version"], cwd=directory, env=environment, text=True).strip()
        print(f"PASS real upstream CLI options: {version}")

        code, output = run_tui(
            binary, "unix://" + str(directory / "missing.sock"), directory, environment,
            check_no_cloud=check_no_cloud,
        )
        assert code != 0, "Missing explicit endpoint must not start an embedded backend"
        assert "failed to connect to remote app server" in output, output
        print("PASS explicit Unix connection failure exits without embedded fallback")

        probe = Probe(directory / "rpc.sock", home, hold_bootstrap=check_no_cloud)
        _, output = run_tui(
            binary, "unix://" + str(probe.path), directory, environment, probe,
            check_no_cloud=check_no_cloud,
        )
        assert not probe.errors, probe.errors
        assert probe.methods[:3] == ["initialize", "initialized", "account/read"], probe.methods
        assert "config/read" in probe.methods, probe.methods
        assert "account/login/start" not in probe.methods and "turn/start" not in probe.methods
        print("PASS Unix WebSocket handshake and auth-free startup through config/read")
        if check_no_cloud:
            print("PASS no Internet connect/send attempts during remote startup with the inert auth sentinel")
        print("No credentials, model turns, or backend implementation were used.")
    except (AssertionError, OSError, subprocess.SubprocessError) as error:
        print(f"Remote startup smoke failed: {error}", file=sys.stderr)
        print(output[-4000:].replace("\x1b", "\\x1b"), file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
