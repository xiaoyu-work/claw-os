"""Real diffs and guarded App-owned file plans; no plan grants authority."""

import hashlib
import io
import json
import os
import pathlib
import stat
import sys
import tempfile
import types
import unittest
from unittest import mock

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent.parent))
from test_support import load_local_module

plans = load_local_module(
    pathlib.Path(__file__).with_name("file_plans.py"),
    "claw_test_fs_file_plans",
    clear_modules=("_shared",),
)
from _shared import atomic
import cos_runtime


class FileChangeError(RuntimeError):
    pass


class TestFilePlans(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = pathlib.Path(self.directory.name).resolve()
        self.data = self.root / "app-data"
        self.data.mkdir(mode=0o700)
        self.target = self.root / "document.txt"
        self.target.write_text("before\n", encoding="utf-8")
        self.environment = mock.patch.dict(os.environ, {"COS_DATA_DIR": str(self.data)})
        self.environment.start()
        self.addCleanup(self.environment.stop)
        self.policy = mock.patch.object(plans.policy, "require")
        self.require = self.policy.start()
        self.addCleanup(self.policy.stop)
        self.snapshot = mock.patch.object(plans.snapshot, "snapshot", return_value=None)
        self.snapshot.start()
        self.addCleanup(self.snapshot.stop)
        self.rpc = mock.Mock(side_effect=self.replace)
        module = types.ModuleType("cos_runtime.file_changes")
        module.FileChangeError = FileChangeError
        module.replace_file = self.rpc
        patch = mock.patch.object(cos_runtime, "file_changes", module, create=True)
        patch.start()
        self.addCleanup(patch.stop)

    def replace(self, path, expected, content):
        current, before = plans._read_target(path)
        if current != expected:
            raise FileChangeError("target conflict")
        changed = expected is None or before.encode("utf-8") != content
        if changed:
            mode = stat.S_IMODE(expected["mode"]) if expected else 0o600
            atomic.atomic_write_bytes(path, content, mode=mode, strict=True)
        return {
            "path": path, "bytes": len(content), "changed": changed,
            "sha256": "sha256:" + hashlib.sha256(content).hexdigest(),
        }

    def prepare(self, content="after\n", target=None, ttl=3600):
        result = plans.run("plan_write", [
            str(target or self.target), "--content", content, "--ttl-seconds", str(ttl)
        ])
        self.assertNotIn("error", result, result)
        return result

    def apply(self, plan, **overrides):
        values = {
            "path": plan["path"], "plan": plan["plan_id"], "review": plan["review"],
            "confirm": "true",
        }
        values.update(overrides)
        return plans.run("plan_apply", [
            values["path"], "--plan", values["plan"], "--review", values["review"],
            "--confirm=" + values["confirm"],
        ])

    def stored(self, plan):
        directory = plans._bucket(plan["path"])
        return pathlib.Path(directory) / (plan["plan_id"] + ".json")

    def test_prepare_produces_a_real_diff_without_writing_the_target(self):
        plan = self.prepare()
        self.assertEqual(self.target.read_text(), "before\n")
        self.assertEqual(plan["state"], "draft")
        self.assertIn("-before\n+after\n", plan["diff"])
        self.assertTrue(plan["would_change"])
        self.assertEqual(plan["after_sha256"], plans._hash(b"after\n"))
        self.assertTrue(plan["reference"].startswith("app://fs/change-plan?"))
        self.assertNotIn("before_text", plan)
        self.assertNotIn("after_text", plan)
        self.assertEqual(stat.S_IMODE(self.stored(plan).stat().st_mode), 0o600)
        self.assertNotIn(mock.call("fs.write", path=str(self.target)), self.require.call_args_list)
        self.rpc.assert_not_called()

    def test_missing_final_newlines_are_explicit_in_the_diff(self):
        self.target.write_text("before", encoding="utf-8")
        plan = self.prepare("after")
        self.assertIn("-before\n\\ No newline at end of file\n", plan["diff"])
        self.assertIn("+after\n\\ No newline at end of file\n", plan["diff"])

    def test_apply_uses_the_broker_after_a_durable_applying_record(self):
        plan = self.prepare()
        original = self.replace

        def replace(path, expected, content):
            record = json.loads(self.stored(plan).read_text())
            self.assertEqual(record["state"], "applying")
            return original(path, expected, content)

        self.rpc.side_effect = replace
        result = self.apply(plan)
        self.assertNotIn("error", result, result)
        self.assertEqual(result["state"], "applied")
        self.assertTrue(result["changed"])
        self.assertEqual(self.target.read_text(), "after\n")
        self.rpc.assert_called_once()
        self.assertIn(mock.call("fs.write", path=str(self.target)), self.require.call_args_list)
        self.assertEqual(self.apply(plan)["code"], "plan_consumed")
        self.rpc.assert_called_once()

    def test_changed_target_is_a_conflict_not_an_overwrite(self):
        plan = self.prepare()
        self.target.write_text("external edit\n", encoding="utf-8")
        result = self.apply(plan)
        self.assertEqual(result["code"], "plan_conflict")
        self.assertEqual(self.target.read_text(), "external edit\n")
        self.assertEqual(json.loads(self.stored(plan).read_text())["state"], "conflicted")
        self.rpc.assert_not_called()

    def test_review_fingerprint_prevents_changed_proposal_content(self):
        plan = self.prepare()
        path = self.stored(plan)
        record = json.loads(path.read_text())
        record["after_text"] = "different unreviewed content\n"
        record["review"] = plans._review(record)
        path.write_text(json.dumps(record), encoding="utf-8")
        result = self.apply(plan)
        self.assertEqual(result["code"], "plan_conflict")
        self.assertEqual(self.target.read_text(), "before\n")
        self.rpc.assert_not_called()

    def test_failed_or_interrupted_apply_never_replays_automatically(self):
        plan = self.prepare()

        def replace_then_fail(path, expected, content):
            self.replace(path, expected, content)
            raise FileChangeError("lost response after replacement")

        self.rpc.side_effect = replace_then_fail
        result = self.apply(plan)
        self.assertEqual(result["code"], "plan_indeterminate")
        self.assertIn("lost response after replacement", result["error"])
        self.assertIn(
            "lost response after replacement",
            json.loads(self.stored(plan).read_text())["diagnostic"],
        )
        self.assertEqual(self.target.read_text(), "after\n")
        self.assertEqual(self.apply(plan)["code"], "plan_indeterminate")
        self.rpc.assert_called_once()

    def test_apply_failure_diagnostics_are_bounded_without_losing_uncertainty(self):
        plan = self.prepare()
        self.rpc.side_effect = FileChangeError("\u00e9" * 3000)
        result = self.apply(plan)
        self.assertEqual(result["code"], "plan_indeterminate")
        stored = json.loads(self.stored(plan).read_text())
        self.assertEqual(stored["state"], "indeterminate")
        self.assertIn("[truncated]", stored["diagnostic"])
        self.assertLessEqual(len(stored["diagnostic"].encode("utf-8")), 4096)
        self.assertEqual(self.target.read_text(), "before\n")
        self.assertEqual(self.apply(plan)["code"], "plan_indeterminate")
        self.rpc.assert_called_once()

    def test_existing_applying_bracket_is_not_retried(self):
        plan = self.prepare()
        record = json.loads(self.stored(plan).read_text())
        record["state"] = "applying"
        self.stored(plan).write_text(json.dumps(record), encoding="utf-8")
        self.assertEqual(self.apply(plan)["code"], "plan_indeterminate")
        self.rpc.assert_not_called()

    def test_post_write_record_failure_is_reported_as_indeterminate(self):
        plan = self.prepare()
        original = plans._save

        def save(directory, record):
            if record["state"] == "applied":
                raise OSError("final record failed")
            return original(directory, record)

        with mock.patch.object(plans, "_save", side_effect=save):
            result = self.apply(plan)
        self.assertEqual(result["code"], "plan_indeterminate")
        self.assertEqual(self.target.read_text(), "after\n")
        self.assertEqual(json.loads(self.stored(plan).read_text())["state"], "indeterminate")

    def test_expiry_confirmation_and_invalid_input_fail_before_effects(self):
        plan = self.prepare(ttl=60)
        self.require.reset_mock()
        self.assertEqual(self.apply(plan, confirm="false")["code"], "invalid_args")
        self.require.assert_not_called()
        with mock.patch.object(plans, "_now", return_value=plans._seconds(plan["expires_at"])):
            self.assertEqual(self.apply(plan)["code"], "plan_expired")
            shown = plans.run("plan_show", [plan["path"], "--plan", plan["plan_id"]])
            self.assertEqual(shown["state"], "expired")
        self.rpc.assert_not_called()
        self.require.reset_mock()
        self.assertEqual(plans.run("plan_write", [str(self.target), "--content", "x" * 65537])["code"], "unsupported_file")
        self.require.assert_not_called()

    def test_invalid_plan_arguments_are_rejected_before_permission_requests(self):
        for command, args in [
            ("plan_write", ["relative", "--content", "text"]),
            ("plan_write", [str(self.target) + "*", "--content", "text"]),
            ("plan_write", [str(self.target) + "[1]", "--content", "text"]),
            ("plan_write", [str(self.root / ".." / "elsewhere"), "--content", "text"]),
            ("plan_write", [str(self.target) + "\ud800", "--content", "text"]),
            ("plan_write", [str(self.target), "--force"]),
            ("plan_apply", [str(self.target), "--plan", "../other", "--review", plans._hash(b"x"), "--confirm=true"]),
            ("plan_prune", [str(self.target), "--keep", "-1", "--confirm=true"]),
        ]:
            self.require.reset_mock()
            self.assertIn("error", plans.run(command, args))
            self.require.assert_not_called()

    def test_absent_and_unchanged_targets_are_explicit(self):
        new = self.root / "new.txt"
        plan = self.prepare("", target=new)
        self.assertFalse(plan["before_exists"])
        self.assertIsNone(plan["before_sha256"])
        self.assertTrue(plan["would_change"])
        applied = self.apply(plan)
        self.assertTrue(applied["changed"])
        self.assertEqual(new.read_bytes(), b"")
        same = self.prepare("before\n")
        self.assertFalse(same["would_change"])
        self.assertFalse(self.apply(same)["changed"])

    def test_preparing_creation_does_not_require_parent_directory_visibility(self):
        target = self.root / "not-visible-in-this-launch" / "new.txt"
        plan = self.prepare("new content", target=target)
        self.assertEqual(plan["state"], "draft")
        self.assertFalse(plan["before_exists"])
        self.assertFalse(target.parent.exists())
        self.require.assert_has_calls([
            mock.call("fs.read", path=str(target)),
            mock.call("data.db.write", name=plans.STORE_SCOPE),
        ])
        self.rpc.assert_not_called()

    def test_plan_show_requires_current_target_read_authority(self):
        plan = self.prepare()
        self.require.reset_mock()
        result = plans.run("plan_show", [plan["path"], "--plan", plan["plan_id"]])
        self.assertEqual(result["diff"], plan["diff"])
        self.require.assert_has_calls([
            mock.call("fs.read", path=plan["path"]),
            mock.call("data.db.read", name=plans.STORE_SCOPE),
        ])
        self.rpc.assert_not_called()

    def test_corrupt_or_cross_target_plans_cannot_be_applied(self):
        plan = self.prepare()
        record = json.loads(self.stored(plan).read_text())
        record["before_text"] = "forged baseline"
        self.stored(plan).write_text(json.dumps(record), encoding="utf-8")
        self.assertEqual(self.apply(plan)["code"], "plan_invalid")
        other = self.root / "other.txt"
        other.write_text("other", encoding="utf-8")
        self.assertIn("error", self.apply(plan, path=str(other)))
        self.assertEqual(other.read_text(), "other")
        self.rpc.assert_not_called()

    def test_special_files_and_hardlinks_are_rejected_without_blocking(self):
        hardlink = self.root / "alias.txt"
        os.link(self.target, hardlink)
        result = plans.run("plan_write", [str(self.target), "--content", "new"])
        self.assertEqual(result["code"], "unsupported_file")
        hardlink.unlink()
        fifo = self.root / "fifo"
        os.mkfifo(fifo)
        result = plans.run("plan_write", [str(fifo), "--content", "new"])
        self.assertEqual(result["code"], "unsupported_file")

    def test_stored_plan_symlinks_and_inconsistent_lifecycle_are_not_trusted(self):
        plan = self.prepare()
        record_path = self.stored(plan)
        record = json.loads(record_path.read_text())
        record["state"] = "applied"
        record_path.write_text(json.dumps(record), encoding="utf-8")
        shown = plans.run("plan_show", [plan["path"], "--plan", plan["plan_id"]])
        self.assertEqual(shown["code"], "plan_invalid")
        record_path.unlink()
        os.symlink(self.target, record_path)
        self.assertIn("error", self.apply(plan))
        self.assertEqual(self.target.read_text(), "before\n")
        self.rpc.assert_not_called()

    def test_prune_preserves_live_and_unknown_plans_and_never_changes_target(self):
        consumed = self.prepare()
        self.apply(consumed)
        unknown = self.prepare("third\n")
        record = json.loads(self.stored(unknown).read_text())
        record["state"] = "applying"
        self.stored(unknown).write_text(json.dumps(record), encoding="utf-8")
        result = plans.run("plan_prune", [str(self.target), "--keep", "0", "--confirm=true"])
        self.assertEqual(result["removed"], [consumed["plan_id"]])
        self.assertFalse(result["target_changed"])
        self.assertTrue(self.stored(unknown).exists())
        self.assertEqual(self.target.read_text(), "after\n")


class TestStrictAtomicPlans(unittest.TestCase):
    def test_strict_fsync_failure_before_replace_preserves_the_target(self):
        with tempfile.TemporaryDirectory() as directory:
            path = pathlib.Path(directory) / "record"
            path.write_bytes(b"before")
            with mock.patch.object(atomic.os, "fsync", side_effect=OSError("fsync failed")):
                with self.assertRaises(OSError):
                    atomic.atomic_write_bytes(str(path), b"after", strict=True)
            self.assertEqual(path.read_bytes(), b"before")

    def test_strict_directory_fsync_failure_does_not_report_success(self):
        with tempfile.TemporaryDirectory() as directory:
            path = pathlib.Path(directory) / "record"
            path.write_bytes(b"before")
            with mock.patch.object(atomic.os, "fsync", side_effect=[None, OSError("directory fsync failed")]):
                with self.assertRaises(OSError):
                    atomic.atomic_write_bytes(str(path), b"after", strict=True)
            self.assertEqual(path.read_bytes(), b"after")

    def test_legacy_best_effort_behavior_is_unchanged(self):
        with tempfile.TemporaryDirectory() as directory:
            path = pathlib.Path(directory) / "record"
            with mock.patch.object(atomic.os, "fsync", side_effect=OSError("unsupported")):
                atomic.atomic_write_bytes(str(path), b"legacy")
            self.assertEqual(path.read_bytes(), b"legacy")


if __name__ == "__main__":
    unittest.main()
