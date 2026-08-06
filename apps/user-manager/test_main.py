import json
import os
import pathlib
from unittest import mock

from test_support import load_local_module


main = load_local_module(
    pathlib.Path(__file__).with_name("main.py"),
    "claw_test_user_manager_main",
    clear_modules=("_shared",),
)


def test_set_password_uses_identity_and_secret_scopes():
    completed = mock.Mock(
        returncode=0,
        stdout=json.dumps({"changed": True}),
        stderr="",
    )
    with mock.patch.dict(os.environ, {"COS_BIN": "/usr/local/bin/cos"}), mock.patch.object(
        main.policy, "require"
    ) as require, mock.patch.object(main.subprocess, "run", return_value=completed):
        main.run("set-password", ["alice", "default/alice-password"])
    assert require.call_args_list == [
        mock.call("sys.identity", name="manage"),
        mock.call("secret.read", name="default/alice-password"),
    ]


def test_delete_user_requires_confirm_before_policy():
    with mock.patch.object(main.policy, "require") as require:
        result = main.run("delete-user", ["alice"])
    assert "error" in result
    require.assert_not_called()
