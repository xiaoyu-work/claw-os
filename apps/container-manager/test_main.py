import ast
import inspect
import json
import os
import pathlib
import re
from unittest import mock

import pytest

from test_support import load_local_module


APP_DIR = pathlib.Path(__file__).parent
COS = "/usr/local/bin/cos"
RUNTIME_CHOICES = ["docker", "podman", "podman-root", "containerd"]
SIGNAL_CHOICES = ["TERM", "KILL", "HUP", "INT", "USR1", "USR2"]
OBSERVE = {"status", "list", "inspect", "logs", "processes", "stats", "namespaces"}
CONTRACTS = {
    "status": [],
    "list": ["runtime", "namespace"],
    "inspect": ["runtime", "target", "namespace"],
    "logs": ["runtime", "target", "lines", "namespace"],
    "processes": ["runtime", "target", "namespace"],
    "stats": ["runtime", "target", "namespace"],
    "namespaces": ["runtime", "target", "namespace"],
    "start": ["runtime", "target", "namespace"],
    "stop": ["runtime", "target", "namespace"],
    "restart": ["runtime", "target", "namespace"],
    "pause": ["runtime", "target", "namespace"],
    "unpause": ["runtime", "target", "namespace"],
    "kill": ["runtime", "target", "signal", "namespace"],
    "remove": ["runtime", "target", "namespace", "confirm"],
}
IMPLEMENTATIONS = {
    "status": "status",
    "list": "list_containers",
    "inspect": "inspect",
    "logs": "logs",
    "processes": "processes",
    "stats": "stats",
    "namespaces": "namespaces",
    "start": "start",
    "stop": "stop",
    "restart": "restart",
    "pause": "pause",
    "unpause": "unpause",
    "kill": "kill",
    "remove": "remove",
}

main = load_local_module(
    APP_DIR / "main.py",
    "claw_test_container_manager_main",
    clear_modules=("_shared",),
)


def _mcp_bindings(source: str) -> dict[str, ast.FunctionDef]:
    bindings = {}
    for node in ast.parse(source).body:
        if not isinstance(node, ast.FunctionDef):
            continue
        for decorator in node.decorator_list:
            if (
                isinstance(decorator, ast.Call)
                and isinstance(decorator.func, ast.Attribute)
                and isinstance(decorator.func.value, ast.Name)
                and decorator.func.value.id == "app"
                and decorator.func.attr == "tool"
                and len(decorator.args) == 1
                and isinstance(decorator.args[0], ast.Constant)
                and isinstance(decorator.args[0].value, str)
            ):
                bindings[decorator.args[0].value] = node
    return bindings


def _ast_signature(node: ast.FunctionDef) -> tuple[list[str], dict[str, object]]:
    positional = [*node.args.posonlyargs, *node.args.args]
    parameters = [*positional, *node.args.kwonlyargs]
    defaults = {
        argument.arg: ast.literal_eval(default)
        for argument, default in zip(
            positional[-len(node.args.defaults) :] if node.args.defaults else [],
            node.args.defaults,
            strict=True,
        )
    }
    defaults.update(
        {
            argument.arg: ast.literal_eval(default)
            for argument, default in zip(
                node.args.kwonlyargs,
                node.args.kw_defaults,
                strict=True,
            )
            if default is not None
        }
    )
    assert all(argument.annotation is not None for argument in parameters)
    assert node.returns is not None
    return [argument.arg for argument in parameters], defaults


def _expected_defaults(args: list[dict[str, object]]) -> dict[str, object]:
    return {
        arg["name"]: arg.get("default")
        for arg in args
        if "default" in arg or not arg.get("required", False)
    }


def test_manifest_and_handlers_are_direct_mcp_only_and_aligned():
    manifest = json.loads((APP_DIR / "app.json").read_text(encoding="utf-8"))
    assert "operations" not in manifest

    tools = {tool["name"]: tool for tool in manifest["mcp"]["tools"]}
    assert list(tools) == [f"container-manager.{name}" for name in CONTRACTS]

    namespace_condition = {
        "kind": "arg-equals",
        "arg": "runtime",
        "value": "containerd",
    }
    for name, argument_names in CONTRACTS.items():
        tool = tools[f"container-manager.{name}"]
        args = tool.get("args", [])
        assert [arg["name"] for arg in args] == argument_names
        scope = "observe" if name in OBSERVE else "control"
        assert tool["needs"] == [
            {
                "verb": "sys.container",
                "scope": {
                    "kind": "fixed",
                    "scope": {"kind": "name", "value": scope},
                },
                "why": tool["needs"][0]["why"],
            }
        ]
        by_name = {arg["name"]: arg for arg in args}
        if "runtime" in by_name:
            assert by_name["runtime"]["choices"] == RUNTIME_CHOICES
        if "namespace" in by_name:
            assert by_name["namespace"]["required_when"] == namespace_condition

    logs_args = {
        arg["name"]: arg
        for arg in tools["container-manager.logs"]["args"]
    }
    assert logs_args["lines"] == {
        "name": "lines",
        "kind": "integer",
        "required": False,
        "default": 100,
        "binding": "positional",
    }
    assert logs_args["namespace"]["binding"] == "flag"
    kill_args = {
        arg["name"]: arg
        for arg in tools["container-manager.kill"]["args"]
    }
    assert kill_args["signal"]["choices"] == SIGNAL_CHOICES
    remove_args = {
        arg["name"]: arg
        for arg in tools["container-manager.remove"]["args"]
    }
    assert remove_args["confirm"] == {
        "name": "confirm",
        "kind": "bool",
        "required": True,
        "choices": [True],
        "binding": "flag",
    }

    main_source = (APP_DIR / "main.py").read_text(encoding="utf-8")
    assert not hasattr(main, "run")
    assert "canonical_argv" not in main_source
    assert "def _base(" not in main_source

    server_source = (APP_DIR / "server.py").read_text(encoding="utf-8")
    assert "serve_manifest_operations" not in server_source
    assert server_source.count("App.from_manifest()") == 1
    bindings = _mcp_bindings(server_source)
    assert list(bindings) == list(tools)

    for name, argument_names in CONTRACTS.items():
        args = tools[f"container-manager.{name}"].get("args", [])
        expected_defaults = _expected_defaults(args)
        assert _ast_signature(bindings[f"container-manager.{name}"]) == (
            argument_names,
            expected_defaults,
        )

        signature = inspect.signature(getattr(main, IMPLEMENTATIONS[name]))
        assert list(signature.parameters) == argument_names
        assert {
            parameter_name: parameter.default
            for parameter_name, parameter in signature.parameters.items()
            if parameter.default is not inspect.Signature.empty
        } == expected_defaults
        assert all(
            parameter.annotation is not inspect.Signature.empty
            for parameter in signature.parameters.values()
        )
        assert signature.return_annotation is not inspect.Signature.empty


@pytest.mark.parametrize(
    ("function_name", "args", "kwargs", "scope", "argv"),
    [
        ("status", (), {}, "observe", [COS, "__container", "status"]),
        (
            "list_containers",
            ("docker",),
            {},
            "observe",
            [COS, "__container", "list", "--runtime", "docker"],
        ),
        (
            "inspect",
            ("docker", "web"),
            {},
            "observe",
            [
                COS,
                "__container",
                "inspect",
                "--runtime",
                "docker",
                "--target",
                "web",
            ],
        ),
        (
            "logs",
            ("docker", "web"),
            {},
            "observe",
            [
                COS,
                "__container",
                "logs",
                "--runtime",
                "docker",
                "--target",
                "web",
                "--lines",
                "100",
            ],
        ),
        (
            "processes",
            ("docker", "web"),
            {},
            "observe",
            [
                COS,
                "__container",
                "processes",
                "--runtime",
                "docker",
                "--target",
                "web",
            ],
        ),
        (
            "stats",
            ("docker", "web"),
            {},
            "observe",
            [
                COS,
                "__container",
                "stats",
                "--runtime",
                "docker",
                "--target",
                "web",
            ],
        ),
        (
            "namespaces",
            ("docker", "web"),
            {},
            "observe",
            [
                COS,
                "__container",
                "namespaces",
                "--runtime",
                "docker",
                "--target",
                "web",
            ],
        ),
        (
            "start",
            ("docker", "web"),
            {},
            "control",
            [
                COS,
                "__container",
                "start",
                "--runtime",
                "docker",
                "--target",
                "web",
            ],
        ),
        (
            "stop",
            ("docker", "web"),
            {},
            "control",
            [
                COS,
                "__container",
                "stop",
                "--runtime",
                "docker",
                "--target",
                "web",
            ],
        ),
        (
            "restart",
            ("docker", "web"),
            {},
            "control",
            [
                COS,
                "__container",
                "restart",
                "--runtime",
                "docker",
                "--target",
                "web",
            ],
        ),
        (
            "pause",
            ("docker", "web"),
            {},
            "control",
            [
                COS,
                "__container",
                "pause",
                "--runtime",
                "docker",
                "--target",
                "web",
            ],
        ),
        (
            "unpause",
            ("docker", "web"),
            {},
            "control",
            [
                COS,
                "__container",
                "unpause",
                "--runtime",
                "docker",
                "--target",
                "web",
            ],
        ),
        (
            "kill",
            ("docker", "web", "TERM"),
            {},
            "control",
            [
                COS,
                "__container",
                "kill",
                "--runtime",
                "docker",
                "--target",
                "web",
                "--signal",
                "TERM",
            ],
        ),
        (
            "remove",
            ("docker", "web"),
            {"confirm": True},
            "control",
            [
                COS,
                "__container",
                "remove",
                "--runtime",
                "docker",
                "--target",
                "web",
                "--confirm",
            ],
        ),
    ],
)
def test_all_routes_use_exact_broker_argv_and_capability(
    function_name, args, kwargs, scope, argv
):
    completed = mock.Mock(
        returncode=0,
        stdout=json.dumps({"ok": True}),
        stderr="",
    )
    with mock.patch.dict(os.environ, {"COS_BIN": COS}), mock.patch.object(
        main.policy, "require"
    ) as require, mock.patch.object(
        main.subprocess, "run", return_value=completed
    ) as run:
        result = getattr(main, function_name)(*args, **kwargs)
    require.assert_called_once_with("sys.container", name=scope)
    assert run.call_args.args[0] == argv
    assert run.call_args.kwargs["timeout"] == main.TIMEOUT_SECS
    assert run.call_args.kwargs["stdin"] is main.subprocess.DEVNULL
    assert result == {"ok": True}


NAMESPACE_ROUTES = tuple(name for name in CONTRACTS if name not in {"status"})


def _call_namespace_route(name: str, runtime: object, namespace: object | None):
    if name == "list":
        return main.list_containers(runtime, namespace)
    if name == "logs":
        return main.logs(runtime, "web", 100, namespace)
    if name == "kill":
        return main.kill(runtime, "web", "TERM", namespace)
    if name == "remove":
        return main.remove(runtime, "web", namespace, confirm=True)
    return getattr(main, name)(runtime, "web", namespace)


@pytest.mark.parametrize("name", NAMESPACE_ROUTES)
@pytest.mark.parametrize(
    ("runtime", "namespace", "message"),
    [
        ("containerd", None, "containerd requires a namespace"),
        ("docker", "default", "only containerd accepts a namespace"),
    ],
)
def test_namespace_condition_is_enforced_before_policy(
    name, runtime, namespace, message
):
    with mock.patch.object(main.policy, "require") as require:
        with pytest.raises(ValueError, match=message):
            _call_namespace_route(name, runtime, namespace)
    require.assert_not_called()


@pytest.mark.parametrize(
    ("call", "message"),
    [
        (lambda: main.inspect(None, "web"), "runtime must be"),
        (lambda: main.inspect("Docker", "web"), "runtime must be"),
        (lambda: main.inspect("docker", None), "target is invalid"),
        (lambda: main.inspect("docker", "bad/name"), "target is invalid"),
        (
            lambda: main.inspect("containerd", "web", "bad/name"),
            "namespace is invalid",
        ),
        (lambda: main.logs("docker", "web", True), "logs lines must be"),
        (lambda: main.logs("docker", "web", 0), "logs lines must be"),
        (lambda: main.logs("docker", "web", 1001), "logs lines must be"),
        (lambda: main.logs("docker", "web", 1.0), "logs lines must be"),
        (lambda: main.logs("docker", "web", "10"), "logs lines must be"),
        (lambda: main.kill("docker", "web", "term"), "signal must be"),
        (lambda: main.kill("docker", "web", "SIGTERM"), "signal must be"),
        (lambda: main.kill("docker", "web", 15), "signal must be"),
        (
            lambda: main.remove("docker", "web", confirm=False),
            "remove requires confirm=true",
        ),
        (
            lambda: main.remove("docker", "web", confirm=1),
            "remove requires confirm=true",
        ),
        (
            lambda: main.remove("docker", "web", confirm="true"),
            "remove requires confirm=true",
        ),
    ],
)
def test_invalid_arguments_are_rejected_before_policy(call, message):
    with mock.patch.object(main.policy, "require") as require:
        with pytest.raises(ValueError, match=message):
            call()
    require.assert_not_called()


def test_containerd_namespace_is_forwarded():
    completed = mock.Mock(
        returncode=0,
        stdout=json.dumps({"ok": True}),
        stderr="",
    )
    with mock.patch.dict(os.environ, {"COS_BIN": COS}), mock.patch.object(
        main.policy, "require"
    ), mock.patch.object(main.subprocess, "run", return_value=completed) as run:
        main.logs("containerd", "web", 25, "default")
    assert run.call_args.args[0][-6:] == [
        "--target",
        "web",
        "--namespace",
        "default",
        "--lines",
        "25",
    ]


@pytest.mark.parametrize(
    ("stdout", "stderr", "expected"),
    [
        (json.dumps({"source": "stdout"}), "", {"source": "stdout"}),
        ("", json.dumps({"source": "stderr"}), {"source": "stderr"}),
        (
            json.dumps({"source": "stdout"}),
            json.dumps({"source": "stderr"}),
            {"source": "stdout"},
        ),
    ],
)
def test_broker_parses_stdout_before_stderr(stdout, stderr, expected):
    completed = mock.Mock(returncode=0, stdout=stdout, stderr=stderr)
    with mock.patch.dict(os.environ, {"COS_BIN": COS}), mock.patch.object(
        main.policy, "require"
    ), mock.patch.object(main.subprocess, "run", return_value=completed):
        assert main.status() == expected


@pytest.mark.parametrize(
    ("returncode", "stdout", "stderr", "message"),
    [
        (0, "{", "", "Container Manager broker returned invalid JSON"),
        (0, "", "{", "Container Manager broker returned invalid JSON"),
        (0, "[]", "", "Container Manager broker returned a non-object result"),
        (
            0,
            json.dumps({"error": None}),
            "",
            "Container Manager broker returned an invalid error payload",
        ),
        (
            0,
            json.dumps({"error": "runtime unavailable"}),
            "",
            "runtime unavailable",
        ),
        (7, "{}", "", "Container Manager broker exited 7"),
        (
            9,
            "{}",
            json.dumps({"error": "container authorization denied"}),
            "container authorization denied",
        ),
        (0, "", "", "Container Manager broker returned invalid JSON"),
    ],
)
def test_broker_payload_failures_raise(returncode, stdout, stderr, message):
    completed = mock.Mock(
        returncode=returncode,
        stdout=stdout,
        stderr=stderr,
    )
    with mock.patch.dict(os.environ, {"COS_BIN": COS}), mock.patch.object(
        main.policy, "require"
    ), mock.patch.object(main.subprocess, "run", return_value=completed):
        with pytest.raises(RuntimeError, match=re.escape(message)):
            main.status()


def test_missing_broker_executable_raises():
    with mock.patch.dict(os.environ, {}, clear=True), mock.patch.object(
        main.shutil, "which", return_value=None
    ), mock.patch.object(main.policy, "require") as require:
        with pytest.raises(
            FileNotFoundError,
            match="cos binary not found; Container Manager broker unavailable",
        ):
            main.status()
    require.assert_called_once_with("sys.container", name="observe")


@pytest.mark.parametrize(
    ("failure", "exception_type", "message"),
    [
        (
            FileNotFoundError("gone"),
            FileNotFoundError,
            "Container Manager broker executable not found",
        ),
        (
            PermissionError("denied"),
            PermissionError,
            "permission denied launching Container Manager broker",
        ),
    ],
)
def test_broker_launch_failures_raise(failure, exception_type, message):
    with mock.patch.dict(os.environ, {"COS_BIN": COS}), mock.patch.object(
        main.policy, "require"
    ), mock.patch.object(main.subprocess, "run", side_effect=failure):
        with pytest.raises(exception_type, match=message):
            main.status()


def test_broker_timeout_raises():
    with mock.patch.dict(os.environ, {"COS_BIN": COS}), mock.patch.object(
        main.policy, "require"
    ), mock.patch.object(
        main.subprocess,
        "run",
        side_effect=main.subprocess.TimeoutExpired(["cos"], main.TIMEOUT_SECS),
    ):
        with pytest.raises(
            TimeoutError,
            match=rf"Container Manager broker exceeded {main.TIMEOUT_SECS}s for status",
        ):
            main.status()
