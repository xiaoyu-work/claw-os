import json
import os
import pathlib
from unittest import mock

from test_support import load_local_module


main = load_local_module(
    pathlib.Path(__file__).with_name("main.py"),
    "claw_test_storage_manager_main",
    clear_modules=("_shared",),
)


def test_mount_requires_exact_device_scope():
    completed = mock.Mock(
        returncode=0,
        stdout=json.dumps({"changed": True}),
        stderr="",
    )
    with mock.patch.dict(os.environ, {"COS_BIN": "/usr/local/bin/cos"}), mock.patch.object(
        main.os.path, "realpath", return_value="/dev/sdb1"
    ), mock.patch.object(main.policy, "require") as require, mock.patch.object(
        main.subprocess, "run", return_value=completed
    ) as run:
        result = main.run("mount", ["/dev/sdb1"])
    require.assert_called_once_with("sys.mount", path="/dev/sdb1")
    assert run.call_args.args[0] == [
        "/usr/local/bin/cos",
        "__storage",
        "mount",
        "--device",
        "/dev/sdb1",
    ]
    assert result["changed"] is True


def test_health_uses_diagnostic_scope():
    completed = mock.Mock(
        returncode=0,
        stdout=json.dumps({"status": "ok"}),
        stderr="",
    )
    with mock.patch.dict(os.environ, {"COS_BIN": "/usr/local/bin/cos"}), mock.patch.object(
        main.os.path, "realpath", return_value="/dev/nvme0n1"
    ), mock.patch.object(main.policy, "require") as require, mock.patch.object(
        main.subprocess, "run", return_value=completed
    ):
        result = main.run("health", ["/dev/nvme0n1"])
    require.assert_called_once_with("sys.storage", name="diagnose")
    assert result["status"] == "ok"


def test_symlink_device_is_rejected_before_policy():
    with mock.patch.object(
        main.os.path, "realpath", return_value="/dev/sdb1"
    ), mock.patch.object(main.policy, "require") as require:
        result = main.run("mount", ["/dev/disk/by-id/example"])
    assert "canonical" in result["error"]
    require.assert_not_called()
