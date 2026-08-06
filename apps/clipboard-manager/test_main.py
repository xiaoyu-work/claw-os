import json
import os
import pathlib
from unittest import mock

from test_support import load_local_module


main = load_local_module(
    pathlib.Path(__file__).with_name("main.py"),
    "claw_test_clipboard_manager_main",
    clear_modules=("_shared",),
)


def test_read_uses_sensitive_clipboard_scope():
    completed = mock.Mock(
        returncode=0,
        stdout=json.dumps({"text": "hello"}),
        stderr="",
    )
    with mock.patch.dict(os.environ, {"COS_BIN": "/usr/local/bin/cos"}), mock.patch.object(
        main.policy, "require"
    ) as require, mock.patch.object(main.subprocess, "run", return_value=completed):
        result = main.run("read", ["text/plain"])
    require.assert_called_once_with("clipboard.read", name="selection")
    assert result["text"] == "hello"


def test_write_uses_clipboard_and_source_scopes():
    completed = mock.Mock(returncode=0, stdout=json.dumps({"written": True}), stderr="")
    with mock.patch.dict(os.environ, {"COS_BIN": "/usr/local/bin/cos"}), mock.patch.object(
        main.os.path, "realpath", side_effect=lambda value: value
    ), mock.patch.object(main.os.path, "islink", return_value=False), mock.patch.object(
        main.policy, "require"
    ) as require, mock.patch.object(main.subprocess, "run", return_value=completed):
        main.run("write", ["/home/user/clip.txt", "text/plain"])
    assert require.call_args_list == [
        mock.call("clipboard.write", name="selection"),
        mock.call("fs.read", path="/home/user/clip.txt"),
    ]


def test_clear_requires_confirm_before_policy():
    with mock.patch.object(main.policy, "require") as require:
        result = main.run("clear", [])
    assert "error" in result
    require.assert_not_called()
