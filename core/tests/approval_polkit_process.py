#!/usr/bin/env python3
"""Exercise real polkit/PAM/pkexec in private mount, PID and network namespaces."""

import argparse
import contextlib
import errno
import fcntl
import json
import os
from pathlib import Path
import pty
import pwd
import re
import select
import shutil
import signal
import socket
import stat
import struct
import subprocess
import sys
import tempfile
import termios
import time
import uuid
import xml.etree.ElementTree as ET


PASSWORD = "Claw-fixture-only-42"
HELPER = Path("/usr/local/bin/claw-approval-helper")
ACTION = "org.clawos.approval.decide"
SOCKET = Path("/run/cos/clawd.sock")


def command(*args, **kwargs):
    return subprocess.run(args, check=True, **kwargs)


def stop(process):
    if process.poll() is None:
        process.terminate()
        try:
            process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait(timeout=5)


def mount_private(target):
    command("mount", "-t", "tmpfs", "-o", "mode=755", "tmpfs", str(target))


def bind(source, target):
    command("mount", "--bind", str(source), str(target))


def child_identity(uid, gid):
    os.setsid()
    fcntl.ioctl(0, termios.TIOCSCTTY, 0)
    os.setgroups([])
    os.setgid(gid)
    os.setuid(uid)


def owner_command(args):
    if args[0] != "/usr/bin/pkexec":
        return subprocess.call(args)
    start = Path("/proc/self/stat").read_text().rsplit(")", 1)[1].split()[19]
    agent = subprocess.Popen(
        ["/usr/bin/pkttyagent", "--process", f"{os.getpid()},{start}",
         "--notify-fd=1"],
        stdout=subprocess.PIPE,
    )
    try:
        ready, _, _ = select.select([agent.stdout], [], [], 5)
        if not ready or agent.stdout.read(1) != b"" or agent.poll() is not None:
            raise RuntimeError("fixture authentication agent did not register")
        return subprocess.call([args[0], "--disable-internal-agent", *args[1:]])
    finally:
        stop(agent)
        agent.stdout.close()


class Terminal:
    def __init__(self, args, uid, gid, home, capture_stdout=False):
        self.output = bytearray()
        self.stdout = bytearray()
        self.master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 36, 120, 0, 0))
        # pkexec authenticates its parent; that parent must also be the fixture user.
        owner_command = [
            "/usr/bin/python3", str(Path(__file__).resolve()), "--owner-command",
            *args,
        ] if args[0] == "/usr/bin/pkexec" else args
        self.process = subprocess.Popen(
            owner_command,
            stdin=slave,
            stdout=subprocess.PIPE if capture_stdout else slave,
            stderr=slave,
            cwd=home,
            env={
                "HOME": str(home),
                "PATH": "/usr/local/bin:/usr/bin:/bin",
                "LANG": "C.UTF-8",
                "LC_ALL": "C.UTF-8",
                "TERM": "xterm-256color",
            },
            preexec_fn=lambda: child_identity(uid, gid),
        )
        os.close(slave)

    def pump(self, predicate, seconds=15):
        deadline = time.monotonic() + seconds
        readers = {self.master: self.output}
        if self.process.stdout is not None:
            readers[self.process.stdout.fileno()] = self.stdout
        while time.monotonic() < deadline:
            if predicate():
                return
            ready, _, _ = select.select(list(readers), [], [], 0.05)
            for fd in ready:
                try:
                    chunk = os.read(fd, 65536)
                except OSError as error:
                    if error.errno != errno.EIO:
                        raise
                    chunk = b""
                if not chunk:
                    readers.pop(fd)
                    continue
                readers[fd].extend(chunk)
                if len(readers[fd]) > 2 * 1024 * 1024:
                    raise AssertionError("terminal fixture output exceeded its bound")
            if not readers and self.process.poll() is not None:
                break
        if not predicate():
            plain = re.sub(rb"\x1b\[[0-?]*[ -/]*[@-~]", b"", bytes(self.output))
            raise AssertionError(
                f"terminal did not reach expected state (exit {self.process.poll()}): "
                f"{plain[-4000:].decode(errors='replace')!r}"
            )

    def expect(self, text, seconds=15):
        self.pump(lambda: text in self.output, seconds)

    def send(self, data):
        os.write(self.master, data)

    def finish(self):
        self.pump(lambda: self.process.poll() is not None)
        result = self.process.wait(timeout=5)
        if self.process.stdout is not None:
            self.stdout.extend(self.process.stdout.read())
        return result

    def require_exit(self, expected):
        status = self.finish()
        if status != expected:
            plain = re.sub(rb"\x1b\[[0-?]*[ -/]*[@-~]", b"", bytes(self.output))
            raise AssertionError(
                f"expected exit {expected}, received {status}; "
                f"terminal={plain[-4000:].decode(errors='replace')!r}; "
                f"stdout={self.stdout[-2000:].decode(errors='replace')!r}"
            )

    def close(self):
        stop(self.process)
        os.close(self.master)
        if self.process.stdout is not None:
            self.process.stdout.close()


def wait_bus():
    for _ in range(100):
        reply = subprocess.run(
            ["busctl", "--system", "list", "--no-pager"],
            capture_output=True, text=True, check=False,
        )
        if reply.returncode == 0 and "org.freedesktop.PolicyKit1" in reply.stdout:
            return
        time.sleep(0.05)
    raise AssertionError("private polkit daemon did not become ready")


def rpc(method, params, uid=None):
    request = json.dumps({
        "v": 2, "id": uuid.uuid4().hex, "command": method, "params": params,
    }).encode()

    def exchange():
        with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as connection:
            connection.settimeout(10)
            connection.connect(str(SOCKET))
            connection.sendall(struct.pack(">4sBBI", b"CBK1", 1, 0, len(request)) + request)

            def exact(length):
                data = bytearray()
                while len(data) < length:
                    chunk = connection.recv(length - len(data))
                    if not chunk:
                        raise AssertionError("truncated approval broker response")
                    data.extend(chunk)
                return data

            magic, kind, flags, length = struct.unpack(">4sBBI", exact(10))
            assert (magic, kind, flags) == (b"CBK1", 2, 0)
            assert length <= 16 * 1024 * 1024
            return json.loads(exact(length))

    if uid is None:
        return exchange()
    # SCM_CREDENTIALS must describe a real process with this UID, not seteuid.
    reader, writer = os.pipe()
    pid = os.fork()
    if pid == 0:
        os.close(reader)
        try:
            os.setgroups([])
            os.setgid(uid)
            os.setuid(uid)
            with os.fdopen(writer, "w") as result:
                json.dump(exchange(), result)
            os._exit(0)
        except BaseException:
            os._exit(1)
    os.close(writer)
    with os.fdopen(reader) as result:
        data = result.read()
    _, status = os.waitpid(pid, 0)
    assert status == 0, f"owner broker request failed: {status}"
    return json.loads(data)


def run(args):
    if os.geteuid() != 0:
        raise RuntimeError("run through sudo unshare --mount --pid --fork --mount-proc --net")
    if os.getpid() != 2 or os.getppid() != 1:
        raise RuntimeError("the fixture must be the child of its private namespace init")
    if not re.fullmatch(r"mnt:\[\d+\]", args.original_mount_namespace):
        raise RuntimeError("invalid original mount namespace")
    if not re.fullmatch(r"pid:\[\d+\]", args.original_pid_namespace):
        raise RuntimeError("invalid original PID namespace")
    if os.readlink("/proc/self/ns/mnt") == args.original_mount_namespace:
        raise RuntimeError("refusing to mount over the host namespace")
    if os.readlink("/proc/self/ns/pid") == args.original_pid_namespace:
        raise RuntimeError("a private PID namespace is required")
    command("mount", "--make-rprivate", "/")
    accounts = pwd.getpwall()
    polkit = pwd.getpwnam("polkitd")
    pam_helper = Path(args.polkitd).with_name("polkit-agent-helper-1")
    socket_helper = not pam_helper.stat().st_mode & stat.S_ISUID
    used = {account.pw_uid for account in accounts}
    uid, other_uid = [value for value in range(45000, 46000) if value not in used][:2]
    with tempfile.TemporaryDirectory(prefix="claw-polkit-test-") as temporary:
        root = Path(temporary)
        root.chmod(0o755)
        with contextlib.ExitStack() as cleanup:
            for name in ("cos", "clawd", "helper", "policy"):
                staged = root / name
                shutil.copyfile(getattr(args, name), staged)
                staged.chmod(0o644 if name == "policy" else 0o755)
                setattr(args, name, staged)
            for target in (
                "/run", "/var/lib", "/etc/pam.d", "/etc/polkit-1",
                "/usr/share/polkit-1", "/usr/local/bin",
            ):
                mount_private(target)
            for path in ("/run/dbus", "/run/cos", "/run/polkit", "/etc/polkit-1/rules.d",
                         "/usr/share/polkit-1/actions", "/usr/share/polkit-1/rules.d"):
                Path(path).mkdir(parents=True, exist_ok=True)
            command("mount", "--bind", "/sys/fs/cgroup", "/sys/fs/cgroup")
            command("mount", "-o", "remount,bind,ro", "/sys/fs/cgroup")
            home = root / "owner"
            home.mkdir(mode=0o700)
            os.chown(home, uid, uid)
            passwd = root / "passwd"
            passwd.write_text(
                "root:x:0:0:root:/root:/bin/sh\n"
                f"polkitd:x:{polkit.pw_uid}:{polkit.pw_gid}:PolicyKit:/:/usr/sbin/nologin\n"
                f"claw-auth-fixture:x:{uid}:{uid}:Claw auth fixture:{home}:/bin/sh\n"
                f"claw-other-fixture:x:{other_uid}:{other_uid}:Other fixture:/nonexistent:/bin/sh\n"
            )
            group = root / "group"
            group.write_text(
                f"root:x:0:\npolkitd:x:{polkit.pw_gid}:\ncos-extension:x:60999:\n"
                f"claw-auth-fixture:x:{uid}:\n"
                f"claw-other-fixture:x:{other_uid}:\n"
            )
            hashed = command(
                "openssl", "passwd", "-6", "-stdin",
                input=PASSWORD + "\n", capture_output=True, text=True,
            ).stdout.strip()
            shadow = root / "shadow"
            shadow.write_text(f"root:!*:20000:0:99999:7:::\nclaw-auth-fixture:{hashed}:20000:0:99999:7:::\n")
            shadow.chmod(0o600)
            nsswitch = root / "nsswitch.conf"
            nsswitch.write_text("passwd: files\ngroup: files\nshadow: files\nhosts: files\n")
            for path in (passwd, group, shadow, nsswitch):
                bind(path, Path("/etc") / path.name)
            Path("/etc/pam.d/polkit-1").write_text(
                "auth required pam_unix.so\naccount required pam_unix.so\nsession required pam_permit.so\n"
            )
            for source, destination in (
                (args.helper, HELPER), (args.cos, Path("/usr/local/bin/cos")),
            ):
                shutil.copyfile(source, destination)
                destination.chmod(0o755)
            policy = Path("/usr/share/polkit-1/actions/org.clawos.approval.policy")
            policy.write_bytes(args.policy.read_bytes())
            defaults = ET.parse(policy).find(f"./action[@id='{ACTION}']/defaults")
            assert defaults is not None
            assert all(defaults.findtext(key) == "auth_self"
                       for key in ("allow_any", "allow_inactive", "allow_active"))
            candidate_policy = policy.read_bytes()
            legacy_policy = ET.fromstring(candidate_policy)
            for key in ("allow_any", "allow_inactive"):
                legacy_policy.find(f"./action/defaults/{key}").text = "no"
            policy.write_bytes(ET.tostring(legacy_policy))
            bus_config = root / "bus.conf"
            bus_config.write_text("""<busconfig>
  <type>system</type><listen>unix:path=/run/dbus/system_bus_socket</listen>
  <auth>EXTERNAL</auth>
  <policy context="default">
    <allow user="*"/><allow own="*"/><allow send_destination="*"/>
    <allow receive_sender="*"/>
  </policy>
</busconfig>""")
            logs = {}
            for name, argv in (
                ("dbus", ["dbus-daemon", "--nofork", "--config-file", str(bus_config)]),
                ("polkit", [args.polkitd, "--no-debug"]),
                ("clawd", [str(args.clawd), "--socket", str(SOCKET), "--socket-mode", "666"]),
            ):
                log = cleanup.enter_context((root / f"{name}.log").open("w+"))
                process = subprocess.Popen(
                    argv, stdout=log, stderr=log,
                    env={"PATH": "/usr/local/bin:/usr/bin:/bin", "HOME": "/root",
                         "LANG": "C.UTF-8", "COS_DATA_DIR": "/var/lib/cos"},
                )
                cleanup.callback(stop, process)
                logs[name] = (log, process)
                if name == "dbus":
                    for _ in range(100):
                        if Path("/run/dbus/system_bus_socket").exists():
                            break
                        time.sleep(0.05)
            if socket_helper:
                log = cleanup.enter_context((root / "pam-socket.log").open("w+"))
                process = subprocess.Popen(
                    ["systemd-socket-activate", "--accept", "--inetd",
                     "--listen=/run/polkit/agent-helper.socket", str(pam_helper),
                     "--socket-activated"],
                    stdout=log, stderr=log,
                    env={"PATH": "/usr/bin:/bin", "LANG": "C.UTF-8"},
                )
                cleanup.callback(stop, process)
                logs["pam-socket"] = (log, process)
                for _ in range(100):
                    if Path("/run/polkit/agent-helper.socket").exists():
                        Path("/run/polkit/agent-helper.socket").chmod(0o666)
                        break
                    time.sleep(0.05)
                else:
                    raise AssertionError("private polkit socket helper did not start")
            try:
                wait_bus()
                with contextlib.closing(Terminal(
                    ["/usr/bin/pkexec", str(HELPER), "--help"], uid, uid, home, True,
                )) as terminal:
                    terminal.require_exit(127)
                    assert b"Password:" not in terminal.output
                    assert b"usage: claw-approval-helper" not in terminal.stdout
                print(json.dumps({"case": "legacy-sessionless-policy-refused", "completed": True}), flush=True)
                policy.write_bytes(candidate_policy)
                for _ in range(100):
                    active = command(
                        "pkaction", "--verbose", "--action-id", ACTION,
                        capture_output=True, text=True,
                    ).stdout
                    if active.count("auth_self") == 3:
                        break
                    time.sleep(0.05)
                else:
                    raise AssertionError("private polkit did not load the new policy")
                with contextlib.closing(Terminal(
                    ["/usr/bin/pkexec", str(HELPER), "--help"], uid, uid, home, True
                )) as terminal:
                    terminal.expect(b"Password:")
                    terminal.send(PASSWORD.encode() + b"\n")
                    terminal.require_exit(0)
                    assert b"usage: claw-approval-helper" in terminal.stdout
                    assert PASSWORD.encode() not in terminal.output
                print(json.dumps({"case": "sessionless-real-pam", "completed": True}), flush=True)
                for _ in range(200):
                    if SOCKET.exists():
                        response = rpc("daemon.health", {})
                        if response["ok"]:
                            break
                    time.sleep(0.05)
                else:
                    raise AssertionError("private clawd did not become ready")
                request = rpc("permission.request", {
                    "verb": "fs.meta", "scope": {"kind": "path", "value": "/**"},
                    "session": "approval-fixture", "reason": "Fixture metadata approval",
                }, uid)["result"]
                with contextlib.closing(Terminal(
                    ["/usr/bin/pkexec", str(HELPER), "--id", request["id"],
                     "--decision", "approve", "--duration", "once"],
                    uid, uid, home, True,
                )) as terminal:
                    terminal.expect(b"Password:")
                    terminal.send(PASSWORD.encode() + b"\n")
                    terminal.require_exit(0)
                    result = json.loads(terminal.stdout)
                    assert result["id"] == request["id"] and result["decision"] == "approved"
                status = rpc("permission.status", {"ids": [request["id"]]}, uid)
                assert status["result"]["statuses"][0]["status"] == "approved"
                print(json.dumps({"case": "real-helper-broker-decision", "completed": True}), flush=True)
                refused = rpc("permission.request", {
                    "verb": "fs.meta", "scope": {"kind": "path", "value": "/**"},
                    "session": "refused-approval-fixture", "reason": "Refusal fixture",
                }, uid)["result"]
                forged = rpc("permission.decide", {
                    "id": refused["id"], "decision": "approve", "owner_uid": uid,
                }, uid)
                assert not forged["ok"], "an ordinary peer must not decide its own request"
                for case, answer in (
                    ("wrong-password", b"not-the-fixture-password\n"),
                    ("cancelled-password", b"\x04"),
                ):
                    with contextlib.closing(Terminal(
                        ["/usr/bin/pkexec", str(HELPER), "--id", refused["id"],
                         "--decision", "approve", "--duration", "once"],
                        uid, uid, home, True,
                    )) as terminal:
                        terminal.expect(b"Password:")
                        terminal.send(answer)
                        assert terminal.finish() in (126, 127)
                        assert not terminal.stdout
                    status = rpc("permission.status", {"ids": [refused["id"]]}, uid)
                    assert status["result"]["statuses"][0]["status"] == "pending"
                    print(json.dumps({"case": case, "completed": True}), flush=True)
                with contextlib.closing(Terminal(
                    ["/usr/bin/pkexec", str(HELPER), "--id", refused["id"], "--decision", "deny"],
                    uid, uid, home, True,
                )) as terminal:
                    terminal.expect(b"Password:")
                    terminal.send(PASSWORD.encode() + b"\n")
                    terminal.require_exit(0)
                    assert json.loads(terminal.stdout)["decision"] == "denied"
                status = rpc("permission.status", {"ids": [refused["id"]]}, uid)
                assert status["result"]["statuses"][0]["status"] == "denied"
                print(json.dumps({"case": "real-helper-broker-denial", "completed": True}), flush=True)
                foreign = rpc("permission.request", {
                    "verb": "fs.meta", "scope": {"kind": "path", "value": "/**"},
                    "session": "foreign-approval-fixture", "reason": "Other owner fixture",
                }, other_uid)["result"]
                with contextlib.closing(Terminal(
                    ["/usr/bin/pkexec", str(HELPER), "--id", foreign["id"],
                     "--decision", "approve", "--duration", "once"], uid, uid, home, True,
                )) as terminal:
                    terminal.expect(b"Password:")
                    terminal.send(PASSWORD.encode() + b"\n")
                    terminal.require_exit(1)
                    assert not terminal.stdout
                status = rpc("permission.status", {"ids": [foreign["id"]]}, other_uid)
                assert status["result"]["statuses"][0]["status"] == "pending"
                print(json.dumps({"case": "authenticated-foreign-owner-refused", "completed": True}), flush=True)
                request = rpc("permission.request", {
                    "verb": "fs.meta", "scope": {"kind": "path", "value": "/**"},
                    "session": "tui-approval-fixture", "reason": "TUI authentication fixture",
                }, uid)["result"]
                with contextlib.closing(Terminal(
                    ["/usr/local/bin/cos", "agent", "chat", "--tui"], uid, uid, home,
                )) as terminal:
                    terminal.expect(b"New Claw conversation")
                    for answer, expected in (
                        (b"not-the-fixture-password\n", "pending"),
                        (b"\x03", "pending"),
                        (PASSWORD.encode() + b"\n", "approved"),
                    ):
                        offset = len(terminal.output)
                        terminal.send(f"/approval {request['id']}".encode())
                        time.sleep(0.2)
                        terminal.send(b"\r")
                        terminal.pump(lambda: b"Approval" in terminal.output[offset:])
                        offset = len(terminal.output)
                        terminal.send(b"\x1b[97;1:2u")
                        settled = time.monotonic() + 0.4
                        terminal.pump(lambda: time.monotonic() >= settled, 1)
                        assert b"Claw OS authorization" not in terminal.output[offset:]
                        terminal.send(b"a")
                        terminal.pump(lambda: b"Password:" in terminal.output[offset:])
                        authentication_end = len(terminal.output)
                        terminal.send(answer)
                        terminal.pump(lambda: b"\x1b[?1049h" in terminal.output[authentication_end:])
                        status = rpc("permission.status", {"ids": [request["id"]]}, uid)
                        assert status["result"]["statuses"][0]["status"] == expected
                        assert not termios.tcgetattr(terminal.master)[3] & (termios.ICANON | termios.ECHO)
                        assert PASSWORD.encode() not in terminal.output
                    terminal.send(b"\x1b[27;1u")
                    time.sleep(0.2)
                    terminal.send(b"/quit\r")
                    terminal.require_exit(0)
                print(json.dumps({"case": "real-tui-pam-broker-approval", "completed": True}), flush=True)
            except BaseException:
                for name, (log, process) in logs.items():
                    log.seek(0)
                    print(f"{name} (exit {process.poll()}): {log.read()[-6000:]}", flush=True)
                raise


if __name__ == "__main__":
    if len(sys.argv) > 1 and sys.argv[1] == "--owner-command":
        sys.exit(owner_command(sys.argv[2:]))
    if os.getpid() == 1:
        pid = os.fork()
        if pid:
            _, status = os.waitpid(pid, 0)
            sys.exit(os.waitstatus_to_exitcode(status))
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("cos", "clawd", "helper", "policy"):
        parser.add_argument(f"--{name}", type=Path, required=True)
    parser.add_argument("--polkitd", default="/usr/lib/polkit-1/polkitd")
    parser.add_argument("--original-mount-namespace", required=True)
    parser.add_argument("--original-pid-namespace", required=True)
    arguments = parser.parse_args()
    for name in ("cos", "clawd", "helper", "policy"):
        setattr(arguments, name, getattr(arguments, name).resolve(strict=True))
    run(arguments)
