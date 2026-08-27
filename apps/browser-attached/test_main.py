from pathlib import Path
from unittest import mock

from test_support import load_local_module


browser = load_local_module(
    Path(__file__).with_name("main.py"),
    "browser_attached_app",
)


def test_delimited_schema_token_reaches_handler_as_data():
    calls = []

    def request(method, payload):
        calls.append((method, payload))
        if method == "tabs.info":
            return {"result": {"host": "example.test"}}
        return {"result": payload}

    with (
        mock.patch.object(browser, "_send_request", side_effect=request),
        mock.patch.object(browser, "_require_host"),
    ):
        result = browser.run("eval", ["7", "--", "--schema"])

    assert result["ok"] is True
    assert calls[-1] == ("eval", {"id": 7, "expr": "--schema"})
