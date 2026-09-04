"""Unit tests for `cos_runtime.memory`.

Uses a fake ``cos`` binary on PATH (a small Python script we
generate at test time) so we can exercise the shell-out plumbing
without depending on the real kernel.
"""

import json
import os
import stat
import sys
import tempfile
import textwrap
import unittest
from unittest import mock

_THIS_DIR = os.path.dirname(__file__)
sys.path.insert(0, os.path.dirname(_THIS_DIR))

from cos_runtime import memory  # noqa: E402


def _wire_success(data: dict) -> str:
    return json.dumps({"ok": True, "wire_version": 1, "data": data})


def _wire_error(error: str, code: str, *, detail: dict | None = None) -> str:
    payload = {
        "ok": False,
        "wire_version": 1,
        "error": error,
        "code": code,
    }
    if detail is not None:
        payload["detail"] = detail
    return json.dumps(payload)


def _write_fake_cos(tmp_dir: str, *, stdout: str = "", stderr: str = "", exit_code: int = 0) -> str:
    """Drop a fake `cos` binary into tmp_dir that echoes the requested
    streams and exits with the requested code. Returns the path."""
    script = textwrap.dedent(
        f"""
        #!/usr/bin/env python3
        import sys
        sys.stdout.write({stdout!r})
        sys.stderr.write({stderr!r})
        sys.exit({exit_code})
        """
    ).lstrip()
    path = os.path.join(tmp_dir, "cos")
    with open(path, "w", encoding="utf-8") as f:
        f.write(script)
    os.chmod(path, os.stat(path).st_mode | stat.S_IEXEC | stat.S_IXGRP | stat.S_IXOTH)
    return path


class RememberArgValidationTests(unittest.TestCase):
    def test_empty_text_rejected(self) -> None:
        with self.assertRaises(memory.MemoryError):
            memory.remember(text="   ", source="expense-tracker")

    def test_missing_source_rejected(self) -> None:
        with self.assertRaises(memory.MemoryError):
            memory.remember(text="ok", source="")


class ForgetArgValidationTests(unittest.TestCase):
    def test_requires_exactly_one_target(self) -> None:
        with self.assertRaises(memory.MemoryError):
            memory.forget()
        with self.assertRaises(memory.MemoryError):
            memory.forget(source="expense-tracker", row_id=1)


class SubprocessShellOutTests(unittest.TestCase):
    def test_remember_returns_envelope_from_stdout(self) -> None:
        envelope = {
            "ok": True,
            "row_id": 42,
            "session_id": "app:expense-tracker",
            "stored_bytes": 18,
            "indexed_semantic": False,
            "text": "hi",
        }
        with tempfile.TemporaryDirectory() as tmp:
            cos = _write_fake_cos(tmp, stdout=_wire_success(envelope))
            with mock.patch.dict(os.environ, {"CLAW_COS_BIN": cos}):
                got = memory.remember(text="hi", source="expense-tracker", indexable=False)
        self.assertEqual(got, envelope)

    def test_permission_denied_uses_typed_error_detail(self) -> None:
        deny_envelope = {
            "decision": "deny",
            "verb": "memory.write",
            "summary": "memory.write not granted to source=calendar",
        }
        with tempfile.TemporaryDirectory() as tmp:
            cos = _write_fake_cos(
                tmp,
                stdout=_wire_error(
                    "memory remember denied",
                    "PERMISSION_DENIED",
                    detail=deny_envelope,
                ),
                exit_code=1,
            )
            with mock.patch.dict(os.environ, {"CLAW_COS_BIN": cos}):
                with self.assertRaises(memory.PermissionDenied) as ctx:
                    memory.remember(text="hi", source="calendar")
        self.assertEqual(ctx.exception.denial["decision"], "deny")
        self.assertEqual(ctx.exception.denial["verb"], "memory.write")

    def test_unstructured_failure_becomes_unavailable(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            cos = _write_fake_cos(tmp, stderr="cos: catastrophic kernel oops", exit_code=2)
            with mock.patch.dict(os.environ, {"CLAW_COS_BIN": cos}):
                with self.assertRaises(memory.MemoryUnavailable):
                    memory.remember(text="hi", source="expense-tracker")

    def test_non_json_stdout_becomes_unavailable(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            cos = _write_fake_cos(tmp, stdout="not json", exit_code=0)
            with mock.patch.dict(os.environ, {"CLAW_COS_BIN": cos}):
                with self.assertRaises(memory.MemoryUnavailable):
                    memory.remember(text="hi", source="expense-tracker")

    def test_forget_by_source_invokes_correct_args(self) -> None:
        envelope = {"removed": 3, "source": "expense-tracker"}
        captured: dict = {}

        class _FakeProc:
            def __init__(self, stdout: str, stderr: str, returncode: int) -> None:
                self.stdout = stdout
                self.stderr = stderr
                self.returncode = returncode

        def fake_run(cmd, **kwargs):  # type: ignore[no-untyped-def]
            captured["cmd"] = list(cmd)
            return _FakeProc(_wire_success(envelope), "", 0)

        with mock.patch.dict(os.environ, {"CLAW_COS_BIN": "/usr/bin/cos"}):
            with mock.patch("subprocess.run", side_effect=fake_run):
                got = memory.forget(source="expense-tracker")
        self.assertEqual(got, envelope)
        argv = captured["cmd"]
        self.assertEqual(argv[:2], ["/usr/bin/cos", "--wire=1"])
        self.assertIn("__memory", argv)
        self.assertIn("forget", argv)
        self.assertIn("--source", argv)
        self.assertIn("expense-tracker", argv)


class BinaryDiscoveryTests(unittest.TestCase):
    def test_missing_cos_binary_raises_unavailable(self) -> None:
        with mock.patch.dict(os.environ, {}, clear=False):
            os.environ.pop("CLAW_COS_BIN", None)
            with mock.patch("shutil.which", return_value=None):
                with self.assertRaises(memory.MemoryUnavailable):
                    memory.remember(text="hi", source="expense-tracker")


if __name__ == "__main__":
    unittest.main()
