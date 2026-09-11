"""Shared object-reference vectors and structural wire conformance."""

from __future__ import annotations

import copy
import json
from pathlib import Path
import unittest
from unittest import mock

from claw_os_sdk import objects
from claw_os_sdk.generated import (
    Manifest,
    WireDecodeError,
    validate_object_ref,
)

_WIRE = Path(__file__).resolve().parents[3] / "wire" / "v1"
_VECTORS = json.loads((_WIRE / "object_ref.vectors.json").read_text(encoding="utf-8"))


def _reference() -> dict[str, str]:
    return {"app_id": "notes", "object_type": "note", "object_id": "x"}


class ObjectReferenceTests(unittest.TestCase):
    def test_shared_canonical_vectors_round_trip_without_identity_changes(self) -> None:
        for case in _VECTORS["valid"]:
            with self.subTest(uri=case["uri"]):
                original = copy.deepcopy(case["reference"])
                self.assertEqual(objects.format_reference(original), case["uri"])
                self.assertEqual(objects.parse_reference(case["uri"]), original)
                self.assertEqual(original, case["reference"])

    def test_shared_invalid_vectors_fail(self) -> None:
        for uri in _VECTORS["invalid_uris"]:
            with self.subTest(uri=uri), self.assertRaises(objects.ObjectRefError):
                objects.parse_reference(uri)
        for reference in _VECTORS["invalid_refs"]:
            with self.subTest(reference=reference), self.assertRaises(objects.ObjectRefError):
                objects.format_reference(reference)

    def test_shared_byte_limits_reject_exactly_one_more_byte(self) -> None:
        for limit in _VECTORS["limits"]:
            with self.subTest(field=limit["field"]):
                text = limit["unit"] * limit["count"]
                self.assertEqual(len(text.encode("utf-8")), limit["bytes"])
                reference = _reference()
                reference[limit["field"]] = text
                uri = objects.format_reference(reference)
                self.assertEqual(objects.parse_reference(uri), reference)
                reference[limit["field"]] += "x"
                with self.assertRaises(objects.ObjectRefError):
                    objects.format_reference(reference)

    def test_maximum_components_and_uri_ceiling(self) -> None:
        reference = {
            "app_id": "a" * 128,
            "object_type": "a" * 64,
            "object_id": "\u00e9" * 512,
            "revision": "\u00e9" * 64,
        }
        uri = objects.format_reference(reference)
        self.assertEqual(len(uri.encode("utf-8")), 3669)
        self.assertEqual(objects.parse_reference(uri), reference)
        with self.assertRaises(objects.ObjectRefError):
            objects.parse_reference("app://notes/note?id=" + "x" * 4096)

    def test_surrogates_are_never_replacement_encoded(self) -> None:
        for text in ("\ud800", "\udfff", "x\ud800", "\ud800\udc00"):
            for field in ("object_id", "revision"):
                reference = _reference()
                reference[field] = text
                with self.subTest(text=repr(text), field=field):
                    with self.assertRaises(objects.ObjectRefError):
                        objects.format_reference(reference)
            with self.assertRaises(objects.ObjectRefError):
                objects.parse_reference("app://notes/note?id=" + text)

    def test_generated_decoder_matches_shared_closed_contract(self) -> None:
        for case in _VECTORS["wire_cases"]:
            with self.subTest(value=case["value"]):
                if case["code"] is None:
                    validate_object_ref(case["value"])
                    continue
                with self.assertRaises(WireDecodeError) as raised:
                    validate_object_ref(case["value"])
                self.assertEqual(raised.exception.code, case["code"])
                self.assertEqual(raised.exception.path, case["path"])
                with self.assertRaises(objects.ObjectRefError):
                    objects.format_reference(case["value"])

    def test_legacy_manifest_and_unrevisioned_reference_remain_compatible(self) -> None:
        self.assertIn("objects", Manifest.__optional_keys__)
        self.assertEqual(Manifest.__required_keys__, {"id", "version", "name"})
        reference = objects.parse_reference("app://notes/note?id=x")
        self.assertNotIn("revision", reference)
        self.assertNotIn("wire_version", reference)

    def test_manifest_schema_declares_bounded_closed_object_resolvers(self) -> None:
        schema = json.loads((_WIRE / "manifest.schema.json").read_text(encoding="utf-8"))
        self.assertNotIn("objects", schema["required"])
        declaration = schema["properties"]["objects"]
        self.assertEqual(declaration["maxProperties"], 64)
        self.assertEqual(declaration["propertyNames"]["maxLength"], 64)
        pattern = declaration["propertyNames"]["pattern"]
        for key in ("note", "note_type", "note-1"):
            self.assertRegex(key, pattern)
        for key in ("Note", "_note", "note.child", "note\n", "note\u2028"):
            self.assertNotRegex(key, pattern)
        self.assertEqual(declaration["additionalProperties"], {"$ref": "#/$defs/objectType"})
        object_type = schema["$defs"]["objectType"]
        self.assertEqual(object_type["required"], ["label", "resolve"])
        self.assertFalse(object_type["additionalProperties"])
        resolver = schema["$defs"]["objectResolver"]
        self.assertEqual(resolver["required"], ["operation", "id_arg"])
        self.assertFalse(resolver["additionalProperties"])
        self.assertEqual(set(resolver["properties"]), {"operation", "id_arg", "revision_arg"})

    def test_helpers_are_pure_and_reject_non_string_uris(self) -> None:
        with mock.patch("builtins.open", side_effect=AssertionError("unexpected file access")), mock.patch(
            "subprocess.run", side_effect=AssertionError("unexpected subprocess")
        ):
            reference = _reference()
            self.assertEqual(objects.parse_reference(objects.format_reference(reference)), reference)
        for value in (None, 1, b"app://notes/note?id=x"):
            with self.assertRaises(objects.ObjectRefError):
                objects.parse_reference(value)
