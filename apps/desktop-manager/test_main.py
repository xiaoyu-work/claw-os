import json
import os
import pathlib
from unittest import mock

from test_support import load_local_module


main = load_local_module(
    pathlib.Path(__file__).with_name("main.py"),
    "claw_test_desktop_manager_main",
    clear_modules=("_shared",),
)


def test_focus_uses_window_control_scope():
    completed = mock.Mock(
        returncode=0,
        stdout=json.dumps({"activated": True}),
        stderr="",
    )
    with mock.patch.dict(os.environ, {"COS_BIN": "/usr/local/bin/cos"}), mock.patch.object(
        main.policy, "require"
    ) as require, mock.patch.object(main.subprocess, "run", return_value=completed) as run:
        result = main.run("focus", ["window-identifier"])
    require.assert_called_once_with("desktop.window", name="control")
    assert run.call_args.args[0] == [
        "/usr/local/bin/cos",
        "__desktop",
        "focus",
        "--identifier",
        "window-identifier",
    ]
    assert result["activated"] is True


def test_restart_requires_window_and_exact_launch_scopes():
    completed = mock.Mock(
        returncode=0,
        stdout=json.dumps({"restarted": True}),
        stderr="",
    )
    with mock.patch.dict(os.environ, {"COS_BIN": "/usr/local/bin/cos"}), mock.patch.object(
        main.policy, "require"
    ) as require, mock.patch.object(main.subprocess, "run", return_value=completed):
        main.run("restart", ["window-identifier", "com.example.App"])
    assert require.call_args_list == [
        mock.call("desktop.window", name="control"),
        mock.call("desktop.launch", name="com.example.App"),
    ]


def test_restart_rejects_glob_app_id_before_policy():
    with mock.patch.object(main.policy, "require") as require:
        result = main.run("restart", ["window-identifier", "*"])
    assert "error" in result
    require.assert_not_called()
