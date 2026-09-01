#!/usr/bin/python3

import importlib.util
import os
import pathlib
import signal
import subprocess
import sys
import tempfile
import time
import unittest
import unittest.mock
import types


PROJECT_DIR = pathlib.Path(__file__).resolve().parents[2]
HELPER = pathlib.Path(
    os.environ.get(
        "COS_GID_SCAN_HELPER_SOURCE",
        PROJECT_DIR / "deb" / "claw-os-agent" / "extension-gid-scan.py",
    )
)
SPEC = importlib.util.spec_from_file_location("extension_gid_scan", HELPER)
SCAN = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
sys.modules[SPEC.name] = SCAN
SPEC.loader.exec_module(SCAN)
GID = 60999


def mount_record(path: pathlib.Path) -> object:
    descriptor = os.open(path, os.O_RDONLY | os.O_CLOEXEC | os.O_NOFOLLOW)
    try:
        mount_id = SCAN.read_mount_id(descriptor)
        metadata = os.fstat(descriptor)
    finally:
        os.close(descriptor)
    return SCAN.MountRecord(
        mount_id=mount_id,
        parent_id=1,
        major=os.major(metadata.st_dev),
        minor=os.minor(metadata.st_dev),
        root="/",
        mountpoint=str(path),
        fs_type="ext4",
    )


class GidScanTests(unittest.TestCase):
    def test_mountinfo_decoding_and_ambiguity(self) -> None:
        records = SCAN.parse_mountinfo(
            b"10 1 8:1 /sub /mnt/space\\040dir rw - ext4 /dev/sda rw\n"
            b"11 1 8:1 /other /mnt/bind rw - ext4 /dev/sda rw\n"
        )
        self.assertEqual(records[0].mountpoint, "/mnt/space dir")
        self.assertEqual(records[1].root, "/other")

        with self.assertRaisesRegex(SCAN.ScanError, "stacked or duplicate"):
            SCAN.parse_mountinfo(
                b"10 1 8:1 / /mnt/stack rw - ext4 /dev/sda rw\n"
                b"11 1 0:2 / /mnt/stack rw - tmpfs tmpfs rw\n"
            )
        with self.assertRaisesRegex(SCAN.ScanError, "duplicate mount id"):
            SCAN.parse_mountinfo(
                b"10 1 8:1 / /mnt/a rw - ext4 /dev/sda rw\n"
                b"10 1 8:2 / /mnt/b rw - ext4 /dev/sdb rw\n"
            )
        with self.assertRaises(SCAN.ScanError):
            SCAN.parse_mountinfo(
                b"10 1 8:1 / /mnt/bad\\011path rw - ext4 /dev/sda rw\n"
            )

    @unittest.skipUnless(os.geteuid() == 0, "actual getfacl integration requires root")
    def test_real_getfacl_access_default_and_mask_entries(self) -> None:
        self.assertTrue(pathlib.Path("/usr/bin/getfacl").exists())
        self.assertTrue(pathlib.Path("/usr/bin/setfacl").exists())
        with tempfile.TemporaryDirectory(dir="/run") as raw:
            root = pathlib.Path(raw)
            target = root / "file"
            target.write_text("data", encoding="utf-8")
            record = mount_record(root)
            SCAN.scan_mount(record, GID)

            subprocess.run(
                ["/usr/bin/setfacl", "-m", f"g:{GID}:r--,m::---", target],
                check=True,
            )
            with self.assertRaisesRegex(SCAN.ScanError, "POSIX ACL"):
                SCAN.scan_mount(record, GID)
            subprocess.run(["/usr/bin/setfacl", "-b", target], check=True)

            directory = root / "directory"
            directory.mkdir()
            subprocess.run(
                ["/usr/bin/setfacl", "-m", f"d:g:{GID}:r-x,d:m::---", directory],
                check=True,
            )
            with self.assertRaisesRegex(SCAN.ScanError, "POSIX ACL"):
                SCAN.scan_mount(record, GID)

    @unittest.skipUnless(os.geteuid() == 0, "installed helper integration requires root")
    def test_root_owned_parent_helper_with_real_getfacl(self) -> None:
        with tempfile.TemporaryDirectory(dir="/run") as raw:
            root = pathlib.Path(raw)
            helper = root / "extension-gid-scan.py"
            helper.write_bytes(HELPER.read_bytes())
            helper.chmod(0o755)
            target = root / "tree"
            target.mkdir()
            record = mount_record(target)
            mountinfo = root / "mountinfo"
            encoded = os.fsencode(str(target)).replace(b"\\", b"\\134").replace(b" ", b"\\040")
            mountinfo.write_bytes(
                f"{record.mount_id} 1 {record.major}:{record.minor} / ".encode()
                + encoded
                + b" rw - ext4 /dev/mock rw\n"
            )
            clean = subprocess.run(
                [
                    "/usr/bin/python3",
                    str(helper),
                    "--gid",
                    str(GID),
                    "--mountinfo",
                    str(mountinfo),
                    "--timeout",
                    "5",
                ],
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                text=True,
                timeout=10,
            )
            self.assertEqual(clean.returncode, 0, clean.stderr)
            acl_target = target / "acl"
            acl_target.write_text("data", encoding="utf-8")
            subprocess.run(
                ["/usr/bin/setfacl", "-m", f"g:{GID}:r--", acl_target],
                check=True,
            )
            collision = subprocess.run(
                [
                    "/usr/bin/python3",
                    str(helper),
                    "--gid",
                    str(GID),
                    "--mountinfo",
                    str(mountinfo),
                    "--timeout",
                    "5",
                ],
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                text=True,
                timeout=10,
            )
            self.assertNotEqual(collision.returncode, 0)
            self.assertIn("POSIX ACL", collision.stderr)

    def test_mount_id_substitution_fails_closed(self) -> None:
        with tempfile.TemporaryDirectory() as raw:
            root = pathlib.Path(raw)
            record = mount_record(root)
            substituted = SCAN.MountRecord(
                mount_id=record.mount_id + 1,
                parent_id=record.parent_id,
                major=record.major,
                minor=record.minor,
                root=record.root,
                mountpoint=record.mountpoint,
                fs_type=record.fs_type,
            )
            with self.assertRaisesRegex(SCAN.ScanError, "mount id changed"):
                SCAN.scan_mount(substituted, GID)

    def test_mountinfo_mutation_is_detected_before_activation(self) -> None:
        with tempfile.TemporaryDirectory() as raw:
            mountinfo = pathlib.Path(raw) / "mountinfo"
            original = b"10 1 0:1 / /proc rw - proc proc rw\n"
            mountinfo.write_bytes(original)
            real_parse = SCAN.parse_mountinfo

            def mutate(data: bytes) -> list[object]:
                records = real_parse(data)
                mountinfo.write_bytes(data + b"11 1 0:2 / /sys rw - sysfs sysfs rw\n")
                return records

            arguments = types.SimpleNamespace(
                mountinfo=str(mountinfo),
                gid=GID,
                timeout=1,
            )
            with (
                unittest.mock.patch.object(SCAN, "verify_installed_helper"),
                unittest.mock.patch.object(SCAN, "parse_mountinfo", side_effect=mutate),
                self.assertRaisesRegex(SCAN.ScanError, "mount topology changed"),
            ):
                SCAN.parent_main(arguments)

    def test_timeout_kills_term_ignoring_child_and_grandchild(self) -> None:
        with tempfile.TemporaryDirectory() as raw:
            marker = pathlib.Path(raw) / "pids"
            script = (
                "import os,signal,time\n"
                "signal.signal(signal.SIGTERM, signal.SIG_IGN)\n"
                "child=os.fork()\n"
                f"open({str(marker)!r},'a').write(str(os.getpid())+'\\n')\n"
                "while True: time.sleep(1)\n"
            )
            started = time.monotonic()
            with self.assertRaisesRegex(SCAN.ScanError, "timed out"):
                SCAN.run_bounded_scan(
                    ["/usr/bin/python3", "-c", script],
                    timeout_seconds=1,
                )
            self.assertLess(time.monotonic() - started, 8)
            pids = [int(value) for value in marker.read_text().splitlines()]
            deadline = time.monotonic() + 1
            while time.monotonic() < deadline and any(
                pathlib.Path(f"/proc/{pid}").exists() for pid in pids
            ):
                time.sleep(0.02)
            self.assertTrue(all(not pathlib.Path(f"/proc/{pid}").exists() for pid in pids))


def mount_namespace_worker() -> int:
    def records_for(path: pathlib.Path) -> bytes:
        encoded = os.fsencode(str(path)).replace(b"\\", b"\\134").replace(b" ", b"\\040")
        lines = []
        for line in pathlib.Path("/proc/self/mountinfo").read_bytes().splitlines():
            fields = line.split()
            if len(fields) > 4 and fields[4] == encoded:
                lines.append(line)
        return b"\n".join(lines) + b"\n"

    with tempfile.TemporaryDirectory(dir="/run") as raw:
        root = pathlib.Path(raw)
        nested = root / "nested"
        nested.mkdir()
        subprocess.run(["/usr/bin/mount", "-t", "tmpfs", "tmpfs", nested], check=True)
        try:
            target = nested / "owned"
            target.write_text("secret", encoding="utf-8")
            os.chown(target, 0, GID)
            records = SCAN.parse_mountinfo(records_for(nested))
            record = next(item for item in records if item.mountpoint == str(nested))
            try:
                SCAN.scan_mount(record, GID)
            except SCAN.ScanError as error:
                if "owns an object" not in str(error):
                    raise
            else:
                raise AssertionError("nested mounted filesystem ownership was missed")
        finally:
            subprocess.run(["/usr/bin/umount", nested], check=True)

        source = root / "source"
        bind = root / "bind"
        source.mkdir()
        bind.mkdir()
        bind_file = source / "owned"
        bind_file.write_text("secret", encoding="utf-8")
        subprocess.run(["/usr/bin/mount", "--bind", source, bind], check=True)
        try:
            records = SCAN.parse_mountinfo(records_for(bind))
            if len(records) != 1 or records[0].root == "/":
                raise AssertionError("bind mount root identity was not preserved")
            os.chown(bind_file, 0, GID)
            try:
                SCAN.scan_mount(records[0], GID)
            except SCAN.ScanError as error:
                if "owns an object" not in str(error):
                    raise
            else:
                raise AssertionError("bind mount ownership was missed")
        finally:
            subprocess.run(["/usr/bin/umount", bind], check=True)

        stacked = root / "stacked"
        stacked.mkdir()
        subprocess.run(["/usr/bin/mount", "-t", "tmpfs", "tmpfs", stacked], check=True)
        subprocess.run(["/usr/bin/mount", "-t", "tmpfs", "tmpfs", stacked], check=True)
        try:
            try:
                SCAN.parse_mountinfo(records_for(stacked))
            except SCAN.ScanError as error:
                if "stacked or duplicate" not in str(error):
                    raise
            else:
                raise AssertionError("stacked mountpoint was accepted")
        finally:
            subprocess.run(["/usr/bin/umount", stacked], check=True)
            subprocess.run(["/usr/bin/umount", stacked], check=True)
    return 0


class RootMountTests(unittest.TestCase):
    @unittest.skipUnless(os.geteuid() == 0, "actual mount integration requires root")
    def test_nested_and_stacked_mounts_in_private_namespace(self) -> None:
        result = subprocess.run(
            [
                "/usr/bin/unshare",
                "--mount",
                "--propagation",
                "private",
                "/usr/bin/python3",
                str(pathlib.Path(__file__).resolve()),
                "--mount-worker",
            ],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            timeout=20,
        )
        self.assertEqual(result.returncode, 0, result.stderr)


if __name__ == "__main__":
    if "--mount-worker" in sys.argv:
        raise SystemExit(mount_namespace_worker())
    unittest.main()
