"""The declared entry object reuses the existing key-scoped get operation."""

import json
import pathlib
import tempfile
import unittest
from unittest import mock

from test_support import load_local_module

kv = load_local_module(
    pathlib.Path(__file__).with_name("main.py"),
    "claw_test_kv_objects",
    clear_modules=("_shared",),
)


class TestEntryObjects(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        store = pathlib.Path(self.directory.name) / "kv.json"
        for name, value in [
            ("DATA_DIR", self.directory.name),
            ("STORE_PATH", str(store)),
            ("LOCK_PATH", str(store) + ".lock"),
        ]:
            patch = mock.patch.object(kv, name, value)
            patch.start()
            self.addCleanup(patch.stop)

    def test_manifest_uses_the_existing_get_operation_and_capability(self):
        manifest = json.loads(
            pathlib.Path(__file__).with_name("app.json").read_text(encoding="utf-8")
        )
        self.assertEqual(
            manifest["objects"]["entry"]["resolve"],
            {"operation": "get", "id_arg": "key"},
        )
        need = manifest["operations"]["get"]["needs"][0]
        self.assertEqual(need["verb"], "data.kv.read")
        self.assertEqual(need["scope"], {"kind": "from-arg", "arg": "key"})

    def test_object_get_preserves_reply_shape_and_exact_key_scope(self):
        with mock.patch.object(kv.policy, "require"):
            kv.run("set", ["release.status", "ready"])
        with mock.patch.object(kv.policy, "require") as require:
            result = kv.run("get", ["--", "release.status"])
        require.assert_called_once_with("data.kv.read", name="release.status")
        self.assertEqual(result, {"key": "release.status", "value": "ready"})

    def test_option_looking_identity_remains_data(self):
        pathlib.Path(kv.STORE_PATH).write_text(
            json.dumps({"--schema": "literal key"}), encoding="utf-8"
        )
        with mock.patch.object(kv.policy, "require") as require:
            result = kv.run("get", ["--", "--schema"])
        require.assert_called_once_with("data.kv.read", name="--schema")
        self.assertEqual(result, {"key": "--schema", "value": "literal key"})

    def test_option_looking_keys_keep_the_same_identity_for_set_and_delete(self):
        with mock.patch.object(kv.policy, "require") as require:
            created = kv.run("set", ["--", "--schema", "literal"])
            require.assert_called_once_with("data.kv.write", name="--schema")
        self.assertEqual(created, {"key": "--schema", "value": "literal"})
        with mock.patch.object(kv.policy, "require") as require:
            deleted = kv.run("del", ["--", "--schema"])
            require.assert_called_once_with("data.kv.delete", name="--schema")
        self.assertEqual(deleted, {"deleted": "--schema"})

    def test_missing_objects_and_missing_ids_do_not_become_success(self):
        with mock.patch.object(kv.policy, "require") as require:
            self.assertIn("error", kv.run("get", []))
            require.assert_not_called()
            result = kv.run("get", ["missing"])
        self.assertIn("error", result)
        self.assertNotIn("value", result)


if __name__ == "__main__":
    unittest.main()
