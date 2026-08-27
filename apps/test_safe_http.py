"""URL authority and redirect checks must use exact effective ports."""

import io
import json
from pathlib import Path
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
    vectors = json.loads(
        Path(safe_http.__file__).with_name("url_host_scope_vectors.json").read_text(
            encoding="utf-8"
        )
    )
    for vector in vectors:
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
    response = _Response("http://0x7f.1/next")
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
    assert final_url == "http://0x7f.1/next"
    assert redirects == ["http://0x7f.1/next"]
    assert require.call_args_list == [
        mock.call("net.dial", host="xn--bcher-kva.example:443"),
        mock.call("net.dial", host="127.0.0.1:80"),
    ]
