import json
import os
import pathlib
from unittest import mock

import pytest

from test_support import load_local_module


main = load_local_module(
    pathlib.Path(__file__).with_name("main.py"),
    "claw_test_power_manager_main",
    clear_modules=("_shared",),
)


def test_reboot_requires_confirm_before_policy():
    with mock.patch.object(main.policy, "require") as require:
        with pytest.raises(ValueError, match="reboot requires confirm=true"):
            main.request_power("reboot", False)
    require.assert_not_called()


def test_confirmed_reboot_uses_critical_power_scope():
    completed = mock.Mock(
        returncode=0,
        stdout=json.dumps({"requested": True}),
        stderr="",
    )
    with mock.patch.dict(os.environ, {"COS_BIN": "/usr/local/bin/cos"}), mock.patch.object(
        main.policy, "require"
    ) as require, mock.patch.object(main.subprocess, "run", return_value=completed) as run:
        result = main.request_power("reboot", True)
    require.assert_called_once_with("sys.power", wild=True)
    assert run.call_args.args[0] == [
        "/usr/local/bin/cos",
        "__power",
        "reboot",
        "--confirm",
    ]
    assert result["requested"] is True


def test_broker_error_raises():
    completed = mock.Mock(
        returncode=1,
        stdout=json.dumps({"error": "reboot failed"}),
        stderr="",
    )
    with mock.patch.dict(os.environ, {"COS_BIN": "/usr/local/bin/cos"}), mock.patch.object(
        main.policy, "require"
    ) as require, mock.patch.object(main.subprocess, "run", return_value=completed):
        with pytest.raises(RuntimeError, match="reboot failed"):
            main.request_power("reboot", True)
    require.assert_called_once_with("sys.power", wild=True)
