"""Deprecated import path for the Claw OS App MCP runtime.

New Apps import :mod:`claw_os_sdk.mcp`. This module remains only while
bundled ``session`` manifests migrate to the MCP-first App contract.
"""

from . import mcp as _mcp
from .mcp import *  # noqa: F403
from .mcp import __all__

_read_bounded_line = _mcp._read_bounded_line
