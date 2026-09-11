# SPDX-License-Identifier: GPL-3.0-only
"""Actual SDK/provider/kernel probes in fresh Root-owned mount/network namespaces."""

import argparse
import ctypes
import json
import os
from pathlib import Path
import shutil
import signal
import sqlite3
import subprocess
import tempfile
import time


def mount(*arguments):
    subprocess.run(["/usr/bin/mount", *map(str, arguments)], check=True)


def owned_directory(path, uid, gid):
    path.mkdir(parents=True, exist_ok=True)
    path.chmod(0o700)
    os.chown(path, uid, gid)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--kernel", type=Path, required=True)
    parser.add_argument("--provider", type=Path, required=True)
    parser.add_argument("--test-binary", type=Path, required=True)
    parser.add_argument("--work-dir", type=Path, required=True)
    arguments = parser.parse_args()
    if os.geteuid() != 0:
        parser.error("Root is required only to create the private fixture; clients run as the source owner")
    for path in (arguments.kernel, arguments.provider, arguments.test_binary):
        if not path.is_absolute() or not path.is_file():
            parser.error(f"expected an absolute built binary: {path}")
    owner = arguments.test_binary.stat()
    uid, gid = owner.st_uid, owner.st_gid
    if uid == 0:
        parser.error("build the fixture as the normal non-Root source owner first")
    arguments.work_dir.mkdir(parents=True, exist_ok=True)

    os.unshare(os.CLONE_NEWNS | os.CLONE_NEWNET)
    mount("--make-rprivate", "/")
    with tempfile.TemporaryDirectory(prefix="kernel-", dir=arguments.work_dir) as temporary:
        root = Path(temporary)
        root.chmod(0o755)
        binary_dir = root / "bin"
        helper_dir = root / "libexec"
        binary_dir.mkdir()
        helper_dir.mkdir()
        shutil.copy2(arguments.kernel, binary_dir / "cos")
        shutil.copy2(arguments.provider, helper_dir / "claw-os-applet-provider")
        mount("--bind", binary_dir, "/usr/local/bin")
        mount("--bind", helper_dir, "/usr/libexec")
        mount("-t", "tmpfs", "-o", "mode=0755,size=16M", "tmpfs", "/run")
        routed = Path("/run/cos/caps") / str(uid)
        (routed / "proc").mkdir(parents=True)
        os.chown(routed, 0, gid)
        routed.chmod(0o750)
        registry_path = routed / "proc/registry.json"
        libc = ctypes.CDLL(None, use_errno=True)

        cases = [
            "missing-session", "missing-grant", "wrong-scope", "wrong-pid",
            "exact-history-read", "untrusted-app", "wrong-app", "wrong-start",
            "calendar-own", "calendar-foreign",
        ]
        for case in cases:
            case_root = root / case
            case_root.mkdir()
            case_root.chmod(0o755)
            for name in ("home", "data", "log", "runtime", "config", "cache", "provenance"):
                owned_directory(case_root / name, uid, gid)
            database_uid = uid + 1 if case == "calendar-foreign" else uid
            owned_directory(case_root / "data", database_uid, database_uid)
            owned_directory(case_root / "data/calendar", database_uid, database_uid)
            database = case_root / "data/calendar/events.db"
            with sqlite3.connect(database) as connection:
                connection.executescript(
                    "CREATE TABLE events (id TEXT, title TEXT, start_time TEXT, end_time TEXT, location TEXT);"
                    "INSERT INTO events VALUES ('private-owner-event', 'Fixture', '2026-09-09', NULL, '');"
                )
            os.chown(database, database_uid, database_uid)
            database.chmod(0o600)
            environment = {
                "HOME": str(case_root / "home"),
                "PATH": "/usr/local/bin:/usr/bin:/bin",
                "LANG": "C.UTF-8",
                "COS_DATA_DIR": str(case_root / "data"),
                "COS_LOG_DIR": str(case_root / "log"),
                "COS_RUNTIME_DIR": str(case_root / "runtime"),
                "COS_CONFIG_DIR": str(case_root / "config"),
                "COS_CACHE_DIR": str(case_root / "cache"),
                "COS_PROVENANCE_RUNTIME_DIR": str(case_root / "provenance"),
                "COS_PROC_DATA_DIR": str(routed),
                "COS_PERMS_MODE": "strict",
                "CLAW_APPLET_KERNEL_CASE": case,
            }
            if case != "missing-session":
                environment["COS_SESSION"] = "applet-private-fixture"
            is_app = case in ("untrusted-app", "wrong-app", "wrong-start")
            if is_app:
                environment["COS_APP_ID"] = "different-app" if case == "wrong-app" else "panel-calendar"
                environment["COS_APP_GUI"] = "1"
            capability = {
                "verb": "data.db.read" if case.startswith("calendar-") else "clipboard.read",
                "scope": {
                    "kind": "name",
                    "value": "calendar" if case.startswith("calendar-") else
                             "selection" if case == "wrong-scope" else "history",
                },
            }
            read_fd, write_fd = os.pipe()
            child = os.fork()
            if child == 0:
                os.close(write_fd)
                os.setgroups([])
                os.setgid(gid)
                os.setuid(uid)
                if libc.prctl(38, 1, 0, 0, 0) != 0:
                    os._exit(121)
                if os.read(read_fd, 1) != b"1":
                    os._exit(122)
                os.close(read_fd)
                os.execve(arguments.test_binary, [
                    str(arguments.test_binary), "--ignored", "--exact", "real_kernel_context",
                    "--nocapture", "--test-threads=1",
                ], environment)
            os.close(read_fd)
            try:
                start = int(Path(f"/proc/{child}/stat").read_text().rsplit(")", 1)[1].split()[19])
                row = {
                    "session_id": "applet-private-fixture",
                    "pid": 4294967295 if case == "wrong-pid" else child,
                    "command": [str(arguments.test_binary)],
                    "started_at": "2026-09-09T00:00:00Z",
                    "stdout_path": "", "stderr_path": "",
                    "caps": [] if case == "missing-grant" else [capability],
                    "pending_bind": False,
                    "start_time_ticks": start + (case == "wrong-start"),
                }
                if is_app:
                    row["app_id"] = "panel-calendar"
                registry_path.write_text(json.dumps({
                    "sessions": [] if case == "missing-session" else [row],
                }))
                registry_path.chmod(0o440)
                os.chown(registry_path, 0, gid)
                print(f"fixture case {case}: actual euid={uid}, no_new_privs=1, exact independent capability", flush=True)
                os.write(write_fd, b"1")
                os.close(write_fd)
                deadline = time.monotonic() + 30
                while True:
                    finished, status = os.waitpid(child, os.WNOHANG)
                    if finished:
                        child = None
                        if os.waitstatus_to_exitcode(status) != 0:
                            raise RuntimeError(f"real kernel case failed: {case}")
                        break
                    if time.monotonic() >= deadline:
                        raise TimeoutError(f"real kernel fixture exceeded its deadline: {case}")
                    time.sleep(0.02)
            finally:
                if child is not None:
                    os.kill(child, signal.SIGKILL)
                    os.waitpid(child, 0)
        print(f"all {len(cases)} actual-kernel cases passed; no GUI/resource admission was granted", flush=True)


if __name__ == "__main__":
    main()
