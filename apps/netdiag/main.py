"""netdiag — active, DNS-pinned network diagnosis."""

from __future__ import annotations

import ipaddress
import os
import pathlib
import queue
import socket
import struct
import sys
import threading
import time

from cos_runtime import egress, policy


DEFAULT_PORT = 443
MAX_ATTEMPTS = 5


def _default_timeout():
    try:
        value = float(os.environ.get("CLAW_NETDIAG_TIMEOUT", "5"))
    except ValueError:
        return 5.0
    return value if 0.1 <= value <= 30 else 5.0


DEFAULT_TIMEOUT = _default_timeout()


def _parse_target(raw):
    if not isinstance(raw, str):
        raise ValueError("target must be a string")
    raw = raw.strip()
    if not raw or any(ch.isspace() or ord(ch) < 32 for ch in raw):
        raise ValueError("target must be a non-empty host or host:port")
    if "://" in raw or "/" in raw or raw.startswith("-"):
        raise ValueError("target must not be a URL, path, or option")

    if raw.startswith("["):
        end = raw.find("]")
        if end < 0:
            raise ValueError("invalid bracketed IPv6 target")
        host = raw[1:end]
        suffix = raw[end + 1 :]
        if not suffix:
            port = DEFAULT_PORT
        elif suffix.startswith(":") and suffix[1:].isdigit():
            port = int(suffix[1:])
        else:
            raise ValueError("invalid bracketed IPv6 port")
    elif raw.count(":") == 1:
        host, maybe_port = raw.rsplit(":", 1)
        if maybe_port.isdigit():
            port = int(maybe_port)
        else:
            host = raw
            port = DEFAULT_PORT
    else:
        host = raw
        port = DEFAULT_PORT

    host = host.rstrip(".")
    if not host or not 1 <= port <= 65535:
        raise ValueError("target host or port is invalid")
    try:
        ipaddress.ip_address(host)
    except ValueError:
        labels = host.split(".")
        if any(
            not label
            or len(label) > 63
            or label.startswith("-")
            or label.endswith("-")
            or not all(ch.isalnum() or ch == "-" for ch in label)
            for label in labels
        ):
            raise ValueError("target hostname is invalid")
    scope = raw
    return host, port, scope


def _resolve(target):
    host, port, scope = _parse_target(target)
    policy.require("net.resolve", host=scope)
    started = time.monotonic()
    try:
        infos = _getaddrinfo_bounded(host, port, DEFAULT_TIMEOUT)
    except (socket.gaierror, OSError, TimeoutError) as exc:
        return {
            "target": scope,
            "host": host,
            "port": port,
            "resolved": False,
            "error": str(exc),
            "latency_ms": round((time.monotonic() - started) * 1000, 2),
            "addresses": [],
            "_targets": [],
        }

    targets = []
    addresses = []
    seen = set()
    for family, socktype, proto, canonical, sockaddr in infos:
        if family not in (socket.AF_INET, socket.AF_INET6) or not sockaddr:
            continue
        key = (family, sockaddr)
        if key in seen:
            continue
        seen.add(key)
        targets.append((family, socktype, proto, sockaddr))
        addresses.append(
            {
                "ip": sockaddr[0],
                "family": "ipv6" if family == socket.AF_INET6 else "ipv4",
                "canonical_name": canonical or None,
            }
        )
    return {
        "target": scope,
        "host": host,
        "port": port,
        "resolved": bool(targets),
        "latency_ms": round((time.monotonic() - started) * 1000, 2),
        "addresses": addresses,
        "_targets": targets,
    }


def _getaddrinfo_bounded(host, port, timeout):
    results = queue.Queue(maxsize=1)

    def resolve():
        try:
            value = socket.getaddrinfo(
                host,
                port,
                family=socket.AF_UNSPEC,
                type=socket.SOCK_STREAM,
                proto=socket.IPPROTO_TCP,
            )
            results.put((True, value))
        except (socket.gaierror, OSError) as exc:
            results.put((False, exc))

    thread = threading.Thread(target=resolve, daemon=True)
    thread.start()
    try:
        ok, value = results.get(timeout=timeout)
    except queue.Empty:
        raise TimeoutError(f"DNS resolution exceeded {timeout}s") from None
    if not ok:
        raise value
    return value


def cmd_dns(args):
    if len(args) != 1:
        return {"error": "dns requires <host-or-host:port>"}
    try:
        result = _resolve(args[0])
    except ValueError as exc:
        return {"error": str(exc)}
    result.pop("_targets", None)
    return result


def _connect_target(target, timeout):
    family, socktype, proto, sockaddr = target
    # A raw TCP probe is exactly what a sandboxed worker may not do: it
    # has no route and no permission to open an `AF_INET` socket. Say so
    # explicitly rather than returning a connection error that reads like
    # the host being down.
    if egress.available() or os.environ.get("COS_WORKER_SANDBOX"):
        return {
            "ok": False,
            "ip": sockaddr[0],
            "error": (
                "raw TCP probing is unavailable inside the worker sandbox; "
                "run netdiag outside a sandboxed operation"
            ),
        }
    sock = None
    try:
        sock = socket.socket(family, socktype, proto)
        sock.settimeout(timeout)
        started = time.monotonic()
        sock.connect(sockaddr)
        return {
            "ok": True,
            "ip": sockaddr[0],
            "latency_ms": round((time.monotonic() - started) * 1000, 2),
        }
    except OSError as exc:
        return {"ok": False, "ip": sockaddr[0], "error": str(exc)}
    finally:
        if sock is not None:
            sock.close()


def cmd_tcp(args):
    if not args:
        return {"error": "tcp requires <host-or-host:port>"}
    target = args[0]
    attempts = 3
    timeout = DEFAULT_TIMEOUT
    index = 1
    while index < len(args):
        if args[index] == "--attempts" and index + 1 < len(args):
            try:
                attempts = int(args[index + 1])
            except ValueError:
                return {"error": "--attempts must be an integer"}
            index += 2
        elif args[index] == "--timeout" and index + 1 < len(args):
            try:
                timeout = float(args[index + 1])
            except ValueError:
                return {"error": "--timeout must be a number"}
            index += 2
        else:
            return {"error": f"unexpected tcp argument: {args[index]}"}
    if not 1 <= attempts <= MAX_ATTEMPTS or not 0.1 <= timeout <= 30:
        return {"error": f"attempts must be 1..{MAX_ATTEMPTS}; timeout must be 0.1..30"}

    try:
        resolved = _resolve(target)
    except ValueError as exc:
        return {"error": str(exc)}
    if not resolved["resolved"]:
        resolved.pop("_targets", None)
        return resolved
    policy.require("net.dial", host=resolved["target"])

    samples = []
    targets = resolved.pop("_targets")
    for attempt in range(attempts):
        probe = _connect_target(targets[attempt % len(targets)], timeout)
        probe["attempt"] = attempt + 1
        samples.append(probe)
    successful = [sample for sample in samples if sample["ok"]]
    latencies = [sample["latency_ms"] for sample in successful]
    return {
        **resolved,
        "reachable": bool(successful),
        "attempts": samples,
        "success_count": len(successful),
        "failure_count": len(samples) - len(successful),
        "latency_ms": {
            "min": min(latencies) if latencies else None,
            "max": max(latencies) if latencies else None,
            "average": round(sum(latencies) / len(latencies), 2) if latencies else None,
        },
    }


def cmd_interfaces(args):
    if args:
        return {"error": "interfaces takes no arguments"}
    policy.require("sys.observe", name="network")
    interfaces = []
    root = pathlib.Path("/sys/class/net")
    if root.is_dir():
        for entry in sorted(root.iterdir(), key=lambda item: item.name):
            interfaces.append(
                {
                    "name": entry.name,
                    "operstate": _read(entry / "operstate"),
                    "carrier": _read(entry / "carrier") == "1",
                    "mtu": _read_int(entry / "mtu"),
                    "address": _read(entry / "address"),
                    "speed_mbps": _read_int(entry / "speed"),
                    "rx_bytes": _read_int(entry / "statistics" / "rx_bytes"),
                    "tx_bytes": _read_int(entry / "statistics" / "tx_bytes"),
                    "rx_errors": _read_int(entry / "statistics" / "rx_errors"),
                    "tx_errors": _read_int(entry / "statistics" / "tx_errors"),
                }
            )
    return {"interfaces": interfaces, "count": len(interfaces)}


def cmd_routes(args):
    if args:
        return {"error": "routes takes no arguments"}
    policy.require("sys.observe", name="network")
    routes = []
    try:
        with open("/proc/net/route", encoding="utf-8") as handle:
            for line in list(handle)[1:]:
                fields = line.split()
                if len(fields) < 11:
                    continue
                destination = _hex_ipv4(fields[1])
                gateway = _hex_ipv4(fields[2])
                mask = _hex_ipv4(fields[7])
                routes.append(
                    {
                        "interface": fields[0],
                        "destination": destination,
                        "gateway": gateway,
                        "mask": mask,
                        "default": fields[1] == "00000000" and fields[7] == "00000000",
                        "metric": _safe_int(fields[6]),
                    }
                )
    except OSError as exc:
        return {"routes": [], "count": 0, "error": str(exc)}
    return {
        "routes": routes,
        "count": len(routes),
        "default_routes": [route for route in routes if route["default"]],
    }


def cmd_diagnose(args):
    if len(args) != 1:
        return {"error": "diagnose requires <host-or-host:port>"}
    interfaces = cmd_interfaces([])
    routes = cmd_routes([])
    tcp = cmd_tcp([args[0], "--attempts", "3"])

    findings = []
    non_loopback = [
        interface
        for interface in interfaces.get("interfaces", [])
        if interface["name"] != "lo"
    ]
    up = [interface for interface in non_loopback if interface["operstate"] == "up"]
    if not non_loopback:
        findings.append(
            {
                "stage": "link",
                "severity": "critical",
                "message": "No non-loopback network interface is present.",
            }
        )
    elif not up:
        findings.append(
            {
                "stage": "link",
                "severity": "critical",
                "message": "No non-loopback interface reports operstate=up.",
            }
        )
    if not routes.get("default_routes"):
        findings.append(
            {
                "stage": "route",
                "severity": "critical",
                "message": "No IPv4 default route is installed.",
            }
        )
    if tcp.get("resolved") is False:
        findings.append(
            {
                "stage": "dns",
                "severity": "critical",
                "message": tcp.get("error") or "DNS resolution failed.",
            }
        )
    elif tcp.get("error") and "reachable" not in tcp:
        findings.append(
            {
                "stage": "probe",
                "severity": "critical",
                "message": tcp["error"],
            }
        )
    elif tcp.get("reachable") is False:
        findings.append(
            {
                "stage": "tcp",
                "severity": "warning",
                "message": "DNS succeeded but the TCP target was unreachable.",
            }
        )
    if not findings:
        findings.append(
            {
                "stage": "tcp",
                "severity": "info",
                "message": "Local link, default route, DNS, and TCP reachability succeeded.",
            }
        )
    status = (
        "critical"
        if any(item["severity"] == "critical" for item in findings)
        else "warn"
        if any(item["severity"] == "warning" for item in findings)
        else "ok"
    )
    return {
        "status": status,
        "target": args[0],
        "findings": findings,
        "interfaces": interfaces,
        "routes": routes,
        "tcp": tcp,
    }


def _hex_ipv4(value):
    try:
        return socket.inet_ntoa(struct.pack("<L", int(value, 16)))
    except (ValueError, OSError, struct.error):
        return value


def _read(path):
    try:
        return path.read_text(encoding="utf-8", errors="replace").strip()
    except OSError:
        return None


def _read_int(path):
    value = _read(path)
    try:
        return int(value) if value is not None else None
    except ValueError:
        return None


def _safe_int(value):
    try:
        return int(value)
    except (TypeError, ValueError):
        return None


def run(command, args):
    from canonical_argv import normalize_canonical_argv
    args = normalize_canonical_argv(args)
    handler = {
        "interfaces": cmd_interfaces,
        "routes": cmd_routes,
        "dns": cmd_dns,
        "tcp": cmd_tcp,
        "diagnose": cmd_diagnose,
    }.get(command)
    if handler is None:
        return {"error": f"unknown command: {command}"}
    try:
        return handler(args)
    except policy.PermissionDenied as denied:
        return {"error": str(denied), "denial": denied.denial}
    except policy.PolicyUnavailable as exc:
        return {"error": f"capability check failed: {exc}"}
