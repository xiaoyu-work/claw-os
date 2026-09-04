import json
import os
import pathlib
from unittest import mock

import pytest

from test_support import load_local_module


main = load_local_module(
    pathlib.Path(__file__).with_name("main.py"),
    "claw_test_clipboard_manager_main",
    clear_modules=("_shared",),
)


def _completed(payload: object, returncode: int = 0) -> mock.Mock:
    return mock.Mock(
        returncode=returncode,
        stdout=json.dumps(payload),
        stderr="",
    )


def test_status_uses_sensitive_clipboard_scope_and_selection():
    with mock.patch.dict(os.environ, {"COS_BIN": "/usr/local/bin/cos"}), mock.patch.object(
        main.policy, "require"
    ) as require, mock.patch.object(
        main.subprocess, "run", return_value=_completed({"types": []})
    ) as run:
        result = main.status(primary=True)
    require.assert_called_once_with("clipboard.read", name="selection")
    assert run.call_args.args[0] == [
        "/usr/local/bin/cos",
        "__clipboard",
        "status",
        "--primary",
    ]
    assert result == {"types": []}


def test_types_uses_sensitive_clipboard_scope():
    with mock.patch.dict(os.environ, {"COS_BIN": "/usr/local/bin/cos"}), mock.patch.object(
        main.policy, "require"
    ) as require, mock.patch.object(
        main.subprocess, "run", return_value=_completed({"types": ["text/plain"]})
    ) as run:
        result = main.list_types()
    require.assert_called_once_with("clipboard.read", name="selection")
    assert run.call_args.args[0] == ["/usr/local/bin/cos", "__clipboard", "types"]
    assert result == {"types": ["text/plain"]}


def test_read_uses_sensitive_clipboard_scope_and_mime():
    with mock.patch.dict(os.environ, {"COS_BIN": "/usr/local/bin/cos"}), mock.patch.object(
        main.policy, "require"
    ) as require, mock.patch.object(
        main.subprocess, "run", return_value=_completed({"text": "hello"})
    ) as run:
        result = main.read("text/plain")
    require.assert_called_once_with("clipboard.read", name="selection")
    assert run.call_args.args[0] == [
        "/usr/local/bin/cos",
        "__clipboard",
        "read",
        "--mime",
        "text/plain",
    ]
    assert result == {"text": "hello"}


def test_write_uses_clipboard_and_source_scopes():
    with mock.patch.dict(os.environ, {"COS_BIN": "/usr/local/bin/cos"}), mock.patch.object(
        main.os.path, "realpath", side_effect=lambda value: value
    ), mock.patch.object(main.os.path, "islink", return_value=False), mock.patch.object(
        main.policy, "require"
    ) as require, mock.patch.object(
        main.subprocess, "run", return_value=_completed({"written": True})
    ) as run:
        result = main.write("/home/user/clip.txt", "text/plain", primary=True)
    assert require.call_args_list == [
        mock.call("clipboard.write", name="selection"),
        mock.call("fs.read", path="/home/user/clip.txt"),
    ]
    assert run.call_args.args[0] == [
        "/usr/local/bin/cos",
        "__clipboard",
        "write",
        "--mime",
        "text/plain",
        "--source",
        "/home/user/clip.txt",
        "--primary",
    ]
    assert run.call_args.kwargs["timeout"] == main.TIMEOUT_SECS
    assert run.call_args.kwargs["stdin"] is main.subprocess.DEVNULL
    assert result == {"written": True}


def test_clear_requires_confirmation_before_policy():
    with mock.patch.object(main.policy, "require") as require:
        with pytest.raises(ValueError, match="requires confirmation"):
            main.clear(False)
    require.assert_not_called()


def test_clear_uses_clipboard_scope_and_confirm_flag():
    with mock.patch.dict(os.environ, {"COS_BIN": "/usr/local/bin/cos"}), mock.patch.object(
        main.policy, "require"
    ) as require, mock.patch.object(
        main.subprocess, "run", return_value=_completed({"cleared": True})
    ) as run:
        result = main.clear(True, primary=True)
    require.assert_called_once_with("clipboard.write", name="selection")
    assert run.call_args.args[0] == [
        "/usr/local/bin/cos",
        "__clipboard",
        "clear",
        "--primary",
        "--confirm",
    ]
    assert result == {"cleared": True}


@pytest.mark.parametrize("primary", [None, 0, 1, "true"])
def test_invalid_selection_is_rejected_before_policy(primary):
    with mock.patch.object(main.policy, "require") as require:
        with pytest.raises(ValueError, match="primary must be a boolean"):
            main.status(primary)
    require.assert_not_called()


@pytest.mark.parametrize(
    "mime",
    [42, "", "text", "-text/plain", "text/plain value", "a/" + "b" * 254],
)
def test_invalid_mime_is_rejected_before_policy(mime):
    with mock.patch.object(main.policy, "require") as require:
        with pytest.raises(ValueError, match="invalid MIME type"):
            main.read(mime)
    require.assert_not_called()


@pytest.mark.parametrize(
    "source",
    [None, "", "relative.txt", "/home/user/../user/clip.txt", "bad\x00path"],
)
def test_invalid_source_is_rejected_before_policy(source):
    with mock.patch.object(
        main.os.path, "realpath", side_effect=lambda value: main.os.path.normpath(value)
    ), mock.patch.object(main.os.path, "islink", return_value=False), mock.patch.object(
        main.policy, "require"
    ) as require:
        with pytest.raises(ValueError, match="canonical non-symlink path"):
            main.write(source)
    require.assert_not_called()


def test_symlink_source_is_rejected_before_policy():
    with mock.patch.object(
        main.os.path, "realpath", side_effect=lambda value: value
    ), mock.patch.object(main.os.path, "islink", return_value=True), mock.patch.object(
        main.policy, "require"
    ) as require:
        with pytest.raises(ValueError, match="canonical non-symlink path"):
            main.write("/home/user/clip.txt")
    require.assert_not_called()


@pytest.mark.parametrize(
    ("returncode", "stdout", "message"),
    [
        (0, "{", "Clipboard Manager broker returned invalid JSON"),
        (0, "[]", "Clipboard Manager broker returned a non-object result"),
        (0, json.dumps({"error": ""}), "invalid error payload"),
        (0, json.dumps({"error": "clipboard unavailable"}), "clipboard unavailable"),
        (7, "{}", "Clipboard Manager broker exited 7"),
        (9, json.dumps({"error": "clipboard denied"}), "clipboard denied"),
    ],
)
def test_broker_payload_failures_raise(returncode, stdout, message):
    completed = mock.Mock(returncode=returncode, stdout=stdout, stderr="")
    with mock.patch.dict(os.environ, {"COS_BIN": "/usr/local/bin/cos"}), mock.patch.object(
        main.policy, "require"
    ) as require, mock.patch.object(main.subprocess, "run", return_value=completed):
        with pytest.raises(RuntimeError, match=message):
            main.status()
    require.assert_called_once_with("clipboard.read", name="selection")


def test_missing_broker_executable_raises():
    with mock.patch.dict(os.environ, {}, clear=True), mock.patch.object(
        main.shutil, "which", return_value=None
    ), mock.patch.object(main.policy, "require") as require:
        with pytest.raises(FileNotFoundError, match="cos binary not found"):
            main.status()
    require.assert_called_once_with("clipboard.read", name="selection")


@pytest.mark.parametrize(
    ("error", "exception", "message"),
    [
        (
            FileNotFoundError("missing"),
            FileNotFoundError,
            "Clipboard Manager broker executable not found",
        ),
        (
            PermissionError("denied"),
            PermissionError,
            "permission denied launching Clipboard Manager broker",
        ),
    ],
)
def test_broker_launch_failures_raise(error, exception, message):
    with mock.patch.dict(os.environ, {"COS_BIN": "/usr/local/bin/cos"}), mock.patch.object(
        main.policy, "require"
    ) as require, mock.patch.object(main.subprocess, "run", side_effect=error):
        with pytest.raises(exception, match=message):
            main.status()
    require.assert_called_once_with("clipboard.read", name="selection")


def test_broker_timeout_raises():
    with mock.patch.dict(os.environ, {"COS_BIN": "/usr/local/bin/cos"}), mock.patch.object(
        main.policy, "require"
    ) as require, mock.patch.object(
        main.subprocess,
        "run",
        side_effect=main.subprocess.TimeoutExpired(["cos"], main.TIMEOUT_SECS),
    ):
        with pytest.raises(TimeoutError, match="Clipboard Manager broker exceeded"):
            main.status()
    require.assert_called_once_with("clipboard.read", name="selection")
