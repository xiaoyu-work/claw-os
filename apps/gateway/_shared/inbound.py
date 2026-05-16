"""Inbound-side helpers for gateway apps.

Two classes of risk we want to keep out of every individual gateway:

1. **Anyone-can-DM-the-bot drives the agent.** Without a sender
   allowlist, the public side of a chat platform turns into a
   wide-open jailbreak for whatever ``cos agent ask`` happens to be
   wired up to. :func:`verify_sender` enforces an allowlist sourced
   from an env var (comma-separated IDs).

2. **One excited / hostile sender pegs the agent.** Even an allowed
   sender shouldn't be able to spam unlimited prompts at the agent.
   :class:`TokenBucket` is a tiny in-process rate limiter (5 calls
   per 60s per sender by default).

We also surface :func:`verify_hmac` for gateways that ingest signed
webhooks (Slack ``X-Slack-Signature``, GitHub ``X-Hub-Signature-256``,
…). The verification is constant-time via :func:`hmac.compare_digest`.
"""

from __future__ import annotations

import hashlib
import hmac
import os
import threading
import time
from typing import Iterable, Optional


class SenderNotAllowed(Exception):
    """The inbound sender is not in the configured allowlist."""


class RateLimited(Exception):
    """The inbound sender is over their token bucket budget."""


def _parse_allowlist(raw: Optional[str]) -> set[str]:
    if not raw:
        return set()
    return {s.strip() for s in raw.split(",") if s.strip()}


def verify_sender(
    sender_id: object,
    allowlist_env_var: str,
    *,
    extra_allowlist: Iterable[str] = (),
) -> None:
    """Raise :class:`SenderNotAllowed` if ``sender_id`` isn't allowed.

    The allowlist is read fresh on every call out of the env var
    named by ``allowlist_env_var`` (comma-separated). Tests can also
    pass an in-process ``extra_allowlist``.

    Empty / unset allowlist == nobody allowed. Gateways that genuinely
    want "any allowed sender" must opt in by setting the env var to a
    wildcard token ``*`` (which we treat as accept-all).
    """
    if sender_id is None:
        raise SenderNotAllowed("sender id missing")
    s = str(sender_id).strip()
    if not s:
        raise SenderNotAllowed("sender id empty")
    allowed = _parse_allowlist(os.environ.get(allowlist_env_var))
    allowed.update(extra_allowlist)
    if "*" in allowed:
        return
    if s not in allowed:
        raise SenderNotAllowed(
            f"sender {s!r} not in allowlist {allowlist_env_var}"
        )


# ---------------------------------------------------------------------------
# Token-bucket rate limiter.
# ---------------------------------------------------------------------------


class TokenBucket:
    """Per-key token bucket. Thread-safe.

    Default budget is 5 tokens / 60 seconds, which matches the
    telegram gateway requirement. The bucket is process-local: this
    is the simplest thing that works for a single-process long-poll
    loop. A cluster-wide limiter would need an external store.
    """

    def __init__(self, capacity: int = 5, refill_seconds: float = 60.0):
        if capacity <= 0:
            raise ValueError("capacity must be positive")
        if refill_seconds <= 0:
            raise ValueError("refill_seconds must be positive")
        self.capacity = capacity
        # Refill rate is "1 token every refill_seconds/capacity".
        # We track an integer + last-refill timestamp per key.
        self._refill_seconds = float(refill_seconds)
        self._state: dict[str, tuple[float, float]] = {}
        self._lock = threading.Lock()

    def _refill(self, key: str, now: float) -> float:
        tokens, last = self._state.get(key, (float(self.capacity), now))
        elapsed = max(0.0, now - last)
        # Refill: add (elapsed / window) * capacity tokens, capped.
        added = (elapsed / self._refill_seconds) * self.capacity
        tokens = min(float(self.capacity), tokens + added)
        self._state[key] = (tokens, now)
        return tokens

    def try_consume(self, key: str, cost: float = 1.0) -> bool:
        """Try to take ``cost`` tokens from ``key``'s bucket.

        Returns True on success (bucket had enough), False otherwise.
        Never blocks.
        """
        with self._lock:
            now = time.monotonic()
            tokens = self._refill(key, now)
            if tokens >= cost:
                self._state[key] = (tokens - cost, now)
                return True
            return False

    def peek(self, key: str) -> float:
        """Return the current token count for ``key`` (debug aid)."""
        with self._lock:
            now = time.monotonic()
            return self._refill(key, now)


# ---------------------------------------------------------------------------
# HMAC signature verification.
# ---------------------------------------------------------------------------


def verify_hmac(
    body: bytes,
    *,
    secret: str,
    expected_sig: str,
    algo: str = "sha256",
    prefix: str = "",
) -> bool:
    """Constant-time HMAC verification.

    Args:
        body:         The raw request body bytes — sign-then-encrypt
                      schemes always sign the unmodified bytes; do
                      not re-serialise.
        secret:       Shared HMAC key.
        expected_sig: The signature string as received over the wire,
                      *including* any leading prefix (``sha256=…``,
                      ``v0=…``). The function strips ``prefix`` first.
        algo:         Digest algorithm name (``hashlib``-compatible).
        prefix:       Optional prefix to strip from ``expected_sig``
                      before comparison (e.g. ``"sha256="``,
                      ``"v0="``).

    Returns:
        True iff the signature matches. False on any mismatch —
        including bad prefix, bad hex, wrong digest.
    """
    if not secret or not expected_sig:
        return False
    if prefix:
        if not expected_sig.startswith(prefix):
            return False
        expected_sig = expected_sig[len(prefix):]
    try:
        digestmod = getattr(hashlib, algo)
    except AttributeError:
        return False
    mac = hmac.new(secret.encode("utf-8"), body, digestmod).hexdigest()
    try:
        return hmac.compare_digest(mac, expected_sig.lower())
    except (TypeError, ValueError):
        return False


__all__ = [
    "SenderNotAllowed",
    "RateLimited",
    "verify_sender",
    "TokenBucket",
    "verify_hmac",
]
