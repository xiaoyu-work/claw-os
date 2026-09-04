"""Tests for the explicit-provider search App."""

import ast
import json
import pathlib
import urllib.error
import urllib.parse
from unittest import mock

import pytest

from test_support import load_local_module


APP_DIR = pathlib.Path(__file__).parent
MANIFEST_PATH = APP_DIR / "app.json"
SERVER_PATH = APP_DIR / "server.py"
TOOL_NAMES = ["search.web", "search.image"]

main = load_local_module(
    APP_DIR / "main.py",
    "claw_test_search_main",
    clear_modules=("_shared",),
)


class _Response:
    def __init__(self, payload: object):
        self.body = json.dumps(payload).encode("utf-8")
        self.read_limit = None

    def __enter__(self):
        return self

    def __exit__(self, *_args):
        return False

    def read(self, limit: int) -> bytes:
        self.read_limit = limit
        return self.body


def _server_bindings() -> dict[str, ast.FunctionDef]:
    bindings: dict[str, ast.FunctionDef] = {}
    for node in ast.parse(SERVER_PATH.read_text(encoding="utf-8")).body:
        if not isinstance(node, ast.FunctionDef):
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
                and isinstance(decorator.args[0].value, str)
            ):
                bindings[decorator.args[0].value] = node
    return bindings


def _credential_values(name: str) -> tuple[str, None]:
    return {
        "GOOGLE_SEARCH_API_KEY": "google-key",
        "GOOGLE_SEARCH_ENGINE_ID": "google-cx",
        "BRAVE_SEARCH_API_KEY": "brave-key",
    }[name], None


def _google_web_payload() -> dict[str, object]:
    return {
        "searchInformation": {"totalResults": "12345"},
        "items": [
            {
                "title": "Example Result",
                "link": "https://example.com",
                "snippet": "An example snippet.",
            }
        ],
    }


def _brave_image_payload() -> dict[str, object]:
    return {
        "results": [
            {
                "title": "Brave Image",
                "url": "https://example.com/image.jpg",
                "thumbnail": {"src": "https://example.com/thumb.jpg"},
                "properties": {"width": 1280, "height": 720},
                "source": "example.com",
            }
        ]
    }


def test_manifest_and_handlers_are_mcp_only_and_aligned():
    manifest = json.loads(MANIFEST_PATH.read_text(encoding="utf-8"))
    assert "operations" not in manifest

    tools = manifest["mcp"]["tools"]
    assert [tool["name"] for tool in tools] == TOOL_NAMES
    expected_args = [
        {
            "name": "provider",
            "kind": "name",
            "required": True,
            "binding": "flag",
            "choices": ["google", "brave"],
        },
        {
            "name": "query",
            "kind": "text",
            "required": True,
            "binding": "positional",
        },
        {
            "name": "max_results",
            "kind": "integer",
            "required": False,
            "binding": "flag",
            "default": 5,
        },
    ]
    assert tools[0]["args"] == expected_args
    assert tools[1]["args"] == expected_args
    for tool in tools:
        needs = tool["needs"]
        assert needs[0]["scope"]["kind"] == "from-arg-map"
        assert needs[0]["scope"]["arg"] == "provider"
        assert needs[0]["scope"]["values"] == {
            "google": {"kind": "host", "value": main.GOOGLE_HOST},
            "brave": {"kind": "host", "value": main.BRAVE_HOST},
        }
        assert needs[2]["scope"]["values"] == {
            "google": {
                "kind": "name",
                "value": "default/GOOGLE_SEARCH_API_KEY",
            },
            "brave": {
                "kind": "name",
                "value": "default/BRAVE_SEARCH_API_KEY",
            },
        }
        assert needs[3]["when"] == {
            "kind": "arg-equals",
            "arg": "provider",
            "value": "google",
        }

    source = SERVER_PATH.read_text(encoding="utf-8")
    assert "from claw_os_sdk.mcp import App" in source
    assert "serve_manifest_operations" not in source
    bindings = _server_bindings()
    assert list(bindings) == TOOL_NAMES
    for function in bindings.values():
        assert [argument.arg for argument in function.args.args] == [
            "provider",
            "query",
            "max_results",
        ]
        assert ast.literal_eval(function.args.defaults[0]) == 5

    main_tree = ast.parse((APP_DIR / "main.py").read_text(encoding="utf-8"))
    assert not any(
        isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef))
        and node.name == "run"
        for node in main_tree.body
    )


@pytest.mark.parametrize("provider", [None, "", "bing", 7])
def test_provider_is_required_before_credentials_or_policy(provider):
    with mock.patch.object(main, "load_credential") as load, mock.patch.object(
        main.policy, "require"
    ) as require:
        with pytest.raises(ValueError, match="provider must be google or brave"):
            main.web(provider, "query")

    load.assert_not_called()
    require.assert_not_called()


@pytest.mark.parametrize(
    ("query", "max_results", "message"),
    [
        ("", 5, "query must"),
        ("   ", 5, "query must"),
        (None, 5, "query must"),
        ("x" * (main.MAX_QUERY_CHARS + 1), 5, "query must"),
        ("query", 0, "max_results must"),
        ("query", 11, "max_results must"),
        ("query", True, "max_results must"),
        ("query", "5", "max_results must"),
    ],
)
def test_request_arguments_are_rejected_before_credentials(
    query,
    max_results,
    message,
):
    with mock.patch.object(main, "load_credential") as load:
        with pytest.raises(ValueError, match=message):
            main.web("google", query, max_results)

    load.assert_not_called()


def test_google_web_search_loads_only_google_credentials():
    response = _Response(_google_web_payload())
    with mock.patch.object(
        main,
        "load_credential",
        side_effect=_credential_values,
    ) as load, mock.patch.object(
        main.policy,
        "require",
    ) as require, mock.patch.object(
        main.memory,
        "remember",
    ) as remember, mock.patch.object(
        main,
        "open_url",
        return_value=(response, "https://example.test/", []),
    ) as open_url:
        result = main.web("google", "example query", 5)

    assert result["provider"] == "google"
    assert result["query"] == "example query"
    assert result["count"] == 1
    assert result["total_results"] == 12345
    assert result["results"][0]["title"] == "Example Result"
    assert load.call_args_list == [
        mock.call("GOOGLE_SEARCH_API_KEY"),
        mock.call("GOOGLE_SEARCH_ENGINE_ID"),
    ]
    require.assert_called_once_with("net.dial", host=main.GOOGLE_HOST)
    request = open_url.call_args.args[0]
    query = urllib.parse.parse_qs(urllib.parse.urlsplit(request.full_url).query)
    assert query["q"] == ["example query"]
    assert query["num"] == ["5"]
    assert response.read_limit == main.MAX_RESPONSE_BYTES + 1
    remember.assert_called_once()
    assert remember.call_args.kwargs["source"] == "search"
    assert "--provider google" in remember.call_args.kwargs["link"]


def test_brave_image_search_loads_only_brave_credential():
    response = _Response(_brave_image_payload())
    with mock.patch.object(
        main,
        "load_credential",
        side_effect=_credential_values,
    ) as load, mock.patch.object(
        main.policy,
        "require",
    ) as require, mock.patch.object(
        main.memory,
        "remember",
    ), mock.patch.object(
        main,
        "open_url",
        return_value=(response, "https://example.test/", []),
    ) as open_url:
        result = main.image("brave", "architecture", 3)

    assert result["provider"] == "brave"
    assert result["count"] == 1
    assert result["results"][0]["width"] == 1280
    assert result["results"][0]["thumbnail"].endswith("/thumb.jpg")
    load.assert_called_once_with("BRAVE_SEARCH_API_KEY")
    require.assert_called_once_with("net.dial", host=main.BRAVE_HOST)
    request = open_url.call_args.args[0]
    headers = {name.lower(): value for name, value in request.header_items()}
    assert headers["x-subscription-token"] == "brave-key"


def test_inherited_secret_environment_is_not_a_credential_fallback():
    with mock.patch.dict(
        main.os.environ,
        {
            "GOOGLE_SEARCH_API_KEY": "inherited-key",
            "GOOGLE_SEARCH_ENGINE_ID": "inherited-cx",
        },
    ), mock.patch.object(
        main,
        "load_credential",
        return_value=(None, "credential unavailable"),
    ) as load:
        with pytest.raises(RuntimeError, match="credential unavailable"):
            main.web("google", "query")

    load.assert_called_once_with("GOOGLE_SEARCH_API_KEY")


def test_google_failure_does_not_fall_back_to_brave():
    error = urllib.error.HTTPError(
        url="https://www.googleapis.com/?key=secret-key",
        code=403,
        msg="Forbidden",
        hdrs={},
        fp=mock.MagicMock(read=lambda _limit: b"forbidden"),
    )

    def credential(name):
        if name == "BRAVE_SEARCH_API_KEY":
            raise AssertionError("Brave credential must not be loaded")
        return _credential_values(name)

    with mock.patch.object(
        main,
        "load_credential",
        side_effect=credential,
    ), mock.patch.object(
        main.policy,
        "require",
    ), mock.patch.object(
        main.memory,
        "remember",
    ) as remember, mock.patch.object(
        main,
        "open_url",
        side_effect=error,
    ):
        with pytest.raises(RuntimeError, match="HTTP 403") as raised:
            main.web("google", "no fallback")

    assert "secret-key" not in str(raised.value)
    remember.assert_not_called()


def test_memory_failure_is_not_silently_ignored():
    response = _Response(_google_web_payload())
    with mock.patch.object(
        main,
        "load_credential",
        side_effect=_credential_values,
    ), mock.patch.object(
        main.policy,
        "require",
    ), mock.patch.object(
        main.memory,
        "remember",
        side_effect=main.memory.MemoryUnavailable("memory unavailable"),
    ), mock.patch.object(
        main,
        "open_url",
        return_value=(response, "https://example.test/", []),
    ):
        with pytest.raises(main.memory.MemoryUnavailable, match="memory unavailable"):
            main.web("google", "remember this")


def test_oversized_provider_response_is_rejected():
    response = _Response({})
    response.body = b"x" * (main.MAX_RESPONSE_BYTES + 1)
    with mock.patch.object(
        main,
        "open_url",
        return_value=(response, "https://example.test/", []),
    ):
        with pytest.raises(RuntimeError, match="response exceeds"):
            main._request_json(f"https://{main.GOOGLE_HOST}/")


def test_invalid_provider_json_is_rejected():
    response = _Response({})
    response.body = b"{"
    with mock.patch.object(
        main,
        "open_url",
        return_value=(response, "https://example.test/", []),
    ):
        with pytest.raises(RuntimeError, match="invalid JSON"):
            main._request_json(f"https://{main.GOOGLE_HOST}/")


def test_invalid_result_shape_is_rejected():
    response = _Response({"items": "not-a-list"})
    with mock.patch.object(
        main,
        "load_credential",
        side_effect=_credential_values,
    ), mock.patch.object(
        main.policy,
        "require",
    ), mock.patch.object(
        main.memory,
        "remember",
    ), mock.patch.object(
        main,
        "open_url",
        return_value=(response, "https://example.test/", []),
    ):
        with pytest.raises(RuntimeError, match="invalid `items` results"):
            main.web("google", "query")
