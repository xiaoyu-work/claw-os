"""Tests for exec app cmd_start scratch naming and shell scope."""

import io
import os
import pathlib
import sys
import tempfile
import time
import unittest
import warnings
from unittest import mock

from test_support import load_local_module

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
        self.main = load_local_module(
            pathlib.Path(__file__).with_name("main.py"),
            "claw_test_exec_main",
            clear_modules=("_shared",),
        )

    def tearDown(self) -> None:
        sys.modules.pop("claw_test_exec_main", None)
        os.environ.pop("COS_DATA_DIR", None)
        os.environ.pop("COS_SENSITIVE_STDIN", None)
        self.tmp.cleanup()

    def _import_main(self):
        return self.main

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
            "claw_test_exec_main.subprocess.Popen", return_value=FakeProc()
        ), mock.patch("builtins.open", side_effect=tracking_open), mock.patch.object(
            main, "_read_start_stdin", return_value=b""
        ), mock.patch.object(
            main, "_process_start_time", return_value=12345
        ):
            out = main.cmd_start(["/usr/bin/true"])

        self.assertEqual(out.get("pid"), 424242)
        self.assertEqual(out.get("start_time_ticks"), 12345)
        self.assertRegex(out.get("launch_id", ""), r"^[0-9a-f]{32}$")
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
        ), mock.patch.object(
            main, "_read_start_stdin", return_value=b""
        ), mock.patch.object(
            main, "_process_start_time", return_value=12345
        ):
            with mock.patch("claw_test_exec_main.subprocess.Popen", return_value=FakeProc1()):
                main.cmd_start(["/usr/bin/true"])
            with mock.patch("claw_test_exec_main.subprocess.Popen", return_value=FakeProc2()):
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

    @unittest.skipUnless(hasattr(os, "memfd_create"), "requires Linux memfd")
    def test_start_forwards_stdin_once_without_persisting_payload(self) -> None:
        main = self._import_main()
        payload = b'{"context":"nested-secret-value"}'
        received = pathlib.Path(self.tmp.name) / "received.bin"
        stdin_kind = pathlib.Path(self.tmp.name) / "stdin-kind.txt"
        dumpable = pathlib.Path(self.tmp.name) / "dumpable.txt"
        command = [
            sys.executable,
            "-c",
            (
                "import ctypes,os,pathlib,sys;"
                f"pathlib.Path({str(stdin_kind)!r}).write_text("
                "os.readlink('/proc/self/fd/0'),encoding='utf-8');"
                "ctypes.CDLL(None).prctl(4,0,0,0,0);"
                f"pathlib.Path({str(dumpable)!r}).write_text("
                "str(ctypes.CDLL(None).prctl(3,0,0,0,0)),encoding='utf-8');"
                f"pathlib.Path({str(received)!r}).write_bytes(sys.stdin.buffer.read())"
            ),
        ]

        stdin = mock.Mock()
        stdin.buffer = io.BytesIO(payload)
        os.environ["COS_SENSITIVE_STDIN"] = "1"

        with warnings.catch_warnings(), mock.patch.object(
            main.policy, "require", return_value=None
        ), mock.patch.object(main.sys, "stdin", stdin), mock.patch.object(
            main, "_require_proc_isolation", return_value=None
        ):
            warnings.simplefilter("ignore", ResourceWarning)
            result = main.cmd_start(command)

        deadline = time.monotonic() + 5
        while not received.exists() and time.monotonic() < deadline:
            time.sleep(0.01)
        os.waitpid(result["pid"], 0)
        self.assertEqual(received.read_bytes(), payload)
        self.assertTrue(stdin_kind.read_text(encoding="utf-8").startswith("/memfd:"))
        self.assertEqual(dumpable.read_text(encoding="utf-8"), "0")
        self.assertEqual(result["command"], command)
        self.assertTrue(result["transient"])
        self.assertFalse(pathlib.Path(main.REGISTRY_FILE).exists())
        self.assertEqual(
            [
                path.name
                for path in pathlib.Path(main.PROC_DIR).glob("*")
                if path.name.startswith(("stdout.", "stderr."))
            ],
            [],
        )

    def test_start_rejects_oversize_stdin_before_spawning(self) -> None:
        main = self._import_main()

        stdin = mock.Mock()
        stdin.buffer = io.BytesIO(b"x" * (main.MAX_START_STDIN_BYTES + 1))
        os.environ["COS_SENSITIVE_STDIN"] = "1"

        with mock.patch.object(main.policy, "require", return_value=None), mock.patch.object(
            main.sys, "stdin", stdin
        ), mock.patch.object(
            main, "_require_proc_isolation", return_value=None
        ), mock.patch.object(
            main, "_set_non_dumpable", return_value=None
        ), mock.patch("claw_test_exec_main.subprocess.Popen") as popen:
            result = main.cmd_start(["/usr/bin/true"])

        self.assertIn("exceeds configured", result["error"])
        popen.assert_not_called()

    def test_ordinary_start_never_reads_inherited_stdin(self) -> None:
        main = self._import_main()
        stdin = mock.Mock()
        stdin.buffer.read.side_effect = AssertionError("stdin was read")

        class FakeProc:
            pid = 424242

        with mock.patch.object(main.policy, "require", return_value=None), mock.patch.object(
            main.sys, "stdin", stdin
        ), mock.patch.object(
            main, "_process_start_time", return_value=12345
        ), mock.patch(
            "claw_test_exec_main.subprocess.Popen", return_value=FakeProc()
        ) as popen:
            result = main.cmd_start(["/usr/bin/true"])

        self.assertEqual(result["pid"], 424242)
        stdin.buffer.read.assert_not_called()
        self.assertEqual(popen.call_args.kwargs["stdin"], main.subprocess.DEVNULL)

    def test_stop_rejects_zero_and_out_of_range_without_signaling(self) -> None:
        main = self._import_main()
        with mock.patch.object(main.os, "pidfd_open") as pidfd_open, mock.patch.object(
            main.signal, "pidfd_send_signal", create=True
        ) as send_signal:
            self.assertIn("invalid PID", main.cmd_stop(["0"])["error"])
            self.assertIn(
                "invalid PID",
                main.cmd_stop([str(main.MAX_PID + 1)])["error"],
            )
        pidfd_open.assert_not_called()
        send_signal.assert_not_called()

    def test_stop_rejects_reused_pid_and_removes_stale_artifacts(self) -> None:
        main = self._import_main()
        os.makedirs(main.PROC_DIR, exist_ok=True)
        main._save_registry(
            [
                {
                    "launch_id": "launch-old",
                    "pid": 4242,
                    "start_time_ticks": 100,
                    "command": ["/usr/bin/old"],
                }
            ]
        )
        for stream in ("stdout", "stderr"):
            pathlib.Path(main.PROC_DIR, f"{stream}.4242").write_text(
                "old", encoding="utf-8"
            )

        with mock.patch.object(main.policy, "require", return_value=None), mock.patch.object(
            main.os, "pidfd_open", return_value=99
        ), mock.patch.object(main.os, "close"), mock.patch.object(
            main, "_process_start_time", return_value=101
        ), mock.patch.object(
            main.signal, "pidfd_send_signal", create=True
        ) as send_signal:
            result = main.cmd_stop(["4242"])

        self.assertIn("stale", result["error"])
        send_signal.assert_not_called()
        self.assertEqual(main._load_registry(), [])
        self.assertFalse(pathlib.Path(main.PROC_DIR, "stdout.4242").exists())
        self.assertFalse(pathlib.Path(main.PROC_DIR, "stderr.4242").exists())

    def test_stop_uses_opaque_launch_id_and_pidfd(self) -> None:
        main = self._import_main()
        os.makedirs(main.PROC_DIR, exist_ok=True)
        main._save_registry(
            [
                {
                    "launch_id": "launch-current",
                    "pid": 4242,
                    "start_time_ticks": 100,
                    "command": ["/usr/bin/current"],
                }
            ]
        )

        with mock.patch.object(main.policy, "require", return_value=None), mock.patch.object(
            main.os, "pidfd_open", return_value=99
        ), mock.patch.object(main.os, "close"), mock.patch.object(
            main, "_process_start_time", return_value=100
        ), mock.patch.object(
            main.signal, "pidfd_send_signal", create=True
        ) as send_signal:
            result = main.cmd_stop(["launch-current"])

        send_signal.assert_called_once_with(99, main.signal.SIGTERM)
        self.assertEqual(result["launch_id"], "launch-current")
        self.assertEqual(main._load_registry(), [])

    def test_stop_preserves_verified_pid_compatibility(self) -> None:
        main = self._import_main()
        os.makedirs(main.PROC_DIR, exist_ok=True)
        main._save_registry(
            [
                {
                    "launch_id": "launch-current",
                    "pid": 4242,
                    "start_time_ticks": 100,
                    "command": ["/usr/bin/current"],
                }
            ]
        )

        with mock.patch.object(main.policy, "require", return_value=None), mock.patch.object(
            main.os, "pidfd_open", return_value=99
        ), mock.patch.object(main.os, "close"), mock.patch.object(
            main, "_process_start_time", return_value=100
        ), mock.patch.object(
            main.signal, "pidfd_send_signal", create=True
        ) as send_signal:
            result = main.cmd_stop(["4242"])

        send_signal.assert_called_once_with(99, main.signal.SIGTERM)
        self.assertEqual(result["launch_id"], "launch-current")

    def test_private_stdin_requires_strong_yama_policy(self) -> None:
        main = self._import_main()
        with mock.patch("builtins.open", mock.mock_open(read_data="1")):
            with self.assertRaises(PermissionError):
                main._require_proc_isolation()
        with mock.patch("builtins.open", mock.mock_open(read_data="2")):
            main._require_proc_isolation()

    def test_process_start_time_reads_current_process_identity(self) -> None:
        main = self._import_main()
        start_time = main._process_start_time(os.getpid())
        self.assertIsInstance(start_time, int)
        self.assertGreater(start_time, 0)


class TestShellScope(unittest.TestCase):
    """Regression coverage for CR-2 (``cos exec run --shell`` scope).

    Shell syntax can execute substitutions, functions and pipelines not
    represented by any first token, so every shell invocation must require
    an explicit wildcard spawn grant.
    """

    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory()
        os.environ["COS_DATA_DIR"] = self.tmp.name
        self.main = load_local_module(
            pathlib.Path(__file__).with_name("main.py"),
            "claw_test_exec_main",
            clear_modules=("_shared",),
        )

    def tearDown(self) -> None:
        sys.modules.pop("claw_test_exec_main", None)
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

    def test_shell_run_requires_wild(self):
        captured, spy = self._capture()
        # Bypass the bounded-Popen drain path with a stub that mimics
        # its successful return so we never spawn a real shell here.
        with mock.patch.object(self.main.policy, "require", side_effect=spy), mock.patch(
            "claw_test_exec_main._run_bounded",
            return_value=(0, "hi\n", "", False, False, None),
        ):
            self.main.cmd_run(["--shell", "echo hi"])

        spawns = [c for c in captured if c["verb"] == "proc.spawn"]
        self.assertTrue(spawns, "cmd_run --shell never reached proc.spawn check")
        self.assertTrue(any(c.get("wild") is True for c in spawns))

    def test_delimited_shell_token_remains_a_positional_command(self):
        captured, spy = self._capture()
        with mock.patch.object(self.main.policy, "require", side_effect=spy), mock.patch(
            "claw_test_exec_main._run_bounded",
            side_effect=FileNotFoundError,
        ):
            self.main.cmd_run(["--", "--shell"])
        spawns = [call for call in captured if call["verb"] == "proc.spawn"]
        self.assertEqual(spawns, [{"verb": "proc.spawn", "name": "--shell"}])

    def test_shell_run_with_env_assignment_still_requires_wild(self):
        captured, spy = self._capture()
        with mock.patch.object(self.main.policy, "require", side_effect=spy), mock.patch(
            "claw_test_exec_main._run_bounded",
            return_value=(0, "", "", False, False, None),
        ):
            self.main.cmd_run(["--shell", "FOO=bar python3 -c 'print(1)'"])

        spawns = [c for c in captured if c["verb"] == "proc.spawn"]
        self.assertTrue(any(c.get("wild") is True for c in spawns))

    def test_shell_run_falls_back_to_wild_for_unparseable(self):
        """Pure shell builtins / empty command — fall back to ``wild=True``
        so the policy check still happens but is honest about scope.
        """
        captured, spy = self._capture()
        with mock.patch.object(self.main.policy, "require", side_effect=spy), mock.patch(
            "claw_test_exec_main._run_bounded",
            return_value=(0, "", "", False, False, None),
        ):
            self.main.cmd_run(["--shell", ""])

        spawns = [c for c in captured if c["verb"] == "proc.spawn"]
        self.assertTrue(spawns)
        # No specific binary => wild=True (caller must hold the wild grant)
        self.assertTrue(any(c.get("wild") is True for c in spawns))


if __name__ == "__main__":
    unittest.main()
