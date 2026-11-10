import json
import unittest
from pathlib import Path

from . import manifest
from .generated import validate_owner_path_need


class OwnerPathTests(unittest.TestCase):
    def test_shared_owner_path_previews_stay_within_the_provided_home(self) -> None:
        vectors = Path(__file__).resolve().parents[3] / "wire" / "v1" / "owner_path.vectors.json"
        for case in json.loads(vectors.read_text(encoding="utf-8"))["cases"]:
            with self.subTest(case=case["name"]):
                if "expected" in case:
                    self.assertEqual(manifest.resolve_owner_path(case["verb"], case["path"], case["home"]), case["expected"])
                else:
                    with self.assertRaises((ValueError, TypeError)):
                        manifest.resolve_owner_path(case["verb"], case["path"], case["home"])
        with self.assertRaises(ValueError):
            manifest.validate_owner_path("fs.read", "é" * 128)
        for field in ["owner_uid", "home", "owner_home"]:
            with self.assertRaises(ValueError):
                validate_owner_path_need({"verb": "fs.read", "scope": {"kind": "owner-path", "path": "Documents/**", field: "/home/bob"}})

    def test_new_bindings_are_validated_on_each_manifest_surface(self) -> None:
        bad = {"needs": [{"verb": "fs.delete", "scope": {"kind": "owner-path", "path": "../bob/**"}}]}
        for value in [
            {"operations": {"remove": bad}},
            {"mcp": {"tools": [bad]}},
            {"desktop": bad},
            {"desktop": {"entries": {"applet": bad}}},
        ]:
            with self.assertRaises(ValueError):
                manifest.validate_owner_path_bindings(value)
