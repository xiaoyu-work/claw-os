import json
import os
import pathlib
from unittest import mock

from test_support import load_local_module


main = load_local_module(
    pathlib.Path(__file__).with_name("main.py"),
    "claw_test_location_manager_main",
    clear_modules=("_shared",),
)


def test_locate_requires_location_capability():
    completed = mock.Mock(
        returncode=0,
        stdout=json.dumps({"provider": "geoclue2"}),
        stderr="",
    )
    with mock.patch.dict(os.environ, {"COS_BIN": "/usr/local/bin/cos"}), mock.patch.object(
        main.policy, "require"
    ) as require, mock.patch.object(main.subprocess, "run", return_value=completed):
        result = main.run("locate", ["street"])
    require.assert_called_once_with("device.location", wild=True)
    assert result["provider"] == "geoclue2"


def test_invalid_accuracy_is_rejected_before_policy():
    with mock.patch.object(main.policy, "require") as require:
        result = main.run("timezone", ["gps"])
    assert "error" in result
    require.assert_not_called()
