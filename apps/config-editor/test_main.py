import json
import os
import pathlib
from unittest import mock

from test_support import load_local_module


main = load_local_module(
    pathlib.Path(__file__).with_name("main.py"),
    "claw_test_config_editor_main",
    clear_modules=("_shared",),
)


def test_apply_uses_exact_target_and_source_scopes():
    completed = mock.Mock(
        returncode=0,
        stdout=json.dumps({"applied": True}),
        stderr="",
    )
    with mock.patch.dict(os.environ, {"COS_BIN": "/usr/local/bin/cos"}), mock.patch.object(
        main.os.path, "lexists", return_value=True
    ), mock.patch.object(main.os.path, "islink", return_value=False), mock.patch.object(
        main.os.path, "realpath", side_effect=lambda value: value
    ), mock.patch.object(main.policy, "require") as require, mock.patch.object(
        main.subprocess, "run", return_value=completed
    ):
        result = main.run("apply", ["/etc/hosts", "/home/user/hosts.new", "--confirm"])
    assert require.call_args_list == [
        mock.call("sys.config", path="/etc/hosts"),
        mock.call("fs.read", path="/home/user/hosts.new"),
    ]
    assert result["applied"] is True


def test_apply_requires_confirm_before_policy():
    with mock.patch.object(main.policy, "require") as require:
        result = main.run("apply", ["/etc/hosts", "/tmp/hosts"])
    assert "error" in result
    require.assert_not_called()


def test_target_symlink_is_rejected():
    with mock.patch.object(
        main.os.path, "lexists", return_value=True
    ), mock.patch.object(main.os.path, "islink", return_value=True), mock.patch.object(
        main.policy, "require"
    ) as require:
        result = main.run("inspect", ["/etc/resolv.conf"])
    assert "symlink" in result["error"]
    require.assert_not_called()
