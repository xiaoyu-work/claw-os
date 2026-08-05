"""Tests for the webhook gateway: policy-gating and redirect-SSRF blocking."""

from __future__ import annotations

import io
import os
import sys
import unittest
import urllib.error
import urllib.request

from test_support import load_local_module

# Make ``apps/gateway/`` importable so ``from _shared import …`` works
# when we drive ``main`` directly.
sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
# Make the cos-runtime stub importable (for the rare case it's present).
sys.path.insert(
    0,
    os.path.join(
        os.path.dirname(__file__), os.pardir, os.pardir, os.pardir,
        "cos-runtime", "python", "src",
    ),
)


def _load_main():
    """Load this gateway's main.py under a unique module name so it
    can coexist with the other gateway test modules in one pytest run."""
    path = os.path.join(os.path.dirname(__file__), "main.py")
    return load_local_module(
        path,
        "gateway_webhook_main",
        clear_modules=("_shared",),
    )


main = _load_main()
from _shared import safe_egress  # noqa: E402


class _DenyingPolicy:
    """A policy stub whose ``require`` always denies."""

    class PermissionDenied(Exception):
        pass

    def require(self, verb_id, host=None, name=None, path=None, wild=False):
        exc = self.PermissionDenied(f"deny {verb_id} -> {host}")
        exc.denial = {"verb": verb_id, "host": host, "decision": "deny"}
        raise exc


class _AllowingPolicy:
    """A policy stub whose ``require`` is a no-op."""

    def require(self, verb_id, host=None, name=None, path=None, wild=False):
        return None


class WebhookPolicyDenialTests(unittest.TestCase):
    """When the kernel denies the verb, no bytes hit the wire."""

    def setUp(self):
        self._orig_policy = safe_egress.policy

    def tearDown(self):
        safe_egress.policy = self._orig_policy

    def test_send_surfaces_permission_denial(self):
        safe_egress.policy = _DenyingPolicy()
        # Use a public host so we don't trip the private-IP check
        # before reaching the policy gate.
        result = main._send(
            target="https://example.com/hook",
            text="hello",
            raw=False,
            bearer=None,
            basic=None,
            api_key=None,
            hmac_secret=None,
        )
        self.assertFalse(result["ok"])
        # The webhook gateway forwards either "permission denied"
        # (kernel denial) or "egress blocked" (if the policy module
        # is missing). Either path means we did NOT actually POST.
        err = result.get("error", "")
        self.assertTrue(
            "permission denied" in err or "egress blocked" in err
            or result.get("denial") is not None,
            f"unexpected error shape: {result!r}",
        )


class RedirectSSRFTests(unittest.TestCase):
    """A 30x response must NOT cause urllib to follow Location:.

    This is the core defence against redirect-SSRF: an attacker who
    controls a hooks.example.com endpoint must not be able to chain
    us into ``http://169.254.169.254/latest/meta-data/`` via a 302.
    """

    def setUp(self):
        self._orig_policy = safe_egress.policy
        safe_egress.policy = _AllowingPolicy()
        # Pre-resolve example.com to a non-private address by setting
        # the env override OFF (default).
        os.environ.pop("COS_GATEWAY_ALLOW_PRIVATE", None)

    def tearDown(self):
        safe_egress.policy = self._orig_policy

    def test_redirect_handler_returns_none(self):
        """The opener's redirect handler MUST short-circuit."""
        handler = safe_egress._NoRedirectHandler()
        # Build a fake 302 — the redirect_request return value of
        # None is what causes urllib to raise HTTPError with the 302
        # rather than chase the Location header.
        result = handler.redirect_request(
            req=None,
            fp=io.BytesIO(b""),
            code=302,
            msg="Found",
            headers={"Location": "http://169.254.169.254/latest/meta-data/"},
            newurl="http://169.254.169.254/latest/meta-data/",
        )
        self.assertIsNone(result)

    def test_opener_has_no_redirect_handler_subclass(self):
        """The module-level opener uses _NoRedirectHandler, not
        the stdlib HTTPRedirectHandler."""
        # The opener stores its handlers in ``handlers``.
        redirect_handlers = [
            h for h in safe_egress._OPENER.handlers
            if isinstance(h, urllib.request.HTTPRedirectHandler)
        ]
        # The base HTTPRedirectHandler may show up because
        # _NoRedirectHandler subclasses it. Verify ALL redirect
        # handlers are our no-op subclass.
        for h in redirect_handlers:
            self.assertIsInstance(h, safe_egress._NoRedirectHandler)


class PrivateHostBlockingTests(unittest.TestCase):
    """RFC1918 / loopback / link-local targets must be refused."""

    def setUp(self):
        self._orig_policy = safe_egress.policy
        safe_egress.policy = _AllowingPolicy()
        os.environ.pop("COS_GATEWAY_ALLOW_PRIVATE", None)

    def tearDown(self):
        safe_egress.policy = self._orig_policy

    def test_imds_ip_is_blocked(self):
        with self.assertRaises(safe_egress.EgressBlocked):
            safe_egress.safe_urlopen(
                "GET",
                "http://169.254.169.254/latest/meta-data/",
                verb_id="net.dial",
            )

    def test_loopback_is_blocked(self):
        with self.assertRaises(safe_egress.EgressBlocked):
            safe_egress.safe_urlopen(
                "GET",
                "http://127.0.0.1:8080/admin",
                verb_id="net.dial",
            )

    def test_file_scheme_is_blocked(self):
        with self.assertRaises(safe_egress.EgressBlocked):
            safe_egress.safe_urlopen(
                "GET",
                "file:///etc/passwd",
                verb_id="net.dial",
            )


if __name__ == "__main__":
    unittest.main()
