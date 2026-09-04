import ast
import inspect
import json
import math
import os
import pathlib
import re
from unittest import mock

import pytest

from test_support import load_local_module


APP_DIR = pathlib.Path(__file__).parent
MANIFEST_PATH = APP_DIR / "app.json"
SERVER_PATH = APP_DIR / "server.py"
COS_BIN = "/usr/local/bin/cos"
LAYOUT_SOURCE = str((APP_DIR / "layout.kdl").resolve())
BACKUP_TOKEN = "ABCDEF0123456789ABCDEF0123456789"
TOOL_NAMES = [
    "display-manager.status",
    "display-manager.enable",
    "display-manager.disable",
    "display-manager.mirror",
    "display-manager.position",
    "display-manager.mode",
    "display-manager.scale",
    "display-manager.apply-layout",
    "display-manager.brightness",
    "display-manager.restore",
]

main = load_local_module(
    APP_DIR / "main.py",
    "claw_test_display_manager_main",
    clear_modules=("_shared",),
)


def _need_signature(need):
    scope = need["scope"]
    if scope["kind"] == "fixed":
        fixed = scope["scope"]
        return need["verb"], "fixed", fixed["kind"], fixed["value"]
    return need["verb"], scope["kind"], scope["arg"]


def _server_bindings():
    tree = ast.parse(SERVER_PATH.read_text(encoding="utf-8"))
    bindings = []
    for node in tree.body:
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
                bindings.append((decorator.args[0].value, node))
    return bindings


def _defaults(node):
    if not node.args.defaults:
        return {}
    arguments = node.args.args[-len(node.args.defaults) :]
    return {
        argument.arg: ast.literal_eval(default)
        for argument, default in zip(arguments, node.args.defaults, strict=True)
    }


def test_manifest_and_handlers_are_mcp_only_and_aligned():
    manifest = json.loads(MANIFEST_PATH.read_text(encoding="utf-8"))
    assert "operations" not in manifest

    tools = manifest["mcp"]["tools"]
    assert [tool["name"] for tool in tools] == TOOL_NAMES
    tool_map = {tool["name"]: tool for tool in tools}

    mirror_args = tool_map["display-manager.mirror"]["args"]
    assert [argument["name"] for argument in mirror_args] == [
        "output",
        "source_output",
    ]

    mode_args = {
        argument["name"]: argument
        for argument in tool_map["display-manager.mode"]["args"]
    }
    assert list(mode_args) == [
        "output",
        "width",
        "height",
        "adaptive_sync",
        "refresh",
        "scale",
        "x",
        "y",
        "transform",
    ]
    for name in ("adaptive_sync", "refresh", "scale", "x", "y", "transform"):
        assert mode_args[name]["binding"] == "flag"
    assert mode_args["adaptive_sync"]["choices"] == [
        "true",
        "automatic",
        "false",
    ]
    assert mode_args["transform"]["choices"] == [
        "normal",
        "rotate90",
        "rotate180",
        "rotate270",
        "flipped",
        "flipped90",
        "flipped180",
        "flipped270",
    ]

    for name in ("apply-layout", "restore"):
        assert tool_map[f"display-manager.{name}"]["args"][-1]["choices"] == [True]

    observe = [("sys.observe", "fixed", "name", "display")]
    manage = [("device.display", "fixed", "name", "manage")]
    assert [
        _need_signature(need)
        for need in tool_map["display-manager.status"]["needs"]
    ] == observe
    for name in (
        "enable",
        "disable",
        "mirror",
        "position",
        "mode",
        "scale",
        "brightness",
        "restore",
    ):
        assert [
            _need_signature(need)
            for need in tool_map[f"display-manager.{name}"]["needs"]
        ] == manage
    assert [
        _need_signature(need)
        for need in tool_map["display-manager.apply-layout"]["needs"]
    ] == [
        ("device.display", "fixed", "name", "manage"),
        ("fs.read", "from-arg", "source"),
    ]

    server_source = SERVER_PATH.read_text(encoding="utf-8")
    assert "serve_manifest_operations" not in server_source
    assert server_source.count("App.from_manifest()") == 1
    bindings = _server_bindings()
    assert len(bindings) == len(TOOL_NAMES)
    assert [name for name, _node in bindings] == TOOL_NAMES

    for tool_name, node in bindings:
        arguments = tool_map[tool_name].get("args", [])
        expected_names = [argument["name"] for argument in arguments]
        expected_defaults = {
            argument["name"]: argument.get("default")
            for argument in arguments
            if not argument.get("required", False)
        }
        assert [argument.arg for argument in node.args.args] == expected_names
        assert all(argument.annotation is not None for argument in node.args.args)
        assert _defaults(node) == expected_defaults

        function_name = tool_name.removeprefix("display-manager.").replace("-", "_")
        implementation = getattr(main, function_name)
        signature = inspect.signature(implementation)
        assert list(signature.parameters) == expected_names
        assert all(
            parameter.annotation is not inspect.Signature.empty
            for parameter in signature.parameters.values()
        )
        assert {
            name: parameter.default
            for name, parameter in signature.parameters.items()
            if parameter.default is not inspect.Signature.empty
        } == expected_defaults
        assert signature.return_annotation is not inspect.Signature.empty

    assert not hasattr(main, "run")
    assert "canonical_argv" not in (APP_DIR / "main.py").read_text(encoding="utf-8")


@pytest.mark.parametrize(
    ("call", "capabilities", "argv"),
    [
        (
            lambda: main.status(),
            [mock.call("sys.observe", name="display")],
            [COS_BIN, "__display", "status"],
        ),
        (
            lambda: main.enable("eDP-1"),
            [mock.call("device.display", name="manage")],
            [COS_BIN, "__display", "enable", "--output", "eDP-1"],
        ),
        (
            lambda: main.disable("DP-1"),
            [mock.call("device.display", name="manage")],
            [COS_BIN, "__display", "disable", "--output", "DP-1"],
        ),
        (
            lambda: main.mirror("HDMI-A-1", "eDP-1"),
            [mock.call("device.display", name="manage")],
            [
                COS_BIN,
                "__display",
                "mirror",
                "--output",
                "HDMI-A-1",
                "--from",
                "eDP-1",
            ],
        ),
        (
            lambda: main.position("DP-1", -1920, 0),
            [mock.call("device.display", name="manage")],
            [
                COS_BIN,
                "__display",
                "position",
                "--output",
                "DP-1",
                "--x",
                "-1920",
                "--y",
                "0",
            ],
        ),
        (
            lambda: main.mode(
                "DP-1",
                3840,
                2160,
                "automatic",
                144.0,
                1.25,
                -1920,
                0,
                "rotate90",
            ),
            [mock.call("device.display", name="manage")],
            [
                COS_BIN,
                "__display",
                "mode",
                "--output",
                "DP-1",
                "--width",
                "3840",
                "--height",
                "2160",
                "--refresh",
                "144.0",
                "--scale",
                "1.25",
                "--x",
                "-1920",
                "--y",
                "0",
                "--transform",
                "rotate90",
                "--adaptive-sync",
                "automatic",
            ],
        ),
        (
            lambda: main.scale("eDP-1", 1.5),
            [mock.call("device.display", name="manage")],
            [COS_BIN, "__display", "scale", "--output", "eDP-1", "--scale", "1.5"],
        ),
        (
            lambda: main.apply_layout(LAYOUT_SOURCE, True),
            [
                mock.call("device.display", name="manage"),
                mock.call("fs.read", path=LAYOUT_SOURCE),
            ],
            [
                COS_BIN,
                "__display",
                "apply-layout",
                "--source",
                LAYOUT_SOURCE,
                "--confirm",
            ],
        ),
        (
            lambda: main.brightness("intel_backlight", 75),
            [mock.call("device.display", name="manage")],
            [
                COS_BIN,
                "__display",
                "brightness",
                "--backlight",
                "intel_backlight",
                "--percent",
                "75",
            ],
        ),
        (
            lambda: main.restore(BACKUP_TOKEN, True),
            [mock.call("device.display", name="manage")],
            [
                COS_BIN,
                "__display",
                "restore",
                "--token",
                BACKUP_TOKEN.lower(),
                "--confirm",
            ],
        ),
    ],
)
def test_routes_use_exact_capabilities_and_broker_argv(call, capabilities, argv):
    completed = mock.Mock(returncode=0, stdout='{"changed":true}', stderr="")
    with mock.patch.dict(os.environ, {"COS_BIN": COS_BIN}), mock.patch.object(
        main.policy, "require"
    ) as require, mock.patch.object(main.subprocess, "run", return_value=completed) as run:
        assert call() == {"changed": True}

    assert require.call_args_list == capabilities
    assert run.call_args.args[0] == argv
    assert run.call_args.kwargs["timeout"] == main.TIMEOUT_SECS
    assert run.call_args.kwargs["stdin"] is main.subprocess.DEVNULL


def test_mode_omits_all_defaulted_options():
    completed = mock.Mock(returncode=0, stdout="{}", stderr="")
    with mock.patch.dict(os.environ, {"COS_BIN": COS_BIN}), mock.patch.object(
        main.policy, "require"
    ), mock.patch.object(main.subprocess, "run", return_value=completed) as run:
        main.mode("eDP-1", 1920, 1080)
    assert run.call_args.args[0] == [
        COS_BIN,
        "__display",
        "mode",
        "--output",
        "eDP-1",
        "--width",
        "1920",
        "--height",
        "1080",
    ]


@pytest.mark.parametrize(
    ("call", "message"),
    [
        (lambda: main.enable(None), "invalid output"),
        (lambda: main.disable("-DP-1"), "invalid output"),
        (lambda: main.mirror("DP-1", True), "invalid output"),
        (lambda: main.position("DP-1", True, 0), "x must be an integer"),
        (lambda: main.position("DP-1", 0, 32769), "y must be -32768..32768"),
        (lambda: main.mode("DP-1", True, 1080), "width must be an integer"),
        (lambda: main.mode("DP-1", 0, 1080), "width must be 1..16384"),
        (lambda: main.mode("DP-1", 1920, 16385), "height must be 1..16384"),
        (
            lambda: main.mode("DP-1", 1920, 1080, adaptive_sync=True),
            "invalid adaptive-sync mode",
        ),
        (
            lambda: main.mode("DP-1", 1920, 1080, adaptive_sync="sometimes"),
            "invalid adaptive-sync mode",
        ),
        (
            lambda: main.mode("DP-1", 1920, 1080, refresh=True),
            "refresh must be a number",
        ),
        (
            lambda: main.mode("DP-1", 1920, 1080, refresh=math.nan),
            "refresh must be finite",
        ),
        (
            lambda: main.mode("DP-1", 1920, 1080, refresh=1001),
            "refresh must be 1..1000",
        ),
        (
            lambda: main.mode("DP-1", 1920, 1080, scale=math.inf),
            "scale must be finite",
        ),
        (
            lambda: main.mode("DP-1", 1920, 1080, scale=0.25),
            "scale must be 0.5..4",
        ),
        (
            lambda: main.mode("DP-1", 1920, 1080, x=True, y=0),
            "x must be an integer",
        ),
        (
            lambda: main.mode("DP-1", 1920, 1080, x=0),
            "x and y must be provided together",
        ),
        (
            lambda: main.mode("DP-1", 1920, 1080, y=0),
            "x and y must be provided together",
        ),
        (
            lambda: main.mode("DP-1", 1920, 1080, transform=1),
            "invalid transform",
        ),
        (
            lambda: main.mode("DP-1", 1920, 1080, transform="sideways"),
            "invalid transform",
        ),
        (lambda: main.scale("DP-1", False), "scale must be a number"),
        (lambda: main.scale("DP-1", 4.1), "scale must be 0.5..4"),
        (lambda: main.brightness("../backlight", 50), "invalid backlight"),
        (
            lambda: main.brightness("intel_backlight", True),
            "brightness percent must be an integer",
        ),
        (
            lambda: main.brightness("intel_backlight", 0),
            "brightness percent must be 1..100",
        ),
        (
            lambda: main.restore("not-a-token", True),
            "backup token must be exactly 32 hexadecimal characters",
        ),
    ],
)
def test_invalid_arguments_are_rejected_before_policy(call, message):
    with mock.patch.object(main.policy, "require") as require:
        with pytest.raises(ValueError, match=re.escape(message)):
            call()
    require.assert_not_called()


@pytest.mark.parametrize(
    "call",
    [
        lambda: main.apply_layout(LAYOUT_SOURCE, False),
        lambda: main.apply_layout(LAYOUT_SOURCE, 1),
        lambda: main.apply_layout(LAYOUT_SOURCE, "true"),
        lambda: main.restore(BACKUP_TOKEN, False),
        lambda: main.restore(BACKUP_TOKEN, 1),
        lambda: main.restore(BACKUP_TOKEN, "true"),
    ],
)
def test_confirms_require_exact_true_before_policy(call):
    with mock.patch.object(main.policy, "require") as require:
        with pytest.raises(ValueError, match="requires confirm=true"):
            call()
    require.assert_not_called()


@pytest.mark.parametrize(
    "source",
    [
        None,
        "layout.kdl",
        f"{LAYOUT_SOURCE}\n",
        f"{LAYOUT_SOURCE}\x00",
    ],
)
def test_invalid_layout_sources_are_rejected_before_policy(source):
    with mock.patch.object(main.policy, "require") as require:
        with pytest.raises(ValueError, match="absolute canonical non-symlink"):
            main.apply_layout(source, True)
    require.assert_not_called()


@pytest.mark.parametrize(
    ("canonical", "is_link"),
    [
        (f"{LAYOUT_SOURCE}.canonical", False),
        (LAYOUT_SOURCE, True),
    ],
)
def test_noncanonical_or_symlink_layout_is_rejected_before_policy(
    canonical, is_link
):
    with mock.patch.object(
        main.os.path, "realpath", return_value=canonical
    ), mock.patch.object(
        main.os.path, "islink", return_value=is_link
    ), mock.patch.object(main.policy, "require") as require:
        with pytest.raises(ValueError, match="absolute canonical non-symlink"):
            main.apply_layout(LAYOUT_SOURCE, True)
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
    with mock.patch.dict(os.environ, {"COS_BIN": COS_BIN}), mock.patch.object(
        main.policy, "require"
    ), mock.patch.object(main.subprocess, "run", return_value=completed):
        assert main.status() == expected


@pytest.mark.parametrize(
    ("returncode", "stdout", "stderr", "message"),
    [
        (0, "{", "", "Display Manager broker returned invalid JSON"),
        (0, "", "{", "Display Manager broker returned invalid JSON"),
        (0, "[]", "", "Display Manager broker returned a non-object result"),
        (
            0,
            json.dumps({"error": None}),
            "",
            "Display Manager broker returned an invalid error payload",
        ),
        (0, json.dumps({"error": "COSMIC unavailable"}), "", "COSMIC unavailable"),
        (7, "{}", "", "Display Manager broker exited 7"),
        (
            9,
            "{}",
            json.dumps({"error": "display authorization denied"}),
            "display authorization denied",
        ),
        (0, "", "", "Display Manager broker returned invalid JSON"),
    ],
)
def test_broker_payload_failures_raise(returncode, stdout, stderr, message):
    completed = mock.Mock(returncode=returncode, stdout=stdout, stderr=stderr)
    with mock.patch.dict(os.environ, {"COS_BIN": COS_BIN}), mock.patch.object(
        main.policy, "require"
    ) as require, mock.patch.object(main.subprocess, "run", return_value=completed):
        with pytest.raises(RuntimeError, match=re.escape(message)):
            main.status()
    require.assert_called_once_with("sys.observe", name="display")


def test_missing_broker_executable_raises():
    with mock.patch.dict(os.environ, {}, clear=True), mock.patch.object(
        main.shutil, "which", return_value=None
    ), mock.patch.object(main.policy, "require") as require:
        with pytest.raises(FileNotFoundError, match="Display Manager broker unavailable"):
            main.status()
    require.assert_called_once_with("sys.observe", name="display")


@pytest.mark.parametrize(
    ("failure", "exception_type", "message"),
    [
        (
            FileNotFoundError("gone"),
            FileNotFoundError,
            "Display Manager broker executable not found",
        ),
        (
            PermissionError("access denied"),
            PermissionError,
            "permission denied launching Display Manager broker",
        ),
        (
            main.subprocess.TimeoutExpired(["cos"], main.TIMEOUT_SECS),
            TimeoutError,
            "Display Manager broker exceeded",
        ),
    ],
)
def test_broker_execution_failures_raise(failure, exception_type, message):
    with mock.patch.dict(os.environ, {"COS_BIN": COS_BIN}), mock.patch.object(
        main.policy, "require"
    ) as require, mock.patch.object(main.subprocess, "run", side_effect=failure):
        with pytest.raises(exception_type, match=message):
            main.status()
    require.assert_called_once_with("sys.observe", name="display")
