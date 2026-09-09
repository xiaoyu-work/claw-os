"""Cancellable, explicit-binary transport for controlled OS primitives.

This is a transport, not an authority source. The CLI inherits the real
broker session; stdin carries only business data. Cancellation cannot undo
an operation already accepted by the OS, so callers must not retry mutations
automatically after an indeterminate transport failure.
"""

from __future__ import annotations

import os
import subprocess
import time
from collections.abc import Callable, Sequence
from typing import Any

from ._transport import (
    WIRE_ARG,
    WireDenied as KernelDenied,
    WireUnavailable as KernelUnavailable,
    decode_response,
)

MAX_INPUT_BYTES = 1024 * 1024
MAX_RESPONSE_BYTES = 4 * 1024 * 1024
__all__ = ["KernelDenied", "KernelUnavailable", "call_json_with_stdin_binary"]


def call_json_with_stdin_binary(
    binary: str,
    args: Sequence[str],
    data: bytes,
    *,
    deadline_unix_ms: int,
    check_cancelled: Callable[[], None] | None = None,
) -> dict[str, Any]:
    """Call an absolute installed CLI with bounded stdin and wire-v1 decoding.

    ``args`` includes the complete primitive argv, without ``--wire=1``.
    The deadline is absolute and is never extended (the transport also has a
    sixty-second ceiling). MCP callers pass ``current_context().raise_if_cancelled``;
    cancellation/deadline failure kills and reaps the CLI child.
    No PATH lookup, shell, environment mutation or duplicate wire codec is used.
    """
    if not isinstance(binary, str) or not os.path.isabs(binary):
        raise ValueError("kernel binary must be an absolute path")
    if isinstance(args, (str, bytes)) or any(not isinstance(arg, str) for arg in args):
        raise ValueError("kernel arguments must be a sequence of strings")
    if not isinstance(data, bytes) or len(data) > MAX_INPUT_BYTES:
        raise ValueError("kernel stdin must be bounded bytes")
    if type(deadline_unix_ms) is not int or deadline_unix_ms <= 0:
        raise ValueError("kernel deadline must be a positive integer")
    label = "controlled OS primitive"
    end_ms = min(deadline_unix_ms, time.time_ns() // 1_000_000 + 60_000)
    monotonic_end = time.monotonic() + (end_ms - time.time_ns() // 1_000_000) / 1000

    def remaining() -> float:
        if check_cancelled is not None:
            check_cancelled()
        seconds = min(
            (end_ms - time.time_ns() // 1_000_000) / 1000,
            monotonic_end - time.monotonic(),
        )
        if seconds <= 0:
            raise KernelUnavailable(f"{label} deadline expired; any accepted effect is not undone")
        return seconds

    remaining()
    try:
        process = subprocess.Popen(
            [binary, WIRE_ARG, *args],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
    except OSError as error:
        raise KernelUnavailable(f"cannot start {label}: {error}") from error
    try:
        pending: bytes | None = data
        while True:
            try:
                stdout, _stderr = process.communicate(pending, timeout=min(remaining(), 0.05))
                break
            except subprocess.TimeoutExpired:
                pending = None
        remaining()
        if len(stdout) > MAX_RESPONSE_BYTES:
            raise KernelUnavailable(f"{label} response exceeds the transport limit")
        try:
            text = stdout.decode("utf-8")
        except UnicodeDecodeError as error:
            raise KernelUnavailable(f"{label} returned invalid UTF-8") from error
        return decode_response(text, process.returncode, label)
    except OSError as error:
        raise KernelUnavailable(f"{label} communication failed; outcome is indeterminate") from error
    finally:
        if process.poll() is None:
            process.kill()
        process.communicate()
