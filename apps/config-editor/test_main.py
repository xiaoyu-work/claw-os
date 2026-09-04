import json
import os
import pathlib
from contextlib import contextmanager
from unittest import mock

import pytest

from test_support import load_local_module


main = load_local_module(
    pathlib.Path(__file__).with_name("main.py"),
    "claw_test_config_editor_main",
    clear_modules=("_shared",),
)


TARGET = "/etc/hosts"
SOURCE = "/home/user/hosts.new"
BACKUP_TOKEN = "ABCDEF0123456789ABCDEF0123456789"


def _completed(payload: object, returncode: int = 0) -> mock.Mock:
    return mock.Mock(
        returncode=returncode,
        stdout=json.dumps(payload),
        stderr="",
    )


@contextmanager
def _canonical_paths():
    with mock.patch.object(
        main.os.path, "lexists", return_value=True
    ), mock.patch.object(main.os.path, "islink", return_value=False), mock.patch.object(
        main.os.path, "realpath", side_effect=lambda value: value
    ):
        yield


def test_inspect_uses_exact_scope_and_argv():
    with _canonical_paths(), mock.patch.dict(
        os.environ, {"COS_BIN": "/usr/local/bin/cos"}
    ), mock.patch.object(main.policy, "require") as require, mock.patch.object(
        main.subprocess, "run", return_value=_completed({"content": "hosts"})
    ) as run:
        result = main.inspect(TARGET)
    require.assert_called_once_with("sys.config", path=TARGET)
    assert run.call_args.args[0] == [
        "/usr/local/bin/cos",
        "__config",
        "inspect",
        "--target",
        TARGET,
    ]
    assert run.call_args.kwargs["timeout"] == main.TIMEOUT_SECS
    assert run.call_args.kwargs["stdin"] is main.subprocess.DEVNULL
    assert result == {"content": "hosts"}


def test_validate_uses_exact_scopes_and_argv():
    with _canonical_paths(), mock.patch.dict(
        os.environ, {"COS_BIN": "/usr/local/bin/cos"}
    ), mock.patch.object(main.policy, "require") as require, mock.patch.object(
        main.subprocess, "run", return_value=_completed({"valid": True})
    ) as run:
        result = main.validate(TARGET, SOURCE)
    assert require.call_args_list == [
        mock.call("sys.config", path=TARGET),
        mock.call("fs.read", path=SOURCE),
    ]
    assert run.call_args.args[0] == [
        "/usr/local/bin/cos",
        "__config",
        "validate",
        "--target",
        TARGET,
        "--source",
        SOURCE,
    ]
    assert result == {"valid": True}


def test_apply_uses_exact_scopes_and_argv():
    with _canonical_paths(), mock.patch.dict(
        os.environ, {"COS_BIN": "/usr/local/bin/cos"}
    ), mock.patch.object(main.policy, "require") as require, mock.patch.object(
        main.subprocess, "run", return_value=_completed({"applied": True})
    ) as run:
        result = main.apply(TARGET, SOURCE, True)
    assert require.call_args_list == [
        mock.call("sys.config", path=TARGET),
        mock.call("fs.read", path=SOURCE),
    ]
    assert run.call_args.args[0] == [
        "/usr/local/bin/cos",
        "__config",
        "apply",
        "--target",
        TARGET,
        "--source",
        SOURCE,
        "--confirm",
    ]
    assert result == {"applied": True}


def test_restore_uses_exact_scope_and_normalized_token_argv():
    with _canonical_paths(), mock.patch.dict(
        os.environ, {"COS_BIN": "/usr/local/bin/cos"}
    ), mock.patch.object(main.policy, "require") as require, mock.patch.object(
        main.subprocess, "run", return_value=_completed({"restored": True})
    ) as run:
        result = main.restore(TARGET, BACKUP_TOKEN, True)
    require.assert_called_once_with("sys.config", path=TARGET)
    assert run.call_args.args[0] == [
        "/usr/local/bin/cos",
        "__config",
        "restore",
        "--target",
        TARGET,
        "--token",
        BACKUP_TOKEN.lower(),
        "--confirm",
    ]
    assert result == {"restored": True}


@pytest.mark.parametrize(
    "target",
    [None, 7, "", "etc/hosts", "/var/lib/config", "/etc/bad\x00path"],
)
def test_invalid_target_is_rejected_before_policy(target):
    with _canonical_paths(), mock.patch.object(main.policy, "require") as require:
        with pytest.raises(ValueError, match="target must"):
            main.inspect(target)
    require.assert_not_called()


def test_noncanonical_target_is_rejected_before_policy():
    with mock.patch.object(
        main.os.path, "lexists", return_value=False
    ), mock.patch.object(
        main.os.path,
        "realpath",
        side_effect=lambda value: main.os.path.normpath(value),
    ), mock.patch.object(main.policy, "require") as require:
        with pytest.raises(ValueError, match="use the canonical target path"):
            main.inspect("/etc/../etc/hosts")
    require.assert_not_called()


def test_target_symlink_is_rejected_before_policy():
    with mock.patch.object(
        main.os.path, "lexists", return_value=True
    ), mock.patch.object(main.os.path, "islink", return_value=True), mock.patch.object(
        main.policy, "require"
    ) as require:
        with pytest.raises(ValueError, match="target symlinks are not allowed"):
            main.inspect("/etc/resolv.conf")
    require.assert_not_called()


@pytest.mark.parametrize(
    "source",
    [None, 7, "", "relative.conf", "/home/user/bad\x00path"],
)
def test_invalid_source_is_rejected_before_policy(source):
    with _canonical_paths(), mock.patch.object(main.policy, "require") as require:
        with pytest.raises(ValueError, match="source must be an absolute file path"):
            main.validate(TARGET, source)
    require.assert_not_called()


def test_noncanonical_source_is_rejected_before_policy():
    with mock.patch.object(
        main.os.path, "lexists", return_value=True
    ), mock.patch.object(main.os.path, "islink", return_value=False), mock.patch.object(
        main.os.path,
        "realpath",
        side_effect=lambda value: main.os.path.normpath(value),
    ), mock.patch.object(main.policy, "require") as require:
        with pytest.raises(ValueError, match="use the canonical source path"):
            main.validate(TARGET, "/home/user/../user/hosts.new")
    require.assert_not_called()


def test_source_symlink_is_rejected_before_policy():
    with mock.patch.object(
        main.os.path, "lexists", return_value=True
    ), mock.patch.object(
        main.os.path, "islink", side_effect=lambda path: path == SOURCE
    ), mock.patch.object(
        main.os.path, "realpath", side_effect=lambda value: value
    ), mock.patch.object(main.policy, "require") as require:
        with pytest.raises(ValueError, match="source symlinks are not allowed"):
            main.validate(TARGET, SOURCE)
    require.assert_not_called()


@pytest.mark.parametrize(
    "backup_token",
    [None, 7, "", "a" * 31, "a" * 33, "g" * 32],
)
def test_invalid_backup_token_is_rejected_before_policy(backup_token):
    with _canonical_paths(), mock.patch.object(main.policy, "require") as require:
        with pytest.raises(ValueError, match="backup_token must"):
            main.restore(TARGET, backup_token, True)
    require.assert_not_called()


@pytest.mark.parametrize(
    ("action", "confirm"),
    [
        ("apply", False),
        ("apply", 1),
        ("apply", "true"),
        ("restore", False),
        ("restore", 1),
        ("restore", "true"),
    ],
)
def test_confirmation_requires_real_true_before_policy(action, confirm):
    with _canonical_paths(), mock.patch.object(main.policy, "require") as require:
        with pytest.raises(ValueError, match=rf"{action} requires confirm=true"):
            if action == "apply":
                main.apply(TARGET, SOURCE, confirm)
            else:
                main.restore(TARGET, BACKUP_TOKEN, confirm)
    require.assert_not_called()


@pytest.mark.parametrize(
    ("returncode", "stdout", "stderr", "message"),
    [
        (
            9,
            "",
            json.dumps({"error": "specific stderr failure"}),
            "specific stderr failure",
        ),
        (0, "{", "", "Safe Config Editor broker returned invalid JSON"),
        (0, "[]", "", "Safe Config Editor broker returned a non-object result"),
        (
            0,
            json.dumps({"error": None}),
            "",
            "Safe Config Editor broker returned an invalid error payload",
        ),
        (
            0,
            json.dumps({"error": "validation failed"}),
            "",
            "validation failed",
        ),
        (7, "{}", "", "Safe Config Editor broker exited 7"),
        (
            9,
            json.dumps({"error": "specific broker failure"}),
            "",
            "specific broker failure",
        ),
    ],
)
def test_broker_payload_failures_raise(returncode, stdout, stderr, message):
    completed = mock.Mock(returncode=returncode, stdout=stdout, stderr=stderr)
    with _canonical_paths(), mock.patch.dict(
        os.environ, {"COS_BIN": "/usr/local/bin/cos"}
    ), mock.patch.object(main.policy, "require"), mock.patch.object(
        main.subprocess, "run", return_value=completed
    ):
        with pytest.raises(RuntimeError, match=message):
            main.inspect(TARGET)


def test_missing_broker_executable_raises():
    with _canonical_paths(), mock.patch.dict(
        os.environ, {}, clear=True
    ), mock.patch.object(main.shutil, "which", return_value=None), mock.patch.object(
        main.policy, "require"
    ):
        with pytest.raises(FileNotFoundError, match="cos binary not found"):
            main.inspect(TARGET)


@pytest.mark.parametrize(
    ("error", "exception", "message"),
    [
        (
            FileNotFoundError("missing"),
            FileNotFoundError,
            "Safe Config Editor broker executable not found",
        ),
        (
            PermissionError("denied"),
            PermissionError,
            "permission denied launching Safe Config Editor broker",
        ),
    ],
)
def test_broker_launch_failures_raise(error, exception, message):
    with _canonical_paths(), mock.patch.dict(
        os.environ, {"COS_BIN": "/usr/local/bin/cos"}
    ), mock.patch.object(main.policy, "require"), mock.patch.object(
        main.subprocess, "run", side_effect=error
    ):
        with pytest.raises(exception, match=message):
            main.inspect(TARGET)


def test_broker_timeout_raises():
    with _canonical_paths(), mock.patch.dict(
        os.environ, {"COS_BIN": "/usr/local/bin/cos"}
    ), mock.patch.object(main.policy, "require"), mock.patch.object(
        main.subprocess,
        "run",
        side_effect=main.subprocess.TimeoutExpired(["cos"], main.TIMEOUT_SECS),
    ):
        with pytest.raises(
            TimeoutError,
            match=rf"Safe Config Editor broker exceeded {main.TIMEOUT_SECS}s for inspect",
        ):
            main.inspect(TARGET)
