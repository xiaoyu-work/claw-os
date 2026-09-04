import json
import os
import pathlib
from unittest import mock

import pytest

from test_support import load_local_module


main = load_local_module(
    pathlib.Path(__file__).with_name("main.py"),
    "claw_test_systemd_main",
    clear_modules=("_shared",),
)


def test_rejects_option_and_path_units():
    with mock.patch.object(main.policy, "require") as require:
        for unit in ("--user", "../ssh.service"):
            with pytest.raises(ValueError, match="unit must be a valid systemd name"):
                main.status(unit)
    require.assert_not_called()


def test_accepts_instantiated_service_unit():
    completed = mock.Mock(returncode=0, stdout="{}", stderr="")
    with mock.patch.dict(os.environ, {"COS_BIN": "/usr/local/bin/cos"}), mock.patch.object(
        main.policy, "require"
    ), mock.patch.object(main.subprocess, "run", return_value=completed) as runner:
        main.status("user@1000.service")
    assert runner.call_args.args[0][-1] == "user@1000.service"


def test_status_uses_observe_capability():
    completed = mock.Mock(
        returncode=0,
        stdout=json.dumps({"state": {"active": True}}),
        stderr="",
    )
    with mock.patch.dict(os.environ, {"COS_BIN": "/usr/local/bin/cos"}), mock.patch.object(
        main.policy, "require"
    ) as require, mock.patch.object(main.subprocess, "run", return_value=completed) as runner:
        result = main.status("ssh.service")
    require.assert_called_once_with("sys.observe", name="ssh.service")
    assert runner.call_args[0][0] == [
        "/usr/local/bin/cos",
        "__systemd",
        "status",
        "ssh.service",
    ]
    assert runner.call_args.kwargs["timeout"] == main.QUERY_TIMEOUT_SECS
    assert result["state"]["active"] is True


def test_restart_uses_service_capability():
    completed = mock.Mock(returncode=0, stdout=json.dumps({"changed": True}), stderr="")
    with mock.patch.dict(os.environ, {"COS_BIN": "/usr/local/bin/cos"}), mock.patch.object(
        main.policy, "require"
    ) as require, mock.patch.object(
        main.subprocess, "run", return_value=completed
    ) as runner:
        result = main.control("restart", "demo.service")
    require.assert_called_once_with("sys.service", name="demo.service")
    assert runner.call_args.kwargs["timeout"] == main.CONTROL_TIMEOUT_SECS
    assert result["changed"] is True


def test_unknown_action_is_rejected_before_policy():
    with mock.patch.object(main.policy, "require") as require:
        with pytest.raises(ValueError, match="unknown systemd action"):
            main.control("unexpected", "demo.service")
    require.assert_not_called()


def test_broker_error_raises():
    completed = mock.Mock(
        returncode=1,
        stdout=json.dumps({"error": "restart failed"}),
        stderr="",
    )
    with mock.patch.dict(os.environ, {"COS_BIN": "/usr/local/bin/cos"}), mock.patch.object(
        main.policy, "require"
    ) as require, mock.patch.object(main.subprocess, "run", return_value=completed):
        with pytest.raises(RuntimeError, match="restart failed"):
            main.control("restart", "demo.service")
    require.assert_called_once_with("sys.service", name="demo.service")


def test_invalid_json_raises():
    completed = mock.Mock(returncode=0, stdout="not-json", stderr="")
    with mock.patch.dict(os.environ, {"COS_BIN": "/usr/local/bin/cos"}), mock.patch.object(
        main.policy, "require"
    ), mock.patch.object(main.subprocess, "run", return_value=completed):
        with pytest.raises(RuntimeError, match="systemd broker returned invalid JSON"):
            main.status("ssh.service")
