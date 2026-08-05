import json
import os
import pathlib
from unittest import mock

from test_support import load_local_module


main = load_local_module(
    pathlib.Path(__file__).with_name("main.py"),
    "claw_test_systemd_main",
    clear_modules=("_shared",),
)


def test_rejects_option_and_path_units():
    assert not main._valid_unit("--user")
    assert not main._valid_unit("../ssh.service")
    assert main._valid_unit("ssh.service")
    assert main._valid_unit("user@1000.service")


def test_status_uses_observe_capability():
    completed = mock.Mock(
        returncode=0,
        stdout=json.dumps({"state": {"active": True}}),
        stderr="",
    )
    with mock.patch.dict(os.environ, {"COS_BIN": "/usr/local/bin/cos"}), mock.patch.object(
        main.policy, "require"
    ) as require, mock.patch.object(main.subprocess, "run", return_value=completed) as runner:
        result = main.run("status", ["ssh.service"])
    require.assert_called_once_with("sys.observe", name="ssh.service")
    assert runner.call_args[0][0] == [
        "/usr/local/bin/cos",
        "__systemd",
        "status",
        "ssh.service",
    ]
    assert result["state"]["active"] is True


def test_restart_uses_service_capability():
    completed = mock.Mock(returncode=0, stdout=json.dumps({"changed": True}), stderr="")
    with mock.patch.dict(os.environ, {"COS_BIN": "/usr/local/bin/cos"}), mock.patch.object(
        main.policy, "require"
    ) as require, mock.patch.object(main.subprocess, "run", return_value=completed):
        result = main.run("restart", ["demo.service"])
    require.assert_called_once_with("sys.service", name="demo.service")
    assert result["changed"] is True
