"""Pure, canonical references to App-owned objects; never discovery or dispatch."""

from __future__ import annotations

import re
import unicodedata
from urllib.parse import quote, unquote_to_bytes

from .generated import ObjectRef, WireDecodeError, validate_object_ref

__all__ = ["ObjectRef", "ObjectRefError", "format_reference", "parse_reference"]

_COMPONENT = re.compile(r"[a-z][a-z0-9_-]*")
_QUERY_VALUE = re.compile(r"(?:[A-Za-z0-9._~-]|%[A-Fa-f0-9]{2})*")
_MAX_URI_BYTES = 4096


class ObjectRefError(ValueError):
    """An invalid object-reference shape, component, or canonical URI."""


def _utf8_length(value: str) -> int:
    try:
        return len(value.encode("utf-8", errors="strict"))
    except UnicodeEncodeError as error:
        raise ObjectRefError("object reference text must be valid UTF-8") from error


def _validate(value: ObjectRef) -> None:
    try:
        validate_object_ref(value)
    except WireDecodeError as error:
        raise ObjectRefError(str(error)) from error
    for field, text, limit in (
        ("app_id", value["app_id"], 128),
        ("object_type", value["object_type"], 64),
    ):
        if len(text) > limit or _COMPONENT.fullmatch(text) is None:
            raise ObjectRefError(
                f"{field} must be a lowercase ASCII component of at most {limit} bytes"
            )
    for field, text, limit in (
        ("object_id", value["object_id"], 1024),
        ("revision", value.get("revision"), 128),
    ):
        if text is None:
            continue
        if (
            not text
            or len(text) > limit
            or _utf8_length(text) > limit
            or any(unicodedata.category(character) == "Cc" for character in text)
        ):
            raise ObjectRefError(
                f"{field} must contain 1..={limit} UTF-8 bytes without control characters"
            )


def format_reference(value: ObjectRef) -> str:
    """Format an identifier without trimming it, resolving data, or granting access."""
    _validate(value)
    uri = (
        f"app://{value['app_id']}/{value['object_type']}?id="
        + quote(value["object_id"], safe="-._~", encoding="utf-8", errors="strict")
    )
    if "revision" in value:
        uri += "&revision=" + quote(
            value["revision"], safe="-._~", encoding="utf-8", errors="strict"
        )
    if len(uri) > _MAX_URI_BYTES:
        raise ObjectRefError("URI exceeds 4096 UTF-8 bytes")
    return uri


def _decode(value: str) -> str:
    if _QUERY_VALUE.fullmatch(value) is None:
        raise ObjectRefError("query values must use RFC3986 percent encoding")
    try:
        return unquote_to_bytes(value).decode("utf-8", errors="strict")
    except UnicodeDecodeError as error:
        raise ObjectRefError("query value is not valid UTF-8") from error


def parse_reference(value: str) -> ObjectRef:
    """Parse only canonical app:// references, without any I/O."""
    if not isinstance(value, str):
        raise ObjectRefError("URI must be a string")
    if len(value) > _MAX_URI_BYTES or _utf8_length(value) > _MAX_URI_BYTES:
        raise ObjectRefError("URI exceeds 4096 UTF-8 bytes")
    if not value.startswith("app://"):
        raise ObjectRefError("URI must use the exact app:// scheme")
    address, separator, query = value[6:].partition("?")
    if not separator:
        raise ObjectRefError("URI requires an id query parameter")
    app_id, separator, object_type = address.partition("/")
    if not separator:
        raise ObjectRefError("URI requires one App and one object type")
    parameters: dict[str, str] = {}
    for parameter in query.split("&"):
        key, separator, encoded = parameter.partition("=")
        if not separator or key not in ("id", "revision") or key in parameters:
            raise ObjectRefError("unknown, repeated, or invalid query parameter")
        parameters[key] = _decode(encoded)
    if "id" not in parameters:
        raise ObjectRefError("URI requires an id query parameter")
    reference: ObjectRef = {
        "app_id": app_id,
        "object_type": object_type,
        "object_id": parameters["id"],
    }
    if "revision" in parameters:
        reference["revision"] = parameters["revision"]
    if format_reference(reference) != value:
        raise ObjectRefError("URI is not canonically spelled")
    return reference
