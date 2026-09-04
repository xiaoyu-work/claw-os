import json
import os
import pathlib
from unittest import mock

import pytest

from test_support import load_local_module


main = load_local_module(
    pathlib.Path(__file__).with_name("main.py"),
    "claw_test_event_center_main",
    clear_modules=("_shared",),
)


def test_security_events_use_event_scope():
    completed = mock.Mock(
        returncode=0,
        stdout=json.dumps({"count": 2}),
        stderr="",
    )
    with mock.patch.dict(os.environ, {"COS_BIN": "/usr/local/bin/cos"}), mock.patch.object(
        main.policy, "require"
    ) as require, mock.patch.object(main.subprocess, "run", return_value=completed) as run:
        result = main.recent(20, "security")
    require.assert_called_once_with("sys.events", name="observe")
    assert run.call_args.args[0] == [
        "/usr/local/bin/cos",
        "__events",
        "recent",
        "--source",
        "security",
        "--limit",
        "20",
    ]
    assert result["count"] == 2


def test_invalid_source_is_rejected_before_policy():
    with mock.patch.object(main.policy, "require") as require:
        with pytest.raises(ValueError, match=r"unknown event source: \*"):
            main.recent(10, "*")
    require.assert_not_called()


@pytest.mark.parametrize("limit", [0, 1001, True, "25"])
def test_invalid_limit_is_rejected_before_policy(limit):
    with mock.patch.object(main.policy, "require") as require:
        with pytest.raises(ValueError, match="limit"):
            main.recent(limit)
    require.assert_not_called()


@pytest.mark.parametrize("pid", [0, 2**32, True, "123"])
def test_invalid_pid_is_rejected_before_policy(pid):
    with mock.patch.object(main.policy, "require") as require:
        with pytest.raises(ValueError, match="pid"):
            main.watch_pid(pid)
    require.assert_not_called()


def test_recent_default_limit_is_forwarded():
    completed = mock.Mock(returncode=0, stdout=json.dumps({"count": 1}), stderr="")
    with mock.patch.dict(os.environ, {"COS_BIN": "/usr/local/bin/cos"}), mock.patch.object(
        main.policy, "require"
    ), mock.patch.object(main.subprocess, "run", return_value=completed) as run:
        main.recent()
    assert run.call_args.args[0][-2:] == ["--limit", "100"]


def test_watch_pid_uses_event_scope_and_safe_argv():
    completed = mock.Mock(returncode=0, stdout=json.dumps({"watching": 123}), stderr="")
    with mock.patch.dict(os.environ, {"COS_BIN": "/usr/local/bin/cos"}), mock.patch.object(
        main.policy, "require"
    ) as require, mock.patch.object(main.subprocess, "run", return_value=completed) as run:
        result = main.watch_pid(123)
    require.assert_called_once_with("sys.events", name="observe")
    assert run.call_args.args[0] == [
        "/usr/local/bin/cos",
        "__events",
        "watch-pid",
        "--pid",
        "123",
    ]
    assert run.call_args.kwargs["timeout"] == main.TIMEOUT_SECS
    assert run.call_args.kwargs["stdin"] is main.subprocess.DEVNULL
    assert result["watching"] == 123


@pytest.mark.parametrize(
    ("returncode", "stdout", "message"),
    [
        (0, "{", "Event Center broker returned invalid JSON"),
        (0, "[]", "Event Center broker returned a non-object result"),
        (0, json.dumps({"error": ""}), "invalid error payload"),
        (0, json.dumps({"error": "event query failed"}), "event query failed"),
        (7, "{}", "Event Center broker exited 7"),
        (
            9,
            json.dumps({"error": "broker detail"}),
            "broker detail",
        ),
    ],
)
def test_broker_payload_failures_raise(returncode, stdout, message):
    completed = mock.Mock(returncode=returncode, stdout=stdout, stderr="")
    with mock.patch.dict(os.environ, {"COS_BIN": "/usr/local/bin/cos"}), mock.patch.object(
        main.policy, "require"
    ) as require, mock.patch.object(main.subprocess, "run", return_value=completed):
        with pytest.raises(RuntimeError, match=message):
            main.status()
    require.assert_called_once_with("sys.events", name="observe")


def test_missing_broker_executable_raises():
    with mock.patch.dict(os.environ, {}, clear=True), mock.patch.object(
        main.shutil, "which", return_value=None
    ), mock.patch.object(main.policy, "require") as require:
        with pytest.raises(FileNotFoundError, match="cos binary not found"):
            main.status()
    require.assert_called_once_with("sys.events", name="observe")


@pytest.mark.parametrize(
    ("error", "exception", "message"),
    [
        (
            FileNotFoundError("missing"),
            FileNotFoundError,
            "Event Center broker executable not found",
        ),
        (
            PermissionError("denied"),
            PermissionError,
            "permission denied launching Event Center broker",
        ),
    ],
)
def test_broker_launch_failures_raise(error, exception, message):
    with mock.patch.dict(os.environ, {"COS_BIN": "/usr/local/bin/cos"}), mock.patch.object(
        main.policy, "require"
    ), mock.patch.object(main.subprocess, "run", side_effect=error):
        with pytest.raises(exception, match=message):
            main.status()


def test_broker_timeout_raises():
    with mock.patch.dict(os.environ, {"COS_BIN": "/usr/local/bin/cos"}), mock.patch.object(
        main.policy, "require"
    ) as require, mock.patch.object(
        main.subprocess,
        "run",
        side_effect=main.subprocess.TimeoutExpired(["cos"], main.TIMEOUT_SECS),
    ):
        with pytest.raises(TimeoutError, match="Event Center broker exceeded"):
            main.status()
    require.assert_called_once_with("sys.events", name="observe")
