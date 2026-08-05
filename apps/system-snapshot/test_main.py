import json
import os
import pathlib
from unittest import mock

from test_support import load_local_module


main = load_local_module(
    pathlib.Path(__file__).with_name("main.py"),
    "claw_test_system_snapshot_main",
    clear_modules=("_shared",),
)


def test_rollback_requires_confirmation():
    result = main.run("rollback", ["snap_" + "a" * 32])
    assert "error" in result


def test_create_uses_snapshot_capability():
    completed = mock.Mock(returncode=0, stdout=json.dumps({"created": {}}), stderr="")
    with mock.patch.dict(os.environ, {"COS_BIN": "/usr/local/bin/cos"}), mock.patch.object(
        main.policy, "require"
    ) as require, mock.patch.object(main.subprocess, "run", return_value=completed):
        main.run("create", ["before upgrade"])
    require.assert_called_once_with("sys.snapshot", wild=True)
