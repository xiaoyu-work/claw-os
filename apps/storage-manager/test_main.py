import json
import os
import pathlib
from unittest import mock

import pytest

from test_support import load_local_module


main = load_local_module(
    pathlib.Path(__file__).with_name("main.py"),
    "claw_test_storage_manager_main",
    clear_modules=("_shared",),
)


def test_mount_requires_exact_device_scope():
    completed = mock.Mock(
        returncode=0,
        stdout=json.dumps({"changed": True}),
        stderr="",
    )
    with mock.patch.dict(os.environ, {"COS_BIN": "/usr/local/bin/cos"}), mock.patch.object(
        main.os.path, "realpath", return_value="/dev/sdb1"
    ), mock.patch.object(main.policy, "require") as require, mock.patch.object(
        main.subprocess, "run", return_value=completed
    ) as run:
        result = main.mount("/dev/sdb1")
    require.assert_called_once_with("sys.mount", path="/dev/sdb1")
    assert run.call_args.args[0] == [
        "/usr/local/bin/cos",
        "__storage",
        "mount",
        "--device",
        "/dev/sdb1",
    ]
    assert result["changed"] is True


def test_health_uses_diagnostic_scope():
    completed = mock.Mock(
        returncode=0,
        stdout=json.dumps({"status": "ok"}),
        stderr="",
    )
    with mock.patch.dict(os.environ, {"COS_BIN": "/usr/local/bin/cos"}), mock.patch.object(
        main.os.path, "realpath", return_value="/dev/nvme0n1"
    ), mock.patch.object(main.policy, "require") as require, mock.patch.object(
        main.subprocess, "run", return_value=completed
    ):
        result = main.health("/dev/nvme0n1")
    require.assert_called_once_with("sys.storage", name="diagnose")
    assert result["status"] == "ok"


def test_symlink_device_is_rejected_before_policy():
    with mock.patch.object(
        main.os.path, "realpath", return_value="/dev/sdb1"
    ), mock.patch.object(main.policy, "require") as require:
        with pytest.raises(ValueError, match="canonical"):
            main.mount("/dev/disk/by-id/example")
    require.assert_not_called()


@pytest.mark.parametrize(
    "device",
    [
        None,
        "",
        "dev/sdb1",
        "/dev/../sdb1",
        "/dev/sdb1\n",
    ],
)
def test_invalid_device_is_rejected_before_policy(device):
    with mock.patch.object(main.policy, "require") as require:
        with pytest.raises(
            ValueError, match="device must be a canonical absolute /dev path"
        ):
            main.mount(device)
    require.assert_not_called()


def test_unknown_action_is_rejected_before_policy():
    with mock.patch.object(main.policy, "require") as require:
        with pytest.raises(ValueError, match="unknown storage action"):
            main._device_action("format", "/dev/sdb1")
    require.assert_not_called()


@pytest.mark.parametrize(
    ("returncode", "stdout", "message"),
    [
        (0, "{", "Storage Manager broker returned invalid JSON"),
        (0, "[]", "Storage Manager broker returned a non-object result"),
        (0, json.dumps({"error": "UDisks2 unavailable"}), "UDisks2 unavailable"),
        (
            0,
            json.dumps({"error": None}),
            "Storage Manager broker returned an invalid error payload",
        ),
        (7, "{}", "Storage Manager broker exited 7"),
    ],
)
def test_broker_failures_raise(returncode, stdout, message):
    completed = mock.Mock(returncode=returncode, stdout=stdout, stderr="")
    with mock.patch.dict(os.environ, {"COS_BIN": "/usr/local/bin/cos"}), mock.patch.object(
        main.policy, "require"
    ) as require, mock.patch.object(main.subprocess, "run", return_value=completed):
        with pytest.raises(RuntimeError, match=message):
            main.status()
    require.assert_called_once_with("sys.observe", name="storage")


def test_missing_broker_executable_raises():
    with mock.patch.dict(os.environ, {}, clear=True), mock.patch.object(
        main.shutil, "which", return_value=None
    ), mock.patch.object(main.policy, "require") as require:
        with pytest.raises(FileNotFoundError, match="cos binary not found"):
            main.status()
    require.assert_called_once_with("sys.observe", name="storage")


def test_broker_execution_failure_raises():
    with mock.patch.dict(os.environ, {"COS_BIN": "/usr/local/bin/cos"}), mock.patch.object(
        main.policy, "require"
    ) as require, mock.patch.object(
        main.subprocess, "run", side_effect=PermissionError("access denied")
    ):
        with pytest.raises(
            RuntimeError, match="Storage Manager broker execution failed: access denied"
        ):
            main.status()
    require.assert_called_once_with("sys.observe", name="storage")


@pytest.mark.parametrize(
    ("call", "timeout", "action"),
    [
        (lambda: main.status(), main.QUERY_TIMEOUT_SECS, "status"),
        (lambda: main.check("/dev/sdb1"), main.CHECK_TIMEOUT_SECS, "check"),
    ],
)
def test_broker_timeout_raises(call, timeout, action):
    with mock.patch.dict(os.environ, {"COS_BIN": "/usr/local/bin/cos"}), mock.patch.object(
        main.os.path, "realpath", side_effect=lambda value: value
    ), mock.patch.object(main.policy, "require"), mock.patch.object(
        main.subprocess,
        "run",
        side_effect=main.subprocess.TimeoutExpired(["cos"], timeout),
    ) as run:
        with pytest.raises(
            RuntimeError,
            match=rf"Storage Manager broker exceeded {timeout}s for {action}",
        ):
            call()
    assert run.call_args.kwargs["timeout"] == timeout
