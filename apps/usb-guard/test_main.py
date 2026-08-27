import json
import os
import pathlib
from unittest import mock

from test_support import load_local_module


main = load_local_module(
    pathlib.Path(__file__).with_name("main.py"),
    "claw_test_usb_guard_main",
    clear_modules=("_shared",),
)


def test_block_uses_usb_control_scope():
    completed = mock.Mock(
        returncode=0,
        stdout=json.dumps({"blocked": True}),
        stderr="",
    )
    with mock.patch.dict(os.environ, {"COS_BIN": "/usr/local/bin/cos"}), mock.patch.object(
        main.policy, "require"
    ) as require, mock.patch.object(main.subprocess, "run", return_value=completed):
        result = main.run("block", ["1-2.3", "--confirm"])
    require.assert_called_once_with("device.usb", name="control")
    assert result["blocked"] is True


def test_deauthorization_requires_confirm_before_policy():
    for args in (["1-2", "off"], ["1-2", "off", "--confirm=false"]):
        with mock.patch.object(main.policy, "require") as require:
            result = main.run("authorize", args)
        assert "error" in result
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
        result = main.run("authorize", ["1-2", "on"])
    require.assert_called_once_with("device.usb", name="control")
    assert result["authorized"] is True


def test_authorization_rejects_unnecessary_confirm_before_policy():
    with mock.patch.object(main.policy, "require") as require:
        result = main.run("authorize", ["1-2", "on", "--confirm"])
    assert "error" in result
    require.assert_not_called()
