"""Strict subprocess wire-v1 response decoding."""

from __future__ import annotations

import json
from typing import Any, Dict, Mapping

from .generated import WireDecodeError, decode_wire_json, validate_envelope


WIRE_ARG = "--wire=1"


class WireUnavailable(RuntimeError):
    """The kernel did not produce a valid wire-v1 response."""


class WireDenied(RuntimeError):
    """The kernel returned a valid wire-v1 error envelope."""

    def __init__(self, payload: Mapping[str, Any]):
        self.payload: Dict[str, Any] = dict(payload)
        super().__init__(str(self.payload["error"]))


def decode_response(text: str, status: int, label: str) -> Dict[str, Any]:
    if not text:
        raise WireUnavailable(f"{label} returned no wire response (exit {status})")
    try:
        envelope = decode_wire_json(text)
    except (json.JSONDecodeError, ValueError) as error:
        raise WireUnavailable(f"{label} returned an invalid wire envelope: {error}") from error
    try:
        validate_envelope(envelope)
    except WireDecodeError as error:
        raise WireUnavailable(f"{label} returned an invalid wire envelope: {error}") from error

    if envelope["ok"]:
        if status != 0:
            raise WireUnavailable(
                f"{label} returned a success envelope with exit {status}"
            )
        return dict(envelope["data"])

    if status == 0:
        raise WireUnavailable(f"{label} returned an error envelope with exit 0")
    raise WireDenied(envelope)
