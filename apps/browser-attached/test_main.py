from pathlib import Path

from test_support import load_local_module


browser = load_local_module(
    Path(__file__).with_name("main.py"),
    "browser_attached_app",
)


def test_delimited_schema_token_reaches_handler_as_data():
    original = browser.HANDLERS["tabs.activate"]
    browser.HANDLERS["tabs.activate"] = lambda args: {"args": args}
    try:
        result = browser.run("tabs.activate", ["--", "--schema"])
    finally:
        browser.HANDLERS["tabs.activate"] = original
    assert result["args"] == ["--schema"]
