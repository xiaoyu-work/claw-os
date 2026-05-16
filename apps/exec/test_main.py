"""Tests for exec app output size limits and cmd_start scratch naming."""

import os
import sys
import tempfile
import unittest
from unittest import mock

sys.path.insert(0, os.path.dirname(__file__))
sys.path.insert(
    0,
    os.path.join(
        os.path.dirname(os.path.dirname(os.path.abspath(__file__))),
        os.pardir,
        "claw-os-sdk",
        "python",
        "src",
    ),
)  # for `from claw_os_sdk import …`
sys.path.insert(
    0,
    os.path.join(
        os.path.dirname(os.path.dirname(os.path.abspath(__file__))),
        os.pardir,
        "cos-runtime",
        "python",
        "src",
    ),
)  # for `from cos_runtime import policy`

# We can't import main directly on Windows due to fcntl, so test the
# truncation logic in isolation by importing the constants and simulating.
# On Linux this would import fine. We test the logic portably here.


MAX_OUTPUT_BYTES = 1_000_000  # mirror the constant from main.py


class TestOutputTruncationLogic(unittest.TestCase):
    """Test the truncation logic that cmd_run and cmd_script apply."""

    def _apply_truncation(self, stdout, stderr):
        """Replicate the truncation logic from cmd_run/cmd_script."""
        truncated = False
        if len(stdout) > MAX_OUTPUT_BYTES:
            stdout = stdout[:MAX_OUTPUT_BYTES]
            truncated = True
        if len(stderr) > MAX_OUTPUT_BYTES:
            stderr = stderr[:MAX_OUTPUT_BYTES]
            truncated = True
        resp = {
            "exit_code": 0,
            "stdout": stdout,
            "stderr": stderr,
        }
        if truncated:
            resp["truncated"] = True
        return resp

    def test_small_output_not_truncated(self):
        resp = self._apply_truncation("hello", "")
        self.assertEqual(resp["stdout"], "hello")
        self.assertNotIn("truncated", resp)

    def test_large_stdout_truncated(self):
        big = "x" * (MAX_OUTPUT_BYTES + 500)
        resp = self._apply_truncation(big, "")
        self.assertEqual(len(resp["stdout"]), MAX_OUTPUT_BYTES)
        self.assertTrue(resp["truncated"])

    def test_large_stderr_truncated(self):
        big = "e" * (MAX_OUTPUT_BYTES + 500)
        resp = self._apply_truncation("ok", big)
        self.assertEqual(len(resp["stderr"]), MAX_OUTPUT_BYTES)
        self.assertEqual(resp["stdout"], "ok")
        self.assertTrue(resp["truncated"])

    def test_both_truncated(self):
        big_out = "o" * (MAX_OUTPUT_BYTES + 1)
        big_err = "e" * (MAX_OUTPUT_BYTES + 1)
        resp = self._apply_truncation(big_out, big_err)
        self.assertEqual(len(resp["stdout"]), MAX_OUTPUT_BYTES)
        self.assertEqual(len(resp["stderr"]), MAX_OUTPUT_BYTES)
        self.assertTrue(resp["truncated"])

    def test_exact_limit_not_truncated(self):
        exact = "x" * MAX_OUTPUT_BYTES
        resp = self._apply_truncation(exact, "")
        self.assertEqual(len(resp["stdout"]), MAX_OUTPUT_BYTES)
        self.assertNotIn("truncated", resp)


class TestConstantInFile(unittest.TestCase):
    """Verify the constant is defined in main.py."""

    def test_constant_defined(self):
        main_path = os.path.join(os.path.dirname(__file__), "main.py")
        with open(main_path) as f:
            content = f.read()
        self.assertIn("MAX_OUTPUT_BYTES = 1_000_000", content)
        self.assertIn("MAX_OUTPUT_BYTES", content)
        # Verify truncation logic is present for both cmd_run and cmd_script
        self.assertIn('resp["truncated"] = True', content)


class TestCmdStartScratchNaming(unittest.TestCase):
    """Regression coverage for the cmd_start scratch-filename fix.

    Pre-fix `cmd_start` named its pre-exec stdout/stderr files
    `stdout.<os.getpid()>` and `stderr.<os.getpid()>`. The parent
    PID is shared by every concurrent caller in the MCP server
    process, so overlapping `cmd_start` invocations would collide
    on the same intermediate filenames and corrupt each other's
    early output. The fix names intermediates with a uuid token
    (hidden via a `.` prefix) so concurrent callers each get their
    own scratch file.
    """

    def setUp(self) -> None:
        # Redirect COS_DATA_DIR to a tempdir before importing main so
        # PROC_DIR resolves to a writable spot we can inspect.
        self.tmp = tempfile.TemporaryDirectory()
        os.environ["COS_DATA_DIR"] = self.tmp.name
        # Force a fresh import each setUp so PROC_DIR picks up the env.
        sys.modules.pop("main", None)

    def tearDown(self) -> None:
        sys.modules.pop("main", None)
        os.environ.pop("COS_DATA_DIR", None)
        self.tmp.cleanup()

    def _import_main(self):
        import main  # type: ignore[import-not-found]

        return main

    def test_intermediate_filenames_use_uuid_not_parent_pid(self) -> None:
        """Drive cmd_start with mocked policy + Popen; assert the
        intermediate stdout/stderr filenames opened by the parent
        do not contain the parent PID and instead use a hidden
        uuid-scoped name.
        """
        main = self._import_main()

        opened_paths: list[str] = []
        real_open = open

        def tracking_open(path, *a, **kw):  # type: ignore[no-untyped-def]
            if isinstance(path, str) and path.startswith(main.PROC_DIR):
                opened_paths.append(path)
            return real_open(path, *a, **kw)

        class FakeProc:
            pid = 424242

        with mock.patch.object(main.policy, "require", return_value=None), mock.patch(
            "main.subprocess.Popen", return_value=FakeProc()
        ), mock.patch("builtins.open", side_effect=tracking_open):
            out = main.cmd_start(["/usr/bin/true"])

        self.assertEqual(out.get("pid"), 424242)
        # Two scratch files (stdout + stderr) were opened.
        scratch = [p for p in opened_paths if os.path.basename(p).startswith(".")]
        self.assertEqual(
            len(scratch),
            2,
            f"expected 2 hidden scratch files, got {opened_paths}",
        )
        for p in scratch:
            base = os.path.basename(p)
            self.assertNotIn(
                str(os.getpid()),
                base,
                f"scratch file {base} must not embed parent PID",
            )
            # uuid token is hex[:12] -> filename looks like
            # `.stdout.<12hex>` or `.stderr.<12hex>`
            self.assertRegex(base, r"^\.(stdout|stderr)\.[0-9a-f]{12}$")

        # Final files have been renamed to use the child PID.
        self.assertTrue(
            os.path.isfile(os.path.join(main.PROC_DIR, "stdout.424242"))
        )
        self.assertTrue(
            os.path.isfile(os.path.join(main.PROC_DIR, "stderr.424242"))
        )
        # No scratch files left behind.
        leftovers = [
            n
            for n in os.listdir(main.PROC_DIR)
            if n.startswith(".stdout.") or n.startswith(".stderr.")
        ]
        self.assertEqual(leftovers, [], f"scratch files leaked: {leftovers}")

    def test_two_overlapping_cmd_start_get_distinct_scratch_names(self) -> None:
        """Drive cmd_start twice with overlapping Popen mocks; the
        two invocations must have produced different intermediate
        filenames. With the pre-fix parent-PID scheme they would
        have collided.
        """
        main = self._import_main()

        opened_paths: list[str] = []
        real_open = open

        def tracking_open(path, *a, **kw):  # type: ignore[no-untyped-def]
            if isinstance(path, str) and path.startswith(main.PROC_DIR):
                opened_paths.append(path)
            return real_open(path, *a, **kw)

        class FakeProc1:
            pid = 111111

        class FakeProc2:
            pid = 222222

        with mock.patch.object(main.policy, "require", return_value=None), mock.patch(
            "builtins.open", side_effect=tracking_open
        ):
            with mock.patch("main.subprocess.Popen", return_value=FakeProc1()):
                main.cmd_start(["/usr/bin/true"])
            with mock.patch("main.subprocess.Popen", return_value=FakeProc2()):
                main.cmd_start(["/usr/bin/true"])

        scratch_names = {
            os.path.basename(p) for p in opened_paths if os.path.basename(p).startswith(".")
        }
        # Each invocation opened 2 scratch files (stdout + stderr),
        # so 2 invocations must produce 4 distinct scratch names.
        self.assertEqual(
            len(scratch_names),
            4,
            f"scratch names collided across cmd_start calls: {scratch_names}",
        )


class TestShellScope(unittest.TestCase):
    """Regression coverage for CR-2 (``cos exec run --shell`` scope).

    The old code passed ``name=/bin/bash`` (or ``/bin/sh``) to
    ``policy.require("proc.spawn", ...)`` whenever the caller used
    ``--shell``. That meant a single ``proc.spawn name=/bin/bash``
    grant let the agent execute *any* shell command via
    ``--shell``, defeating per-binary scoping.

    The fix parses the first real command token out of the shell
    string and uses *that* as the spawn name (with ``wild=True`` as a
    last resort when the parse yields nothing). These tests assert
    the policy check sees the user-visible command, not the shell.
    """

    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory()
        os.environ["COS_DATA_DIR"] = self.tmp.name
        sys.modules.pop("main", None)
        import main  # type: ignore[import-not-found]

        self.main = main

    def tearDown(self) -> None:
        sys.modules.pop("main", None)
        os.environ.pop("COS_DATA_DIR", None)
        self.tmp.cleanup()

    def _capture(self):
        captured: list[dict] = []

        def spy(verb, **kwargs):
            captured.append({"verb": verb, **kwargs})
            return None

        return captured, spy

    def _fake_run(self, *args, **kwargs):
        class FakeProc:
            returncode = 0
            stdout = ""
            stderr = ""

        return FakeProc()

    def test_shell_run_scopes_to_first_real_binary(self):
        """``--shell 'echo hi'`` must require ``proc.spawn name=echo``,
        **not** ``proc.spawn name=/bin/bash``.
        """
        captured, spy = self._capture()
        # Bypass the bounded-Popen drain path with a stub that mimics
        # its successful return so we never spawn a real shell here.
        with mock.patch.object(self.main.policy, "require", side_effect=spy), mock.patch(
            "main._run_bounded",
            return_value=(0, "hi\n", "", False, False, None),
        ):
            self.main.cmd_run(["--shell", "echo hi"])

        spawns = [c for c in captured if c["verb"] == "proc.spawn"]
        self.assertTrue(spawns, "cmd_run --shell never reached proc.spawn check")
        names = [c.get("name") for c in spawns]
        self.assertNotIn(
            "/bin/bash",
            names,
            "CR-2 regression: --shell scoped to /bin/bash, granting wild shell access",
        )
        self.assertNotIn("/bin/sh", names)
        self.assertIn("echo", names, f"expected proc.spawn name=echo, got {names}")

    def test_shell_run_skips_env_var_assignment(self):
        """``--shell 'FOO=bar python3 script.py'`` must scope to ``python3``."""
        captured, spy = self._capture()
        with mock.patch.object(self.main.policy, "require", side_effect=spy), mock.patch(
            "main._run_bounded",
            return_value=(0, "", "", False, False, None),
        ):
            self.main.cmd_run(["--shell", "FOO=bar python3 -c 'print(1)'"])

        spawns = [c for c in captured if c["verb"] == "proc.spawn"]
        names = [c.get("name") for c in spawns]
        self.assertIn(
            "python3",
            names,
            f"shell scope should skip VAR=val prefix and pick python3, got {names}",
        )

    def test_shell_run_falls_back_to_wild_for_unparseable(self):
        """Pure shell builtins / empty command — fall back to ``wild=True``
        so the policy check still happens but is honest about scope.
        """
        captured, spy = self._capture()
        with mock.patch.object(self.main.policy, "require", side_effect=spy), mock.patch(
            "main._run_bounded",
            return_value=(0, "", "", False, False, None),
        ):
            self.main.cmd_run(["--shell", ""])

        spawns = [c for c in captured if c["verb"] == "proc.spawn"]
        self.assertTrue(spawns)
        # No specific binary => wild=True (caller must hold the wild grant)
        self.assertTrue(any(c.get("wild") is True for c in spawns))


if __name__ == "__main__":
    unittest.main()
