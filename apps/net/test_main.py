import ast
import inspect
import io
import json
import os
import pathlib
import urllib.error
from unittest import mock

import pytest

from test_support import load_local_module


APP_DIR = pathlib.Path(__file__).parent
MANIFEST_PATH = APP_DIR / "app.json"
SERVER_PATH = APP_DIR / "server.py"
URL = "https://example.test/resource"

main = load_local_module(
    APP_DIR / "main.py",
    "claw_test_net_main",
    clear_modules=("_shared",),
)


class _Response:
    def __init__(
        self,
        data: bytes,
        *,
        status: int = 200,
        headers: list[tuple[str, str]] | None = None,
        fail_after_reads: int | None = None,
        on_read=None,
    ):
        self._body = io.BytesIO(data)
        self._headers = headers or [("Content-Type", "text/plain")]
        self._fail_after_reads = fail_after_reads
        self._on_read = on_read
        self.status = status
        self.reads = 0
        self.exited = False

    def __enter__(self):
        return self

    def __exit__(self, *_args):
        self.exited = True

    def read(self, size: int) -> bytes:
        if self._on_read is not None:
            self._on_read()
        if self._fail_after_reads is not None and self.reads >= self._fail_after_reads:
            raise urllib.error.URLError("connection reset")
        self.reads += 1
        return self._body.read(size)

    def getheaders(self) -> list[tuple[str, str]]:
        return list(self._headers)


class _StringSubclass(str):
    pass


def _mcp_bindings(source: str) -> dict[str, list[ast.FunctionDef]]:
    bindings: dict[str, list[ast.FunctionDef]] = {}
    for node in ast.parse(source).body:
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
                bindings.setdefault(decorator.args[0].value, []).append(node)
    return bindings


def _argument_contract(
    function: ast.FunctionDef,
) -> tuple[list[str], dict[str, object], dict[str, str]]:
    names = [argument.arg for argument in function.args.args]
    default_names = names[len(names) - len(function.args.defaults) :]
    defaults = {
        name: ast.literal_eval(default)
        for name, default in zip(
            default_names,
            function.args.defaults,
            strict=True,
        )
    }
    annotations = {
        argument.arg: ast.unparse(argument.annotation)
        for argument in function.args.args
        if argument.annotation is not None
    }
    return names, defaults, annotations


def test_manifest_and_handlers_are_mcp_only_and_aligned() -> None:
    manifest = json.loads(MANIFEST_PATH.read_text(encoding="utf-8"))
    assert "operations" not in manifest

    tools = manifest["mcp"]["tools"]
    assert [tool["name"] for tool in tools] == ["net.fetch", "net.download"]
    assert tools[0]["args"] == [
        {
            "name": "url",
            "kind": "text",
            "required": True,
            "binding": "positional",
        },
        {
            "name": "method",
            "kind": "text",
            "required": False,
            "binding": "flag",
            "default": "GET",
            "choices": ["GET", "POST", "PUT", "DELETE"],
        },
        {
            "name": "data",
            "kind": "text",
            "required": False,
            "binding": "flag",
        },
        {
            "name": "header",
            "kind": "text",
            "required": False,
            "binding": "flag",
            "repeatable": True,
        },
        {
            "name": "timeout",
            "kind": "integer",
            "required": False,
            "binding": "flag",
            "default": 30,
        },
    ]
    assert tools[1]["args"] == [
        {
            "name": "url",
            "kind": "text",
            "required": True,
            "binding": "positional",
        },
        {
            "name": "output",
            "kind": "path",
            "required": True,
            "binding": "positional",
        },
    ]
    assert tools[0]["needs"] == [
        {
            "verb": "net.dial",
            "scope": {
                "kind": "from-arg",
                "arg": "url",
                "transform": "url-host",
            },
            "why": {
                "en": "Open a network connection to the URL you asked to fetch."
            },
        }
    ]
    assert tools[1]["needs"] == [
        {
            "verb": "net.dial",
            "scope": {
                "kind": "from-arg",
                "arg": "url",
                "transform": "url-host",
            },
            "why": {
                "en": "Open a network connection to the URL you asked to download from."
            },
        },
        {
            "verb": "fs.write",
            "scope": {"kind": "from-arg", "arg": "output"},
            "why": {
                "en": "Save the downloaded file to the path you specified."
            },
        },
    ]

    server_source = SERVER_PATH.read_text(encoding="utf-8")
    assert "serve_manifest_operations" not in server_source
    assert server_source.count("App.from_manifest()") == 1
    bindings = _mcp_bindings(server_source)
    assert list(bindings) == ["net.fetch", "net.download"]
    assert all(len(handlers) == 1 for handlers in bindings.values())
    assert _argument_contract(bindings["net.fetch"][0]) == (
        ["url", "method", "data", "header", "timeout"],
        {"method": "GET", "data": None, "header": None, "timeout": 30},
        {
            "url": "str",
            "method": "str",
            "data": "str | None",
            "header": "list[str] | None",
            "timeout": "int",
        },
    )
    assert _argument_contract(bindings["net.download"][0]) == (
        ["url", "output"],
        {},
        {"url": "str", "output": "str"},
    )

    main_source = (APP_DIR / "main.py").read_text(encoding="utf-8")
    main_tree = ast.parse(main_source)
    function_names = {
        node.name
        for node in main_tree.body
        if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef))
    }
    assert "argparse" not in main_source
    assert "serve_manifest_operations" not in main_source
    assert "run" not in function_names
    assert not any(name.startswith("cmd_") for name in function_names)
    assert list(inspect.signature(main.fetch).parameters) == [
        "url",
        "method",
        "data",
        "header",
        "timeout",
    ]
    assert {
        name: parameter.default
        for name, parameter in inspect.signature(main.fetch).parameters.items()
        if parameter.default is not inspect.Parameter.empty
    } == {"method": "GET", "data": None, "header": None, "timeout": 30}
    assert (
        str(inspect.signature(main.fetch).parameters["header"].annotation)
        == "list[str] | None"
    )
    assert list(inspect.signature(main.download).parameters) == ["url", "output"]


def _assert_fetch_rejected(*args, **kwargs) -> None:
    with mock.patch.object(main.policy, "require") as require, mock.patch.object(
        main.safe_http, "open_url"
    ) as open_url:
        with pytest.raises(ValueError):
            main.fetch(*args, **kwargs)
    require.assert_not_called()
    open_url.assert_not_called()


@pytest.mark.parametrize(
    "url",
    [
        None,
        "",
        " ",
        "ftp://example.test/file",
        "https://user:secret@example.test/file",
        _StringSubclass(URL),
    ],
)
def test_fetch_rejects_invalid_urls_before_authority(url) -> None:
    _assert_fetch_rejected(url)


@pytest.mark.parametrize(
    "method",
    [True, 1, "get", "PATCH", _StringSubclass("GET")],
)
def test_fetch_rejects_non_exact_methods_before_authority(method) -> None:
    _assert_fetch_rejected(URL, method=method)


@pytest.mark.parametrize(
    "data",
    [1, b"{}", _StringSubclass("{}")],
)
def test_fetch_rejects_non_string_data_before_authority(data) -> None:
    _assert_fetch_rejected(URL, data=data)


def test_fetch_rejects_oversized_data_before_authority() -> None:
    with mock.patch.object(main, "MAX_REQUEST_DATA_BYTES", 3):
        _assert_fetch_rejected(URL, data="éé")


@pytest.mark.parametrize(
    "header",
    [
        "X-Test: yes",
        ("X-Test: yes",),
        [1],
        [_StringSubclass("X-Test: yes")],
        ["missing-colon"],
        [": empty-name"],
        ["Bad Name: value"],
        ["X-Test: ok\r\nInjected: yes"],
        ["X-Test: bad\x7fvalue"],
        ["X-Test: 😀"],
    ],
)
def test_fetch_rejects_invalid_headers_before_authority(header) -> None:
    _assert_fetch_rejected(URL, header=header)


def test_fetch_enforces_header_count_and_size_before_authority() -> None:
    with mock.patch.object(main, "MAX_HEADER_COUNT", 1):
        _assert_fetch_rejected(URL, header=["A: 1", "B: 2"])
    with mock.patch.object(main, "MAX_HEADER_LINE_BYTES", 4):
        _assert_fetch_rejected(URL, header=["A: 123"])
    with mock.patch.object(main, "MAX_HEADER_BYTES", 7):
        _assert_fetch_rejected(URL, header=["A: 1", "B: 2"])


@pytest.mark.parametrize("timeout", [True, 0, 301, 1.5, "30"])
def test_fetch_rejects_invalid_timeouts_before_authority(timeout) -> None:
    _assert_fetch_rejected(URL, timeout=timeout)


def _assert_download_rejected(output) -> None:
    with mock.patch.object(main.policy, "require") as require, mock.patch.object(
        main.safe_http, "open_url"
    ) as open_url:
        with pytest.raises(ValueError):
            main.download(URL, output)
    require.assert_not_called()
    open_url.assert_not_called()


@pytest.mark.parametrize(
    "url",
    [
        None,
        "",
        "file:///etc/passwd",
        "https://user@example.test/file",
        _StringSubclass(URL),
    ],
)
def test_download_rejects_invalid_urls_before_authority(
    url,
    tmp_path: pathlib.Path,
) -> None:
    destination = tmp_path / "download.bin"
    with mock.patch.object(main.policy, "require") as require, mock.patch.object(
        main.safe_http, "open_url"
    ) as open_url:
        with pytest.raises(ValueError):
            main.download(url, os.fspath(destination))
    require.assert_not_called()
    open_url.assert_not_called()


def test_download_requires_real_absolute_canonical_output(
    tmp_path: pathlib.Path,
) -> None:
    _assert_download_rejected(pathlib.Path("/workspace/file"))
    _assert_download_rejected("")
    _assert_download_rejected("relative/file")
    _assert_download_rejected(os.fspath(tmp_path / "bad\nname"))
    _assert_download_rejected(os.fspath(tmp_path / "bad\x00name"))
    _assert_download_rejected(
        os.path.join(os.fspath(tmp_path), "nested", "..", "download.bin")
    )


def test_download_rejects_symlink_target_before_authority(
    tmp_path: pathlib.Path,
) -> None:
    target = tmp_path / "target.bin"
    target.write_bytes(b"existing")
    link = tmp_path / "download.bin"
    link.symlink_to(target)

    _assert_download_rejected(os.fspath(link))


def test_download_rejects_symlink_parent_before_authority(
    tmp_path: pathlib.Path,
) -> None:
    real_parent = tmp_path / "real"
    real_parent.mkdir()
    alias = tmp_path / "alias"
    alias.symlink_to(real_parent, target_is_directory=True)

    _assert_download_rejected(os.fspath(alias / "download.bin"))


def test_download_policy_errors_propagate_before_network(
    tmp_path: pathlib.Path,
) -> None:
    destination = tmp_path / "download.bin"
    error = main.policy.PolicyUnavailable("policy unavailable")
    with mock.patch.object(
        main.policy,
        "require",
        side_effect=error,
    ) as require, mock.patch.object(main.safe_http, "open_url") as open_url:
        with pytest.raises(main.policy.PolicyUnavailable) as raised:
            main.download(URL, os.fspath(destination))

    assert raised.value is error
    require.assert_called_once_with(
        "fs.write",
        path=os.path.realpath(destination),
    )
    open_url.assert_not_called()


def test_fetch_defaults_and_success_shape() -> None:
    response = _Response(
        b"hello",
        status=201,
        headers=[("Content-Type", "text/plain"), ("X-Result", "ok")],
    )
    captured = {}

    def open_url(request, *, timeout):
        captured["request"] = request
        captured["timeout"] = timeout
        return response, URL, []

    with mock.patch.object(main.safe_http, "open_url", side_effect=open_url):
        result = main.fetch(URL)

    request = captured["request"]
    headers = {name.lower(): value for name, value in request.header_items()}
    assert request.full_url == URL
    assert request.get_method() == "GET"
    assert request.data is None
    assert captured["timeout"] == 30
    assert headers["user-agent"] == main.USER_AGENT
    assert result == {
        "url": URL,
        "status": 201,
        "headers": {"Content-Type": "text/plain", "X-Result": "ok"},
        "body": "hello",
    }
    assert response.exited


def test_fetch_passes_typed_flags_and_repeatable_headers() -> None:
    response = _Response(b'{"ok":true}')
    captured = {}

    def open_url(request, *, timeout):
        captured["request"] = request
        captured["timeout"] = timeout
        return response, URL, []

    with mock.patch.object(main.safe_http, "open_url", side_effect=open_url):
        main.fetch(
            URL,
            method="POST",
            data='{"request":true}',
            header=["X-First: one", "Content-Type: text/plain", "X-Last: two"],
            timeout=12,
        )

    request = captured["request"]
    headers = {name.lower(): value for name, value in request.header_items()}
    assert request.get_method() == "POST"
    assert request.data == b'{"request":true}'
    assert captured["timeout"] == 12
    assert headers["x-first"] == "one"
    assert headers["content-type"] == "text/plain"
    assert headers["x-last"] == "two"


def test_fetch_adds_json_content_type_for_data() -> None:
    response = _Response(b"ok")
    captured = {}

    def open_url(request, *, timeout):
        captured["request"] = request
        return response, URL, []

    with mock.patch.object(main.safe_http, "open_url", side_effect=open_url):
        main.fetch(URL, method="PUT", data="{}")

    headers = {
        name.lower(): value
        for name, value in captured["request"].header_items()
    }
    assert headers["content-type"] == "application/json"


def test_fetch_response_limit_sets_truncated_without_overreading() -> None:
    response = _Response(b"12345")
    with mock.patch.object(main, "MAX_RESPONSE_BYTES", 4), mock.patch.object(
        main.safe_http,
        "open_url",
        return_value=(response, URL, []),
    ):
        result = main.fetch(URL)

    assert result["body"] == "1234"
    assert result["truncated"] is True


def test_fetch_exact_response_limit_is_not_truncated() -> None:
    response = _Response(b"1234")
    with mock.patch.object(main, "MAX_RESPONSE_BYTES", 4), mock.patch.object(
        main.safe_http,
        "open_url",
        return_value=(response, URL, []),
    ):
        result = main.fetch(URL)

    assert result["body"] == "1234"
    assert "truncated" not in result


@pytest.mark.parametrize(
    "error",
    [
        urllib.error.HTTPError(URL, 503, "unavailable", {}, io.BytesIO(b"error")),
        urllib.error.URLError("offline"),
        main.policy.PolicyError("denied"),
    ],
)
def test_fetch_errors_propagate(error) -> None:
    with mock.patch.object(main.safe_http, "open_url", side_effect=error):
        with pytest.raises(type(error)) as raised:
            main.fetch(URL)
    assert raised.value is error


def test_fetch_midstream_network_error_propagates() -> None:
    response = _Response(b"partial", fail_after_reads=1)
    with mock.patch.object(
        main.safe_http,
        "open_url",
        return_value=(response, URL, []),
    ):
        with pytest.raises(urllib.error.URLError, match="connection reset"):
            main.fetch(URL)
    assert response.exited


def _run_download(response: _Response, destination: pathlib.Path):
    with mock.patch.object(main.policy, "require") as require, mock.patch.object(
        main.safe_http,
        "open_url",
        return_value=(response, URL, []),
    ):
        try:
            return main.download(URL, os.fspath(destination))
        finally:
            require.assert_called_once_with(
                "fs.write",
                path=os.path.realpath(destination),
            )


def _assert_only_destination(
    directory: pathlib.Path,
    destination: pathlib.Path,
) -> None:
    assert set(directory.iterdir()) == {destination}


def test_network_open_failure_preserves_existing_destination(
    tmp_path: pathlib.Path,
) -> None:
    destination = tmp_path / "download.bin"
    destination.write_bytes(b"original")
    error = urllib.error.URLError("offline")

    with mock.patch.object(main.policy, "require") as require, mock.patch.object(
        main.safe_http,
        "open_url",
        side_effect=error,
    ), mock.patch.object(main.tempfile, "mkstemp") as mkstemp:
        with pytest.raises(urllib.error.URLError) as raised:
            main.download(URL, os.fspath(destination))

    assert raised.value is error
    require.assert_called_once_with(
        "fs.write",
        path=os.path.realpath(destination),
    )
    mkstemp.assert_not_called()
    assert destination.read_bytes() == b"original"
    _assert_only_destination(tmp_path, destination)


def test_download_http_error_propagates_without_temporary_file(
    tmp_path: pathlib.Path,
) -> None:
    destination = tmp_path / "download.bin"
    error = urllib.error.HTTPError(URL, 404, "not found", {}, None)
    with mock.patch.object(main.policy, "require"), mock.patch.object(
        main.safe_http,
        "open_url",
        side_effect=error,
    ), mock.patch.object(main.tempfile, "mkstemp") as mkstemp:
        with pytest.raises(urllib.error.HTTPError) as raised:
            main.download(URL, os.fspath(destination))
    assert raised.value is error
    mkstemp.assert_not_called()


def test_midstream_network_failure_removes_temporary_file(
    tmp_path: pathlib.Path,
) -> None:
    destination = tmp_path / "download.bin"
    destination.write_bytes(b"original")
    response = _Response(b"partial", fail_after_reads=1)

    with pytest.raises(urllib.error.URLError, match="connection reset"):
        _run_download(response, destination)

    assert destination.read_bytes() == b"original"
    _assert_only_destination(tmp_path, destination)


def test_size_limit_is_hard_error_and_removes_temporary_file(
    tmp_path: pathlib.Path,
) -> None:
    destination = tmp_path / "download.bin"
    destination.write_bytes(b"original")
    response = _Response(b"12345")

    with mock.patch.object(main, "MAX_DOWNLOAD_BYTES", 4):
        with pytest.raises(
            main.DownloadLimitExceeded,
            match="download exceeds size limit of 4 bytes",
        ):
            _run_download(response, destination)

    assert destination.read_bytes() == b"original"
    _assert_only_destination(tmp_path, destination)


def test_exact_size_limit_is_successful(tmp_path: pathlib.Path) -> None:
    destination = tmp_path / "download.bin"
    response = _Response(b"1234")

    with mock.patch.object(main, "MAX_DOWNLOAD_BYTES", 4):
        result = _run_download(response, destination)

    assert result["bytes"] == 4
    assert destination.read_bytes() == b"1234"
    _assert_only_destination(tmp_path, destination)


def test_download_returns_resolved_exact_output(tmp_path: pathlib.Path) -> None:
    destination = tmp_path / "bound.bin"
    response = _Response(b"bound")

    result = _run_download(response, destination)

    resolved = os.path.realpath(destination)
    assert result == {"url": URL, "path": resolved, "bytes": 5}
    assert destination.read_bytes() == b"bound"


def test_fsync_failure_removes_temporary_file_without_replacing(
    tmp_path: pathlib.Path,
) -> None:
    destination = tmp_path / "download.bin"
    destination.write_bytes(b"original")
    response = _Response(b"replacement")

    with mock.patch.object(
        main.os,
        "fsync",
        side_effect=OSError("fsync failed"),
    ), mock.patch.object(main.os, "replace") as replace:
        with pytest.raises(OSError, match="fsync failed"):
            _run_download(response, destination)

    replace.assert_not_called()
    assert destination.read_bytes() == b"original"
    _assert_only_destination(tmp_path, destination)


def test_replace_failure_removes_temporary_file_and_preserves_destination(
    tmp_path: pathlib.Path,
) -> None:
    destination = tmp_path / "download.bin"
    destination.write_bytes(b"original")
    response = _Response(b"replacement")

    with mock.patch.object(
        main.os,
        "replace",
        side_effect=OSError("replace failed"),
    ):
        with pytest.raises(OSError, match="replace failed"):
            _run_download(response, destination)

    assert destination.read_bytes() == b"original"
    _assert_only_destination(tmp_path, destination)


def test_cleanup_failure_does_not_mask_network_failure(
    tmp_path: pathlib.Path,
) -> None:
    destination = tmp_path / "download.bin"
    destination.write_bytes(b"original")
    response = _Response(b"partial", fail_after_reads=1)
    real_unlink = main.os.unlink

    def unlink_then_fail(path):
        if pathlib.Path(path).name.startswith(".download.bin."):
            real_unlink(path)
            raise OSError("cleanup failed")
        return real_unlink(path)

    with mock.patch.object(main.os, "unlink", side_effect=unlink_then_fail):
        with pytest.raises(urllib.error.URLError, match="connection reset"):
            _run_download(response, destination)

    assert destination.read_bytes() == b"original"
    _assert_only_destination(tmp_path, destination)


def test_success_uses_private_same_directory_temp_and_replaces_after_fsync(
    tmp_path: pathlib.Path,
) -> None:
    destination = tmp_path / "download.bin"
    destination.write_bytes(b"original")
    observed = {}
    events = []

    def inspect_temp():
        if "path" in observed:
            return
        candidates = set(tmp_path.iterdir()) - {destination}
        assert len(candidates) == 1
        staged = candidates.pop()
        observed["path"] = staged

    response = _Response(b"replacement", on_read=inspect_temp)
    real_fchmod = main.os.fchmod
    real_fsync = main.os.fsync
    real_replace = main.os.replace

    def require(*_args, **_kwargs):
        events.append("fs.write")

    def open_url(*_args, **_kwargs):
        events.append("open_url")
        return response, URL, []

    def fchmod(fd, mode):
        events.append("fchmod")
        observed["requested_mode"] = mode
        return real_fchmod(fd, mode)

    def fsync(fd):
        events.append("fsync")
        return real_fsync(fd)

    def replace(source, target):
        events.append("replace")
        assert response.exited
        assert pathlib.Path(source) == observed["path"]
        return real_replace(source, target)

    with mock.patch.object(
        main.policy,
        "require",
        side_effect=require,
    ) as required, mock.patch.object(
        main.safe_http,
        "open_url",
        side_effect=open_url,
    ), mock.patch.object(
        main.os,
        "fchmod",
        side_effect=fchmod,
    ), mock.patch.object(
        main.os,
        "fsync",
        side_effect=fsync,
    ), mock.patch.object(
        main.os,
        "replace",
        side_effect=replace,
    ):
        result = main.download(URL, os.fspath(destination))

    required.assert_called_once_with(
        "fs.write",
        path=os.path.realpath(destination),
    )
    assert events[:2] == ["fs.write", "open_url"]
    assert events.index("fsync") < events.index("replace")
    assert observed["path"].parent == destination.parent
    assert observed["requested_mode"] == 0o600
    assert result == {
        "url": URL,
        "path": os.path.realpath(destination),
        "bytes": len(b"replacement"),
    }
    assert destination.read_bytes() == b"replacement"
    _assert_only_destination(tmp_path, destination)
