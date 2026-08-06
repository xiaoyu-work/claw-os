import json
import os
import pathlib
from unittest import mock

from test_support import load_local_module


main = load_local_module(
    pathlib.Path(__file__).with_name("main.py"),
    "claw_test_camera_manager_main",
    clear_modules=("_shared",),
)


def test_capture_uses_camera_and_destination_scopes():
    completed = mock.Mock(
        returncode=0,
        stdout=json.dumps({"captured": True}),
        stderr="",
    )
    with mock.patch.dict(os.environ, {"COS_BIN": "/usr/local/bin/cos"}), mock.patch.object(
        main.os.path, "realpath", side_effect=lambda value: value
    ), mock.patch.object(main.os.path, "lexists", return_value=False), mock.patch.object(
        main.policy, "require"
    ) as require, mock.patch.object(main.subprocess, "run", return_value=completed):
        main.run("capture", ["42", "100", "/home/user/photo.png", "png"])
    assert require.call_args_list == [
        mock.call("device.camera", name="capture"),
        mock.call("fs.write", path="/home/user/photo.png"),
    ]


def test_capture_rejects_existing_destination_before_policy():
    with mock.patch.object(
        main.os.path, "realpath", side_effect=lambda value: value
    ), mock.patch.object(main.os.path, "lexists", return_value=True), mock.patch.object(
        main.policy, "require"
    ) as require:
        result = main.run("capture", ["42", "100", "/home/user/photo.png", "png"])
    assert "error" in result
    require.assert_not_called()
