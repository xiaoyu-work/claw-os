import json
import os
import pathlib
from unittest import mock

from test_support import load_local_module


main = load_local_module(
    pathlib.Path(__file__).with_name("main.py"),
    "claw_test_event_center_main",
    clear_modules=("_shared",),
)


def test_security_events_use_event_scope():
    completed = mock.Mock(
        returncode=0,
        stdout=json.dumps({"count": 2}),
        stderr="",
    )
    with mock.patch.dict(os.environ, {"COS_BIN": "/usr/local/bin/cos"}), mock.patch.object(
        main.policy, "require"
    ) as require, mock.patch.object(main.subprocess, "run", return_value=completed) as run:
        result = main.run("recent", ["security", "20"])
    require.assert_called_once_with("sys.events", name="observe")
    assert run.call_args.args[0] == [
        "/usr/local/bin/cos",
        "__events",
        "recent",
        "--source",
        "security",
        "--limit",
        "20",
    ]
    assert result["count"] == 2


def test_invalid_source_is_rejected_before_policy():
    with mock.patch.object(main.policy, "require") as require:
        result = main.run("recent", ["*"])
    assert "error" in result
    require.assert_not_called()
