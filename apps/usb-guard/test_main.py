import json
import os
import pathlib
from unittest import mock

import pytest

from test_support import load_local_module


main = load_local_module(
    pathlib.Path(__file__).with_name("main.py"),
    "claw_test_usb_guard_main",
    clear_modules=("_shared",),
)


def test_block_uses_usb_control_scope_and_broker_command():
    completed = mock.Mock(
        returncode=0,
        stdout=json.dumps({"blocked": True}),
        stderr="",
    )
    with mock.patch.dict(os.environ, {"COS_BIN": "/usr/local/bin/cos"}), mock.patch.object(
        main.policy, "require"
    ) as require, mock.patch.object(main.subprocess, "run", return_value=completed) as run:
        result = main.block("1-2.3", True)
    require.assert_called_once_with("device.usb", name="control")
    assert run.call_args.args[0] == [
        "/usr/local/bin/cos",
        "__usb",
        "block",
        "--device",
        "1-2.3",
        "--confirm",
    ]
    assert run.call_args.kwargs["timeout"] == main.TIMEOUT_SECS
    assert result["blocked"] is True


@pytest.mark.parametrize(
    ("call", "message"),
    [
        (lambda: main.authorize("not-a-device", "on"), "device must be"),
        (lambda: main.authorize("1-2", "invalid"), "state must be on or off"),
        (lambda: main.authorize("1-2", "off", False), "deauthorization requires"),
        (lambda: main.authorize("1-2", "on", True), "authorization does not accept"),
        (lambda: main.authorize("1-2", "on", None), "confirm must be a boolean"),
        (lambda: main.block("1-2", False), "block requires confirm=true"),
        (lambda: main.eject("invalid", True), "device must be"),
        (lambda: main.unblock("not-a-rule", True), "rule_id must be"),
        (lambda: main.restore("not-a-token", True), "backup_token must be"),
        (lambda: main._execute("unexpected"), "unknown USB Guard action"),
    ],
)
def test_invalid_inputs_are_rejected_before_policy(call, message):
    with mock.patch.object(main.policy, "require") as require:
        with pytest.raises(ValueError, match=message):
            call()
    require.assert_not_called()


def test_authorization_without_confirm_preserves_historical_behavior():
    completed = mock.Mock(
        returncode=0,
        stdout=json.dumps({"authorized": True}),
        stderr="",
    )
    with mock.patch.dict(os.environ, {"COS_BIN": "/usr/local/bin/cos"}), mock.patch.object(
        main.policy, "require"
    ) as require, mock.patch.object(main.subprocess, "run", return_value=completed):
        result = main.authorize("1-2", "on")
    require.assert_called_once_with("device.usb", name="control")
    assert result["authorized"] is True


def test_unblock_normalizes_rule_id():
    completed = mock.Mock(returncode=0, stdout=json.dumps({"unblocked": True}), stderr="")
    rule_id = "ABCDEF0123456789ABCDEF0123456789"
    with mock.patch.dict(os.environ, {"COS_BIN": "/usr/local/bin/cos"}), mock.patch.object(
        main.policy, "require"
    ), mock.patch.object(main.subprocess, "run", return_value=completed) as run:
        main.unblock(rule_id, True)
    assert run.call_args.args[0] == [
        "/usr/local/bin/cos",
        "__usb",
        "unblock",
        "--rule-id",
        rule_id.lower(),
        "--confirm",
    ]


@pytest.mark.parametrize(
    ("returncode", "stdout", "message"),
    [
        (0, "{", "USB Guard broker returned invalid JSON"),
        (0, "[]", "USB Guard broker returned a non-object result"),
        (0, json.dumps({"error": "usb control failed"}), "usb control failed"),
        (
            0,
            json.dumps({"error": None}),
            "USB Guard broker returned an invalid error payload",
        ),
        (7, "{}", "USB Guard broker exited 7"),
    ],
)
def test_broker_failures_raise(returncode, stdout, message):
    completed = mock.Mock(returncode=returncode, stdout=stdout, stderr="")
    with mock.patch.dict(os.environ, {"COS_BIN": "/usr/local/bin/cos"}), mock.patch.object(
        main.policy, "require"
    ) as require, mock.patch.object(main.subprocess, "run", return_value=completed):
        with pytest.raises(RuntimeError, match=message):
            main.status()
    require.assert_called_once_with("sys.observe", name="usb")


def test_missing_broker_executable_raises():
    with mock.patch.dict(os.environ, {}, clear=True), mock.patch.object(
        main.shutil, "which", return_value=None
    ), mock.patch.object(main.policy, "require") as require:
        with pytest.raises(FileNotFoundError, match="USB Guard broker unavailable"):
            main.status()
    require.assert_called_once_with("sys.observe", name="usb")


def test_broker_execution_failure_raises():
    with mock.patch.dict(os.environ, {"COS_BIN": "/usr/local/bin/cos"}), mock.patch.object(
        main.policy, "require"
    ) as require, mock.patch.object(
        main.subprocess, "run", side_effect=PermissionError("access denied")
    ):
        with pytest.raises(
            RuntimeError, match="USB Guard broker execution failed: access denied"
        ):
            main.status()
    require.assert_called_once_with("sys.observe", name="usb")


def test_broker_timeout_raises():
    with mock.patch.dict(os.environ, {"COS_BIN": "/usr/local/bin/cos"}), mock.patch.object(
        main.policy, "require"
    ) as require, mock.patch.object(
        main.subprocess,
        "run",
        side_effect=main.subprocess.TimeoutExpired(["cos"], main.TIMEOUT_SECS),
    ):
        with pytest.raises(RuntimeError, match="USB Guard broker exceeded"):
            main.status()
    require.assert_called_once_with("sys.observe", name="usb")
