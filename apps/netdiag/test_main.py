import json
import pathlib
from decimal import Decimal
from unittest import mock

import pytest

from test_support import load_local_module


APP_DIR = pathlib.Path(__file__).parent
main = load_local_module(
    APP_DIR / "main.py",
    "claw_test_netdiag_main",
)


def test_manifest_is_mcp_only():
    manifest = json.loads((APP_DIR / "app.json").read_text(encoding="utf-8"))
    assert "operations" not in manifest
    assert [tool["name"] for tool in manifest["mcp"]["tools"]] == [
        "netdiag.interfaces",
        "netdiag.routes",
        "netdiag.dns",
        "netdiag.tcp",
        "netdiag.diagnose",
    ]
    server = (APP_DIR / "server.py").read_text(encoding="utf-8")
    assert "App.from_manifest()" in server
    assert "serve_manifest_operations" not in server
    assert "def run(" not in (APP_DIR / "main.py").read_text(encoding="utf-8")
    needs = [
        need["verb"]
        for tool in manifest["mcp"]["tools"]
        for need in tool.get("needs", [])
    ]
    assert "net.dial" not in needs
    assert needs.count("net.probe") == 2


def test_interfaces_use_exact_observation_scope():
    expected = {"interfaces": [], "count": 0}
    with mock.patch.object(main.policy, "require") as require, mock.patch.object(
        main.network_diagnostics,
        "request",
        return_value=expected,
    ) as request:
        assert main.interfaces() == expected
    require.assert_called_once_with("sys.observe", name="network")
    request.assert_called_once_with("interfaces")


def test_dns_uses_exact_target_scope():
    expected = {"resolved": True, "addresses": [{"ip": "203.0.113.10"}]}
    with mock.patch.object(main.policy, "require") as require, mock.patch.object(
        main.network_diagnostics,
        "request",
        return_value=expected,
    ) as request:
        assert main.dns("example.com") == expected
    require.assert_called_once_with("net.resolve", host="example.com")
    request.assert_called_once_with("dns", target="example.com")


def test_tcp_requires_explicit_port_and_passes_bounded_defaults():
    expected = {"reachable": True}
    with mock.patch.object(main.policy, "require") as require, mock.patch.object(
        main.network_diagnostics,
        "request",
        return_value=expected,
    ) as request:
        assert main.tcp("example.com:443") == expected
    assert require.call_args_list == [
        mock.call("net.resolve", host="example.com:443"),
        mock.call("net.probe", host="example.com:443"),
    ]
    request.assert_called_once_with(
        "tcp",
        target="example.com:443",
        attempts=3,
        timeout_ms=5000,
    )


def test_diagnose_uses_all_exact_capabilities():
    expected = {"status": "ok"}
    with mock.patch.object(main.policy, "require") as require, mock.patch.object(
        main.network_diagnostics,
        "request",
        return_value=expected,
    ) as request:
        assert main.diagnose("[2001:db8::1]:443") == expected
    assert require.call_args_list == [
        mock.call("sys.observe", name="network"),
        mock.call("net.resolve", host="[2001:db8::1]:443"),
        mock.call("net.probe", host="[2001:db8::1]:443"),
    ]
    request.assert_called_once_with("diagnose", target="[2001:db8::1]:443")


@pytest.mark.parametrize(
    "target",
    [
        "",
        " example.com",
        "https://example.com",
        "example.com/path",
        "example.com:0",
        "example.com:65536",
        "[127.0.0.1]:443",
    ],
)
def test_invalid_targets_are_rejected_before_policy(target):
    with mock.patch.object(main.policy, "require") as require, mock.patch.object(
        main.network_diagnostics,
        "request",
    ) as request:
        with pytest.raises(ValueError):
            main.dns(target)
    require.assert_not_called()
    request.assert_not_called()


def test_tcp_requires_explicit_port_before_policy():
    with mock.patch.object(main.policy, "require") as require, mock.patch.object(
        main.network_diagnostics,
        "request",
    ) as request:
        with pytest.raises(ValueError, match="explicit host:port"):
            main.tcp("example.com")
    require.assert_not_called()
    request.assert_not_called()


def test_tcp_accepts_lossless_sdk_decimal_timeout():
    with mock.patch.object(main.policy, "require"), mock.patch.object(
        main.network_diagnostics,
        "request",
        return_value={"reachable": True},
    ) as request:
        main.tcp("example.com:443", 1, Decimal("0.1"))
    request.assert_called_once_with(
        "tcp",
        target="example.com:443",
        attempts=1,
        timeout_ms=100,
    )


@pytest.mark.parametrize(
    ("attempts", "timeout"),
    [
        (0, 5),
        (True, 5),
        (6, 1),
        (1, 0),
        (1, True),
        (5, 4.1),
        (3, Decimal("6.6666")),
    ],
)
def test_invalid_probe_options_are_rejected_before_policy(attempts, timeout):
    with mock.patch.object(main.policy, "require") as require, mock.patch.object(
        main.network_diagnostics,
        "request",
    ) as request:
        with pytest.raises(ValueError):
            main.tcp("example.com:443", attempts, timeout)
    require.assert_not_called()
    request.assert_not_called()


def test_provider_failures_are_not_returned_as_success_data():
    with mock.patch.object(main.policy, "require"), mock.patch.object(
        main.network_diagnostics,
        "request",
        side_effect=main.network_diagnostics.NetworkDiagnosticsFailed("provider failed"),
    ):
        with pytest.raises(
            main.network_diagnostics.NetworkDiagnosticsFailed,
            match="provider failed",
        ):
            main.dns("example.com")
