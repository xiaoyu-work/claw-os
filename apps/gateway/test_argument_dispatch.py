"""List-based gateway dispatch must match each app manifest."""

from __future__ import annotations

from unittest import mock

import pytest

from test_support import load_local_module


def _load(name):
    path = __file__.replace("test_argument_dispatch.py", f"{name}/main.py")
    return load_local_module(
        path,
        f"claw_test_gateway_{name.replace('-', '_')}",
        clear_modules=("_shared",),
    )


@pytest.mark.parametrize(
    ("name", "argv", "expected_args", "expected_kwargs"),
    [
        (
            "webhook",
            [
                "hello",
                "--target",
                "https://example.test/hook",
                "--raw",
                "--bearer",
                "token",
                "--basic",
                "user:pass",
                "--api-key",
                "key",
                "--hmac-sha256",
                "secret",
            ],
            (
                "https://example.test/hook",
                "hello",
                True,
                "token",
                "user:pass",
                "key",
                "secret",
            ),
            {},
        ),
    ],
)
def test_list_dispatch_forwards_manifest_options(
    name, argv, expected_args, expected_kwargs
):
    module = _load(name)
    with mock.patch.object(
        module, "_send", return_value={"ok": True}
    ) as send, mock.patch.object(module.gateway_memory, "remember_send"):
        result = module.run("send", argv)
    assert result == {"ok": True}
    assert send.call_args.args == expected_args
    assert send.call_args.kwargs == expected_kwargs


@pytest.mark.parametrize(
    ("name", "argv"),
    [
        ("webhook", ["https://example.test/hook", "hello"]),
    ],
)
def test_removed_leading_positionals_are_rejected(name, argv):
    module = _load(name)
    with mock.patch.object(
        module, "_send", return_value={"ok": True}
    ) as send, mock.patch.object(module.gateway_memory, "remember_send"):
        result = module.run("send", argv)
    assert result["ok"] is False
    assert "too many positional arguments" in result["error"]
    send.assert_not_called()


@pytest.mark.parametrize(
    "name",
    [
        "webhook",
    ],
)
def test_one_positional_is_always_message_text(name):
    module = _load(name)
    with mock.patch.object(
        module, "_send", return_value={"ok": True}
    ) as send, mock.patch.object(module.gateway_memory, "remember_send"):
        module.run("send", ["hello"])

    assert send.call_args.args[:2] == ("", "hello")
