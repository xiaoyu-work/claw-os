"""Path-safety helpers shared by the apps.

The single rule these helpers enforce is: *the path the kernel sees
through ``policy.require()`` must be the same path the OS will
actually touch when we call ``open()`` / ``os.unlink`` / ``os.rename``
etc.*

``os.path.abspath`` does not resolve symlinks, so a path like
``/workspace/foo`` where ``/workspace/foo`` is a symlink to
``/etc/shadow`` would pass an `fs.read /workspace/foo` cap check and
then leak ``/etc/shadow`` on ``open()``. Always run paths through
:func:`safe_realpath` *before* handing them to ``policy.require``.

Where the app declares a scope root (e.g. ``/workspace`` for the
fs app), pass it as ``scope_root`` to also reject any realpath that
escapes via ``..`` resolution or a symlink chain.
"""

from __future__ import annotations

import os
from typing import Optional


class PathOutsideScope(Exception):
    """Raised when a realpath escapes the declared scope root."""


def safe_realpath(path: str, scope_root: Optional[str] = None) -> str:
    """Return ``os.path.realpath(path)``; optionally enforce that the
    result lies inside ``scope_root``.

    ``scope_root`` is itself realpath-resolved before the containment
    check so a symlink chain on the scope root does not defeat the
    comparison. ``os.path.commonpath`` is used (not naive prefix
    matching) so ``/foo/barbaz`` is not accepted as "inside ``/foo/bar``".
    """
    if not isinstance(path, str):
        raise TypeError(f"path must be a string, got {type(path).__name__}")
    resolved = os.path.realpath(path)
    if scope_root is None:
        return resolved
    root = os.path.realpath(scope_root)
    # os.path.commonpath raises ValueError on mixed-drive / empty input;
    # treat those as "outside scope".
    try:
        common = os.path.commonpath([root, resolved])
    except ValueError as exc:
        raise PathOutsideScope(
            f"path {path!r} (realpath {resolved!r}) is outside scope {root!r}"
        ) from exc
    if common != root:
        raise PathOutsideScope(
            f"path {path!r} (realpath {resolved!r}) is outside scope {root!r}"
        )
    return resolved
