import json
import os
import pathlib
from unittest import mock

from test_support import load_local_module


main = load_local_module(
    pathlib.Path(__file__).with_name("main.py"),
    "claw_test_firewall_manager_main",
    clear_modules=("_shared",),
)


def test_add_uses_fixed_firewall_scope_and_normalizes_cidr():
    completed = mock.Mock(
        returncode=0,
        stdout=json.dumps({"action": "add"}),
        stderr="",
    )
    with mock.patch.dict(os.environ, {"COS_BIN": "/usr/local/bin/cos"}), mock.patch.object(
        main.policy, "require"
    ) as require, mock.patch.object(main.subprocess, "run", return_value=completed) as run:
        main.run(
            "add",
            ["deny", "input", "tcp", "22", "--remote", "192.0.2.1/24"],
        )
    require.assert_called_once_with("net.firewall", name="manage")
    assert run.call_args.args[0][-2:] == ["--remote", "192.0.2.0/24"]


def test_clear_requires_confirm_before_policy():
    with mock.patch.object(main.policy, "require") as require:
        result = main.run("clear", [])
    assert "error" in result
    require.assert_not_called()
