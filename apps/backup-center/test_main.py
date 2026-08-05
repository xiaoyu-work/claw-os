import json
import os
import pathlib
from unittest import mock

from test_support import load_local_module


main = load_local_module(
    pathlib.Path(__file__).with_name("main.py"),
    "claw_test_backup_center_main",
    clear_modules=("_shared",),
)


def test_backup_requests_repo_source_and_secret_scopes():
    completed = mock.Mock(
        returncode=0,
        stdout=json.dumps({"action": "backup"}),
        stderr="",
    )
    with mock.patch.dict(os.environ, {"COS_BIN": "/usr/local/bin/cos"}), mock.patch.object(
        main.os.path, "lexists", return_value=True
    ), mock.patch.object(main.os.path, "islink", return_value=False), mock.patch.object(
        main.os.path, "realpath", side_effect=lambda value: value
    ), mock.patch.object(main.policy, "require") as require, mock.patch.object(
        main.subprocess, "run", return_value=completed
    ):
        main.run(
            "backup",
            ["/media/user/backup/repo", "/home/user/Documents", "default/restic"],
        )
    assert require.call_args_list == [
        mock.call("data.backup", path="/media/user/backup/repo"),
        mock.call("data.backup", path="/home/user/Documents"),
        mock.call("secret.read", name="default/restic"),
    ]


def test_restore_requires_confirm_before_policy():
    with mock.patch.object(main.policy, "require") as require:
        result = main.run(
            "restore",
            ["/media/user/backup/repo", "latest", "/home/user/restore", "default/restic"],
        )
    assert "error" in result
    require.assert_not_called()
