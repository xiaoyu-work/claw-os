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
MANIFEST_PATH = APP_DIR / "app.json"
SERVER_PATH = APP_DIR / "server.py"
ADAPTER = "AA:BB:CC:DD:EE:FF"
DEVICE = "11:22:33:44:AA:BB"
PAIRING_ID = "ABCDEF0123456789ABCDEF0123456789"

main = load_local_module(
    APP_DIR / "main.py",
    "claw_test_bluetooth_manager_main",
    clear_modules=("_shared",),
)


def test_manifest_and_handlers_are_mcp_only_and_aligned():
    manifest = json.loads(MANIFEST_PATH.read_text(encoding="utf-8"))
    assert "operations" not in manifest

    tools = {
        tool["name"]: tool
        for tool in manifest["mcp"]["tools"]
    }
    expected_names = {
        "bluetooth-manager.status",
        "bluetooth-manager.power",
        "bluetooth-manager.scan",
        "bluetooth-manager.pair",
        "bluetooth-manager.pair-status",
        "bluetooth-manager.pair-respond",
        "bluetooth-manager.pair-cancel",
        "bluetooth-manager.connect",
        "bluetooth-manager.disconnect",
        "bluetooth-manager.trust",
        "bluetooth-manager.untrust",
        "bluetooth-manager.forget",
    }
    assert set(tools) == expected_names
    power_args = {
        argument["name"]: argument
        for argument in tools["bluetooth-manager.power"]["args"]
    }
    assert power_args["state"]["choices"] == ["on", "off"]
    scan_args = {
        argument["name"]: argument
        for argument in tools["bluetooth-manager.scan"]["args"]
    }
    assert scan_args["seconds"]["default"] == 10

    server_source = SERVER_PATH.read_text(encoding="utf-8")
    assert "serve_manifest_operations" not in server_source
    assert server_source.count("App.from_manifest()") == 1
    server_tree = ast.parse(server_source)
    bindings = {}
    for node in server_tree.body:
        if not isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)):
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
            ):
                bindings[decorator.args[0].value] = node
    assert set(bindings) == expected_names

    for name, tool in tools.items():
        expected_args = tool.get("args", [])
        expected_arg_names = [argument["name"] for argument in expected_args]
        expected_defaults = {
            argument["name"]: argument["default"]
            for argument in expected_args
            if "default" in argument
        }

        server_handler = bindings[name]
        assert [argument.arg for argument in server_handler.args.args] == expected_arg_names
        assert all(argument.annotation is not None for argument in server_handler.args.args)
        server_defaults = {}
        if server_handler.args.defaults:
            default_args = server_handler.args.args[-len(server_handler.args.defaults):]
            server_defaults = {
                argument.arg: ast.literal_eval(default)
                for argument, default in zip(
                    default_args,
                    server_handler.args.defaults,
                    strict=True,
                )
            }
        assert server_defaults == expected_defaults

        function_name = name.removeprefix("bluetooth-manager.").replace("-", "_")
        implementation = getattr(main, function_name)
        signature = inspect.signature(implementation)
        assert list(signature.parameters) == expected_arg_names
        assert all(
            parameter.annotation is not inspect.Signature.empty
            for parameter in signature.parameters.values()
        )
        assert {
            parameter_name: parameter.default
            for parameter_name, parameter in signature.parameters.items()
            if parameter.default is not inspect.Signature.empty
        } == expected_defaults
        assert signature.return_annotation is not inspect.Signature.empty

    assert not hasattr(main, "run")
    assert "canonical_argv" not in (APP_DIR / "main.py").read_text(encoding="utf-8")


@pytest.mark.parametrize(
    ("function_name", "args", "capability", "argv"),
    [
        (
            "status",
            (),
            mock.call("sys.observe", name="bluetooth"),
            ["/usr/local/bin/cos", "__bluetooth", "status"],
        ),
        (
            "power",
            (ADAPTER.lower(), "on"),
            mock.call("device.bluetooth", name="control"),
            [
                "/usr/local/bin/cos",
                "__bluetooth",
                "power",
                "--adapter",
                ADAPTER,
                "--state",
                "on",
            ],
        ),
        (
            "scan",
            (ADAPTER.lower(),),
            mock.call("device.bluetooth", name="control"),
            [
                "/usr/local/bin/cos",
                "__bluetooth",
                "scan",
                "--adapter",
                ADAPTER,
                "--seconds",
                "10",
            ],
        ),
        (
            "pair",
            (ADAPTER.lower(), DEVICE.lower()),
            mock.call("device.bluetooth", name="control"),
            [
                "/usr/local/bin/cos",
                "__bluetooth",
                "pair",
                "--adapter",
                ADAPTER,
                "--device",
                DEVICE,
            ],
        ),
        (
            "pair_status",
            (PAIRING_ID,),
            mock.call("device.bluetooth", name="control"),
            [
                "/usr/local/bin/cos",
                "__bluetooth",
                "pair-status",
                "--pairing-id",
                PAIRING_ID.lower(),
            ],
        ),
        (
            "pair_respond",
            (PAIRING_ID, "123456"),
            mock.call("device.bluetooth", name="control"),
            [
                "/usr/local/bin/cos",
                "__bluetooth",
                "pair-respond",
                "--pairing-id",
                PAIRING_ID.lower(),
                "--response-stdin",
            ],
        ),
        (
            "pair_cancel",
            (PAIRING_ID,),
            mock.call("device.bluetooth", name="control"),
            [
                "/usr/local/bin/cos",
                "__bluetooth",
                "pair-cancel",
                "--pairing-id",
                PAIRING_ID.lower(),
            ],
        ),
        *[
            (
                action,
                (ADAPTER.lower(), DEVICE.lower()),
                mock.call("device.bluetooth", name="control"),
                [
                    "/usr/local/bin/cos",
                    "__bluetooth",
                    action,
                    "--adapter",
                    ADAPTER,
                    "--device",
                    DEVICE,
                ],
            )
            for action in ("connect", "disconnect", "trust", "untrust", "forget")
        ],
    ],
)
def test_routes_use_exact_capability_and_broker_argv(
    function_name, args, capability, argv
):
    completed = mock.Mock(
        returncode=0,
        stdout=json.dumps({"changed": True}),
        stderr="",
    )
    with mock.patch.dict(os.environ, {"COS_BIN": "/usr/local/bin/cos"}), mock.patch.object(
        main.policy, "require"
    ) as require, mock.patch.object(main.subprocess, "run", return_value=completed) as run:
        result = getattr(main, function_name)(*args)

    require.assert_called_once_with(*capability.args, **capability.kwargs)
    assert run.call_args.args[0] == argv
    assert run.call_args.kwargs["timeout"] == main.TIMEOUT_SECS
    if function_name == "pair_respond":
        assert run.call_args.kwargs["input"] == "123456\n"
        assert "stdin" not in run.call_args.kwargs
        assert "123456" not in argv
    else:
        assert run.call_args.kwargs["stdin"] is main.subprocess.DEVNULL
        assert "input" not in run.call_args.kwargs
    assert result == {"changed": True}


@pytest.mark.parametrize(
    ("function_name", "args", "message"),
    [
        ("power", (None, "on"), "adapter must be a Bluetooth MAC address"),
        ("power", ("hci0", "on"), "adapter must be a Bluetooth MAC address"),
        ("power", (ADAPTER, True), "state must be on or off"),
        ("power", (ADAPTER, "toggle"), "state must be on or off"),
        ("scan", (ADAPTER, True), "scan seconds must be an integer"),
        ("scan", (ADAPTER, "10"), "scan seconds must be an integer"),
        ("scan", (ADAPTER, 1.0), "scan seconds must be an integer"),
        ("scan", (ADAPTER, 0), "scan seconds must be 1..60"),
        ("scan", (ADAPTER, 61), "scan seconds must be 1..60"),
        ("pair", (ADAPTER, "not-a-device"), "device must be a Bluetooth MAC address"),
        (
            "pair_status",
            (123,),
            "pairing id must be exactly 32 hexadecimal characters",
        ),
        (
            "pair_cancel",
            ("not-a-pairing-id",),
            "pairing id must be exactly 32 hexadecimal characters",
        ),
        (
            "pair_respond",
            (PAIRING_ID, True),
            "pairing response must be a string of 1..32 characters without controls",
        ),
        (
            "pair_respond",
            (PAIRING_ID, ""),
            "pairing response must be a string of 1..32 characters without controls",
        ),
        (
            "pair_respond",
            (PAIRING_ID, "x" * 33),
            "pairing response must be a string of 1..32 characters without controls",
        ),
        (
            "pair_respond",
            (PAIRING_ID, "yes\n"),
            "pairing response must be a string of 1..32 characters without controls",
        ),
        (
            "pair_respond",
            (PAIRING_ID, "yes\x00"),
            "pairing response must be a string of 1..32 characters without controls",
        ),
        (
            "pair_respond",
            (PAIRING_ID, "yes\x7f"),
            "pairing response must be a string of 1..32 characters without controls",
        ),
    ],
)
def test_invalid_arguments_are_rejected_before_policy(function_name, args, message):
    with mock.patch.object(main.policy, "require") as require:
        with pytest.raises(ValueError, match=re.escape(message)):
            getattr(main, function_name)(*args)
    require.assert_not_called()


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
    with mock.patch.dict(os.environ, {"COS_BIN": "/usr/local/bin/cos"}), mock.patch.object(
        main.policy, "require"
    ), mock.patch.object(main.subprocess, "run", return_value=completed):
        assert main.status() == expected


@pytest.mark.parametrize(
    ("returncode", "stdout", "stderr", "message"),
    [
        (0, "{", "", "Bluetooth Manager broker returned invalid JSON"),
        (0, "", "{", "Bluetooth Manager broker returned invalid JSON"),
        (0, "[]", "", "Bluetooth Manager broker returned a non-object result"),
        (
            0,
            json.dumps({"error": None}),
            "",
            "Bluetooth Manager broker returned an invalid error payload",
        ),
        (0, json.dumps({"error": "BlueZ unavailable"}), "", "BlueZ unavailable"),
        (7, "{}", "", "Bluetooth Manager broker exited 7"),
        (
            9,
            "{}",
            json.dumps({"error": "Bluetooth authorization denied"}),
            "Bluetooth authorization denied",
        ),
        (0, "", "", "Bluetooth Manager broker returned invalid JSON"),
    ],
)
def test_broker_payload_failures_raise(returncode, stdout, stderr, message):
    completed = mock.Mock(returncode=returncode, stdout=stdout, stderr=stderr)
    with mock.patch.dict(os.environ, {"COS_BIN": "/usr/local/bin/cos"}), mock.patch.object(
        main.policy, "require"
    ) as require, mock.patch.object(main.subprocess, "run", return_value=completed):
        with pytest.raises(RuntimeError, match=re.escape(message)):
            main.status()
    require.assert_called_once_with("sys.observe", name="bluetooth")


def test_missing_broker_executable_raises():
    with mock.patch.dict(os.environ, {}, clear=True), mock.patch.object(
        main.shutil, "which", return_value=None
    ), mock.patch.object(main.policy, "require") as require:
        with pytest.raises(FileNotFoundError, match="Bluetooth Manager broker unavailable"):
            main.status()
    require.assert_called_once_with("sys.observe", name="bluetooth")


@pytest.mark.parametrize(
    ("failure", "exception_type", "message"),
    [
        (
            FileNotFoundError("gone"),
            FileNotFoundError,
            "Bluetooth Manager broker executable not found",
        ),
        (
            PermissionError("access denied"),
            PermissionError,
            "permission denied launching Bluetooth Manager broker",
        ),
        (
            main.subprocess.TimeoutExpired(["cos"], main.TIMEOUT_SECS),
            TimeoutError,
            "Bluetooth Manager broker exceeded",
        ),
    ],
)
def test_broker_execution_failures_raise(failure, exception_type, message):
    with mock.patch.dict(os.environ, {"COS_BIN": "/usr/local/bin/cos"}), mock.patch.object(
        main.policy, "require"
    ) as require, mock.patch.object(main.subprocess, "run", side_effect=failure):
        with pytest.raises(exception_type, match=message):
            main.status()
    require.assert_called_once_with("sys.observe", name="bluetooth")
