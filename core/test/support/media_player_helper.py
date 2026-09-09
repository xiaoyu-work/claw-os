"""Exercise the real dropped-owner helper in a private mount namespace.

Run after native fixture and clawd builds:
sudo unshare --mount --propagation private python3 core/test/support/media_player_helper.py \
  --clawd target/debug/clawd --fixture <media-player-mpris-fixture> --owner <uid>
No user's bus, installed executable or media state is changed.
"""

import argparse
import ctypes
import json
import os
from pathlib import Path
import pwd
import selectors
import shutil
import socket
import subprocess
import time


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--clawd", required=True, type=Path)
    parser.add_argument("--fixture", required=True, type=Path)
    parser.add_argument("--owner", required=True, type=int)
    args = parser.parse_args()
    assert os.geteuid() == 0 and args.owner != 0, "requires root and a non-root owner"
    assert os.readlink("/proc/self/ns/mnt") != os.readlink("/proc/1/ns/mnt"), "requires a private mount namespace"
    owner = pwd.getpwuid(args.owner)
    clawd = args.clawd.resolve()
    native = args.fixture.resolve()
    root = Path.cwd() / "build/player-helper-fixture"
    root.mkdir()
    mounts = []
    children = []
    libc = ctypes.CDLL(None, use_errno=True)
    libc.mount.argtypes = [ctypes.c_char_p, ctypes.c_char_p, ctypes.c_char_p, ctypes.c_ulong, ctypes.c_void_p]
    libc.umount2.argtypes = [ctypes.c_char_p, ctypes.c_int]

    def bind(source, target):
        if libc.mount(os.fsencode(source), os.fsencode(target), None, 4096, None):
            raise OSError(ctypes.get_errno(), "private bind mount")
        mounts.append(target)

    def harden():
        if libc.prctl(38, 1, 0, 0, 0):
            raise OSError(ctypes.get_errno(), "PR_SET_NO_NEW_PRIVS")

    def start(command, **kwargs):
        process = subprocess.Popen(
            command, user=args.owner, group=owner.pw_gid, extra_groups=[],
            preexec_fn=harden, env={"LC_ALL": "C.UTF-8", **kwargs.pop("env", {})},
            stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, **kwargs,
        )
        children.append(process)
        return process

    def ready(process):
        with selectors.DefaultSelector() as selector:
            selector.register(process.stdout, selectors.EVENT_READ)
            assert selector.select(8), "native fixture failed to start"
            line = process.stdout.readline().strip()
            assert line, process.stderr.read()
            return line

    def finish(process):
        if process.poll() is None:
            process.terminate()
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait(timeout=5)

    try:
        installed = root / "usr-bin"
        installed.mkdir(mode=0o755)
        shutil.copy2(Path("/usr/bin/dbus-daemon").resolve(), installed / "dbus-daemon")
        shutil.copy2(native, installed / "cosmic-player")
        for entry in installed.iterdir():
            entry.chmod(0o755)
            os.chown(entry, 0, 0)
        runtime = root / "run-user"
        runtime.mkdir(mode=0o755)
        user_runtime = runtime / str(args.owner)
        user_runtime.mkdir(mode=0o700)
        os.chown(user_runtime, args.owner, owner.pw_gid)
        bind(installed, "/usr/bin")
        bind(runtime, "/run/user")
        address = f"unix:path=/run/user/{args.owner}/bus"
        bus = start(["/usr/bin/dbus-daemon", "--session", "--nofork", "--nopidfile",
                     "--nosyslog", f"--address={address}", "--print-address=1"])
        assert ready(bus).startswith(address)
        env = {"DBUS_SESSION_BUS_ADDRESS": address}
        other = start(["/usr/bin/cosmic-player", "--other"], env=env)
        assert ready(other) == "other-ready"
        ui = start(["/usr/bin/cosmic-player"], env=env, stdin=subprocess.PIPE)
        assert ready(ui) == f"org.mpris.MediaPlayer2.com.clawos.Player.pid{ui.pid}"

        def call(action, *, permit=True, timeout_ms=4000, success=True):
            parent, child = socket.socketpair()
            try:
                helper = start(
                    [str(clawd), "--media-player-helper", action,
                     str(int(time.time() * 1000) + timeout_ms), str(child.fileno())],
                    pass_fds=(child.fileno(),),
                )
                child.close()
                parent.settimeout(6)
                question = parent.recv(1)
                if question:
                    assert question == b"\x01"
                    assert "NoNewPrivs:\t1" in Path(f"/proc/{helper.pid}/status").read_text()
                    if permit is not None:
                        parent.sendall(b"\x01" if permit else b"\x00")
                stdout, stderr = helper.communicate(timeout=6)
                if success:
                    assert helper.returncode == 0, stderr
                    return json.loads(stdout)
                assert helper.returncode != 0 and stdout == "", (stdout, stderr)
                return stderr
            finally:
                parent.close()
                child.close()

        assert call("status")["title"] == "Synthetic visible track 0"
        for action, status, track in [
            ("play", "Playing", 0), ("pause", "Paused", 0), ("toggle", "Playing", 0),
            ("next", "Playing", 1), ("previous", "Playing", 0), ("stop", "Stopped", 0),
        ]:
            assert call(action) == {"ok": True}
            live = call("status")
            assert (live["status"], live["title"]) == (status, f"Synthetic visible track {track}")
        ui.stdin.write("ui-title:Native UI changed\n")
        ui.stdin.flush()
        assert ready(ui) == "updated"
        assert call("status")["title"] == "Native UI changed"
        assert "withdrawn" in call("play", permit=False, success=False)
        assert "timed out" in call("play", permit=None, timeout_ms=300, success=False)
        assert call("status")["status"] == "Stopped"
        second = start(["/usr/bin/cosmic-player"], env=env, stdin=subprocess.PIPE)
        ready(second)
        assert "ambiguous" in call("status", success=False)
        finish(second)
        finish(ui)
        assert "no native Media Player" in call("status", success=False)
        assert other.poll() is None
        # The same backend outside the installed executable is not the product.
        spoof = start([str(native)], env=env, stdin=subprocess.PIPE)
        ready(spoof)
        assert "not the installed native product" in call("play", success=False)
        print("Real owner-session helper, installed executable binding, seven actions/live state, ambiguity, spoofing and dispatch cancellation passed")
    finally:
        for child in reversed(children):
            finish(child)
        for target in reversed(mounts):
            if libc.umount2(os.fsencode(target), 2):
                raise OSError(ctypes.get_errno(), "unmount private fixture")
        shutil.rmtree(root)


if __name__ == "__main__":
    main()
