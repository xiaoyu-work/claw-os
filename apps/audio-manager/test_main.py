import json
import os
import pathlib
from unittest import mock

from test_support import load_local_module


main = load_local_module(
    pathlib.Path(__file__).with_name("main.py"),
    "claw_test_audio_manager_main",
    clear_modules=("_shared",),
)


def test_output_volume_uses_output_scope():
    completed = mock.Mock(
        returncode=0,
        stdout=json.dumps({"changed": True}),
        stderr="",
    )
    with mock.patch.dict(os.environ, {"COS_BIN": "/usr/local/bin/cos"}), mock.patch.object(
        main.policy, "require"
    ) as require, mock.patch.object(main.subprocess, "run", return_value=completed) as run:
        result = main.run("output-volume", ["75"])
    require.assert_called_once_with("device.audio", name="output")
    assert run.call_args.args[0] == [
        "/usr/local/bin/cos",
        "__audio",
        "output-volume",
        "--value",
        "75",
    ]
    assert result["changed"] is True


def test_profile_requires_pipewire_route_scope():
    completed = mock.Mock(returncode=0, stdout=json.dumps({"changed": True}), stderr="")
    with mock.patch.dict(os.environ, {"COS_BIN": "/usr/local/bin/cos"}), mock.patch.object(
        main.policy, "require"
    ) as require, mock.patch.object(main.subprocess, "run", return_value=completed):
        main.run("profile", ["42", "3"])
    require.assert_called_once_with("device.media-route", name="pipewire")


def test_input_volume_rejects_amplification():
    with mock.patch.object(main.policy, "require") as require:
        result = main.run("input-volume", ["101"])
    assert "error" in result
    require.assert_not_called()
