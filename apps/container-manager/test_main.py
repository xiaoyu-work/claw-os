import json
import os
import pathlib
from unittest import mock

from test_support import load_local_module


main = load_local_module(
    pathlib.Path(__file__).with_name("main.py"),
    "claw_test_container_manager_main",
    clear_modules=("_shared",),
)


def test_docker_logs_use_observe_scope_and_bounded_lines():
    completed = mock.Mock(
        returncode=0,
        stdout=json.dumps({"lines": "50"}),
        stderr="",
    )
    with mock.patch.dict(os.environ, {"COS_BIN": "/usr/local/bin/cos"}), mock.patch.object(
        main.policy, "require"
    ) as require, mock.patch.object(main.subprocess, "run", return_value=completed) as run:
        result = main.run("logs", ["docker", "web", "50"])
    require.assert_called_once_with("sys.container", name="observe")
    assert run.call_args.args[0] == [
        "/usr/local/bin/cos",
        "__container",
        "logs",
        "--runtime",
        "docker",
        "--target",
        "web",
        "--lines",
        "50",
    ]
    assert result["lines"] == "50"


def test_remove_requires_confirm_before_policy():
    with mock.patch.object(main.policy, "require") as require:
        result = main.run("remove", ["docker", "web"])
    assert "error" in result
    require.assert_not_called()


def test_containerd_requires_namespace():
    with mock.patch.object(main.policy, "require") as require:
        result = main.run("inspect", ["containerd", "web"])
    assert "error" in result
    require.assert_not_called()
