import ast
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
COS_BIN = "/usr/local/bin/cos"
SHELL = "/opt/claw-shell"
CREDENTIAL = "default/alice-password"
BACKUP_TOKEN = "ABCDEF0123456789ABCDEF0123456789"

main = load_local_module(
    APP_DIR / "main.py",
    "claw_test_user_manager_main",
    clear_modules=("_shared",),
)


def _need_signature(need):
    scope = need["scope"]
    if scope["kind"] == "fixed":
        fixed = scope["scope"]
        return need["verb"], "fixed", fixed["kind"], fixed["value"]
    return need["verb"], scope["kind"], scope["arg"]


def _server_signatures():
    tree = ast.parse(SERVER_PATH.read_text(encoding="utf-8"))
    signatures = {}
    for node in tree.body:
        if not isinstance(node, ast.FunctionDef):
            continue
        tool_name = None
        for decorator in node.decorator_list:
            if (
                isinstance(decorator, ast.Call)
                and isinstance(decorator.func, ast.Attribute)
                and decorator.func.attr == "tool"
                and len(decorator.args) == 1
                and isinstance(decorator.args[0], ast.Constant)
            ):
                tool_name = decorator.args[0].value
        if tool_name is None:
            continue
        names = [argument.arg for argument in node.args.args]
        default_names = names[len(names) - len(node.args.defaults) :]
        defaults = {
            name: ast.literal_eval(default)
            for name, default in zip(default_names, node.args.defaults)
        }
        signatures[tool_name] = (names, defaults)
    return signatures


def test_manifest_is_mcp_only_with_all_tools_needs_and_cli_bindings():
    manifest = json.loads(MANIFEST_PATH.read_text(encoding="utf-8"))
    assert "operations" not in manifest

    tools = {tool["name"]: tool for tool in manifest["mcp"]["tools"]}
    assert set(tools) == {
        "user-manager.status",
        "user-manager.create-user",
        "user-manager.delete-user",
        "user-manager.lock-user",
        "user-manager.unlock-user",
        "user-manager.set-shell",
        "user-manager.set-password",
        "user-manager.create-group",
        "user-manager.delete-group",
        "user-manager.add-to-group",
        "user-manager.remove-from-group",
        "user-manager.restore",
    }

    create_args = tools["user-manager.create-user"]["args"]
    assert create_args == [
        {"name": "user", "kind": "name", "required": True},
        {
            "name": "groups",
            "kind": "text",
            "binding": "flag",
            "required": False,
        },
        {
            "name": "full_name",
            "kind": "text",
            "binding": "flag",
            "required": False,
        },
        {
            "name": "shell",
            "kind": "path",
            "binding": "flag",
            "required": False,
        },
    ]

    manage = [("sys.identity", "fixed", "name", "manage")]
    assert [_need_signature(need) for need in tools["user-manager.status"]["needs"]] == [
        ("sys.observe", "fixed", "name", "identities")
    ]
    for command in (
        "create-user",
        "delete-user",
        "lock-user",
        "unlock-user",
        "set-shell",
        "create-group",
        "delete-group",
        "add-to-group",
        "remove-from-group",
        "restore",
    ):
        assert [
            _need_signature(need)
            for need in tools[f"user-manager.{command}"]["needs"]
        ] == manage
    assert [
        _need_signature(need)
        for need in tools["user-manager.set-password"]["needs"]
    ] == [
        ("sys.identity", "fixed", "name", "manage"),
        ("secret.read", "from-arg", "credential"),
    ]
    for command in ("delete-user", "delete-group", "restore"):
        assert tools[f"user-manager.{command}"]["args"][-1]["choices"] == [True]


def test_server_handlers_match_manifest_argument_names_and_defaults():
    assert _server_signatures() == {
        "user-manager.status": ([], {}),
        "user-manager.create-user": (
            ["user", "groups", "full_name", "shell"],
            {"groups": None, "full_name": None, "shell": None},
        ),
        "user-manager.delete-user": (["user", "confirm"], {}),
        "user-manager.lock-user": (["user"], {}),
        "user-manager.unlock-user": (["user"], {}),
        "user-manager.set-shell": (["user", "shell"], {}),
        "user-manager.set-password": (["user", "credential"], {}),
        "user-manager.create-group": (["group"], {}),
        "user-manager.delete-group": (["group", "confirm"], {}),
        "user-manager.add-to-group": (["user", "group"], {}),
        "user-manager.remove-from-group": (["user", "group"], {}),
        "user-manager.restore": (["backup_token", "confirm"], {}),
    }


@pytest.mark.parametrize(
    ("call", "capabilities", "argv"),
    [
        (
            lambda: main.status(),
            [mock.call("sys.observe", name="identities")],
            [COS_BIN, "__users", "status"],
        ),
        (
            lambda: main.create_user("alice"),
            [mock.call("sys.identity", name="manage")],
            [COS_BIN, "__users", "create-user", "--user", "alice"],
        ),
        (
            lambda: main.delete_user("alice", True),
            [mock.call("sys.identity", name="manage")],
            [COS_BIN, "__users", "delete-user", "--user", "alice", "--confirm"],
        ),
        (
            lambda: main.lock_user("alice"),
            [mock.call("sys.identity", name="manage")],
            [COS_BIN, "__users", "lock-user", "--user", "alice"],
        ),
        (
            lambda: main.unlock_user("alice"),
            [mock.call("sys.identity", name="manage")],
            [COS_BIN, "__users", "unlock-user", "--user", "alice"],
        ),
        (
            lambda: main.set_shell("alice", SHELL),
            [mock.call("sys.identity", name="manage")],
            [COS_BIN, "__users", "set-shell", "--user", "alice", "--shell", SHELL],
        ),
        (
            lambda: main.set_password("alice", CREDENTIAL),
            [
                mock.call("sys.identity", name="manage"),
                mock.call("secret.read", name=CREDENTIAL),
            ],
            [
                COS_BIN,
                "__users",
                "set-password",
                "--user",
                "alice",
                "--credential",
                CREDENTIAL,
            ],
        ),
        (
            lambda: main.create_group("developers"),
            [mock.call("sys.identity", name="manage")],
            [COS_BIN, "__users", "create-group", "--group", "developers"],
        ),
        (
            lambda: main.delete_group("developers", True),
            [mock.call("sys.identity", name="manage")],
            [
                COS_BIN,
                "__users",
                "delete-group",
                "--group",
                "developers",
                "--confirm",
            ],
        ),
        (
            lambda: main.add_to_group("alice", "developers"),
            [mock.call("sys.identity", name="manage")],
            [
                COS_BIN,
                "__users",
                "add-to-group",
                "--user",
                "alice",
                "--group",
                "developers",
            ],
        ),
        (
            lambda: main.remove_from_group("alice", "developers"),
            [mock.call("sys.identity", name="manage")],
            [
                COS_BIN,
                "__users",
                "remove-from-group",
                "--user",
                "alice",
                "--group",
                "developers",
            ],
        ),
        (
            lambda: main.restore(BACKUP_TOKEN, True),
            [mock.call("sys.identity", name="manage")],
            [
                COS_BIN,
                "__users",
                "restore",
                "--token",
                BACKUP_TOKEN.lower(),
                "--confirm",
            ],
        ),
    ],
)
def test_all_routes_use_exact_capabilities_and_broker_argv(
    call, capabilities, argv
):
    completed = mock.Mock(
        returncode=0,
        stdout=json.dumps({"changed": True}),
        stderr="",
    )
    with mock.patch.dict(os.environ, {"COS_BIN": COS_BIN}), mock.patch.object(
        main.policy, "require"
    ) as require, mock.patch.object(main.subprocess, "run", return_value=completed) as run:
        result = call()
    assert require.call_args_list == capabilities
    assert run.call_args.args[0] == argv
    assert run.call_args.kwargs["timeout"] == main.TIMEOUT_SECS
    assert run.call_args.kwargs["stdin"] == main.subprocess.DEVNULL
    assert result == {"changed": True}


def test_create_user_normalizes_and_forwards_optional_flags():
    completed = mock.Mock(returncode=0, stdout="{}", stderr="")
    with mock.patch.dict(os.environ, {"COS_BIN": COS_BIN}), mock.patch.object(
        main.policy, "require"
    ), mock.patch.object(main.subprocess, "run", return_value=completed) as run:
        main.create_user(
            "alice",
            groups="wheel, audio",
            full_name="Alice Example",
            shell=SHELL,
        )
    assert run.call_args.args[0] == [
        COS_BIN,
        "__users",
        "create-user",
        "--user",
        "alice",
        "--full-name",
        "Alice Example",
        "--shell",
        SHELL,
        "--groups",
        "wheel,audio",
    ]


@pytest.mark.parametrize(
    ("call", "message"),
    [
        (lambda: main.create_user("Alice"), "invalid user name"),
        (
            lambda: main.create_user("alice", groups=1),
            "groups must be a comma-separated string",
        ),
        (
            lambda: main.create_user("alice", groups="wheel,,audio"),
            "groups must contain 1-64 non-empty names",
        ),
        (
            lambda: main.create_user("alice", groups="wheel, wheel"),
            "groups must not contain duplicates",
        ),
        (
            lambda: main.create_user("alice", groups="wheel,Invalid"),
            "invalid group name",
        ),
        (
            lambda: main.create_user(
                "alice",
                groups=",".join(f"group{index}" for index in range(65)),
            ),
            "groups must contain 1-64 non-empty names",
        ),
        (
            lambda: main.create_user("alice", full_name=""),
            "full_name must be 1..128 characters",
        ),
        (
            lambda: main.create_user("alice", full_name="Alice:Admin"),
            "full_name must be 1..128 characters",
        ),
        (
            lambda: main.create_user("alice", full_name="Alice\nAdmin"),
            "full_name must be 1..128 characters",
        ),
        (
            lambda: main.create_user("alice", full_name="A" * 129),
            "full_name must be 1..128 characters",
        ),
        (
            lambda: main.create_user("alice", shell="bin/bash"),
            "shell must be an absolute canonical non-symlink path",
        ),
        (
            lambda: main.set_shell("alice", "/bin/\x00bash"),
            "shell must be an absolute canonical non-symlink path",
        ),
        (
            lambda: main.set_shell("alice", "/bin/bash\n"),
            "shell must be an absolute canonical non-symlink path",
        ),
        (
            lambda: main.set_password("alice", "not-a-reference"),
            "credential must use namespace/name form",
        ),
        (
            lambda: main.set_password("alice", "-default/password"),
            "credential must use namespace/name form",
        ),
        (lambda: main.create_group("Developers"), "invalid group name"),
        (
            lambda: main.add_to_group("alice", "Developers"),
            "invalid group name",
        ),
        (
            lambda: main.remove_from_group("Alice", "developers"),
            "invalid user name",
        ),
        (
            lambda: main.restore("not-a-token", True),
            "backup_token must be exactly 32 hexadecimal characters",
        ),
    ],
)
def test_invalid_values_are_rejected_before_policy(call, message):
    with mock.patch.object(main.policy, "require") as require:
        with pytest.raises(ValueError, match=re.escape(message)):
            call()
    require.assert_not_called()


@pytest.mark.parametrize(
    ("raw", "canonical", "is_link"),
    [
        ("/usr/bin/../bin/bash", "/usr/bin/bash", False),
        ("/usr/bin/bash", "/usr/bin/bash", True),
    ],
)
def test_noncanonical_or_symlink_shell_is_rejected_before_policy(
    raw, canonical, is_link
):
    with mock.patch.object(
        main.os.path, "realpath", return_value=canonical
    ), mock.patch.object(
        main.os.path, "islink", return_value=is_link
    ), mock.patch.object(main.policy, "require") as require:
        with pytest.raises(
            ValueError,
            match="shell must be an absolute canonical non-symlink path",
        ):
            main.set_shell("alice", raw)
    require.assert_not_called()


@pytest.mark.parametrize(
    "call",
    [
        lambda: main.delete_user("alice", False),
        lambda: main.delete_user("alice", 1),
        lambda: main.delete_user("alice", "true"),
        lambda: main.delete_group("developers", False),
        lambda: main.delete_group("developers", 1),
        lambda: main.delete_group("developers", "true"),
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
    with mock.patch.dict(os.environ, {"COS_BIN": COS_BIN}), mock.patch.object(
        main.policy, "require"
    ), mock.patch.object(main.subprocess, "run", return_value=completed):
        assert main.status() == expected


@pytest.mark.parametrize(
    ("returncode", "stdout", "stderr", "message"),
    [
        (0, "{", "", "User Manager broker returned invalid JSON"),
        (0, "", "{", "User Manager broker returned invalid JSON"),
        (0, "[]", "", "User Manager broker returned a non-object result"),
        (
            0,
            json.dumps({"error": None}),
            "",
            "User Manager broker returned an invalid error payload",
        ),
        (0, json.dumps({"error": "identity unavailable"}), "", "identity unavailable"),
        (7, "{}", "", "User Manager broker exited 7"),
        (
            9,
            "{}",
            json.dumps({"error": "identity authorization denied"}),
            "identity authorization denied",
        ),
        (0, "", "", "User Manager broker returned invalid JSON"),
    ],
)
def test_broker_payload_failures_raise(returncode, stdout, stderr, message):
    completed = mock.Mock(returncode=returncode, stdout=stdout, stderr=stderr)
    with mock.patch.dict(os.environ, {"COS_BIN": COS_BIN}), mock.patch.object(
        main.policy, "require"
    ) as require, mock.patch.object(main.subprocess, "run", return_value=completed):
        with pytest.raises(RuntimeError, match=re.escape(message)):
            main.status()
    require.assert_called_once_with("sys.observe", name="identities")


def test_missing_broker_executable_raises():
    with mock.patch.dict(os.environ, {}, clear=True), mock.patch.object(
        main.shutil, "which", return_value=None
    ), mock.patch.object(main.policy, "require") as require:
        with pytest.raises(FileNotFoundError, match="User Manager broker unavailable"):
            main.status()
    require.assert_called_once_with("sys.observe", name="identities")


@pytest.mark.parametrize(
    ("failure", "exception_type", "message"),
    [
        (
            FileNotFoundError("gone"),
            FileNotFoundError,
            "User Manager broker executable not found",
        ),
        (
            PermissionError("access denied"),
            PermissionError,
            "permission denied launching User Manager broker",
        ),
        (
            main.subprocess.TimeoutExpired(["cos"], main.TIMEOUT_SECS),
            TimeoutError,
            "User Manager broker exceeded",
        ),
    ],
)
def test_broker_execution_failures_raise(failure, exception_type, message):
    with mock.patch.dict(os.environ, {"COS_BIN": COS_BIN}), mock.patch.object(
        main.policy, "require"
    ) as require, mock.patch.object(main.subprocess, "run", side_effect=failure):
        with pytest.raises(exception_type, match=message):
            main.status()
    require.assert_called_once_with("sys.observe", name="identities")
