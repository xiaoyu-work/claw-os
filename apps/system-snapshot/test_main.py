import json
import os
import pathlib
from unittest import mock

import pytest

from test_support import load_local_module


main = load_local_module(
    pathlib.Path(__file__).with_name("main.py"),
    "claw_test_system_snapshot_main",
    clear_modules=("_shared",),
)


SNAPSHOT_ID = "snap_" + "a" * 32


def test_rollback_requires_confirmation_before_policy():
    with mock.patch.object(main.policy, "require") as require:
        with pytest.raises(ValueError, match="rollback requires confirm=true"):
            main.rollback_snapshot(SNAPSHOT_ID, False)
    require.assert_not_called()


def test_invalid_snapshot_id_is_rejected_before_policy():
    with mock.patch.object(main.policy, "require") as require:
        with pytest.raises(ValueError, match="snapshot id must match"):
            main.delete_snapshot("snap_invalid")
    require.assert_not_called()


def test_create_uses_snapshot_capability():
    completed = mock.Mock(returncode=0, stdout=json.dumps({"created": {}}), stderr="")
    with mock.patch.dict(os.environ, {"COS_BIN": "/usr/local/bin/cos"}), mock.patch.object(
        main.policy, "require"
    ) as require, mock.patch.object(main.subprocess, "run", return_value=completed) as run:
        result = main.create_snapshot("before upgrade")
    require.assert_called_once_with("sys.snapshot", wild=True)
    assert run.call_args.args[0] == [
        "/usr/local/bin/cos",
        "__snapshot",
        "create",
        "before upgrade",
    ]
    assert result == {"created": {}}


def test_create_preserves_default_description():
    completed = mock.Mock(returncode=0, stdout=json.dumps({"created": {}}), stderr="")
    with mock.patch.dict(os.environ, {"COS_BIN": "/usr/local/bin/cos"}), mock.patch.object(
        main.policy, "require"
    ), mock.patch.object(main.subprocess, "run", return_value=completed) as run:
        main.create_snapshot()
    assert run.call_args.args[0][-1] == "Claw OS recovery point"


def test_broker_error_payload_raises():
    completed = mock.Mock(
        returncode=1,
        stdout=json.dumps({"error": "snapshot creation failed"}),
        stderr="",
    )
    with mock.patch.dict(os.environ, {"COS_BIN": "/usr/local/bin/cos"}), mock.patch.object(
        main.policy, "require"
    ), mock.patch.object(main.subprocess, "run", return_value=completed):
        with pytest.raises(RuntimeError, match="snapshot creation failed"):
            main.create_snapshot()
