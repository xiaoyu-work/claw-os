import json
import os
import pathlib
from unittest import mock

from test_support import load_local_module


main = load_local_module(
    pathlib.Path(__file__).with_name("main.py"),
    "claw_test_printer_manager_main",
    clear_modules=("_shared",),
)


def test_print_uses_printer_and_file_scopes():
    completed = mock.Mock(
        returncode=0,
        stdout=json.dumps({"job_id": "office-1"}),
        stderr="",
    )
    with mock.patch.dict(os.environ, {"COS_BIN": "/usr/local/bin/cos"}), mock.patch.object(
        main.os.path, "realpath", side_effect=lambda value: value
    ), mock.patch.object(main.os.path, "islink", return_value=False), mock.patch.object(
        main.policy, "require"
    ) as require, mock.patch.object(main.subprocess, "run", return_value=completed):
        main.run("print", ["office", "/home/user/document.pdf"])
    assert require.call_args_list == [
        mock.call("device.printer", name="print"),
        mock.call("fs.read", path="/home/user/document.pdf"),
    ]


def test_cancel_requires_confirm_before_policy():
    with mock.patch.object(main.policy, "require") as require:
        result = main.run("cancel", ["office-1"])
    assert "error" in result
    require.assert_not_called()
