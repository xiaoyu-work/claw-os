"""URL authority and redirect checks must use exact effective ports."""

import io
import json
from pathlib import Path
import urllib.error
import urllib.request
from unittest import mock

import pytest

from _shared import safe_http
from test_support import load_local_module


class _Response:
    def __init__(self, url):
        self._url = url

    def geturl(self):
        return self._url

    def close(self):
        pass


def test_host_scope_includes_effective_ports_and_ipv6_brackets():
    vectors = json.loads(
        Path(safe_http.__file__).with_name("url_host_scope_vectors.json").read_text(
            encoding="utf-8"
        )
    )
    for vector in vectors:
        if vector.get("error"):
            with pytest.raises(ValueError):
                safe_http.host_scope(safe_http.parse_url(vector["url"]))
        else:
            assert (
                safe_http.host_scope(safe_http.parse_url(vector["url"]))
                == vector["scope"]
            )


def test_every_redirect_hop_uses_the_same_effective_port_scope():
    first = urllib.error.HTTPError(
        "https://bücher.example/start",
        302,
        "Found",
        {"Location": "http://0x7f.1/next"},
        io.BytesIO(),
    )
    response = _Response("http://127.0.0.1/next")
    addresses = [(None, None, None, None, ("203.0.113.10", 80))]
    request = urllib.request.Request("https://bücher.example/start")

    with mock.patch.object(
        safe_http, "resolve_public", return_value=addresses
    ), mock.patch.object(
        safe_http, "_open_pinned", side_effect=[first, response]
    ), mock.patch.object(
        safe_http.policy, "require"
    ) as require:
        result, final_url, redirects = safe_http.open_url(request, timeout=1)

    assert result is response
    assert final_url == "http://127.0.0.1/next"
    assert redirects == ["http://0x7f.1/next"]
    assert require.call_args_list == [
        mock.call("net.dial", host="xn--bcher-kva.example:443"),
        mock.call("net.dial", host="127.0.0.1:80"),
    ]


def test_initial_authorized_request_uses_the_authorized_canonical_host():
    response = _Response("https://example.com/path")

    def open_pinned(request, _timeout, _addresses):
        assert request.full_url == "https://example.com/path"
        assert request.get_header("Host") is None
        return response

    request = urllib.request.Request(
        "https://exam\u00adple.com/path",
        headers={"Host": "attacker.example"},
    )
    with mock.patch.object(
        safe_http, "resolve_public", return_value=[("dns",)]
    ), mock.patch.object(
        safe_http, "_open_pinned", side_effect=open_pinned
    ), mock.patch.object(
        safe_http.policy, "require"
    ) as require:
        result, final_url, _ = safe_http.open_url(
            request,
            timeout=1,
            initial_authorized=True,
        )

    assert result is response
    assert final_url == "https://example.com/path"
    require.assert_not_called()


def test_idna_dependency_failure_is_fail_closed():
    with mock.patch.object(
        safe_http, "_IDNA_ERROR", ImportError("idna missing")
    ), pytest.raises(RuntimeError, match="idna"):
        safe_http.parse_url("https://example.com")


def test_idna_runtime_dependency_is_packaged_and_version_supported():
    version = tuple(int(part) for part in safe_http.idna.__version__.split(".")[:2])
    assert (3, 3) <= version < (4, 0)

    repository = Path(__file__).resolve().parent.parent
    control = (
        repository / "packaging/deb/claw-os-agent/control"
    ).read_text(encoding="utf-8")
    packages = (
        repository / "rootfs/features/base/packages.txt"
    ).read_text(encoding="utf-8").splitlines()
    assert "python3-idna (>= 3.3)" in control
    assert "python3-idna (<< 4)" in control
    assert "python3-idna" in packages


def test_web_normalizes_the_launched_url_before_policy_and_network_use():
    web = load_local_module(
        Path(__file__).with_name("web") / "main.py",
        "claw_test_web_url_canonicalization",
        clear_modules=("_shared",),
    )
    assert (
        web._normalize_url("https://exam\u00adple.com/path")
        == "https://example.com/path"
    )
