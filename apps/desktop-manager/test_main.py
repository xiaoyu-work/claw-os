import json
import os
import pathlib
from unittest import mock

import pytest

from test_support import load_local_module


main = load_local_module(
    pathlib.Path(__file__).with_name("main.py"),
    "claw_test_desktop_manager_main",
    clear_modules=("_shared",),
)


def test_focus_uses_window_control_scope():
    completed = mock.Mock(
        returncode=0,
        stdout=json.dumps({"activated": True}),
        stderr="",
    )
    with mock.patch.dict(os.environ, {"COS_BIN": "/usr/local/bin/cos"}), mock.patch.object(
        main.policy, "require"
    ) as require, mock.patch.object(main.subprocess, "run", return_value=completed) as run:
        result = main.focus_window("window-identifier")
    require.assert_called_once_with("desktop.window", name="control")
    assert run.call_args.args[0] == [
        "/usr/local/bin/cos",
        "__desktop",
        "focus",
        "--identifier",
        "window-identifier",
    ]
    assert result["activated"] is True


def test_restart_requires_window_and_exact_launch_scopes():
    completed = mock.Mock(
        returncode=0,
        stdout=json.dumps({"restarted": True}),
        stderr="",
    )
    with mock.patch.dict(os.environ, {"COS_BIN": "/usr/local/bin/cos"}), mock.patch.object(
        main.policy, "require"
    ) as require, mock.patch.object(main.subprocess, "run", return_value=completed):
        main.restart_application("window-identifier", "com.example.App")
    assert require.call_args_list == [
        mock.call("desktop.window", name="control"),
        mock.call("desktop.launch", name="com.example.App"),
    ]


@pytest.mark.parametrize(
    ("identifier", "app_id", "message"),
    [
        ("--window", "com.example.App", "identifier must be a valid window identifier"),
        ("window-identifier", "*", "app_id must be an exact desktop AppID"),
    ],
)
def test_restart_validates_before_policy(identifier, app_id, message):
    with mock.patch.object(main.policy, "require") as require:
        with pytest.raises(ValueError, match=message):
            main.restart_application(identifier, app_id)
    require.assert_not_called()


@pytest.mark.parametrize(
    ("returncode", "stdout", "message"),
    [
        (0, "{", "Desktop Manager broker returned invalid JSON"),
        (0, "[]", "Desktop Manager broker returned a non-object result"),
        (
            0,
            json.dumps({"error": "compositor unavailable"}),
            "compositor unavailable",
        ),
        (
            0,
            json.dumps({"error": None}),
            "Desktop Manager broker returned an invalid error payload",
        ),
        (7, "{}", "Desktop Manager broker exited 7"),
    ],
)
def test_broker_failures_raise(returncode, stdout, message):
    completed = mock.Mock(returncode=returncode, stdout=stdout, stderr="")
    with mock.patch.dict(os.environ, {"COS_BIN": "/usr/local/bin/cos"}), mock.patch.object(
        main.policy, "require"
    ) as require, mock.patch.object(main.subprocess, "run", return_value=completed):
        with pytest.raises(RuntimeError, match=message):
            main.list_windows()
    require.assert_called_once_with("sys.observe", name="desktop")


def test_missing_broker_executable_raises():
    with mock.patch.dict(os.environ, {}, clear=True), mock.patch.object(
        main.shutil, "which", return_value=None
    ), mock.patch.object(main.policy, "require") as require:
        with pytest.raises(FileNotFoundError, match="cos binary not found"):
            main.list_windows()
    require.assert_called_once_with("sys.observe", name="desktop")


def test_broker_execution_failure_raises():
    with mock.patch.dict(os.environ, {"COS_BIN": "/usr/local/bin/cos"}), mock.patch.object(
        main.policy, "require"
    ) as require, mock.patch.object(
        main.subprocess, "run", side_effect=PermissionError("access denied")
    ):
        with pytest.raises(
            RuntimeError, match="Desktop Manager broker execution failed: access denied"
        ):
            main.list_windows()
    require.assert_called_once_with("sys.observe", name="desktop")


def test_broker_timeout_raises():
    with mock.patch.dict(os.environ, {"COS_BIN": "/usr/local/bin/cos"}), mock.patch.object(
        main.policy, "require"
    ) as require, mock.patch.object(
        main.subprocess,
        "run",
        side_effect=main.subprocess.TimeoutExpired(["cos"], main.TIMEOUT_SECS),
    ):
        with pytest.raises(RuntimeError, match="Desktop Manager broker exceeded"):
            main.list_windows()
    require.assert_called_once_with("sys.observe", name="desktop")
