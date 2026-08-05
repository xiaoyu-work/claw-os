import pathlib
import socket
import time
from unittest import mock

from test_support import load_local_module


main = load_local_module(
    pathlib.Path(__file__).with_name("main.py"),
    "claw_test_netdiag_main",
)


def test_target_parser_handles_host_port_and_ipv6():
    assert main._parse_target("example.com:8443")[:2] == ("example.com", 8443)
    assert main._parse_target("[::1]:8080")[:2] == ("::1", 8080)


def test_dns_authorizes_exact_target():
    answer = [(socket.AF_INET, socket.SOCK_STREAM, socket.IPPROTO_TCP, "", ("203.0.113.10", 443))]
    with mock.patch.object(main.policy, "require") as require, mock.patch.object(
        main.socket, "getaddrinfo", return_value=answer
    ):
        result = main.cmd_dns(["example.com"])
    require.assert_called_once_with("net.resolve", host="example.com")
    assert result["resolved"] is True


def test_diagnose_stops_at_dns_failure():
    with mock.patch.object(
        main,
        "cmd_interfaces",
        return_value={"interfaces": [{"name": "eth0", "operstate": "up"}]},
    ), mock.patch.object(
        main,
        "cmd_routes",
        return_value={"default_routes": [{"interface": "eth0"}]},
    ), mock.patch.object(
        main,
        "cmd_tcp",
        return_value={"resolved": False, "error": "name not found"},
    ):
        result = main.cmd_diagnose(["bad.example"])
    assert result["status"] == "critical"
    assert result["findings"][0]["stage"] == "dns"


def test_socket_creation_failure_is_structured():
    target = (socket.AF_INET6, socket.SOCK_STREAM, socket.IPPROTO_TCP, ("::1", 443, 0, 0))
    with mock.patch.object(main.socket, "socket", side_effect=OSError("IPv6 disabled")):
        result = main._connect_target(target, 1)
    assert result["ok"] is False
    assert "IPv6 disabled" in result["error"]


def test_diagnose_does_not_turn_probe_error_into_success():
    with mock.patch.object(
        main,
        "cmd_interfaces",
        return_value={"interfaces": [{"name": "eth0", "operstate": "up"}]},
    ), mock.patch.object(
        main,
        "cmd_routes",
        return_value={"default_routes": [{"interface": "eth0"}]},
    ), mock.patch.object(
        main,
        "cmd_tcp",
        return_value={"error": "probe configuration failed"},
    ):
        result = main.cmd_diagnose(["example.com"])
    assert result["status"] == "critical"
    assert result["findings"][0]["stage"] == "probe"


def test_dns_resolution_timeout_is_bounded():
    with mock.patch.object(main.socket, "getaddrinfo", side_effect=lambda *a, **k: time.sleep(1)):
        started = time.monotonic()
        with mock.patch.object(main, "DEFAULT_TIMEOUT", 0.05):
            result = main.cmd_dns(["example.com"])
    assert time.monotonic() - started < 0.5
    assert result["resolved"] is False
    assert "exceeded" in result["error"]
