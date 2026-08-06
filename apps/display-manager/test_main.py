import json
import os
import pathlib
from unittest import mock

from test_support import load_local_module


main = load_local_module(
    pathlib.Path(__file__).with_name("main.py"),
    "claw_test_display_manager_main",
    clear_modules=("_shared",),
)


def test_scale_uses_display_manage_scope():
    completed = mock.Mock(
        returncode=0,
        stdout=json.dumps({"changed": True}),
        stderr="",
    )
    with mock.patch.dict(os.environ, {"COS_BIN": "/usr/local/bin/cos"}), mock.patch.object(
        main.policy, "require"
    ) as require, mock.patch.object(main.subprocess, "run", return_value=completed):
        result = main.run("scale", ["eDP-1", "1.25"])
    require.assert_called_once_with("device.display", name="manage")
    assert result["changed"] is True


def test_apply_layout_requires_confirm_before_policy():
    with mock.patch.object(main.policy, "require") as require:
        result = main.run("apply-layout", ["/home/user/layout.kdl"])
    assert "error" in result
    require.assert_not_called()
