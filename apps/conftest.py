"""Per-apps pytest conftest.

The Python apps under ``apps/*`` call ``cos_runtime.policy.require``,
which shells out to the hidden policy bridge. In CI / dev
environments without a built ``cos`` binary on ``$PATH`` the policy
helper raises ``PolicyUnavailable`` and every functional test breaks.

This conftest writes a tiny shell stub at session start and points
``CLAW_COS_BIN`` at it so the policy helper receives a strict wire-v1
allow decision. Real cap enforcement is
covered by integration tests against the actual ``cos`` binary; the
per-app unit tests here only need a stub to exercise their own
logic.
"""

from __future__ import annotations

import os
import stat
import tempfile

import pytest


_STUB = """#!/bin/sh
# pytest cos-stub: every policy check returns allow.
case "$1:$2:$3" in
  --wire=1:__policy:check)
    echo '{"ok":true,"wire_version":1,"data":{"decision":"allow"}}'
    exit 0
    ;;
esac
echo "stub cos: unsupported subcommand: $*" 1>&2
exit 99
"""


@pytest.fixture(scope="session", autouse=True)
def _cos_stub():
    """Install a permissive ``cos`` stub for the test session."""
    if os.environ.get("CLAW_COS_BIN"):
        yield
        return
    tmpdir = tempfile.mkdtemp(prefix="cos-stub-")
    path = os.path.join(tmpdir, "cos")
    with open(path, "w") as f:
        f.write(_STUB)
    os.chmod(
        path,
        os.stat(path).st_mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH,
    )
    prev = os.environ.get("CLAW_COS_BIN")
    os.environ["CLAW_COS_BIN"] = path
    try:
        yield
    finally:
        if prev is None:
            os.environ.pop("CLAW_COS_BIN", None)
        else:
            os.environ["CLAW_COS_BIN"] = prev
        try:
            os.unlink(path)
            os.rmdir(tmpdir)
        except OSError:
            pass
