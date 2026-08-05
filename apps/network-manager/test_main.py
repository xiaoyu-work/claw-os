import json
import os
import pathlib
from unittest import mock

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
    ) as require, mock.patch.object(main.subprocess, "run", return_value=completed):
        result = main.run("wifi-connect", ["Cafe", "default/cafe_psk"])
    assert require.call_args_list == [
        mock.call("net.manage", name="wifi"),
        mock.call("secret.read", name="default/cafe_psk"),
    ]
    assert result["changed"] is True


def test_airplane_maps_to_fixed_scope():
    completed = mock.Mock(returncode=0, stdout=json.dumps({"changed": True}), stderr="")
    with mock.patch.dict(os.environ, {"COS_BIN": "/usr/local/bin/cos"}), mock.patch.object(
        main.policy, "require"
    ) as require, mock.patch.object(main.subprocess, "run", return_value=completed):
        main.run("airplane", ["on"])
    require.assert_called_once_with("net.manage", name="airplane")
