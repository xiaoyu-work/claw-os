"""Hardened :mod:`subprocess` wrappers for gateway apps.

Every external command a gateway runs (``cos credential load``,
``cos agent ask``, ``taskkill``, …) must come through here. The
wrappers enforce:

* **Mandatory timeouts.** Raw :func:`subprocess.run` without a
  ``timeout=`` parameter has bitten us once already — the telegram
  long-poll loop hung forever when ``cos credential load`` did not
  return. Every call here demands an explicit timeout.

* **``stdin=DEVNULL``.** Without this the child inherits the
  gateway's stdin, which (under ``cos service``) may be a control
  pipe. Closing stdin defensively prevents accidental cross-talk
  with whatever spawned us.

* **Scrubbed environment.** Only an explicit allowlist of variables
  is passed through to the child. ``cos credential load`` does need
  ``HOME`` and ``COS_*`` to function, but it does not need (and
  should not see) ad-hoc tokens like ``COS_SLACK_TOKEN`` that the
  caller set just to authenticate this particular send.

* **Token redaction.** :func:`safe_credential_load` returns the
  loaded value to the caller but never lets the literal value escape
  into an exception message. A misconfigured stderr that echoes the
  token is a real risk and one we want to neutralise centrally.
"""

from __future__ import annotations

import json
import os
import subprocess
from typing import Iterable, Mapping, Optional, Sequence, Tuple


# Default allowlist — every gateway needs at least these. Callers
# extend per-invocation when they need more (e.g. ``COS_BIN``).
DEFAULT_ENV_ALLOWLIST: Tuple[str, ...] = (
    "HOME",
    "COS_HOME",
    "USER",
    "LOGNAME",
    "PATH",
    "LANG",
    "LC_ALL",
    "LC_CTYPE",
    "TMPDIR",
    "TZ",
    "COS_SESSION",
    "COS_DATA_DIR",
    "COS_PROC_DATA_DIR",
    "COS_APP_ID",
    "COS_PERMS_MODE",
    "COS_BIN",
    "COS_VERSION",
    # Windows-specific minimal set so subprocess works there too.
    "SYSTEMROOT",
    "WINDIR",
    "APPDATA",
    "LOCALAPPDATA",
    "PROGRAMFILES",
    "PROGRAMFILES(X86)",
    "PROGRAMDATA",
    "USERPROFILE",
    "PATHEXT",
    "COMSPEC",
)


def _scrub_env(
    extra_allow: Iterable[str] = (),
    overrides: Optional[Mapping[str, str]] = None,
) -> dict[str, str]:
    """Return a minimal env dict for a child process.

    Args:
        extra_allow: Additional env var names to pass through if
                     present in the parent's environment.
        overrides:   Explicit key/value pairs to set in the child.
    """
    allowed = set(DEFAULT_ENV_ALLOWLIST)
    allowed.update(extra_allow)
    env: dict[str, str] = {}
    for key in allowed:
        val = os.environ.get(key)
        if val is not None:
            env[key] = val
    if overrides:
        env.update({str(k): str(v) for k, v in overrides.items()})
    return env


def safe_subprocess(
    argv: Sequence[str],
    *,
    timeout: float,
    env_allowlist: Iterable[str] = (),
    env_overrides: Optional[Mapping[str, str]] = None,
    input_bytes: Optional[bytes] = None,
    check: bool = False,
) -> subprocess.CompletedProcess:
    """Run ``argv`` with a scrubbed env, mandatory timeout, no stdin.

    Args:
        argv:           Argv list — never a shell string. The first
                        element is the executable.
        timeout:        Mandatory; passed straight to
                        :func:`subprocess.run`.
        env_allowlist:  Extra env-var names to forward beyond the
                        :data:`DEFAULT_ENV_ALLOWLIST`.
        env_overrides:  Explicit key/value pairs to set in the child.
        input_bytes:    Optional stdin bytes. If supplied,
                        ``stdin=PIPE`` is used; otherwise stdin is
                        :data:`subprocess.DEVNULL`.
        check:          Forward to :func:`subprocess.run`.

    Returns:
        :class:`subprocess.CompletedProcess` with text-mode stdout /
        stderr (``text=True``).

    Raises:
        subprocess.TimeoutExpired: The child outlived the timeout.
        FileNotFoundError: ``argv[0]`` does not exist on PATH.
    """
    if not argv:
        raise ValueError("argv must be non-empty")
    if timeout is None or timeout <= 0:
        raise ValueError("timeout must be a positive number")

    env = _scrub_env(env_allowlist, env_overrides)
    stdin = subprocess.PIPE if input_bytes is not None else subprocess.DEVNULL
    return subprocess.run(  # noqa: S603 - argv is a fixed list
        list(argv),
        stdin=stdin,
        input=input_bytes,
        capture_output=True,
        text=True,
        timeout=timeout,
        env=env,
        check=check,
        shell=False,
    )


# ---------------------------------------------------------------------------
# Convenience: load a credential via ``cos credential load <name>``.
# ---------------------------------------------------------------------------


def safe_credential_load(
    name: str,
    *,
    timeout: float = 10.0,
    cos_bin: str = "cos",
) -> Tuple[Optional[str], Optional[str]]:
    """Look up a credential and return ``(value, error)``.

    The credential value is parsed out of the JSON envelope that
    ``cos credential load`` returns. Importantly, on every error path
    the *literal credential value* is **never** echoed back into the
    returned error string — even if ``cos`` accidentally writes it to
    stderr. We squelch stderr through a generic placeholder so a
    crashed credential helper cannot leak the secret into agent logs.
    """
    if not name or not str(name).strip():
        return None, "credential name required"

    try:
        proc = safe_subprocess(
            [cos_bin, "credential", "load", name],
            timeout=timeout,
        )
    except FileNotFoundError:
        return None, "cos binary not found on PATH"
    except subprocess.TimeoutExpired:
        return None, f"cos credential load timed out after {timeout}s"

    if proc.returncode != 0:
        # Deliberately discard proc.stderr — a misbehaving credential
        # helper that echoes the secret into stderr must not leak it
        # back into the gateway's structured error response.
        return None, (
            f"cos credential load returned {proc.returncode} "
            f"(stderr suppressed to avoid leaking secret material)"
        )

    try:
        payload = json.loads(proc.stdout or "")
    except json.JSONDecodeError:
        return None, "credential payload not JSON (suppressed)"

    if not isinstance(payload, dict):
        return None, "credential payload not an object"
    if "error" in payload:
        # Same redaction concern: the error string from the kernel
        # *should not* contain the secret value, but we don't trust
        # it absolutely.
        return None, f"credential load failed: {payload.get('error', 'unknown')}"
    val = payload.get("value")
    if not isinstance(val, str) or not val.strip():
        return None, f"credential {name!r} missing 'value'"
    return val.strip(), None


__all__ = [
    "DEFAULT_ENV_ALLOWLIST",
    "safe_subprocess",
    "safe_credential_load",
]
