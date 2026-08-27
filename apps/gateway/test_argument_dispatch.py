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
            "dingtalk",
            [
                "hello",
                "--markdown=false",
                "--title=--urgent",
                "--keyword",
                "Key",
                "--at-mobiles",
                "1,2",
                "--at-user-ids",
                "u1,u2",
                "--at-all",
            ],
            ("hello",),
            {
                "markdown": False,
                "title": "--urgent",
                "keyword": "Key",
                "at_mobiles": ["1", "2"],
                "at_user_ids": ["u1", "u2"],
                "at_all": True,
            },
        ),
        (
            "googlechat",
            ["hello", "--recipient", "space", "--title", "Title", "--thread-key", "thread"],
            ("space", "hello", "Title", "thread"),
            {},
        ),
        (
            "larksuite",
            ["hello", "--post", "--title", "Title", "--card", "--card-json", "{}"],
            ("hello",),
            {"post": True, "title": "Title", "card": True, "card_json": "{}"},
        ),
        (
            "mattermost",
            ["hello", "--recipient", "town-square", "--username", "bot", "--icon-url", "https://x"],
            ("town-square", "hello", "bot", "https://x"),
            {},
        ),
        (
            "pushover",
            [
                "hello",
                "--recipient",
                "user",
                "--title",
                "Title",
                "--priority",
                "2",
                "--sound",
                "magic",
                "--url",
                "https://x",
                "--url-title",
                "Open",
                "--device",
                "phone",
                "--html",
                "--ttl",
                "60",
                "--retry",
                "30",
                "--expire",
                "120",
            ],
            ("hello",),
            {
                "recipient": "user",
                "title": "Title",
                "priority": 2,
                "sound": "magic",
                "url": "https://x",
                "url_title": "Open",
                "device": "phone",
                "html": True,
                "ttl": 60,
                "retry": 30,
                "expire": 120,
            },
        ),
        (
            "teams",
            ["hello", "--recipient", "channel", "--title", "Title", "--legacy"],
            ("channel", "hello", "Title", True),
            {},
        ),
        (
            "webex",
            ["person@example.com", "hello", "--plain"],
            ("person@example.com", "hello", True),
            {},
        ),
        (
            "ntfy",
            ["hello", "--topic", "alerts", "--title", "Title", "--markdown"],
            ("alerts", "hello"),
            {
                "title": "Title",
                "priority": None,
                "tags": None,
                "click": None,
                "markdown": True,
                "server": None,
                "bearer": None,
                "basic": None,
            },
        ),
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
    ("name", "argv", "expected_args"),
    [
        ("googlechat", ["hello"], ("", "hello", "", "")),
        ("googlechat", ["space", "hello"], ("space", "hello", "", "")),
        ("mattermost", ["hello"], ("", "hello", "", "")),
        ("mattermost", ["town-square", "hello"], ("town-square", "hello", "", "")),
        ("teams", ["hello"], ("", "hello", "", False)),
        ("teams", ["channel", "hello"], ("channel", "hello", "", False)),
        ("ntfy", ["hello"], (None, "hello")),
        ("ntfy", ["alerts", "hello"], ("alerts", "hello")),
        (
            "webhook",
            ["hello"],
            ("", "hello", False, None, None, None, None),
        ),
        (
            "webhook",
            ["https://example.test/hook", "hello"],
            ("https://example.test/hook", "hello", False, None, None, None, None),
        ),
    ],
)
def test_list_dispatch_preserves_historical_positional_forms(
    name, argv, expected_args
):
    module = _load(name)
    with mock.patch.object(
        module, "_send", return_value={"ok": True}
    ) as send, mock.patch.object(module.gateway_memory, "remember_send"):
        result = module.run("send", argv)
    assert result == {"ok": True}
    assert send.call_args.args == expected_args


@pytest.mark.parametrize(
    "name",
    [
        "googlechat",
        "mattermost",
        "ntfy",
        "teams",
        "webhook",
    ],
)
def test_one_positional_is_always_message_text(name):
    module = _load(name)
    with mock.patch.object(
        module, "_send", return_value={"ok": True}
    ) as send, mock.patch.object(module.gateway_memory, "remember_send"):
        module.run("send", ["hello"])

    if name == "ntfy":
        assert send.call_args.args[:2] == (None, "hello")
    elif name == "webhook":
        assert send.call_args.args[:2] == ("", "hello")
    else:
        assert send.call_args.args[1] == "hello"
        assert send.call_args.args[0] == ""


def test_end_of_options_preserves_flag_shaped_message_text():
    module = _load("googlechat")
    with mock.patch.object(
        module, "_send", return_value={"ok": True}
    ) as send, mock.patch.object(module.gateway_memory, "remember_send"):
        module.run("send", ["--", "--literal"])
    assert send.call_args.args[:2] == ("", "--literal")


def test_ntfy_materialized_server_is_shared_by_send_and_status():
    module = _load("ntfy")
    with mock.patch.object(
        module, "_send", return_value={"ok": True}
    ) as send, mock.patch.object(module.gateway_memory, "remember_send"):
        module.run("send", ["hello", "--server=https://notify.example:8443"])
    assert send.call_args.kwargs["server"] == "https://notify.example:8443"

    status = module.run("status", ["--server=https://notify.example:8443"])
    assert status["server"] == "https://notify.example:8443"


def test_ntfy_fallback_host_never_receives_stored_token():
    module = _load("ntfy")
    with mock.patch.object(
        module, "_load_credential", return_value=("private-token", None)
    ), mock.patch.object(
        module.safe_egress,
        "safe_urlopen",
        return_value=(200, {}, b"{}"),
    ) as request:
        result = module._send("alerts", "hello", server="https://ntfy.sh")
    assert result["ok"]
    assert "Authorization" not in request.call_args.kwargs["headers"]
