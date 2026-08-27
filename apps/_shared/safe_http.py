"""HTTP helpers that authorize every redirect hop before connecting."""

import ipaddress
import http.client
import socket
import ssl
import unicodedata
import urllib.error
import urllib.parse
import urllib.request

from cos_runtime import policy

REDIRECT_CODES = {301, 302, 303, 307, 308}
MAX_REDIRECTS = 10


class _NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return None


def _parse_ipv4_number(part):
    radix = 10
    digits = part
    if digits.lower().startswith("0x"):
        radix = 16
        digits = digits[2:]
    elif len(digits) > 1 and digits.startswith("0"):
        radix = 8
        digits = digits[1:]
    if not digits:
        return 0
    valid = {
        8: "01234567",
        10: "0123456789",
        16: "0123456789abcdefABCDEF",
    }[radix]
    if any(char not in valid for char in digits):
        return None
    return int(digits, radix)


def _canonical_ipv4(host):
    parts = host.split(".")
    if parts and parts[-1] == "":
        parts.pop()
    if not parts or len(parts) > 4:
        return None
    numbers = [_parse_ipv4_number(part) for part in parts]
    if numbers[-1] is None:
        return None
    if any(number is None for number in numbers):
        raise ValueError("invalid IPv4 address")
    if any(number > 255 for number in numbers[:-1]):
        raise ValueError("invalid IPv4 address")
    last_limit = 256 ** (5 - len(numbers))
    if numbers[-1] >= last_limit:
        raise ValueError("invalid IPv4 address")
    value = numbers[-1]
    for index, number in enumerate(numbers[:-1]):
        value += number * (256 ** (3 - index))
    return str(ipaddress.IPv4Address(value))


def _canonical_domain(host):
    host = unicodedata.normalize("NFKC", host).translate(
        str.maketrans({"\u3002": ".", "\uff0e": ".", "\uff61": "."})
    )
    labels = host.split(".")
    canonical = []
    for label in labels:
        normalized = unicodedata.normalize("NFC", label).lower()
        if not normalized:
            canonical.append("")
        elif normalized.isascii():
            canonical.append(normalized)
        else:
            canonical.append("xn--" + normalized.encode("punycode").decode("ascii"))
    return ".".join(canonical)


def canonical_host(host):
    """Canonicalize a URL hostname with the WHATWG forms used by Rust url."""
    if ":" in host:
        return ipaddress.IPv6Address(host).compressed
    ipv4 = _canonical_ipv4(host)
    if ipv4 is not None:
        return ipv4
    return _canonical_domain(host)


def host_scope(parsed):
    """Return the exact host:port scope used by kernel URL authority."""
    host = parsed.hostname
    if not host:
        raise ValueError("URL has no host")
    host = canonical_host(host)
    port = parsed.port
    if port is None:
        if parsed.scheme == "http":
            port = 80
        elif parsed.scheme == "https":
            port = 443
        else:
            raise ValueError(f"scheme {parsed.scheme!r} has no known port")
    if ":" in host:
        return f"[{host}]:{port}"
    return f"{host}:{port}"


def _socket_host(parsed):
    host = parsed.hostname
    if not host:
        raise ValueError("URL has no host")
    return canonical_host(host)


def parse_url(url):
    parsed = urllib.parse.urlparse(url)
    if parsed.scheme not in {"http", "https"}:
        raise ValueError(f"scheme {parsed.scheme!r} is not allowed")
    if parsed.username is not None or parsed.password is not None:
        raise ValueError("URL userinfo is not allowed")
    host = _socket_host(parsed)
    if not host or host.lower() in {"localhost", "ip6-localhost"}:
        raise ValueError("URL host is not allowed")
    return parsed


def resolve_public(parsed):
    host = _socket_host(parsed)
    port = parsed.port or (443 if parsed.scheme == "https" else 80)
    addresses = socket.getaddrinfo(host, port, type=socket.SOCK_STREAM)
    if not addresses:
        raise ValueError(f"DNS returned no addresses for {host}")
    for entry in addresses:
        ip = ipaddress.ip_address(entry[4][0])
        if not ip.is_global:
            raise ValueError(f"host {host} resolved to blocked address {ip}")
    return addresses


def validate_and_authorize(url):
    parsed = parse_url(url)
    policy.require("net.dial", host=host_scope(parsed))
    addresses = resolve_public(parsed)
    return parsed, addresses


class _PinnedHTTPConnection(http.client.HTTPConnection):
    def __init__(self, host, *, pinned_ip, **kwargs):
        self._pinned_ip = pinned_ip
        super().__init__(host, **kwargs)

    def connect(self):
        self.sock = socket.create_connection(
            (self._pinned_ip, self.port),
            self.timeout,
            self.source_address,
        )


class _PinnedHTTPSConnection(http.client.HTTPSConnection):
    def __init__(self, host, *, pinned_ip, **kwargs):
        self._pinned_ip = pinned_ip
        super().__init__(host, **kwargs)

    def connect(self):
        sock = socket.create_connection(
            (self._pinned_ip, self.port),
            self.timeout,
            self.source_address,
        )
        if self._tunnel_host:
            self.sock = sock
            self._tunnel()
            sock = self.sock
        self.sock = self._context.wrap_socket(
            sock,
            server_hostname=self.host,
        )


class _PinnedHTTPHandler(urllib.request.HTTPHandler):
    def __init__(self, pinned_ip):
        super().__init__()
        self._pinned_ip = pinned_ip

    def http_open(self, req):
        return self.do_open(
            lambda host, **kwargs: _PinnedHTTPConnection(
                host,
                pinned_ip=self._pinned_ip,
                **kwargs,
            ),
            req,
        )


class _PinnedHTTPSHandler(urllib.request.HTTPSHandler):
    def __init__(self, pinned_ip):
        super().__init__(context=ssl.create_default_context())
        self._pinned_ip = pinned_ip

    def https_open(self, req):
        return self.do_open(
            lambda host, **kwargs: _PinnedHTTPSConnection(
                host,
                pinned_ip=self._pinned_ip,
                **kwargs,
            ),
            req,
        )


def _open_pinned(request, timeout, addresses):
    pinned_ip = addresses[0][4][0]
    opener = urllib.request.build_opener(
        urllib.request.ProxyHandler({}),
        _NoRedirect(),
        _PinnedHTTPHandler(pinned_ip),
        _PinnedHTTPSHandler(pinned_ip),
    )
    return opener.open(request, timeout=timeout)


def open_url(
    request,
    *,
    timeout,
    max_redirects=MAX_REDIRECTS,
    initial_authorized=False,
):
    current = request
    redirects = []
    for hop in range(max_redirects + 1):
        if hop == 0 and initial_authorized:
            parsed = parse_url(current.full_url)
            addresses = resolve_public(parsed)
        else:
            _, addresses = validate_and_authorize(current.full_url)
        try:
            response = _open_pinned(current, timeout, addresses)
            if response.geturl() != current.full_url:
                response.close()
                raise ValueError("HTTP client followed an unauthorized redirect")
            parse_url(response.geturl())
            return response, response.geturl(), redirects
        except urllib.error.HTTPError as error:
            if error.code not in REDIRECT_CODES:
                raise
            try:
                location = error.headers.get("Location")
                if not location:
                    raise
                next_url = urllib.parse.urljoin(current.full_url, location)
                parse_url(next_url)
                redirects.append(next_url)

                method = current.get_method()
                data = current.data
                headers = dict(current.header_items())
                if error.code == 303 or (
                    error.code in {301, 302} and method == "POST"
                ):
                    method = "GET"
                    data = None
                    headers = {
                        key: value
                        for key, value in headers.items()
                        if key.lower() not in {"content-length", "content-type"}
                    }

                old = urllib.parse.urlparse(current.full_url)
                new = urllib.parse.urlparse(next_url)
                if (old.scheme, old.hostname, old.port) != (
                    new.scheme,
                    new.hostname,
                    new.port,
                ):
                    headers = {
                        key: value
                        for key, value in headers.items()
                        if key.lower()
                        not in {"authorization", "cookie", "proxy-authorization"}
                    }
                headers = {
                    key: value
                    for key, value in headers.items()
                    if key.lower() != "host"
                }
                current = urllib.request.Request(
                    next_url,
                    data=data,
                    headers=headers,
                    method=method,
                )
            finally:
                error.close()
    raise urllib.error.HTTPError(
        current.full_url,
        310,
        "too many redirects",
        {},
        None,
    )
