import json
import os
import pathlib
from unittest import mock

import pytest

from test_support import load_local_module


main = load_local_module(
    pathlib.Path(__file__).with_name("main.py"),
    "claw_test_crash_doctor_main",
    clear_modules=("_shared",),
)


def test_diagnose_uses_sensitive_crash_scope_and_explicit_bounds():
    completed = mock.Mock(
        returncode=0,
        stdout=json.dumps({"status": "warning"}),
        stderr="",
    )
    with mock.patch.dict(os.environ, {"COS_BIN": "/usr/local/bin/cos"}), mock.patch.object(
        main.policy, "require"
    ) as require, mock.patch.object(main.subprocess, "run", return_value=completed) as run:
        result = main.diagnose(120, 10)
    require.assert_called_once_with("sys.crash", name="system")
    assert run.call_args.args[0] == [
        "/usr/local/bin/cos",
        "__crash",
        "diagnose",
        "--since-minutes",
        "120",
        "--limit",
        "10",
    ]
    assert result["status"] == "warning"


def test_backtrace_rejects_untrusted_selector():
    with mock.patch.object(main.policy, "require") as require:
        with pytest.raises(ValueError, match="coredump id"):
            main.backtrace("../../etc/shadow")
    require.assert_not_called()


@pytest.mark.parametrize(
    ("function", "since_minutes", "limit", "message"),
    [
        (main.recent, 0, 20, "since_minutes must be"),
        (main.diagnose, 60, 101, "limit must be"),
        (main.recent, True, 20, "must be integers"),
        (main.diagnose, 60, "10", "must be integers"),
    ],
)
def test_query_bounds_are_validated_before_policy(
    function, since_minutes, limit, message
):
    with mock.patch.object(main.policy, "require") as require:
        with pytest.raises(ValueError, match=message):
            function(since_minutes, limit)
    require.assert_not_called()


def test_backtrace_normalizes_id_and_uses_sensitive_crash_scope():
    coredump_id = "ABCDEF0123456789ABCDEF0123456789:42:1700000000000000"
    completed = mock.Mock(
        returncode=0,
        stdout=json.dumps({"frames": []}),
        stderr="",
    )
    with mock.patch.dict(os.environ, {"COS_BIN": "/usr/local/bin/cos"}), mock.patch.object(
        main.policy, "require"
    ) as require, mock.patch.object(main.subprocess, "run", return_value=completed) as run:
        result = main.backtrace(coredump_id)
    require.assert_called_once_with("sys.crash", name="system")
    assert run.call_args.args[0] == [
        "/usr/local/bin/cos",
        "__crash",
        "backtrace",
        "--id",
        coredump_id.lower(),
    ]
    assert result == {"frames": []}


@pytest.mark.parametrize(
    ("returncode", "stdout", "message"),
    [
        (0, "{", "Crash Doctor broker returned invalid JSON"),
        (0, "[]", "Crash Doctor broker returned a non-object result"),
        (0, json.dumps({"error": "journal unavailable"}), "journal unavailable"),
        (
            0,
            json.dumps({"error": None}),
            "Crash Doctor broker returned an invalid error payload",
        ),
        (7, "{}", "Crash Doctor broker exited 7"),
    ],
)
def test_broker_failures_raise(returncode, stdout, message):
    completed = mock.Mock(returncode=returncode, stdout=stdout, stderr="")
    with mock.patch.dict(os.environ, {"COS_BIN": "/usr/local/bin/cos"}), mock.patch.object(
        main.policy, "require"
    ) as require, mock.patch.object(main.subprocess, "run", return_value=completed):
        with pytest.raises(RuntimeError, match=message):
            main.recent()
    require.assert_called_once_with("sys.crash", name="system")


def test_missing_broker_executable_raises():
    with mock.patch.dict(os.environ, {}, clear=True), mock.patch.object(
        main.shutil, "which", return_value=None
    ), mock.patch.object(main.policy, "require") as require:
        with pytest.raises(FileNotFoundError, match="cos binary not found"):
            main.recent()
    require.assert_called_once_with("sys.crash", name="system")


@pytest.mark.parametrize(
    ("exception", "error_type", "message"),
    [
        (
            FileNotFoundError("missing"),
            FileNotFoundError,
            "Crash Doctor broker executable not found",
        ),
        (
            PermissionError("access denied"),
            PermissionError,
            "permission denied launching Crash Doctor broker",
        ),
    ],
)
def test_broker_launch_failures_raise_precise_exceptions(
    exception, error_type, message
):
    with mock.patch.dict(os.environ, {"COS_BIN": "/usr/local/bin/cos"}), mock.patch.object(
        main.policy, "require"
    ) as require, mock.patch.object(main.subprocess, "run", side_effect=exception):
        with pytest.raises(error_type, match=message):
            main.recent()
    require.assert_called_once_with("sys.crash", name="system")


def test_broker_timeout_raises():
    with mock.patch.dict(os.environ, {"COS_BIN": "/usr/local/bin/cos"}), mock.patch.object(
        main.policy, "require"
    ) as require, mock.patch.object(
        main.subprocess,
        "run",
        side_effect=main.subprocess.TimeoutExpired(["cos"], main.TIMEOUT_SECS),
    ):
        with pytest.raises(TimeoutError, match="Crash Doctor broker exceeded"):
            main.recent()
    require.assert_called_once_with("sys.crash", name="system")
