from __future__ import annotations

import json
import pathlib
from collections.abc import Callable
from unittest import mock

import pytest

from test_support import load_local_module


main = load_local_module(
    pathlib.Path(__file__).with_name("main.py"),
    "claw_test_notify_main",
    clear_modules=("_shared",),
)


@pytest.fixture
def notifications_file(
    tmp_path: pathlib.Path, monkeypatch: pytest.MonkeyPatch
) -> pathlib.Path:
    path = tmp_path / "notifications.json"
    monkeypatch.setattr(main, "NOTIFICATIONS_FILE", str(path))
    return path


def test_send_checks_policy_then_saves_atomically_under_lock(
    notifications_file: pathlib.Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    events: list[object] = []
    payloads: list[bytes] = []
    real_flock = main.fcntl.flock

    def require(*args: object, **kwargs: object) -> None:
        events.append(("policy", args, kwargs))

    def flock(lock_fd: object, operation: int) -> None:
        if operation == main.fcntl.LOCK_EX:
            events.append("lock")
        elif operation == main.fcntl.LOCK_UN:
            events.append("unlock")
        real_flock(lock_fd, operation)

    def atomic_write(path: str, payload: bytes) -> None:
        assert path == str(notifications_file)
        events.append("atomic")
        payloads.append(payload)

    require_mock = mock.Mock(side_effect=require)
    monkeypatch.setattr(main.policy, "require", require_mock)
    monkeypatch.setattr(main.fcntl, "flock", flock)
    monkeypatch.setattr(main, "atomic_write_bytes", atomic_write)

    result = main.send("Deployment complete", urgent=True)

    assert events == [
        ("policy", ("ui.notify",), {"wild": True}),
        "lock",
        "atomic",
        "unlock",
    ]
    require_mock.assert_called_once_with("ui.notify", wild=True)
    assert json.loads(payloads[0]) == [{**result, "read": False}]


def test_persisted_shape_and_newest_first_limit(
    notifications_file: pathlib.Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    require = mock.Mock()
    monkeypatch.setattr(main.policy, "require", require)

    first = main.send("First")
    second = main.send("Second", urgent=True)
    listed = main.list_notifications(limit=1)

    persisted = json.loads(notifications_file.read_text(encoding="utf-8"))
    assert persisted == [
        {**first, "read": False},
        {**second, "read": False},
    ]
    assert set(persisted[0]) == {
        "id",
        "message",
        "urgent",
        "timestamp",
        "read",
    }
    assert listed == {"notifications": [persisted[1]], "total": 2}
    assert require.call_args_list == [
        mock.call("ui.notify", wild=True),
        mock.call("ui.notify", wild=True),
        mock.call("data.inbox.read", wild=True),
    ]


@pytest.mark.parametrize(
    ("invoke", "message"),
    [
        (lambda: main.send(None), "message must be a non-empty string"),
        (lambda: main.send(" \n\t"), "message must be a non-empty string"),
        (lambda: main.send("hello", urgent=1), "urgent must be a boolean"),
        (lambda: main.list_notifications(True), "limit must be an integer"),
        (lambda: main.list_notifications("20"), "limit must be an integer"),
        (lambda: main.list_notifications(0), "limit must be 1..100"),
        (lambda: main.list_notifications(101), "limit must be 1..100"),
    ],
)
def test_validation_happens_before_policy(
    monkeypatch: pytest.MonkeyPatch,
    invoke: Callable[[], object],
    message: str,
) -> None:
    require = mock.Mock()
    with_lock = mock.Mock()
    monkeypatch.setattr(main.policy, "require", require)
    monkeypatch.setattr(main, "_with_lock", with_lock)

    with pytest.raises(ValueError, match=message):
        invoke()

    require.assert_not_called()
    with_lock.assert_not_called()


def test_missing_store_is_an_empty_list_after_policy(
    notifications_file: pathlib.Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    events: list[object] = []
    real_flock = main.fcntl.flock

    def require(*args: object, **kwargs: object) -> None:
        events.append(("policy", args, kwargs))

    def flock(lock_fd: object, operation: int) -> None:
        if operation == main.fcntl.LOCK_EX:
            events.append("lock")
        elif operation == main.fcntl.LOCK_UN:
            events.append("unlock")
        real_flock(lock_fd, operation)

    require_mock = mock.Mock(side_effect=require)
    monkeypatch.setattr(main.policy, "require", require_mock)
    monkeypatch.setattr(main.fcntl, "flock", flock)

    assert main.list_notifications() == {"notifications": [], "total": 0}
    assert events == [
        ("policy", ("data.inbox.read",), {"wild": True}),
        "lock",
        "unlock",
    ]
    require_mock.assert_called_once_with("data.inbox.read", wild=True)
    assert not notifications_file.exists()


def test_corrupt_store_fails_closed(
    notifications_file: pathlib.Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    notifications_file.write_text("{", encoding="utf-8")
    monkeypatch.setattr(main.policy, "require", mock.Mock())

    with pytest.raises(json.JSONDecodeError):
        main.list_notifications()


@pytest.mark.parametrize(
    ("contents", "message"),
    [
        ("{}", "notifications store must contain a JSON list"),
        ("[1]", "notifications store entry 0 must be a JSON object"),
        (
            '[{"message": "valid"}, null]',
            "notifications store entry 1 must be a JSON object",
        ),
    ],
)
def test_invalid_store_shape_fails_closed(
    notifications_file: pathlib.Path,
    monkeypatch: pytest.MonkeyPatch,
    contents: str,
    message: str,
) -> None:
    notifications_file.write_text(contents, encoding="utf-8")
    monkeypatch.setattr(main.policy, "require", mock.Mock())

    with pytest.raises(ValueError, match=message):
        main.list_notifications()


@pytest.mark.parametrize(
    ("invoke", "error"),
    [
        (
            lambda: main.send("hello"),
            main.policy.PermissionDenied({"summary": "notification denied"}),
        ),
        (
            lambda: main.list_notifications(),
            main.policy.PolicyUnavailable("policy unavailable"),
        ),
    ],
)
def test_policy_errors_propagate_before_storage(
    monkeypatch: pytest.MonkeyPatch,
    invoke: Callable[[], object],
    error: Exception,
) -> None:
    with_lock = mock.Mock()
    monkeypatch.setattr(main.policy, "require", mock.Mock(side_effect=error))
    monkeypatch.setattr(main, "_with_lock", with_lock)

    with pytest.raises(type(error)) as raised:
        invoke()

    assert raised.value is error
    with_lock.assert_not_called()
