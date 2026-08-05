import json
import os
import pathlib
from unittest import mock

from test_support import load_local_module


main = load_local_module(
    pathlib.Path(__file__).with_name("main.py"),
    "claw_test_bluetooth_manager_main",
    clear_modules=("_shared",),
)


def test_pair_normalizes_addresses_and_uses_fixed_scope():
    completed = mock.Mock(returncode=0, stdout=json.dumps({"changed": True}), stderr="")
    with mock.patch.dict(os.environ, {"COS_BIN": "/usr/local/bin/cos"}), mock.patch.object(
        main.policy, "require"
    ) as require, mock.patch.object(main.subprocess, "run", return_value=completed) as run:
        result = main.run("pair", ["aa:bb:cc:dd:ee:ff", "11:22:33:44:55:66"])
    require.assert_called_once_with("device.bluetooth", name="control")
    assert run.call_args.args[0] == [
        "/usr/local/bin/cos",
        "__bluetooth",
        "pair",
        "--adapter",
        "AA:BB:CC:DD:EE:FF",
        "--device",
        "11:22:33:44:55:66",
    ]
    assert result["changed"] is True


def test_scan_duration_is_validated_before_policy():
    with mock.patch.object(main.policy, "require") as require:
        result = main.run("scan", ["AA:BB:CC:DD:EE:FF", "61"])
    assert "error" in result
    require.assert_not_called()


def test_invalid_address_is_rejected_before_policy():
    with mock.patch.object(main.policy, "require") as require:
        result = main.run("connect", ["hci0", "not-a-device"])
    assert "error" in result
    require.assert_not_called()


def test_pair_response_uses_stdin_not_argv():
    completed = mock.Mock(
        returncode=0,
        stdout=json.dumps({"status": "pending"}),
        stderr="",
    )
    pairing_id = "0123456789abcdef0123456789abcdef"
    with mock.patch.dict(os.environ, {"COS_BIN": "/usr/local/bin/cos"}), mock.patch.object(
        main.policy, "require"
    ), mock.patch.object(main.subprocess, "run", return_value=completed) as run:
        main.run("pair-respond", [pairing_id, "123456"])
    assert run.call_args.args[0] == [
        "/usr/local/bin/cos",
        "__bluetooth",
        "pair-respond",
        "--pairing-id",
        pairing_id,
        "--response-stdin",
    ]
    assert run.call_args.kwargs["input"] == "123456\n"
