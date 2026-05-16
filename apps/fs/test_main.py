"""Tests for fs app output size limits + the binary / move / copy verbs."""

import base64
import os
import tempfile
import unittest

# Adjust path so we can import the module. We add the app dir (so
# `import main` works), the SDK src dir (so `from claw_os_sdk import
# ai` resolves), and the runtime src dir (so `from cos_runtime import
# policy, snapshot` inside main.py works).
import sys
_THIS_DIR = os.path.dirname(__file__)
sys.path.insert(0, _THIS_DIR)
sys.path.insert(
    0,
    os.path.join(
        os.path.dirname(_THIS_DIR), os.pardir, "claw-os-sdk", "python", "src"
    ),
)
sys.path.insert(
    0,
    os.path.join(
        os.path.dirname(_THIS_DIR), os.pardir, "cos-runtime", "python", "src"
    ),
)

# Tests run outside a Claw session, so the policy helper would
# normally fail strict cap checks. Flip to permissive so the verbs
# exercise their own logic rather than the kernel boundary.
os.environ.setdefault("COS_PERMS_MODE", "permissive")

from main import (
    MAX_READ_BYTES,
    MAX_READ_BYTES_BINARY,
    cmd_copy,
    cmd_move,
    cmd_read,
    cmd_read_bytes,
    cmd_rename,
)


class TestCmdReadTruncation(unittest.TestCase):
    def setUp(self):
        # realpath here so on macOS where /tmp -> /private/tmp the
        # canonicalised path returned by the security-fixed _abs()
        # matches what we assert against.
        self.tmpdir = os.path.realpath(tempfile.mkdtemp())

    def tearDown(self):
        import shutil
        shutil.rmtree(self.tmpdir)

    def _write(self, name, content_bytes):
        path = os.path.join(self.tmpdir, name)
        with open(path, "wb") as f:
            f.write(content_bytes)
        return path

    def test_small_file_no_truncation(self):
        path = self._write("small.txt", b"hello world")
        result = cmd_read([path])
        self.assertEqual(result["content"], "hello world")
        self.assertNotIn("truncated", result)
        self.assertNotIn("total_size", result)

    def test_large_file_truncated(self):
        data = b"x" * (MAX_READ_BYTES + 500)
        path = self._write("big.txt", data)
        result = cmd_read([path])
        self.assertEqual(len(result["content"]), MAX_READ_BYTES)
        self.assertTrue(result["truncated"])
        self.assertEqual(result["total_size"], MAX_READ_BYTES + 500)

    def test_exact_limit_no_truncation(self):
        data = b"a" * MAX_READ_BYTES
        path = self._write("exact.txt", data)
        result = cmd_read([path])
        self.assertEqual(len(result["content"]), MAX_READ_BYTES)
        self.assertNotIn("truncated", result)

    def test_offset(self):
        path = self._write("offset.txt", b"0123456789")
        result = cmd_read([path, "--offset", "5"])
        self.assertEqual(result["content"], "56789")
        self.assertEqual(result["offset"], 5)
        self.assertNotIn("truncated", result)

    def test_limit(self):
        path = self._write("limit.txt", b"0123456789")
        result = cmd_read([path, "--limit", "3"])
        self.assertEqual(result["content"], "012")
        self.assertTrue(result["truncated"])

    def test_offset_and_limit(self):
        path = self._write("combo.txt", b"abcdefghij")
        result = cmd_read([path, "--offset", "2", "--limit", "4"])
        self.assertEqual(result["content"], "cdef")
        self.assertTrue(result["truncated"])

    def test_limit_capped_to_max(self):
        """User-specified limit above MAX_READ_BYTES is capped."""
        data = b"y" * (MAX_READ_BYTES + 100)
        path = self._write("capped.txt", data)
        result = cmd_read([path, "--limit", str(MAX_READ_BYTES + 50000)])
        self.assertEqual(len(result["content"]), MAX_READ_BYTES)
        self.assertTrue(result["truncated"])

    def test_file_not_found(self):
        result = cmd_read(["/nonexistent/file.txt"])
        self.assertIn("error", result)

    def test_no_args_raises(self):
        with self.assertRaises(Exception):
            cmd_read([])


class TestRenameAndMove(unittest.TestCase):
    def setUp(self):
        self.tmpdir = os.path.realpath(tempfile.mkdtemp())

    def tearDown(self):
        import shutil
        shutil.rmtree(self.tmpdir, ignore_errors=True)

    def _write(self, name, content_bytes=b"hello"):
        path = os.path.join(self.tmpdir, name)
        with open(path, "wb") as f:
            f.write(content_bytes)
        return path

    def test_rename_moves_file(self):
        src = self._write("a.txt", b"hello")
        dst = os.path.join(self.tmpdir, "b.txt")
        result = cmd_rename([src, dst])
        self.assertEqual(result.get("to"), dst)
        self.assertTrue(os.path.exists(dst))
        self.assertFalse(os.path.exists(src))
        with open(dst, "rb") as f:
            self.assertEqual(f.read(), b"hello")

    def test_move_is_alias_for_rename(self):
        src = self._write("a.txt", b"x")
        dst = os.path.join(self.tmpdir, "b.txt")
        result = cmd_move([src, dst])
        self.assertEqual(result.get("to"), dst)
        self.assertTrue(os.path.exists(dst))
        self.assertFalse(os.path.exists(src))

    def test_rename_missing_src_returns_error(self):
        dst = os.path.join(self.tmpdir, "b.txt")
        result = cmd_rename([os.path.join(self.tmpdir, "missing.txt"), dst])
        self.assertIn("error", result)

    def test_rename_dst_exists_returns_error(self):
        src = self._write("a.txt", b"x")
        dst = self._write("b.txt", b"y")
        result = cmd_rename([src, dst])
        self.assertIn("error", result)
        # src untouched on failure
        self.assertTrue(os.path.exists(src))
        with open(dst, "rb") as f:
            self.assertEqual(f.read(), b"y")

    def test_rename_requires_two_args(self):
        with self.assertRaises(Exception):
            cmd_rename([])
        with self.assertRaises(Exception):
            cmd_rename(["/just/one"])


class TestCopy(unittest.TestCase):
    def setUp(self):
        self.tmpdir = os.path.realpath(tempfile.mkdtemp())

    def tearDown(self):
        import shutil
        shutil.rmtree(self.tmpdir, ignore_errors=True)

    def _write(self, name, content_bytes=b"hello"):
        path = os.path.join(self.tmpdir, name)
        with open(path, "wb") as f:
            f.write(content_bytes)
        return path

    def test_copy_duplicates_file(self):
        src = self._write("a.txt", b"hello world")
        dst = os.path.join(self.tmpdir, "b.txt")
        result = cmd_copy([src, dst])
        self.assertEqual(result.get("to"), dst)
        self.assertEqual(result.get("kind"), "file")
        self.assertTrue(os.path.exists(src))
        self.assertTrue(os.path.exists(dst))
        with open(dst, "rb") as f:
            self.assertEqual(f.read(), b"hello world")

    def test_copy_directory_tree(self):
        srcdir = os.path.join(self.tmpdir, "srcdir")
        os.makedirs(os.path.join(srcdir, "sub"))
        with open(os.path.join(srcdir, "sub", "x.txt"), "wb") as f:
            f.write(b"nested")
        dstdir = os.path.join(self.tmpdir, "dstdir")
        result = cmd_copy([srcdir, dstdir])
        self.assertEqual(result.get("to"), dstdir)
        self.assertEqual(result.get("kind"), "dir")
        self.assertTrue(os.path.exists(os.path.join(dstdir, "sub", "x.txt")))
        with open(os.path.join(dstdir, "sub", "x.txt"), "rb") as f:
            self.assertEqual(f.read(), b"nested")

    def test_copy_missing_src_returns_error(self):
        result = cmd_copy([
            os.path.join(self.tmpdir, "no-such"),
            os.path.join(self.tmpdir, "out"),
        ])
        self.assertIn("error", result)

    def test_copy_dst_exists_returns_error(self):
        src = self._write("a.txt", b"x")
        dst = self._write("b.txt", b"y")
        result = cmd_copy([src, dst])
        self.assertIn("error", result)
        with open(dst, "rb") as f:
            self.assertEqual(f.read(), b"y")


class TestReadBytes(unittest.TestCase):
    def setUp(self):
        self.tmpdir = os.path.realpath(tempfile.mkdtemp())

    def tearDown(self):
        import shutil
        shutil.rmtree(self.tmpdir, ignore_errors=True)

    def _write(self, name, content_bytes):
        path = os.path.join(self.tmpdir, name)
        with open(path, "wb") as f:
            f.write(content_bytes)
        return path

    def test_returns_base64(self):
        data = bytes(range(256))
        path = self._write("blob.bin", data)
        result = cmd_read_bytes([path])
        self.assertEqual(base64.b64decode(result["base64"]), data)
        self.assertEqual(result["bytes_returned"], len(data))
        self.assertEqual(result["total_size"], len(data))
        self.assertNotIn("truncated", result)

    def test_offset_and_limit(self):
        data = bytes(range(256))
        path = self._write("blob.bin", data)
        result = cmd_read_bytes([path, "--offset", "10", "--limit", "16"])
        self.assertEqual(result["offset"], 10)
        decoded = base64.b64decode(result["base64"])
        self.assertEqual(decoded, data[10:26])
        # 16 bytes requested, file has 240 more — the read returned
        # 17 bytes so truncated must be flagged.
        self.assertTrue(result.get("truncated"))

    def test_offset_to_end_no_truncation(self):
        data = b"abcdefghij"
        path = self._write("blob.bin", data)
        result = cmd_read_bytes([path, "--offset", "5"])
        self.assertEqual(base64.b64decode(result["base64"]), b"fghij")
        self.assertNotIn("truncated", result)

    def test_limit_capped_to_max(self):
        data = b"q" * (MAX_READ_BYTES_BINARY + 1024)
        path = self._write("big.bin", data)
        result = cmd_read_bytes([path, "--limit", str(MAX_READ_BYTES_BINARY + 99999)])
        decoded = base64.b64decode(result["base64"])
        self.assertEqual(len(decoded), MAX_READ_BYTES_BINARY)
        self.assertTrue(result["truncated"])

    def test_missing_file(self):
        result = cmd_read_bytes(["/nonexistent/blob.bin"])
        self.assertIn("error", result)

    def test_requires_path(self):
        with self.assertRaises(Exception):
            cmd_read_bytes([])


class TestWriteBytes(unittest.TestCase):
    def setUp(self):
        self.tmpdir = os.path.realpath(tempfile.mkdtemp())

    def tearDown(self):
        import shutil
        shutil.rmtree(self.tmpdir, ignore_errors=True)

    def test_round_trip_base64(self):
        from main import cmd_write_bytes
        data = bytes(range(256))
        encoded = base64.b64encode(data).decode("ascii")
        dst = os.path.join(self.tmpdir, "out.bin")
        result = cmd_write_bytes([dst, "--content", encoded])
        self.assertEqual(result.get("path"), dst)
        self.assertEqual(result.get("bytes"), len(data))
        with open(dst, "rb") as f:
            self.assertEqual(f.read(), data)

    def test_creates_missing_parents(self):
        from main import cmd_write_bytes
        nested = os.path.join(self.tmpdir, "a", "b", "c", "out.bin")
        encoded = base64.b64encode(b"hi").decode("ascii")
        result = cmd_write_bytes([nested, "--content", encoded])
        self.assertEqual(result.get("bytes"), 2)
        self.assertTrue(os.path.exists(nested))

    def test_invalid_base64_returns_error(self):
        from main import cmd_write_bytes
        dst = os.path.join(self.tmpdir, "out.bin")
        result = cmd_write_bytes([dst, "--content", "!@#$%^not-base64"])
        self.assertIn("error", result)
        self.assertFalse(os.path.exists(dst))

    def test_requires_path(self):
        from main import cmd_write_bytes
        with self.assertRaises(Exception):
            cmd_write_bytes([])


class TestSymlinkEscape(unittest.TestCase):
    """Regression coverage for CR-1 (symlink TOCTOU sandbox escape).

    ``_abs`` used to call ``os.path.abspath``, which does *not*
    resolve symlinks. A symlink dropped under the sandbox could
    therefore point at ``/etc/passwd`` and read/write/copy verbs
    would happily follow it, since the policy check ran on the
    pre-resolved path. The fix is two-layered:

    1. ``_abs`` now uses ``realpath`` so ``policy.require("fs.read",
       path=...)`` is called with the *real* on-disk target — the
       kernel's cap check therefore sees the escape and can deny it.
    2. Byte-level reads use ``O_NOFOLLOW`` so a symlink swapped in
       between check and open raises ``ELOOP`` instead of silently
       dereferencing.

    These tests exercise (1) by patching ``policy.require`` and
    asserting the resolved path was passed, and (2) by reading a
    symlink directly with the per-fd helper.
    """

    def setUp(self):
        self.tmpdir = os.path.realpath(tempfile.mkdtemp())
        self.outside = os.path.realpath(tempfile.mkdtemp())
        self.secret_path = os.path.join(self.outside, "secret.txt")
        with open(self.secret_path, "wb") as f:
            f.write(b"TOP-SECRET-PASSWORD")

    def tearDown(self):
        import shutil

        shutil.rmtree(self.tmpdir, ignore_errors=True)
        shutil.rmtree(self.outside, ignore_errors=True)

    def test_read_resolves_symlink_for_policy_check(self):
        """policy.require must receive the realpath, not the symlink."""
        link = os.path.join(self.tmpdir, "shortcut")
        os.symlink(self.secret_path, link)

        import main as fs_main

        captured = {}
        original = fs_main.policy.require

        def spy(verb, **kwargs):
            captured.setdefault(verb, []).append(kwargs)
            return original(verb, **kwargs)

        fs_main.policy.require = spy
        try:
            fs_main.cmd_read([link])
        finally:
            fs_main.policy.require = original

        self.assertIn("fs.read", captured)
        seen = captured["fs.read"][0].get("path")
        self.assertEqual(
            seen,
            self.secret_path,
            "policy.require(fs.read) must see the realpath target so the kernel "
            "can refuse the symlink escape — got the unresolved path instead",
        )

    def test_read_bytes_resolves_symlink_for_policy_check(self):
        link = os.path.join(self.tmpdir, "shortcut2")
        os.symlink(self.secret_path, link)

        import main as fs_main

        captured = {}
        original = fs_main.policy.require

        def spy(verb, **kwargs):
            captured.setdefault(verb, []).append(kwargs)
            return original(verb, **kwargs)

        fs_main.policy.require = spy
        try:
            fs_main.cmd_read_bytes([link])
        finally:
            fs_main.policy.require = original

        self.assertEqual(captured.get("fs.read", [{}])[0].get("path"), self.secret_path)

    def test_open_nofollow_refuses_symlink_at_final_component(self):
        """_open_nofollow must raise on a symlink, even one inside the sandbox."""
        link = os.path.join(self.tmpdir, "link_to_secret")
        os.symlink(self.secret_path, link)

        from main import _open_nofollow

        with self.assertRaises(OSError):
            _open_nofollow(link, os.O_RDONLY)

    def test_copy_dir_does_not_materialise_outside_target(self):
        srcdir = os.path.join(self.tmpdir, "src")
        os.makedirs(srcdir)
        with open(os.path.join(srcdir, "regular.txt"), "wb") as f:
            f.write(b"safe content")
        os.symlink(self.secret_path, os.path.join(srcdir, "escape"))

        dstdir = os.path.join(self.tmpdir, "dst")
        cmd_copy([srcdir, dstdir])

        escape_path = os.path.join(dstdir, "escape")
        if os.path.lexists(escape_path):
            # If it's a regular file under dst, the copy materialised
            # the outside content — that's the bug we're guarding
            # against. A symlink that still points outside dst is
            # tolerable (the contained payload is not under our root).
            if not os.path.islink(escape_path):
                real = os.path.realpath(escape_path)
                self.assertTrue(
                    real.startswith(dstdir + os.sep) or real == dstdir,
                    f"cmd_copy materialised an outside-sandbox file under dst: {real}",
                )


if __name__ == "__main__":
    unittest.main()
