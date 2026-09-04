import json
import os
import pathlib
from unittest import mock

import pytest

from test_support import load_local_module


main = load_local_module(
    pathlib.Path(__file__).with_name("main.py"),
    "claw_test_accessibility_manager_main",
    clear_modules=("_shared",),
)


def test_magnifier_uses_accessibility_control_scope():
    completed = mock.Mock(
        returncode=0,
        stdout=json.dumps({"changed": True}),
        stderr="",
    )
    with mock.patch.dict(os.environ, {"COS_BIN": "/usr/local/bin/cos"}), mock.patch.object(
        main.policy, "require"
    ) as require, mock.patch.object(main.subprocess, "run", return_value=completed) as run:
        result = main.set_toggle("magnifier", "on")
    require.assert_called_once_with("ui.accessibility", name="control")
    assert run.call_args.args[0] == [
        "/usr/local/bin/cos",
        "__accessibility",
        "magnifier",
        "--value",
        "on",
    ]
    assert result["changed"] is True


def test_invalid_filter_is_rejected_before_policy():
    with mock.patch.object(main.policy, "require") as require:
        with pytest.raises(ValueError, match="filter requires one of"):
            main.set_filter("custom")
    require.assert_not_called()
