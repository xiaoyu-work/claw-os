import json
import os
import pathlib
from unittest import mock

import pytest

from test_support import load_local_module


main = load_local_module(
    pathlib.Path(__file__).with_name("main.py"),
    "claw_test_audio_manager_main",
    clear_modules=("_shared",),
)


def _completed(payload: object, returncode: int = 0) -> mock.Mock:
    return mock.Mock(
        returncode=returncode,
        stdout=json.dumps(payload),
        stderr="",
    )


@pytest.mark.parametrize(
    ("function_name", "args", "expected_policy", "expected_argv"),
    [
        (
            "status",
            (),
            mock.call("sys.observe", name="audio"),
            ["/usr/local/bin/cos", "__audio", "status"],
        ),
        (
            "output_volume",
            (75,),
            mock.call("device.audio", name="output"),
            [
                "/usr/local/bin/cos",
                "__audio",
                "output-volume",
                "--value",
                "75",
            ],
        ),
        (
            "input_volume",
            (80,),
            mock.call("device.microphone", name="input"),
            [
                "/usr/local/bin/cos",
                "__audio",
                "input-volume",
                "--value",
                "80",
            ],
        ),
        (
            "output_mute",
            ("toggle",),
            mock.call("device.audio", name="output"),
            [
                "/usr/local/bin/cos",
                "__audio",
                "output-mute",
                "--value",
                "toggle",
            ],
        ),
        (
            "input_mute",
            ("off",),
            mock.call("device.microphone", name="input"),
            [
                "/usr/local/bin/cos",
                "__audio",
                "input-mute",
                "--value",
                "off",
            ],
        ),
        (
            "output_default",
            (42,),
            mock.call("device.media-route", name="pipewire"),
            [
                "/usr/local/bin/cos",
                "__audio",
                "output-default",
                "--target",
                "42",
            ],
        ),
        (
            "input_default",
            (43,),
            mock.call("device.media-route", name="pipewire"),
            [
                "/usr/local/bin/cos",
                "__audio",
                "input-default",
                "--target",
                "43",
            ],
        ),
        (
            "output_route",
            (42, 3),
            mock.call("device.media-route", name="pipewire"),
            [
                "/usr/local/bin/cos",
                "__audio",
                "output-route",
                "--target",
                "42",
                "--value",
                "3",
            ],
        ),
        (
            "input_route",
            (43, 4),
            mock.call("device.media-route", name="pipewire"),
            [
                "/usr/local/bin/cos",
                "__audio",
                "input-route",
                "--target",
                "43",
                "--value",
                "4",
            ],
        ),
        (
            "profile",
            (5, 2),
            mock.call("device.media-route", name="pipewire"),
            [
                "/usr/local/bin/cos",
                "__audio",
                "profile",
                "--target",
                "5",
                "--value",
                "2",
            ],
        ),
    ],
)
def test_commands_use_expected_capability_and_argv(
    function_name, args, expected_policy, expected_argv
):
    with mock.patch.dict(os.environ, {"COS_BIN": "/usr/local/bin/cos"}), mock.patch.object(
        main.policy, "require"
    ) as require, mock.patch.object(
        main.subprocess, "run", return_value=_completed({"changed": True})
    ) as run:
        result = getattr(main, function_name)(*args)
    assert require.call_args == expected_policy
    assert run.call_args.args[0] == expected_argv
    assert run.call_args.kwargs["timeout"] == main.TIMEOUT_SECS
    assert run.call_args.kwargs["stdin"] is main.subprocess.DEVNULL
    assert result == {"changed": True}


@pytest.mark.parametrize(
    ("function_name", "args", "message"),
    [
        ("output_volume", (True,), "percentage must be an integer"),
        ("output_volume", ("75",), "percentage must be an integer"),
        ("output_volume", (151,), "percentage must be 0..150"),
        ("input_volume", (101,), "percentage must be 0..100"),
        ("output_mute", ("muted",), "mute state must be on, off, or toggle"),
        ("input_mute", (True,), "mute state must be on, off, or toggle"),
        ("output_default", (0,), "node id must be 1..4096"),
        ("input_default", ("42",), "node id must be an integer"),
        ("output_route", (42, True), "route index must be an integer"),
        ("input_route", (4097, 2), "node id must be 1..4096"),
        ("profile", (3, -1), "profile index must be 0..4096"),
    ],
)
def test_invalid_arguments_are_rejected_before_policy(function_name, args, message):
    with mock.patch.object(main.policy, "require") as require:
        with pytest.raises(ValueError, match=message):
            getattr(main, function_name)(*args)
    require.assert_not_called()


@pytest.mark.parametrize(
    ("returncode", "stdout", "message"),
    [
        (0, "{", "Audio Manager broker returned invalid JSON"),
        (0, "[]", "Audio Manager broker returned a non-object result"),
        (
            0,
            json.dumps({"error": None}),
            "Audio Manager broker returned an invalid error payload",
        ),
        (0, json.dumps({"error": "PipeWire unavailable"}), "PipeWire unavailable"),
        (7, "{}", "Audio Manager broker exited 7"),
        (9, json.dumps({"error": "audio denied"}), "audio denied"),
    ],
)
def test_broker_payload_failures_raise(returncode, stdout, message):
    completed = mock.Mock(returncode=returncode, stdout=stdout, stderr="")
    with mock.patch.dict(os.environ, {"COS_BIN": "/usr/local/bin/cos"}), mock.patch.object(
        main.policy, "require"
    ), mock.patch.object(main.subprocess, "run", return_value=completed):
        with pytest.raises(RuntimeError, match=message):
            main.status()


def test_missing_broker_executable_raises():
    with mock.patch.dict(os.environ, {}, clear=True), mock.patch.object(
        main.shutil, "which", return_value=None
    ), mock.patch.object(main.policy, "require"):
        with pytest.raises(FileNotFoundError, match="cos binary not found"):
            main.status()


@pytest.mark.parametrize(
    ("error", "exception", "message"),
    [
        (
            FileNotFoundError("missing"),
            FileNotFoundError,
            "Audio Manager broker executable not found",
        ),
        (
            PermissionError("denied"),
            PermissionError,
            "permission denied launching Audio Manager broker",
        ),
    ],
)
def test_broker_launch_failures_raise(error, exception, message):
    with mock.patch.dict(os.environ, {"COS_BIN": "/usr/local/bin/cos"}), mock.patch.object(
        main.policy, "require"
    ), mock.patch.object(main.subprocess, "run", side_effect=error):
        with pytest.raises(exception, match=message):
            main.status()


def test_broker_timeout_raises():
    with mock.patch.dict(os.environ, {"COS_BIN": "/usr/local/bin/cos"}), mock.patch.object(
        main.policy, "require"
    ), mock.patch.object(
        main.subprocess,
        "run",
        side_effect=main.subprocess.TimeoutExpired(["cos"], main.TIMEOUT_SECS),
    ):
        with pytest.raises(
            TimeoutError,
            match=rf"Audio Manager broker exceeded {main.TIMEOUT_SECS}s for status",
        ):
            main.status()
