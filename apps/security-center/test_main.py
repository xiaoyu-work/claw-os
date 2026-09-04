import json
import os
import pathlib
from unittest import mock

import pytest

from test_support import load_local_module


main = load_local_module(
    pathlib.Path(__file__).with_name("main.py"),
    "claw_test_security_center_main",
    clear_modules=("_shared",),
)


def test_summary_uses_sensitive_security_scope():
    completed = mock.Mock(
        returncode=0,
        stdout=json.dumps({"status": "warning"}),
        stderr="",
    )
    with mock.patch.dict(os.environ, {"COS_BIN": "/usr/local/bin/cos"}), mock.patch.object(
        main.policy, "require"
    ) as require, mock.patch.object(main.subprocess, "run", return_value=completed) as run:
        result = main.summary()
    require.assert_called_once_with("sys.security", name="audit")
    assert run.call_args.args[0] == ["/usr/local/bin/cos", "__security", "summary"]
    assert result["status"] == "warning"


def test_security_commands_reject_arguments_before_policy():
    with mock.patch.object(main.policy, "require") as require:
        with pytest.raises(TypeError):
            main.ports("unexpected")
    require.assert_not_called()


def test_broker_error_raises():
    completed = mock.Mock(
        returncode=1,
        stdout=json.dumps({"error": "security inspection failed"}),
        stderr="",
    )
    with mock.patch.dict(os.environ, {"COS_BIN": "/usr/local/bin/cos"}), mock.patch.object(
        main.policy, "require"
    ) as require, mock.patch.object(main.subprocess, "run", return_value=completed):
        with pytest.raises(RuntimeError, match="security inspection failed"):
            main.ports()
    require.assert_called_once_with("sys.security", name="audit")
