"""Permission helper for Claw OS Python apps.

Every Python app should import this module and call ``policy.require()``
at the top of every operation that touches files, the network,
secrets, or anything else the kernel knows how to gate. The helper
shells out to the hidden policy bridge — the kernel's authoritative
enforcement entry point — so the answer here is exactly the same as
the answer the Rust side would give.

Typical usage::

    from cos_runtime import policy

    def handle_rm(args):
        policy.require("fs.delete", path=args["path"])
        os.remove(args["path"])

If the user has not granted the requested capability, ``require()``
raises :class:`PermissionDenied`, which the app's top-level driver
turns into a structured JSON error for the agent.

Why a subprocess and not an in-process check?
---------------------------------------------

The Python app already runs inside a process the kernel spawned with
``COS_SESSION`` set, so the subprocess call inherits the session and
PID-ancestry context the kernel needs to validate the request.
Centralising the decision in the hidden policy bridge keeps the Python
helper tiny, removes any risk of the rules drifting between Rust and
Python, and gives audit / logging a single chokepoint.
"""

from __future__ import annotations

import os
import shutil
import subprocess
from typing import Any, Mapping, Optional

from claw_os_sdk._transport import (
    WIRE_ARG,
    WireDenied,
    WireUnavailable,
    decode_response,
)


# Subprocess timeout for every shell-out to the hidden policy bridge. A hung
# child translates to PolicyUnavailable, which the caller can decide
# to treat as deny or warn-and-continue.
_DEFAULT_TIMEOUT_S = 60


# ---------------------------------------------------------------------------
# Exceptions
# ---------------------------------------------------------------------------


class PolicyError(Exception):
    """Base class for every error this module raises."""


class PermissionDenied(PolicyError):
    """The kernel refused the requested capability.

    ``denial`` holds the structured envelope returned by
    policy bridge — verb, requested scope, granted scopes,
    reason, hint — and is suitable for forwarding straight back to
    the caller as JSON.
    """

    def __init__(self, denial: Mapping[str, Any]):
        self.denial = dict(denial)
        super().__init__(self.denial.get("summary") or "permission denied")


class PolicyUnavailable(PolicyError):
    """The ``cos`` binary could not be invoked or returned garbage.

    This is distinct from :class:`PermissionDenied` so callers can
    decide whether a missing kernel implies "deny" (production) or
    "warn and continue" (dev tooling).
    """


# ---------------------------------------------------------------------------
# Public API
# ---------------------------------------------------------------------------


def require(
    verb: str,
    *,
    path: Optional[str] = None,
    host: Optional[str] = None,
    name: Optional[str] = None,
    self_ref: Optional[str] = None,
    wild: bool = False,
) -> None:
    """Check whether the current session may exercise ``verb``.

    Exactly one of ``path`` / ``host`` / ``name`` / ``self_ref`` /
    ``wild`` should be supplied. Unscoped verbs (``ui.notify``,
    ``time.delay`` …) take no keyword argument.

    Raises :class:`PermissionDenied` on deny, :class:`PolicyUnavailable`
    when the kernel cannot be reached. Returns ``None`` on allow.
    """
    decision = check(
        verb,
        path=path,
        host=host,
        name=name,
        self_ref=self_ref,
        wild=wild,
    )
    if decision.get("decision") != "allow":
        raise PermissionDenied(decision)


def check(
    verb: str,
    *,
    path: Optional[str] = None,
    host: Optional[str] = None,
    name: Optional[str] = None,
    self_ref: Optional[str] = None,
    wild: bool = False,
) -> dict:
    """Same as :func:`require` but returns the raw envelope instead of
    raising. Useful when an app wants to surface a "would-be-denied"
    notice without aborting.
    """
    cmd = [_cos_binary(), WIRE_ARG, "__policy", "check", verb]
    cmd.extend(_scope_flag(path=path, host=host, name=name, self_ref=self_ref, wild=wild))

    try:
        proc = subprocess.run(
            cmd,
            capture_output=True,
            text=True,
            check=False,
            timeout=_DEFAULT_TIMEOUT_S,
        )
    except subprocess.TimeoutExpired as exc:
        raise PolicyUnavailable(
            f"policy check timed out after {_DEFAULT_TIMEOUT_S}s"
        ) from exc

    try:
        decision = decode_response(
            (proc.stdout or "").strip(),
            proc.returncode,
            "policy check",
        )
    except (WireDenied, WireUnavailable) as error:
        raise PolicyUnavailable(str(error)) from error

    if "decision" not in decision:
        raise PolicyUnavailable("policy check response omitted `decision`")
    return decision


# ---------------------------------------------------------------------------
# Internals
# ---------------------------------------------------------------------------


def _scope_flag(
    *,
    path: Optional[str],
    host: Optional[str],
    name: Optional[str],
    self_ref: Optional[str],
    wild: bool,
) -> list:
    supplied = [
        ("--path", path),
        ("--host", host),
        ("--name", name),
        ("--self", self_ref),
    ]
    supplied = [(flag, val) for flag, val in supplied if val is not None]
    if wild:
        supplied.append(("--wild", None))
    if len(supplied) > 1:
        raise TypeError(
            "require()/check() accept at most one of path / host / name / self_ref / wild"
        )
    if not supplied:
        return []
    flag, val = supplied[0]
    return [flag] if val is None else [flag, val]


def _cos_binary() -> str:
    """Locate the ``cos`` binary. Honours ``CLAW_COS_BIN`` for tests."""
    override = os.environ.get("CLAW_COS_BIN")
    if override:
        return override
    found = shutil.which("cos")
    if found is None:
        raise PolicyUnavailable(
            "the `cos` binary is not on PATH; cannot enforce permissions"
        )
    return found
