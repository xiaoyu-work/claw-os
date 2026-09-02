#!/usr/bin/python3

import argparse
import dataclasses
import os
import signal
import stat
import subprocess
import sys
import tempfile
import time


VIRTUAL_FILESYSTEMS = {
    "binfmt_misc",
    "bpf",
    "cgroup",
    "cgroup2",
    "configfs",
    "debugfs",
    "devpts",
    "fusectl",
    "mqueue",
    "nsfs",
    "proc",
    "pstore",
    "securityfs",
    "sysfs",
    "tracefs",
}


class ScanError(RuntimeError):
    pass


@dataclasses.dataclass(frozen=True)
class MountRecord:
    mount_id: int
    parent_id: int
    major: int
    minor: int
    root: str
    mountpoint: str
    fs_type: str


def decode_mount_path(value: bytes) -> str:
    output = bytearray()
    index = 0
    while index < len(value):
        byte = value[index]
        if byte != 0x5C:
            output.append(byte)
            index += 1
            continue
        escape = value[index + 1 : index + 4]
        if escape == b"040":
            output.append(0x20)
        elif escape == b"134":
            output.append(0x5C)
        else:
            raise ScanError("mountinfo contains an unsupported escaped path")
        index += 4
    path = os.fsdecode(bytes(output))
    if not path.startswith("/"):
        raise ScanError("mountinfo path is not absolute")
    return path


def parse_mountinfo(data: bytes) -> list[MountRecord]:
    records: list[MountRecord] = []
    mount_ids: set[int] = set()
    mountpoints: set[str] = set()
    for line in data.splitlines():
        fields = line.split()
        try:
            separator = fields.index(b"-")
        except ValueError as error:
            raise ScanError("mountinfo record has no separator") from error
        if len(fields) < 10 or separator < 6 or separator + 3 >= len(fields):
            raise ScanError("mountinfo record has an invalid field count")
        try:
            mount_id = int(fields[0])
            parent_id = int(fields[1])
            major_text, minor_text = fields[2].split(b":", 1)
            major = int(major_text)
            minor = int(minor_text)
        except (ValueError, TypeError) as error:
            raise ScanError("mountinfo record has an invalid numeric identity") from error
        if mount_id <= 0 or parent_id <= 0 or major < 0 or minor < 0:
            raise ScanError("mountinfo record has an invalid identity")
        root = decode_mount_path(fields[3])
        mountpoint = decode_mount_path(fields[4])
        fs_type = fields[separator + 1].decode("ascii", "strict")
        if mount_id in mount_ids:
            raise ScanError(f"duplicate mount id {mount_id}")
        if mountpoint in mountpoints:
            raise ScanError(f"stacked or duplicate mountpoint is ambiguous: {mountpoint}")
        mount_ids.add(mount_id)
        mountpoints.add(mountpoint)
        records.append(
            MountRecord(
                mount_id=mount_id,
                parent_id=parent_id,
                major=major,
                minor=minor,
                root=root,
                mountpoint=mountpoint,
                fs_type=fs_type,
            )
        )
    if not records:
        raise ScanError("mountinfo is empty")
    return records


def process_group_members(process_group: int) -> list[int]:
    members: list[int] = []
    for name in os.listdir("/proc"):
        if not name.isdigit():
            continue
        try:
            with open(f"/proc/{name}/stat", "r", encoding="ascii") as status:
                raw = status.read()
            fields = raw[raw.rfind(")") + 2 :].split()
            if len(fields) > 2 and int(fields[2]) == process_group:
                members.append(int(name))
        except (FileNotFoundError, PermissionError, ValueError, OSError):
            continue
    return members


def terminate_process_group(process: subprocess.Popen[bytes], grace: float = 2.0) -> None:
    try:
        os.killpg(process.pid, signal.SIGTERM)
    except ProcessLookupError:
        pass
    try:
        process.wait(timeout=grace)
    except subprocess.TimeoutExpired:
        try:
            os.killpg(process.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        process.wait()


def run_process_group(command: list[str], timeout_seconds: int) -> subprocess.CompletedProcess[bytes]:
    with tempfile.TemporaryFile() as stdout_file, tempfile.TemporaryFile() as stderr_file:
        process = subprocess.Popen(
            command,
            stdin=subprocess.DEVNULL,
            stdout=stdout_file,
            stderr=stderr_file,
            start_new_session=True,
            env={"LC_ALL": "C", "LANG": "C", "PATH": "/usr/bin:/bin"},
        )
        timed_out = False
        try:
            process.wait(timeout=timeout_seconds)
        except subprocess.TimeoutExpired:
            timed_out = True
            terminate_process_group(process)
        members = process_group_members(process.pid)
        if members:
            try:
                os.killpg(process.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            deadline = time.monotonic() + 1
            while time.monotonic() < deadline and process_group_members(process.pid):
                time.sleep(0.02)
        residual = process_group_members(process.pid)
        stdout_file.seek(0)
        stderr_file.seek(0)
        stdout = stdout_file.read()
        stderr = stderr_file.read()
        if timed_out:
            raise ScanError(
                f"scan timed out after {timeout_seconds}s; residual process group members: "
                f"{residual}; stderr={stderr.decode('utf-8', 'replace').strip()}"
            )
        if residual:
            raise ScanError(f"scan left residual process group members: {residual}")
        return subprocess.CompletedProcess(command, process.returncode, stdout, stderr)


def run_bounded_scan(command: list[str], timeout_seconds: int) -> subprocess.CompletedProcess[bytes]:
    bounded = [
        "/usr/bin/timeout",
        "--foreground",
        "--signal=TERM",
        "--kill-after=2s",
        f"{timeout_seconds}s",
        *command,
    ]
    result = run_process_group(bounded, timeout_seconds + 3)
    if result.returncode != 0:
        detail = result.stderr.decode("utf-8", "replace").strip()
        raise ScanError(
            f"scan timed out or failed with status {result.returncode}: {detail}"
        )
    return result


def read_mount_id(fd: int) -> int:
    with open(f"/proc/self/fdinfo/{fd}", "r", encoding="ascii") as info:
        for line in info:
            if line.startswith("mnt_id:"):
                return int(line.split(":", 1)[1].strip())
    raise ScanError("opened mount descriptor has no mnt_id")


def stat_identity(value: os.stat_result) -> tuple[int, int, int, int, int]:
    return (value.st_dev, value.st_ino, value.st_mode, value.st_uid, value.st_gid)


def verify_mount_descriptor(fd: int, record: MountRecord) -> tuple[int, int, int, int, int]:
    if read_mount_id(fd) != record.mount_id:
        raise ScanError(f"mount id changed for {record.mountpoint}")
    descriptor = os.fstat(fd)
    pathname = os.stat(record.mountpoint, follow_symlinks=False)
    if stat.S_ISLNK(pathname.st_mode) or stat_identity(descriptor) != stat_identity(pathname):
        raise ScanError(f"mount path identity changed for {record.mountpoint}")
    if os.major(descriptor.st_dev) != record.major or os.minor(descriptor.st_dev) != record.minor:
        raise ScanError(f"mount device changed for {record.mountpoint}")
    return stat_identity(descriptor)


def run_checked(command: list[str], fd: int) -> subprocess.CompletedProcess[bytes]:
    return subprocess.run(
        command,
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        env={"LC_ALL": "C", "LANG": "C", "PATH": "/usr/bin:/bin"},
        pass_fds=(fd,),
        check=False,
    )


def scan_mount(record: MountRecord, gid: int) -> None:
    flags = os.O_RDONLY | os.O_CLOEXEC | os.O_NOFOLLOW
    try:
        fd = os.open(record.mountpoint, flags)
    except OSError as error:
        raise ScanError(f"cannot open mount {record.mountpoint}: {error}") from error
    try:
        before = verify_mount_descriptor(fd, record)
        scan_root = f"/proc/self/fd/{fd}"
        if stat.S_ISDIR(os.fstat(fd).st_mode):
            scan_root += "/."

        ownership = run_checked(
            [
                "/usr/bin/find",
                "-H",
                scan_root,
                "-xdev",
                "-gid",
                str(gid),
                "-print",
                "-quit",
            ],
            fd,
        )
        if ownership.returncode != 0:
            raise ScanError(
                f"ownership scan failed for {record.mountpoint}: "
                f"{ownership.stderr.decode('utf-8', 'replace').strip()}"
            )
        if ownership.stdout:
            raise ScanError(f"gid {gid} owns an object on {record.mountpoint}")

        with tempfile.TemporaryFile() as acl_output:
            acl = subprocess.run(
                [
                    "/usr/bin/find",
                    "-H",
                    scan_root,
                    "-xdev",
                    "-exec",
                    "/usr/bin/getfacl",
                    "-P",
                    "-n",
                    "-p",
                    "-s",
                    "--",
                    "{}",
                    "+",
                ],
                stdin=subprocess.DEVNULL,
                stdout=acl_output,
                stderr=subprocess.PIPE,
                env={"LC_ALL": "C", "LANG": "C", "PATH": "/usr/bin:/bin"},
                pass_fds=(fd,),
                check=False,
            )
            if acl.returncode != 0:
                raise ScanError(
                    f"ACL scan failed for {record.mountpoint}: "
                    f"{acl.stderr.decode('utf-8', 'replace').strip()}"
                )
            acl_output.seek(0)
            access_prefix = f"group:{gid}:".encode()
            default_prefix = f"default:group:{gid}:".encode()
            for line in acl_output:
                stripped = line.strip()
                if stripped.startswith(access_prefix) or stripped.startswith(default_prefix):
                    raise ScanError(
                        f"gid {gid} appears in a POSIX ACL on {record.mountpoint}"
                    )

        after = verify_mount_descriptor(fd, record)
        if after != before:
            raise ScanError(f"mount descriptor identity changed for {record.mountpoint}")
    finally:
        os.close(fd)


def worker_main(arguments: argparse.Namespace) -> int:
    record = MountRecord(
        mount_id=arguments.mount_id,
        parent_id=arguments.parent_id,
        major=arguments.major,
        minor=arguments.minor,
        root=arguments.root,
        mountpoint=arguments.mountpoint,
        fs_type=arguments.fs_type,
    )
    scan_mount(record, arguments.gid)
    return 0


def verify_installed_helper() -> None:
    path = os.path.realpath(__file__)
    metadata = os.lstat(path)
    if (
        not stat.S_ISREG(metadata.st_mode)
        or metadata.st_uid != 0
        or metadata.st_gid != 0
        or metadata.st_mode & 0o022
        or metadata.st_nlink != 1
    ):
        raise ScanError("extension gid helper is not a single-link root-owned immutable file")


def parent_main(arguments: argparse.Namespace) -> int:
    verify_installed_helper()
    with open(arguments.mountinfo, "rb") as mountinfo:
        snapshot = mountinfo.read()
    records = parse_mountinfo(snapshot)
    for record in records:
        if record.fs_type in VIRTUAL_FILESYSTEMS:
            continue
        command = [
            "/usr/bin/python3",
            os.path.realpath(__file__),
            "--worker",
            "--gid",
            str(arguments.gid),
            "--mount-id",
            str(record.mount_id),
            "--parent-id",
            str(record.parent_id),
            "--major",
            str(record.major),
            "--minor",
            str(record.minor),
            "--root",
            record.root,
            "--mountpoint",
            record.mountpoint,
            "--fs-type",
            record.fs_type,
        ]
        run_bounded_scan(command, arguments.timeout)
    with open(arguments.mountinfo, "rb") as mountinfo:
        if mountinfo.read() != snapshot:
            raise ScanError("mount topology changed during extension gid scan")
    return 0


def parse_arguments() -> argparse.Namespace:
    parser = argparse.ArgumentParser(add_help=False)
    parser.add_argument("--worker", action="store_true")
    parser.add_argument("--gid", type=int, required=True)
    parser.add_argument("--mountinfo")
    parser.add_argument("--timeout", type=int, default=300)
    parser.add_argument("--mount-id", type=int)
    parser.add_argument("--parent-id", type=int)
    parser.add_argument("--major", type=int)
    parser.add_argument("--minor", type=int)
    parser.add_argument("--root")
    parser.add_argument("--mountpoint")
    parser.add_argument("--fs-type")
    arguments = parser.parse_args()
    if arguments.gid <= 0 or arguments.gid > 4294967295:
        raise ScanError("candidate gid is invalid")
    if arguments.timeout <= 0:
        raise ScanError("scan timeout is invalid")
    if not arguments.worker and not arguments.mountinfo:
        raise ScanError("mountinfo path is required")
    return arguments


def main() -> int:
    try:
        arguments = parse_arguments()
        if arguments.worker:
            return worker_main(arguments)
        return parent_main(arguments)
    except (OSError, ScanError, ValueError) as error:
        print(f"claw-os-agent: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
