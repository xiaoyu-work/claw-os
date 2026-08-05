import json
import os
import pathlib
from unittest import mock

from test_support import load_local_module


main = load_local_module(
    pathlib.Path(__file__).with_name("main.py"),
    "claw_test_crash_doctor_main",
    clear_modules=("_shared",),
)


def test_diagnose_uses_sensitive_crash_scope_and_explicit_bounds():
    completed = mock.Mock(
        returncode=0,
        stdout=json.dumps({"status": "warning"}),
        stderr="",
    )
    with mock.patch.dict(os.environ, {"COS_BIN": "/usr/local/bin/cos"}), mock.patch.object(
        main.policy, "require"
    ) as require, mock.patch.object(main.subprocess, "run", return_value=completed) as run:
        result = main.run("diagnose", ["120", "10"])
    require.assert_called_once_with("sys.crash", name="system")
    assert run.call_args.args[0] == [
        "/usr/local/bin/cos",
        "__crash",
        "diagnose",
        "--since-minutes",
        "120",
        "--limit",
        "10",
    ]
    assert result["status"] == "warning"


def test_backtrace_rejects_untrusted_selector():
    with mock.patch.object(main.policy, "require") as require:
        result = main.run("backtrace", ["../../etc/shadow"])
    assert "error" in result
    require.assert_not_called()


def test_query_bounds_are_bounded():
    assert "error" in main.run("recent", ["0"])
    assert "error" in main.run("diagnose", ["60", "101"])
