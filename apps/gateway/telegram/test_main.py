"""Tests for the telegram gateway: sender allowlist + rate limiting.

We exercise ``_ask_agent`` end-to-end with the kernel policy stubbed
and ``cos agent ask`` short-circuited to a fake subprocess result, so
the test focuses on the *gate* logic (sender allowlist, rate limiter,
policy.require) rather than the subprocess plumbing.
"""

from __future__ import annotations

import importlib.util
import os
import sys
import unittest

# Make ``apps/gateway/`` importable so ``from _shared import …`` works.
sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
# Make the cos-runtime package importable.
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
    spec = importlib.util.spec_from_file_location("gateway_telegram_main", path)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


main = _load_main()
from _shared import inbound, safe_egress, safe_subprocess  # noqa: E402

try:
    from cos_runtime import policy as _cos_policy  # type: ignore[import-not-found]
except Exception:  # pragma: no cover
    _cos_policy = None


class _AllowingPolicy:
    def require(self, verb_id, host=None, name=None, path=None, wild=False):
        return None


class _FakeProc:
    def __init__(self, stdout="ok", stderr="", returncode=0):
        self.stdout = stdout
        self.stderr = stderr
        self.returncode = returncode


class TelegramSenderAllowlistTests(unittest.TestCase):
    """``_ask_agent`` must reject senders not in
    ``COS_TELEGRAM_ALLOWED_CHATS``."""

    def setUp(self):
        self._saved_env = {
            k: os.environ.get(k) for k in (main.ENV_ALLOWED_CHATS, main.ENV_RPM)
        }
        self._orig_policy = safe_egress.policy
        safe_egress.policy = _AllowingPolicy()
        # Also bypass the kernel-side policy gate used by
        # ``_ask_agent`` directly (``from cos_runtime import policy``).
        if _cos_policy is not None:
            self._orig_cos_require = _cos_policy.require
            _cos_policy.require = lambda *a, **kw: None
        else:
            self._orig_cos_require = None
        main._reset_rate_limiter_for_tests()

    def tearDown(self):
        for k, v in self._saved_env.items():
            if v is None:
                os.environ.pop(k, None)
            else:
                os.environ[k] = v
        safe_egress.policy = self._orig_policy
        if _cos_policy is not None and self._orig_cos_require is not None:
            _cos_policy.require = self._orig_cos_require
        main._reset_rate_limiter_for_tests()

    def test_empty_allowlist_rejects_everyone(self):
        """No env var set ⇒ nobody is allowed."""
        os.environ.pop(main.ENV_ALLOWED_CHATS, None)
        with self.assertRaises(inbound.SenderNotAllowed):
            main._ask_agent(42, "hi")

    def test_unlisted_sender_rejected(self):
        os.environ[main.ENV_ALLOWED_CHATS] = "1,2,3"
        with self.assertRaises(inbound.SenderNotAllowed):
            main._ask_agent(42, "hi")

    def test_listed_sender_passes_allowlist(self):
        """An allowlisted sender clears the gate (and would reach
        ``safe_subprocess`` if we didn't stub it)."""
        os.environ[main.ENV_ALLOWED_CHATS] = "42"
        # Stub the subprocess call so we don't shell out to `cos`.
        orig = safe_subprocess.safe_subprocess
        safe_subprocess.safe_subprocess = lambda *a, **kw: _FakeProc("hi back")
        try:
            reply = main._ask_agent(42, "ping")
        finally:
            safe_subprocess.safe_subprocess = orig
        self.assertEqual(reply, "hi back")


class TelegramRateLimitTests(unittest.TestCase):
    """Allowlisted sender, but pummeling the gateway should trip
    ``inbound.RateLimited`` once the per-minute budget is gone."""

    def setUp(self):
        self._saved_env = {
            k: os.environ.get(k) for k in (main.ENV_ALLOWED_CHATS, main.ENV_RPM)
        }
        os.environ[main.ENV_ALLOWED_CHATS] = "42"
        os.environ[main.ENV_RPM] = "5"
        self._orig_policy = safe_egress.policy
        safe_egress.policy = _AllowingPolicy()
        if _cos_policy is not None:
            self._orig_cos_require = _cos_policy.require
            _cos_policy.require = lambda *a, **kw: None
        else:
            self._orig_cos_require = None
        main._reset_rate_limiter_for_tests()
        # Stub the subprocess so the rate-limiter gate is what we
        # are actually testing.
        self._orig_subp = safe_subprocess.safe_subprocess
        safe_subprocess.safe_subprocess = lambda *a, **kw: _FakeProc("ok")

    def tearDown(self):
        safe_subprocess.safe_subprocess = self._orig_subp
        for k, v in self._saved_env.items():
            if v is None:
                os.environ.pop(k, None)
            else:
                os.environ[k] = v
        safe_egress.policy = self._orig_policy
        if _cos_policy is not None and self._orig_cos_require is not None:
            _cos_policy.require = self._orig_cos_require
        main._reset_rate_limiter_for_tests()

    def test_sixth_call_within_minute_is_rate_limited(self):
        # First 5 calls succeed.
        for _ in range(5):
            reply = main._ask_agent(42, "ping")
            self.assertEqual(reply, "ok")
        # 6th call within the same minute must trip RateLimited.
        with self.assertRaises(inbound.RateLimited):
            main._ask_agent(42, "ping")


if __name__ == "__main__":
    unittest.main()
