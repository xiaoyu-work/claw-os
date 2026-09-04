import json
import os
import pathlib
from unittest import mock

import pytest

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
    ) as require, mock.patch.object(
        main.subprocess, "run", return_value=completed
    ) as run:
        result = main.capture(42, 100, "/home/user/photo.png", "png")
    assert require.call_args_list == [
        mock.call("device.camera", name="capture"),
        mock.call("fs.write", path="/home/user/photo.png"),
    ]
    assert run.call_args.args[0] == [
        "/usr/local/bin/cos",
        "__camera",
        "capture",
        "--node-id",
        "42",
        "--expected-serial",
        "100",
        "--destination",
        "/home/user/photo.png",
        "--format",
        "png",
        "--width",
        "1280",
        "--height",
        "720",
    ]
    assert run.call_args.kwargs["timeout"] == main.TIMEOUT_SECS
    assert run.call_args.kwargs["stdin"] is main.subprocess.DEVNULL
    assert result == {"captured": True}


def test_capture_rejects_existing_destination_before_policy():
    with mock.patch.object(
        main.os.path, "realpath", side_effect=lambda value: value
    ), mock.patch.object(main.os.path, "lexists", return_value=True), mock.patch.object(
        main.policy, "require"
    ) as require:
        with pytest.raises(ValueError, match="canonical new path"):
            main.capture(42, 100, "/home/user/photo.png", "png")
    require.assert_not_called()


@pytest.mark.parametrize(
    ("node_id", "expected_serial", "width", "height", "message"),
    [
        (True, 100, 1280, 720, "must be integers"),
        (42, "100", 1280, 720, "must be integers"),
        (42, 100, 1280.0, 720, "must be integers"),
        (0, 100, 1280, 720, "out of bounds"),
        (42, 0, 1280, 720, "out of bounds"),
        (42, 100, 15, 720, "out of bounds"),
        (42, 100, 1280, 4321, "out of bounds"),
    ],
)
def test_capture_rejects_invalid_camera_arguments_before_policy(
    node_id, expected_serial, width, height, message
):
    with mock.patch.object(main.policy, "require") as require:
        with pytest.raises(ValueError, match=message):
            main.capture(
                node_id,
                expected_serial,
                "/home/user/photo.png",
                "png",
                width,
                height,
            )
    require.assert_not_called()


@pytest.mark.parametrize(
    ("destination", "image_format", "message"),
    [
        (None, "png", "destination must be a path"),
        ("/home/user/photo.png", "gif", "format must be png or jpeg"),
        ("/home/user/photo.png", None, "format must be png or jpeg"),
    ],
)
def test_capture_rejects_invalid_destination_or_format_before_policy(
    destination, image_format, message
):
    with mock.patch.object(
        main.os.path, "realpath", side_effect=lambda value: value
    ), mock.patch.object(main.os.path, "lexists", return_value=False), mock.patch.object(
        main.policy, "require"
    ) as require:
        with pytest.raises(ValueError, match=message):
            main.capture(42, 100, destination, image_format)
    require.assert_not_called()


def test_status_uses_camera_observe_scope():
    completed = mock.Mock(
        returncode=0,
        stdout=json.dumps({"cameras": []}),
        stderr="",
    )
    with mock.patch.dict(os.environ, {"COS_BIN": "/usr/local/bin/cos"}), mock.patch.object(
        main.policy, "require"
    ) as require, mock.patch.object(main.subprocess, "run", return_value=completed) as run:
        result = main.status()
    require.assert_called_once_with("sys.observe", name="camera")
    assert run.call_args.args[0] == ["/usr/local/bin/cos", "__camera", "status"]
    assert result == {"cameras": []}


@pytest.mark.parametrize(
    ("returncode", "stdout", "message"),
    [
        (0, "{", "Camera Manager broker returned invalid JSON"),
        (0, "[]", "Camera Manager broker returned a non-object result"),
        (0, json.dumps({"error": ""}), "invalid error payload"),
        (0, json.dumps({"error": "camera unavailable"}), "camera unavailable"),
        (7, "{}", "Camera Manager broker exited 7"),
        (9, json.dumps({"error": "capture denied"}), "capture denied"),
    ],
)
def test_broker_payload_failures_raise(returncode, stdout, message):
    completed = mock.Mock(returncode=returncode, stdout=stdout, stderr="")
    with mock.patch.dict(os.environ, {"COS_BIN": "/usr/local/bin/cos"}), mock.patch.object(
        main.policy, "require"
    ) as require, mock.patch.object(main.subprocess, "run", return_value=completed):
        with pytest.raises(RuntimeError, match=message):
            main.status()
    require.assert_called_once_with("sys.observe", name="camera")


def test_missing_broker_executable_raises():
    with mock.patch.dict(os.environ, {}, clear=True), mock.patch.object(
        main.shutil, "which", return_value=None
    ), mock.patch.object(main.policy, "require") as require:
        with pytest.raises(FileNotFoundError, match="cos binary not found"):
            main.status()
    require.assert_called_once_with("sys.observe", name="camera")


@pytest.mark.parametrize(
    ("error", "exception", "message"),
    [
        (
            FileNotFoundError("missing"),
            FileNotFoundError,
            "Camera Manager broker executable not found",
        ),
        (
            PermissionError("denied"),
            PermissionError,
            "permission denied launching Camera Manager broker",
        ),
    ],
)
def test_broker_launch_failures_raise(error, exception, message):
    with mock.patch.dict(os.environ, {"COS_BIN": "/usr/local/bin/cos"}), mock.patch.object(
        main.policy, "require"
    ) as require, mock.patch.object(main.subprocess, "run", side_effect=error):
        with pytest.raises(exception, match=message):
            main.status()
    require.assert_called_once_with("sys.observe", name="camera")


def test_broker_timeout_raises():
    with mock.patch.dict(os.environ, {"COS_BIN": "/usr/local/bin/cos"}), mock.patch.object(
        main.policy, "require"
    ) as require, mock.patch.object(
        main.subprocess,
        "run",
        side_effect=main.subprocess.TimeoutExpired(["cos"], main.TIMEOUT_SECS),
    ):
        with pytest.raises(TimeoutError, match="Camera Manager broker exceeded"):
            main.status()
    require.assert_called_once_with("sys.observe", name="camera")
