import json
import os
import pathlib
from unittest import mock

import pytest

from test_support import load_local_module


main = load_local_module(
    pathlib.Path(__file__).with_name("main.py"),
    "claw_test_network_manager_main",
    clear_modules=("_shared",),
)


def test_wifi_connect_requests_network_and_secret_scopes():
    completed = mock.Mock(returncode=0, stdout=json.dumps({"changed": True}), stderr="")
    with mock.patch.dict(os.environ, {"COS_BIN": "/usr/local/bin/cos"}), mock.patch.object(
        main.policy, "require"
    ) as require, mock.patch.object(main.subprocess, "run", return_value=completed) as run:
        result = main.connect_wifi("Cafe", "default/cafe_psk")
    assert require.call_args_list == [
        mock.call("net.manage", name="wifi"),
        mock.call("secret.read", name="default/cafe_psk"),
    ]
    assert run.call_args.args[0] == [
        "/usr/local/bin/cos",
        "__network",
        "wifi-connect",
        "--target",
        "Cafe",
        "--credential",
        "default/cafe_psk",
    ]
    assert result["changed"] is True


@pytest.mark.parametrize(
    ("ssid", "credential", "message"),
    [
        ("", None, "ssid must be a non-empty string"),
        ("Cafe", "", "credential must be a non-empty string"),
    ],
)
def test_wifi_connect_validates_before_policy(ssid, credential, message):
    with mock.patch.object(main.policy, "require") as require:
        with pytest.raises(ValueError, match=message):
            main.connect_wifi(ssid, credential)
    require.assert_not_called()


def test_airplane_maps_to_fixed_scope():
    completed = mock.Mock(returncode=0, stdout=json.dumps({"changed": True}), stderr="")
    with mock.patch.dict(os.environ, {"COS_BIN": "/usr/local/bin/cos"}), mock.patch.object(
        main.policy, "require"
    ) as require, mock.patch.object(main.subprocess, "run", return_value=completed) as run:
        main.set_airplane_mode("on")
    require.assert_called_once_with("net.manage", name="airplane")
    assert run.call_args.args[0] == [
        "/usr/local/bin/cos",
        "__network",
        "airplane",
        "--state",
        "on",
    ]


def test_invalid_airplane_state_is_rejected_before_policy():
    with mock.patch.object(main.policy, "require") as require:
        with pytest.raises(ValueError, match=r"airplane requires on\|off"):
            main.set_airplane_mode("auto")
    require.assert_not_called()


@pytest.mark.parametrize(
    ("returncode", "stdout", "message"),
    [
        (0, "{", "Network Manager broker returned invalid JSON"),
        (0, "[]", "Network Manager broker returned a non-object result"),
        (
            0,
            json.dumps({"error": "NetworkManager unavailable"}),
            "NetworkManager unavailable",
        ),
        (7, "{}", "Network Manager broker exited 7"),
    ],
)
def test_broker_failures_raise(returncode, stdout, message):
    completed = mock.Mock(returncode=returncode, stdout=stdout, stderr="")
    with mock.patch.dict(os.environ, {"COS_BIN": "/usr/local/bin/cos"}), mock.patch.object(
        main.policy, "require"
    ) as require, mock.patch.object(main.subprocess, "run", return_value=completed):
        with pytest.raises(RuntimeError, match=message):
            main.status()
    require.assert_called_once_with("sys.observe", name="network")


def test_missing_broker_executable_raises():
    with mock.patch.dict(os.environ, {}, clear=True), mock.patch.object(
        main.shutil, "which", return_value=None
    ), mock.patch.object(main.policy, "require") as require:
        with pytest.raises(FileNotFoundError, match="cos binary not found"):
            main.status()
    require.assert_called_once_with("sys.observe", name="network")


def test_broker_timeout_raises():
    with mock.patch.dict(os.environ, {"COS_BIN": "/usr/local/bin/cos"}), mock.patch.object(
        main.policy, "require"
    ) as require, mock.patch.object(
        main.subprocess,
        "run",
        side_effect=main.subprocess.TimeoutExpired(["cos"], main.TIMEOUT_SECS),
    ):
        with pytest.raises(RuntimeError, match="Network Manager broker exceeded"):
            main.status()
    require.assert_called_once_with("sys.observe", name="network")
