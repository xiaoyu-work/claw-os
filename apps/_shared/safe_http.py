"""HTTP helpers that authorize every redirect hop before connecting."""

import ipaddress
import http.client
import socket
import ssl
import urllib.error
import urllib.parse
import urllib.request

from cos_runtime import egress, policy

try:
    import idna
except ImportError as error:  # pragma: no cover - exercised by packaging checks
    idna = None
    _IDNA_ERROR = error
else:
    _IDNA_ERROR = None
    _IDNA_VERSION = tuple(int(part) for part in idna.__version__.split(".")[:2])
    if not (3, 3) <= _IDNA_VERSION < (4, 0):
        _IDNA_ERROR = RuntimeError(
            f"idna >= 3.3, < 4 is required; found {idna.__version__}"
        )

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
    if _IDNA_ERROR is not None:
        raise RuntimeError(
            "standards-conformant URL host validation requires idna >= 3.3, < 4"
        ) from _IDNA_ERROR
    try:
        if host.isascii():
            canonical = idna.uts46_remap(
                host,
                std3_rules=True,
                transitional=False,
            )
            if not canonical.isascii():
                raise ValueError("ASCII URL host remapped to non-ASCII")
            return canonical
        return idna.encode(
            host, uts46=True, std3_rules=True, transitional=False
        ).decode("ascii")
    except idna.IDNAError as error:
        raise ValueError(f"URL host is not valid UTS-46: {error}") from None


def canonical_host(host):
    """Canonicalize a URL hostname with the WHATWG forms used by Rust url."""
    if _IDNA_ERROR is not None:
        raise RuntimeError(
            "standards-conformant URL host validation requires idna >= 3.3, < 4"
        ) from _IDNA_ERROR
    if ":" in host:
        return ipaddress.IPv6Address(host).compressed
    ipv4 = _canonical_ipv4(host)
    if ipv4 is not None:
        return ipv4
    return _canonical_domain(host)


def canonical_url(parsed):
    """Serialize a parsed URL with the canonical host used for authority."""
    host = canonical_host(parsed.hostname)
    authority = f"[{host}]" if ":" in host else host
    if parsed.port is not None:
        authority = f"{authority}:{parsed.port}"
    return urllib.parse.urlunparse(parsed._replace(netloc=authority))


def _canonical_request(request, parsed):
    url = canonical_url(parsed)
    headers = {
        key: value
        for key, value in request.header_items()
        if key.lower() != "host"
    }
    return urllib.request.Request(
        url,
        data=request.data,
        headers=headers,
        method=request.get_method(),
    )


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
    """Resolve and screen the addresses this request may reach.

    Inside a worker sandbox there is nothing to resolve *here*: the
    process has no route to a resolver and no permission to open an
    ``AF_INET`` socket, and the egress broker resolves the name and
    screens every answer itself before it connects. Returning ``None``
    tells the transport below to ask the broker instead of dialling.
    """
    if egress.available():
        return None
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


def _connect(host, port, timeout, pinned_ip):
    """Open the transport for one hop.

    Brokered when the operation holds a brokered-egress endpoint, which
    is the only transport a sandboxed worker has; otherwise the legacy
    pinned-address dial, unchanged, for code running outside a sandbox.
    There is no third path: a brokered refusal is raised, never retried
    directly.
    """
    if egress.available():
        return egress.create_connection(host, port, timeout)
    return socket.create_connection((pinned_ip, port), timeout)


class _PinnedHTTPConnection(http.client.HTTPConnection):
    def __init__(self, host, *, pinned_ip, **kwargs):
        self._pinned_ip = pinned_ip
        super().__init__(host, **kwargs)

    def connect(self):
        self.sock = _connect(self.host, self.port, self.timeout, self._pinned_ip)


class _PinnedHTTPSConnection(http.client.HTTPSConnection):
    def __init__(self, host, *, pinned_ip, **kwargs):
        self._pinned_ip = pinned_ip
        super().__init__(host, **kwargs)

    def connect(self):
        sock = _connect(self.host, self.port, self.timeout, self._pinned_ip)
        if self._tunnel_host:
            self.sock = sock
            self._tunnel()
            sock = self.sock
        # Whatever carried the bytes, the certificate must still name the
        # host this request asked for. The broker pins the transport; TLS
        # pins the identity.
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
    # `None` means the egress broker owns resolution and pinning; the
    # placeholder address is never dialled in that case.
    pinned_ip = addresses[0][4][0] if addresses else ""
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
        parsed = parse_url(current.full_url)
        current = _canonical_request(current, parsed)
        if hop == 0 and initial_authorized:
            addresses = resolve_public(parsed)
        else:
            policy.require("net.dial", host=host_scope(parsed))
            addresses = resolve_public(parsed)
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
