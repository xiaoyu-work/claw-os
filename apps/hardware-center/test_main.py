import json
import os
import pathlib
from unittest import mock

from test_support import load_local_module


main = load_local_module(
    pathlib.Path(__file__).with_name("main.py"),
    "claw_test_hardware_center_main",
    clear_modules=("_shared",),
)


def test_summary_uses_hardware_observe_scope():
    completed = mock.Mock(
        returncode=0,
        stdout=json.dumps({"schema": 1}),
        stderr="",
    )
    with mock.patch.dict(os.environ, {"COS_BIN": "/usr/local/bin/cos"}), mock.patch.object(
        main.policy, "require"
    ) as require, mock.patch.object(main.subprocess, "run", return_value=completed) as run:
        result = main.run("summary", [])
    require.assert_called_once_with("sys.observe", name="hardware")
    assert run.call_args.args[0] == ["/usr/local/bin/cos", "__hardware", "summary"]
    assert result["schema"] == 1


def test_hardware_commands_reject_arguments_before_policy():
    with mock.patch.object(main.policy, "require") as require:
        result = main.run("cpu", ["unexpected"])
    assert "error" in result
    require.assert_not_called()
