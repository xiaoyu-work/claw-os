import ast
import inspect
import json
import os
import pathlib
import re
from contextlib import contextmanager
from unittest import mock

import pytest

from test_support import load_local_module


APP_DIR = pathlib.Path(__file__).parent
REPO = "/media/user/backup/repo"
SOURCE = "/home/user/Documents"
DESTINATION = "/home/user/restore"
CREDENTIAL = "default/restic"
SNAPSHOT_ID = "ABCDEF12"
COS = "/usr/local/bin/cos"

main = load_local_module(
    APP_DIR / "main.py",
    "claw_test_backup_center_main",
    clear_modules=("_shared",),
)


def _completed(payload: object, returncode: int = 0) -> mock.Mock:
    return mock.Mock(returncode=returncode, stdout=json.dumps(payload), stderr="")


@contextmanager
def _paths(
    *,
    missing: frozenset[str] = frozenset(),
    symlinks: frozenset[str] = frozenset(),
    canonical: dict[str, str] | None = None,
):
    canonical = canonical or {}
    with mock.patch.object(
        main.os.path, "lexists", side_effect=lambda value: value not in missing
    ), mock.patch.object(
        main.os.path, "islink", side_effect=lambda value: value in symlinks
    ), mock.patch.object(
        main.os.path, "realpath", side_effect=lambda value: canonical.get(value, value)
    ):
        yield


def _argv(action: str, *tail: str) -> list[str]:
    return [
        COS,
        "__backup",
        action,
        "--repo",
        REPO,
        "--credential",
        CREDENTIAL,
        *tail,
    ]


def _mcp_bindings(source: str) -> dict[str, ast.FunctionDef | ast.AsyncFunctionDef]:
    bindings = {}
    for node in ast.parse(source).body:
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
    return bindings


def test_manifest_and_handlers_are_mcp_only_and_aligned():
    manifest = json.loads((APP_DIR / "app.json").read_text(encoding="utf-8"))
    assert "operations" not in manifest

    tools = {tool["name"]: tool for tool in manifest["mcp"]["tools"]}
    contracts = {
        "init": (["repo", "credential"], ["repo", "credential"]),
        "snapshots": (["repo", "credential"], ["repo", "credential"]),
        "check": (["repo", "credential"], ["repo", "credential"]),
        "backup": (
            ["repo", "source", "credential", "tag"],
            ["repo", "source", "credential"],
        ),
        "restore": (
            ["repo", "snapshot", "destination", "credential", "confirm"],
            ["repo", "destination", "credential"],
        ),
        "forget": (
            ["repo", "snapshot", "credential", "confirm"],
            ["repo", "credential"],
        ),
        "retention": (
            [
                "repo",
                "credential",
                "keep_daily",
                "keep_weekly",
                "keep_monthly",
                "confirm",
            ],
            ["repo", "credential"],
        ),
    }
    assert list(tools) == [f"backup-center.{name}" for name in contracts]
    for name, (argument_names, scope_args) in contracts.items():
        tool = tools[f"backup-center.{name}"]
        assert [argument["name"] for argument in tool.get("args", [])] == argument_names
        assert [
            (need["verb"], need["scope"]["arg"])
            for need in tool["needs"]
        ] == [
            (
                "secret.read" if scope_arg == "credential" else "data.backup",
                scope_arg,
            )
            for scope_arg in scope_args
        ]

    assert tools["backup-center.backup"]["args"][-1] == {
        "name": "tag",
        "kind": "name",
        "required": False,
    }
    for name in ("restore", "forget", "retention"):
        assert tools[f"backup-center.{name}"]["args"][-1] == {
            "name": "confirm",
            "kind": "bool",
            "required": True,
            "choices": [True],
        }

    server_source = (APP_DIR / "server.py").read_text(encoding="utf-8")
    assert "serve_manifest_operations" not in server_source
    assert server_source.count("App.from_manifest()") == 1
    bindings = _mcp_bindings(server_source)
    assert set(bindings) == set(tools)

    implementations = {
        "init": main.init_repository,
        "snapshots": main.snapshots,
        "check": main.check,
        "backup": main.backup,
        "restore": main.restore,
        "forget": main.forget,
        "retention": main.retention,
    }
    for name, (argument_names, _scope_args) in contracts.items():
        expected_defaults = {"tag": None} if name == "backup" else {}
        handler = bindings[f"backup-center.{name}"]
        assert [argument.arg for argument in handler.args.args] == argument_names
        assert all(argument.annotation is not None for argument in handler.args.args)
        defaults = handler.args.defaults
        assert (
            {
                argument.arg: ast.literal_eval(default)
                for argument, default in zip(
                    handler.args.args[-len(defaults):],
                    defaults,
                    strict=True,
                )
            }
            if defaults
            else {}
        ) == expected_defaults

        signature = inspect.signature(implementations[name])
        assert list(signature.parameters) == argument_names
        assert {
            parameter_name: parameter.default
            for parameter_name, parameter in signature.parameters.items()
            if parameter.default is not inspect.Signature.empty
        } == expected_defaults

    assert not hasattr(main, "run")
    assert "canonical_argv" not in (APP_DIR / "main.py").read_text(encoding="utf-8")


@pytest.mark.parametrize(
    ("function_name", "args", "other_scope", "argv"),
    [
        ("init_repository", (REPO, CREDENTIAL), None, _argv("init")),
        ("snapshots", (REPO, CREDENTIAL), None, _argv("snapshots")),
        ("check", (REPO, CREDENTIAL), None, _argv("check")),
        (
            "backup",
            (REPO, SOURCE, CREDENTIAL, "nightly"),
            SOURCE,
            _argv("backup", "--source", SOURCE, "--tag", "nightly"),
        ),
        (
            "restore",
            (REPO, SNAPSHOT_ID, DESTINATION, CREDENTIAL, True),
            DESTINATION,
            _argv(
                "restore",
                "--destination",
                DESTINATION,
                "--snapshot",
                SNAPSHOT_ID.lower(),
                "--confirm",
            ),
        ),
        (
            "forget",
            (REPO, SNAPSHOT_ID, CREDENTIAL, True),
            None,
            _argv("forget", "--snapshot", SNAPSHOT_ID.lower(), "--confirm"),
        ),
        (
            "retention",
            (REPO, CREDENTIAL, 365, 260, 120, True),
            None,
            _argv(
                "retention",
                "--keep-daily",
                "365",
                "--keep-weekly",
                "260",
                "--keep-monthly",
                "120",
                "--confirm",
            ),
        ),
    ],
)
def test_routes_use_exact_capability_order_and_broker_argv(
    function_name,
    args,
    other_scope,
    argv,
):
    with _paths(), mock.patch.dict(
        os.environ, {"COS_BIN": COS}
    ), mock.patch.object(main.policy, "require") as require, mock.patch.object(
        main.subprocess, "run", return_value=_completed({"ok": True})
    ) as run:
        assert getattr(main, function_name)(*args) == {"ok": True}

    expected_capabilities = [mock.call("data.backup", path=REPO)]
    if other_scope is not None:
        expected_capabilities.append(mock.call("data.backup", path=other_scope))
    expected_capabilities.append(mock.call("secret.read", name=CREDENTIAL))
    assert require.call_args_list == expected_capabilities
    assert run.call_args.args[0] == argv
    assert run.call_args.kwargs["timeout"] == main.TIMEOUT_SECS
    assert run.call_args.kwargs["stdin"] is main.subprocess.DEVNULL


def test_optional_tag_defaults_to_none_and_is_omitted():
    with _paths(), mock.patch.dict(
        os.environ, {"COS_BIN": COS}
    ), mock.patch.object(main.policy, "require"), mock.patch.object(
        main.subprocess, "run", return_value=_completed({"ok": True})
    ) as run:
        main.backup(REPO, SOURCE, CREDENTIAL)
    assert run.call_args.args[0] == _argv("backup", "--source", SOURCE)


@pytest.mark.parametrize(
    ("function_name", "args", "missing", "argv_tail"),
    [
        ("init_repository", (REPO, CREDENTIAL), frozenset({REPO}), ()),
        (
            "restore",
            (REPO, "latest", DESTINATION, CREDENTIAL, True),
            frozenset({DESTINATION}),
            ("--destination", DESTINATION, "--snapshot", "latest", "--confirm"),
        ),
    ],
)
def test_init_and_restore_allow_missing_canonical_paths(
    function_name,
    args,
    missing,
    argv_tail,
):
    with _paths(missing=missing), mock.patch.dict(
        os.environ, {"COS_BIN": COS}
    ), mock.patch.object(main.policy, "require"), mock.patch.object(
        main.subprocess, "run", return_value=_completed({"ok": True})
    ) as run:
        getattr(main, function_name)(*args)
    if argv_tail:
        assert tuple(run.call_args.args[0][-len(argv_tail):]) == argv_tail


@pytest.mark.parametrize(
    ("call", "missing", "symlinks", "canonical", "message"),
    [
        (
            lambda: main.snapshots("relative/repo", CREDENTIAL),
            frozenset(),
            frozenset(),
            {},
            "repository must be an absolute path",
        ),
        (
            lambda: main.snapshots(f"{REPO}\x00bad", CREDENTIAL),
            frozenset(),
            frozenset(),
            {},
            "without NUL bytes",
        ),
        (
            lambda: main.snapshots(REPO, CREDENTIAL),
            frozenset({REPO}),
            frozenset(),
            {},
            "repository does not exist",
        ),
        (
            lambda: main.backup(REPO, SOURCE, CREDENTIAL),
            frozenset(),
            frozenset({SOURCE}),
            {},
            "source symlinks are not allowed",
        ),
        (
            lambda: main.backup(
                REPO, "/home/user/../user/Documents", CREDENTIAL
            ),
            frozenset(),
            frozenset(),
            {"/home/user/../user/Documents": SOURCE},
            "use the canonical source path",
        ),
    ],
)
def test_invalid_paths_are_rejected_before_policy(
    call,
    missing,
    symlinks,
    canonical,
    message,
):
    with _paths(
        missing=missing, symlinks=symlinks, canonical=canonical
    ), mock.patch.object(main.policy, "require") as require:
        with pytest.raises(ValueError, match=message):
            call()
    require.assert_not_called()


@pytest.mark.parametrize(
    ("function_name", "args", "message"),
    [
        ("snapshots", (REPO, "default"), "credential must use namespace/name"),
        ("snapshots", (REPO, "default/restic\x00"), "credential must use namespace/name"),
        ("backup", (REPO, SOURCE, CREDENTIAL, "-nightly"), "invalid backup tag"),
        ("backup", (REPO, SOURCE, CREDENTIAL, "nightly\x00"), "invalid backup tag"),
        (
            "restore",
            (REPO, "LATEST", DESTINATION, CREDENTIAL, True),
            "snapshot must be latest",
        ),
        (
            "restore",
            (REPO, "not-hex", DESTINATION, CREDENTIAL, True),
            "snapshot must be latest",
        ),
        (
            "forget",
            (REPO, "latest", CREDENTIAL, True),
            "snapshot must be an exact",
        ),
        (
            "retention",
            (REPO, CREDENTIAL, True, 0, 0, True),
            "keep_daily must be an integer",
        ),
        (
            "retention",
            (REPO, CREDENTIAL, -1, 0, 0, True),
            "keep_daily must be 0..365",
        ),
        (
            "retention",
            (REPO, CREDENTIAL, 366, 0, 0, True),
            "keep_daily must be 0..365",
        ),
        (
            "retention",
            (REPO, CREDENTIAL, 0, 261, 0, True),
            "keep_weekly must be 0..260",
        ),
        (
            "retention",
            (REPO, CREDENTIAL, 0, 0, 121, True),
            "keep_monthly must be 0..120",
        ),
    ],
)
def test_invalid_values_are_rejected_before_policy(function_name, args, message):
    with _paths(), mock.patch.object(main.policy, "require") as require:
        with pytest.raises(ValueError, match=re.escape(message)):
            getattr(main, function_name)(*args)
    require.assert_not_called()


@pytest.mark.parametrize(
    ("function_name", "args"),
    [
        ("restore", (REPO, "latest", DESTINATION, CREDENTIAL, False)),
        ("restore", (REPO, "latest", DESTINATION, CREDENTIAL, 1)),
        ("restore", (REPO, "latest", DESTINATION, CREDENTIAL, "true")),
        ("forget", (REPO, SNAPSHOT_ID, CREDENTIAL, False)),
        ("forget", (REPO, SNAPSHOT_ID, CREDENTIAL, 1)),
        ("forget", (REPO, SNAPSHOT_ID, CREDENTIAL, "true")),
        ("retention", (REPO, CREDENTIAL, 1, 1, 1, False)),
        ("retention", (REPO, CREDENTIAL, 1, 1, 1, 1)),
        ("retention", (REPO, CREDENTIAL, 1, 1, 1, "true")),
    ],
)
def test_destructive_actions_require_exact_true_before_policy(function_name, args):
    with _paths(), mock.patch.object(main.policy, "require") as require:
        with pytest.raises(ValueError, match="requires confirm=true"):
            getattr(main, function_name)(*args)
    require.assert_not_called()


def _call_snapshots(
    *,
    completed: mock.Mock | None = None,
    failure: BaseException | None = None,
):
    run_options = (
        {"side_effect": failure}
        if failure is not None
        else {"return_value": completed}
    )
    with _paths(), mock.patch.dict(
        os.environ, {"COS_BIN": COS}
    ), mock.patch.object(main.policy, "require"), mock.patch.object(
        main.subprocess, "run", **run_options
    ):
        return main.snapshots(REPO, CREDENTIAL)


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
    assert _call_snapshots(completed=completed) == expected


@pytest.mark.parametrize(
    ("returncode", "stdout", "stderr", "message"),
    [
        (0, "{", "", "Backup Center broker returned invalid JSON"),
        (0, "", "{", "Backup Center broker returned invalid JSON"),
        (0, "[]", "", "Backup Center broker returned a non-object result"),
        (0, json.dumps({"error": None}), "", "invalid error payload"),
        (0, json.dumps({"error": "repository unavailable"}), "", "repository unavailable"),
        (7, "{}", "", "Backup Center broker exited 7"),
        (
            9,
            "{}",
            json.dumps({"error": "repository authorization denied"}),
            "repository authorization denied",
        ),
        (0, "", "", "Backup Center broker returned invalid JSON"),
    ],
)
def test_broker_payload_failures_raise(returncode, stdout, stderr, message):
    completed = mock.Mock(returncode=returncode, stdout=stdout, stderr=stderr)
    with pytest.raises(RuntimeError, match=re.escape(message)):
        _call_snapshots(completed=completed)


def test_missing_broker_executable_raises():
    with _paths(), mock.patch.dict(os.environ, {}, clear=True), mock.patch.object(
        main.shutil, "which", return_value=None
    ), mock.patch.object(main.policy, "require"):
        with pytest.raises(FileNotFoundError, match="Backup Center broker unavailable"):
            main.snapshots(REPO, CREDENTIAL)


@pytest.mark.parametrize(
    ("failure", "exception_type", "message"),
    [
        (
            FileNotFoundError("gone"),
            FileNotFoundError,
            "Backup Center broker executable not found",
        ),
        (
            PermissionError("access denied"),
            PermissionError,
            "permission denied launching Backup Center broker",
        ),
        (
            main.subprocess.TimeoutExpired(["cos"], main.TIMEOUT_SECS),
            TimeoutError,
            "Backup Center broker exceeded",
        ),
    ],
)
def test_broker_execution_failures_raise(failure, exception_type, message):
    with pytest.raises(exception_type, match=message):
        _call_snapshots(failure=failure)
