"""Network Doctor operations backed by the daemon-owned host provider."""

from __future__ import annotations

import ipaddress
import math
from decimal import Decimal

from cos_runtime import network_diagnostics, policy


DEFAULT_ATTEMPTS = 3
DEFAULT_TIMEOUT = 5.0
MAX_ATTEMPTS = 5
MAX_TIMEOUT = 30.0
MAX_PROBE_BUDGET_MS = 20_000


def _validate_target(value: object, *, require_port: bool) -> str:
    if not isinstance(value, str) or not value or value.strip() != value:
        raise ValueError("target must be a non-empty host or host:port")
    if (
        value.startswith("-")
        or "://" in value
        or any(character.isspace() or ord(character) < 32 for character in value)
        or any(separator in value for separator in ("/", "\\", "@", "#", "?"))
    ):
        raise ValueError("target must be a host or host:port")

    explicit_port = False
    if value.startswith("["):
        end = value.find("]")
        if end < 0:
            raise ValueError("target has an invalid bracketed IPv6 address")
        host = value[1:end]
        suffix = value[end + 1 :]
        try:
            address = ipaddress.ip_address(host)
        except ValueError as exc:
            raise ValueError(
                "brackets are only valid around an IPv6 address"
            ) from exc
        if address.version != 6:
            raise ValueError("brackets are only valid around an IPv6 address")
        if suffix:
            if not suffix.startswith(":"):
                raise ValueError("target has an invalid bracketed IPv6 port")
            _validate_port(suffix[1:])
            explicit_port = True
    else:
        try:
            ipaddress.ip_address(value)
            host = value
        except ValueError:
            if ":" in value:
                host, raw_port = value.rsplit(":", 1)
                _validate_port(raw_port)
                explicit_port = True
            else:
                host = value
            _validate_hostname(host)

    if require_port and not explicit_port:
        raise ValueError("TCP diagnostics require an explicit host:port target")
    return value


def _validate_port(value: str) -> int:
    if not value or not value.isascii() or not value.isdigit():
        raise ValueError("target port must be an integer")
    port = int(value)
    if not 1 <= port <= 65535:
        raise ValueError("target port is out of range")
    return port


def _validate_hostname(value: str) -> None:
    host = value[:-1] if value.endswith(".") and not value.endswith("..") else value
    if not host:
        raise ValueError("target hostname is invalid")
    try:
        ascii_host = host.encode("idna").decode("ascii")
    except UnicodeError as exc:
        raise ValueError("target hostname is invalid") from exc
    if len(ascii_host) > 253 or any(
        not label
        or len(label) > 63
        or label.startswith("-")
        or label.endswith("-")
        or not all(character.isascii() and (character.isalnum() or character == "-")
                   for character in label)
        for label in ascii_host.split(".")
    ):
        raise ValueError("target hostname is invalid")


def _validate_probe_options(attempts: object, timeout: object) -> tuple[int, int]:
    if type(attempts) is not int or not 1 <= attempts <= MAX_ATTEMPTS:
        raise ValueError(f"attempts must be between 1 and {MAX_ATTEMPTS}")
    if isinstance(timeout, bool) or not isinstance(timeout, (int, float, Decimal)):
        raise ValueError(f"timeout must be between 0.1 and {MAX_TIMEOUT:g} seconds")
    if isinstance(timeout, Decimal):
        valid_timeout = (
            timeout.is_finite()
            and Decimal("0.1") <= timeout <= Decimal(str(MAX_TIMEOUT))
        )
    else:
        valid_timeout = math.isfinite(timeout) and 0.1 <= timeout <= MAX_TIMEOUT
    if not valid_timeout:
        raise ValueError(f"timeout must be between 0.1 and {MAX_TIMEOUT:g} seconds")
    timeout_ms = round(float(timeout) * 1000)
    if attempts * timeout_ms > MAX_PROBE_BUDGET_MS:
        raise ValueError(
            "attempts multiplied by timeout must not exceed "
            f"{MAX_PROBE_BUDGET_MS // 1000:g} seconds"
        )
    return attempts, timeout_ms


def interfaces() -> dict[str, object]:
    policy.require("sys.observe", name="network")
    return network_diagnostics.request("interfaces")


def routes() -> dict[str, object]:
    policy.require("sys.observe", name="network")
    return network_diagnostics.request("routes")


def dns(target: str) -> dict[str, object]:
    target = _validate_target(target, require_port=False)
    policy.require("net.resolve", host=target)
    return network_diagnostics.request("dns", target=target)


def tcp(
    target: str,
    attempts: int = DEFAULT_ATTEMPTS,
    timeout: int | float | Decimal = DEFAULT_TIMEOUT,
) -> dict[str, object]:
    target = _validate_target(target, require_port=True)
    attempts, timeout_ms = _validate_probe_options(attempts, timeout)
    policy.require("net.resolve", host=target)
    policy.require("net.probe", host=target)
    return network_diagnostics.request(
        "tcp",
        target=target,
        attempts=attempts,
        timeout_ms=timeout_ms,
    )


def diagnose(target: str) -> dict[str, object]:
    target = _validate_target(target, require_port=True)
    policy.require("sys.observe", name="network")
    policy.require("net.resolve", host=target)
    policy.require("net.probe", host=target)
    return network_diagnostics.request("diagnose", target=target)
