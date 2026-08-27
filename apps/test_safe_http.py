"""URL authority and redirect checks must use exact effective ports."""

import io
import urllib.error
import urllib.request
from unittest import mock

from _shared import safe_http


class _Response:
    def __init__(self, url):
        self._url = url

    def geturl(self):
        return self._url

    def close(self):
        pass


def test_host_scope_includes_effective_ports_and_ipv6_brackets():
    for url, expected in [
        ("http://example.test/path", "example.test:80"),
        ("https://example.test/path", "example.test:443"),
        ("https://example.test:8443/path", "example.test:8443"),
        ("http://[2001:db8::1]/path", "[2001:db8::1]:80"),
        ("https://[2001:db8::2]:9443/path", "[2001:db8::2]:9443"),
    ]:
        assert safe_http.host_scope(safe_http.parse_url(url)) == expected


def test_every_redirect_hop_uses_the_same_effective_port_scope():
    first = urllib.error.HTTPError(
        "http://origin.example/start",
        302,
        "Found",
        {"Location": "https://redirect.example/next"},
        io.BytesIO(),
    )
    response = _Response("https://redirect.example/next")
    addresses = [(None, None, None, None, ("203.0.113.10", 443))]
    request = urllib.request.Request("http://origin.example/start")

    with mock.patch.object(
        safe_http, "resolve_public", return_value=addresses
    ), mock.patch.object(
        safe_http, "_open_pinned", side_effect=[first, response]
    ), mock.patch.object(
        safe_http.policy, "require"
    ) as require:
        result, final_url, redirects = safe_http.open_url(request, timeout=1)

    assert result is response
    assert final_url == "https://redirect.example/next"
    assert redirects == ["https://redirect.example/next"]
    assert require.call_args_list == [
        mock.call("net.dial", host="origin.example:80"),
        mock.call("net.dial", host="redirect.example:443"),
    ]
