import json
import os
import pathlib
import re
from unittest import mock

import pytest

from test_support import load_local_module


APP_DIR = pathlib.Path(__file__).parent
MANIFEST_PATH = APP_DIR / "app.json"
RULE_ID = "ABCDEF0123456789ABCDEF0123456789"
BACKUP_TOKEN = "0123456789ABCDEF0123456789ABCDEF"

main = load_local_module(
    APP_DIR / "main.py",
    "claw_test_firewall_manager_main",
    clear_modules=("_shared",),
)


def test_manifest_is_mcp_only_with_closed_choices_and_cli_bindings():
    manifest = json.loads(MANIFEST_PATH.read_text(encoding="utf-8"))
    assert "operations" not in manifest

    tools = {tool["name"]: tool for tool in manifest["mcp"]["tools"]}
    assert set(tools) == {
        "firewall-manager.status",
        "firewall-manager.add",
        "firewall-manager.delete",
        "firewall-manager.clear",
        "firewall-manager.restore",
    }

    add_args = {
        argument["name"]: argument
        for argument in tools["firewall-manager.add"]["args"]
    }
    assert add_args["action"]["choices"] == ["allow", "deny"]
    assert add_args["direction"]["choices"] == ["input", "output"]
    assert add_args["protocol"]["choices"] == ["tcp", "udp"]
    assert add_args["remote"]["binding"] == "flag"
    assert add_args["interface"]["binding"] == "flag"

    assert tools["firewall-manager.clear"]["args"][0]["choices"] == [True]
    assert tools["firewall-manager.restore"]["args"][1]["choices"] == [True]
    assert tools["firewall-manager.status"]["needs"][0]["verb"] == "sys.observe"
    for name in ("add", "delete", "clear", "restore"):
        assert (
            tools[f"firewall-manager.{name}"]["needs"][0]["verb"]
            == "net.firewall"
        )


@pytest.mark.parametrize(
    ("call", "capability", "argv"),
    [
        (
            lambda: main.status(),
            mock.call("sys.observe", name="firewall"),
            ["/usr/local/bin/cos", "__firewall", "status"],
        ),
        (
            lambda: main.add(
                "deny",
                "input",
                "tcp",
                22,
                "192.0.2.129/24",
                "eth0",
            ),
            mock.call("net.firewall", name="manage"),
            [
                "/usr/local/bin/cos",
                "__firewall",
                "add",
                "--rule-action",
                "deny",
                "--direction",
                "input",
                "--protocol",
                "tcp",
                "--port",
                "22",
                "--remote",
                "192.0.2.0/24",
                "--interface",
                "eth0",
            ],
        ),
        (
            lambda: main.delete(RULE_ID),
            mock.call("net.firewall", name="manage"),
            [
                "/usr/local/bin/cos",
                "__firewall",
                "delete",
                "--rule-id",
                RULE_ID.lower(),
            ],
        ),
        (
            lambda: main.clear(True),
            mock.call("net.firewall", name="manage"),
            [
                "/usr/local/bin/cos",
                "__firewall",
                "clear",
                "--confirm",
            ],
        ),
        (
            lambda: main.restore(BACKUP_TOKEN, True),
            mock.call("net.firewall", name="manage"),
            [
                "/usr/local/bin/cos",
                "__firewall",
                "restore",
                "--token",
                BACKUP_TOKEN.lower(),
                "--confirm",
            ],
        ),
    ],
)
def test_routes_use_exact_capability_and_broker_argv(call, capability, argv):
    completed = mock.Mock(
        returncode=0,
        stdout=json.dumps({"changed": True}),
        stderr="",
    )
    with mock.patch.dict(os.environ, {"COS_BIN": "/usr/local/bin/cos"}), mock.patch.object(
        main.policy, "require"
    ) as require, mock.patch.object(main.subprocess, "run", return_value=completed) as run:
        result = call()
    require.assert_called_once_with(*capability.args, **capability.kwargs)
    assert run.call_args.args[0] == argv
    assert run.call_args.kwargs["timeout"] == main.TIMEOUT_SECS
    assert result == {"changed": True}


def test_add_omits_optional_flags_when_not_supplied():
    completed = mock.Mock(returncode=0, stdout="{}", stderr="")
    with mock.patch.dict(os.environ, {"COS_BIN": "/usr/local/bin/cos"}), mock.patch.object(
        main.policy, "require"
    ), mock.patch.object(main.subprocess, "run", return_value=completed) as run:
        main.add("allow", "output", "udp", 53)
    assert run.call_args.args[0] == [
        "/usr/local/bin/cos",
        "__firewall",
        "add",
        "--rule-action",
        "allow",
        "--direction",
        "output",
        "--protocol",
        "udp",
        "--port",
        "53",
    ]


@pytest.mark.parametrize(
    ("call", "message"),
    [
        (
            lambda: main.add("drop", "input", "tcp", 22),
            "action must be allow or deny",
        ),
        (
            lambda: main.add(1.0, "input", "tcp", 22),
            "action must be allow or deny",
        ),
        (
            lambda: main.add("allow", "inbound", "tcp", 22),
            "direction must be input or output",
        ),
        (
            lambda: main.add("allow", 1.0, "tcp", 22),
            "direction must be input or output",
        ),
        (
            lambda: main.add("allow", "input", "icmp", 22),
            "protocol must be tcp or udp",
        ),
        (
            lambda: main.add("allow", "input", 1.0, 22),
            "protocol must be tcp or udp",
        ),
        (
            lambda: main.add("allow", "input", "tcp", True),
            "port must be an integer",
        ),
        (
            lambda: main.add("allow", "input", "tcp", 22.0),
            "port must be an integer",
        ),
        (
            lambda: main.add("allow", "input", "tcp", 0),
            "port must be 1..65535",
        ),
        (
            lambda: main.add("allow", "input", "tcp", 65536),
            "port must be 1..65535",
        ),
        (
            lambda: main.add("allow", "input", "tcp", 22, 3221225985),
            "remote must be a CIDR string",
        ),
        (
            lambda: main.add("allow", "input", "tcp", 22, "not-a-cidr"),
            "invalid remote CIDR",
        ),
        (
            lambda: main.add(
                "allow",
                "input",
                "tcp",
                22,
                interface=1.0,
            ),
            "invalid interface name",
        ),
        (
            lambda: main.add(
                "allow",
                "input",
                "tcp",
                22,
                interface="interface-name-too-long",
            ),
            "invalid interface name",
        ),
        (lambda: main.delete(1.0), "rule_id must be exactly 32 hexadecimal"),
        (lambda: main.delete("not-a-rule"), "rule_id must be exactly 32 hexadecimal"),
        (
            lambda: main.restore(1.0, True),
            "backup_token must be exactly 32 hexadecimal",
        ),
        (
            lambda: main.restore("not-a-token", True),
            "backup_token must be exactly 32 hexadecimal",
        ),
    ],
)
def test_invalid_values_are_rejected_before_policy(call, message):
    with mock.patch.object(main.policy, "require") as require:
        with pytest.raises(ValueError, match=re.escape(message)):
            call()
    require.assert_not_called()


@pytest.mark.parametrize(
    "call",
    [
        lambda: main.clear(False),
        lambda: main.clear(1),
        lambda: main.clear("true"),
        lambda: main.restore(BACKUP_TOKEN, False),
        lambda: main.restore(BACKUP_TOKEN, 1),
        lambda: main.restore(BACKUP_TOKEN, "true"),
    ],
)
def test_destructive_actions_require_real_true_before_policy(call):
    with mock.patch.object(main.policy, "require") as require:
        with pytest.raises(ValueError, match="requires confirm=true"):
            call()
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
        (0, "{", "", "Firewall Manager broker returned invalid JSON"),
        (0, "", "{", "Firewall Manager broker returned invalid JSON"),
        (0, "[]", "", "Firewall Manager broker returned a non-object result"),
        (
            0,
            json.dumps({"error": None}),
            "",
            "Firewall Manager broker returned an invalid error payload",
        ),
        (0, json.dumps({"error": "nftables unavailable"}), "", "nftables unavailable"),
        (7, "{}", "", "Firewall Manager broker exited 7"),
        (
            9,
            "{}",
            json.dumps({"error": "firewall authorization denied"}),
            "firewall authorization denied",
        ),
        (0, "", "", "Firewall Manager broker returned invalid JSON"),
    ],
)
def test_broker_payload_failures_raise(returncode, stdout, stderr, message):
    completed = mock.Mock(returncode=returncode, stdout=stdout, stderr=stderr)
    with mock.patch.dict(os.environ, {"COS_BIN": "/usr/local/bin/cos"}), mock.patch.object(
        main.policy, "require"
    ) as require, mock.patch.object(main.subprocess, "run", return_value=completed):
        with pytest.raises(RuntimeError, match=re.escape(message)):
            main.status()
    require.assert_called_once_with("sys.observe", name="firewall")


def test_missing_broker_executable_raises():
    with mock.patch.dict(os.environ, {}, clear=True), mock.patch.object(
        main.shutil, "which", return_value=None
    ), mock.patch.object(main.policy, "require") as require:
        with pytest.raises(FileNotFoundError, match="Firewall Manager broker unavailable"):
            main.status()
    require.assert_called_once_with("sys.observe", name="firewall")


@pytest.mark.parametrize(
    ("failure", "exception_type", "message"),
    [
        (
            FileNotFoundError("gone"),
            FileNotFoundError,
            "Firewall Manager broker executable not found",
        ),
        (
            PermissionError("access denied"),
            PermissionError,
            "permission denied launching Firewall Manager broker",
        ),
        (
            main.subprocess.TimeoutExpired(["cos"], main.TIMEOUT_SECS),
            TimeoutError,
            "Firewall Manager broker exceeded",
        ),
    ],
)
def test_broker_execution_failures_raise(failure, exception_type, message):
    with mock.patch.dict(os.environ, {"COS_BIN": "/usr/local/bin/cos"}), mock.patch.object(
        main.policy, "require"
    ) as require, mock.patch.object(main.subprocess, "run", side_effect=failure):
        with pytest.raises(exception_type, match=message):
            main.status()
    require.assert_called_once_with("sys.observe", name="firewall")
