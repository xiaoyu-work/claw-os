import copy
import fcntl
import hashlib
import io
import json
import os
from pathlib import Path
import shutil
import struct
import subprocess
import sys
import tarfile
import unittest
from unittest.mock import Mock, patch
from types import SimpleNamespace
import uuid

import build
import staging


class BuilderTests(unittest.TestCase):
    def setUp(self):
        self.directory = build.BUILD / "tests" / str(uuid.uuid4())
        self.directory.mkdir(parents=True)
        self.addCleanup(shutil.rmtree, self.directory)
        self.pin = build.load_pin()
        strip_selector = patch.object(
            build, "select_strip_tool",
            side_effect=lambda target: staging.StripTool(
                Path("/bin/true"), target.replace("-unknown", "") + "-strip",
                "GNU strip fixture", target,
            ),
        )
        strip_selector.start()
        self.addCleanup(strip_selector.stop)

    def write_pin(self, pin):
        path = self.directory / "source.json"
        path.write_text(json.dumps(pin), encoding="utf-8")
        return path

    @staticmethod
    def elf_fixture(machine=62, *, debug=False):
        header = bytearray(64)
        header[:7] = b"\x7fELF\x02\x01\x01"
        struct.pack_into("<HHI", header, 16, 3, machine, 1)
        struct.pack_into("<Q", header, 24, 4096)
        struct.pack_into("<H", header, 52, 64)
        if debug:
            names = b"\0.shstrtab\0.debug_info\0"
            sections = bytearray(3 * 64)
            struct.pack_into("<Q", header, 40, 64)
            struct.pack_into("<HHH", header, 58, 64, 3, 1)
            struct.pack_into("<I", sections, 64, 1)
            struct.pack_into("<QQ", sections, 64 + 24, 64 + len(sections), len(names))
            struct.pack_into("<I", sections, 128, names.index(b".debug_info"))
            return bytes(header) + bytes(sections) + names
        return bytes(header) + b"artifact fixture"

    def build_info(self, target="x86_64-unknown-linux-gnu"):
        return {
            "target": target,
            "profile": self.pin["cargo"]["profile"],
            "default_features": self.pin["cargo"]["default_features"],
            "features": self.pin["cargo"]["features"],
            "patches": self.pin["patches"],
        }

    def bundle(self, target="x86_64-unknown-linux-gnu"):
        source = self.directory / "source"
        for name in self.pin["runtime"]["license_files"]:
            path = source / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(f"unchanged {name}\n".encode())
            if name in self.pin["checksums"]:
                self.pin["checksums"][name] = build.sha256(path)
        compiled = self.directory / "codex"
        compiled.write_bytes(self.elf_fixture(build.SUPPORTED_TARGETS[target]))
        output = self.directory / "output"
        build.stage_artifacts(source, compiled, self.pin, output, self.build_info(target))
        return output

    def update_marker(self, root, update):
        marker = root / self.pin["runtime"]["assets_directory"] / "build-info.json"
        value = json.loads(marker.read_text())
        update(value)
        marker.write_text(json.dumps(value), encoding="utf-8")

    def test_rejects_symbolic_revision(self):
        self.pin["upstream"]["revision"] = "main"
        with self.assertRaisesRegex(build.BuildError, "full lowercase"):
            build.load_pin(self.write_pin(self.pin))

    def test_rejects_unstructured_patch_entries(self):
        self.pin["patches"] = ["change.diff"]
        with self.assertRaisesRegex(build.BuildError, "Patch entries require"):
            build.load_pin(self.write_pin(self.pin))

    def test_rejects_reduced_feature_build(self):
        self.pin["cargo"]["default_features"] = False
        with self.assertRaisesRegex(build.BuildError, "without removing"):
            build.load_pin(self.write_pin(self.pin))

    def test_rejects_unpinned_toolchain(self):
        self.pin["cargo"]["toolchain"] = "stable"
        with self.assertRaisesRegex(build.BuildError, "exact Rust toolchain"):
            build.load_pin(self.write_pin(self.pin))

    def test_requires_lockfile_checksum(self):
        self.pin["checksums"].pop("codex-rs/Cargo.lock")
        with self.assertRaisesRegex(build.BuildError, "Pin the lockfile"):
            build.load_pin(self.write_pin(self.pin))

    def test_requires_runtime_attribution(self):
        self.pin["runtime"]["license_files"].remove("NOTICE")
        with self.assertRaisesRegex(build.BuildError, "preserve upstream LICENSE and NOTICE"):
            build.load_pin(self.write_pin(self.pin))

    def test_rejects_unsafe_paths(self):
        for value in ("", ".", "..", "/absolute", "../outside", "a/../b", "a\\b", None):
            with self.subTest(value=value), self.assertRaises(build.BuildError):
                build.relative_path(value)
        self.assertEqual(build.relative_path("third_party/voice/NOTICE.md"), Path("third_party/voice/NOTICE.md"))

    def test_rejects_wrong_checkout_revision_before_archiving(self):
        with patch.object(build, "git", return_value="0" * 40):
            with self.assertRaisesRegex(build.BuildError, "not at pinned revision"):
                build.verify_checkout(self.directory, self.pin["upstream"]["revision"])

    def test_rejects_tracked_and_untracked_changes(self):
        for status in (" M main.rs", "?? extra.rs"):
            with self.subTest(status=status):
                with patch.object(build, "git", side_effect=[self.pin["upstream"]["revision"], status]):
                    with self.assertRaisesRegex(build.BuildError, "dirty"):
                        build.verify_checkout(self.directory, self.pin["upstream"]["revision"])

    def test_local_source_never_fetches(self):
        with patch.object(build, "verify_checkout") as verify, patch.object(build, "run") as run:
            source = build.acquire_repository(self.pin, self.directory, self.directory / "unused")
        self.assertEqual(source, self.directory.resolve())
        verify.assert_called_once_with(source, self.pin["upstream"]["revision"])
        run.assert_not_called()

    def test_verifies_canonical_input_bytes(self):
        path = self.directory / "LICENSE"
        path.write_bytes(b"canonical\n")
        pin = {"checksums": {"LICENSE": hashlib.sha256(b"canonical\n").hexdigest()}}
        build.verify_inputs(self.directory, pin)
        path.write_bytes(b"canonical\r\n")
        with self.assertRaisesRegex(build.BuildError, "checksum mismatch: LICENSE"):
            build.verify_inputs(self.directory, pin)

    def archive(self, name, contents, *, leading_symlink=None):
        data = io.BytesIO()
        with tarfile.open(fileobj=data, mode="w") as archive:
            if leading_symlink is not None:
                link = tarfile.TarInfo(leading_symlink[0])
                link.type = tarfile.SYMTYPE
                link.linkname = leading_symlink[1]
                archive.addfile(link)
            member = tarfile.TarInfo(name)
            member.size = len(contents)
            archive.addfile(member, io.BytesIO(contents))
        path = self.directory / "archive.tar"
        path.write_bytes(data.getvalue())
        archive_process = Mock(stdout=path.open("rb"), wait=Mock(return_value=0))
        native_popen = subprocess.Popen

        def popen(command, **kwargs):
            if command[0] == "git":
                return archive_process
            return native_popen(command, stderr=subprocess.DEVNULL, **kwargs)

        return patch.object(build.subprocess, "Popen", side_effect=popen)

    def test_extraction_replaces_old_tree_with_git_objects(self):
        destination = self.directory / "source"
        destination.mkdir()
        (destination / "untracked.rs").write_bytes(b"not source")
        with self.archive("codex-rs/Cargo.toml", b"pinned\n"):
            build.extract_source(self.directory, "pinned", destination)
        self.assertEqual((destination / "codex-rs" / "Cargo.toml").read_bytes(), b"pinned\n")
        self.assertFalse((destination / "untracked.rs").exists())

    def test_extraction_rejects_archive_path_escape(self):
        destination = self.directory / "source"
        with self.archive("../escape", b"not allowed"):
            with self.assertRaisesRegex(build.BuildError, "Source extraction failed"):
                build.extract_source(self.directory, "pinned", destination)
        self.assertFalse((self.directory / "escape").exists())

    def test_extraction_rejects_writing_through_archive_symlinks(self):
        destination = self.directory / "source"
        outside = self.directory / "outside"
        outside.mkdir()
        with self.archive("link/escape", b"not allowed", leading_symlink=("link", "../outside")):
            with self.assertRaisesRegex(build.BuildError, "Source extraction failed"):
                build.extract_source(self.directory, "pinned", destination)
        self.assertFalse((outside / "escape").exists())

    def patch_fixture(self):
        data = (
            b"diff --git a/example.txt b/example.txt\n"
            b"--- a/example.txt\n+++ b/example.txt\n"
            b"@@ -1,2 +1,2 @@\n"
            b"-before\n+after\n context\n"
        )
        entry = {
            "path": "patches/example.patch",
            "sha256": hashlib.sha256(data).hexdigest(),
            "description": "Fixture modification",
        }
        path = self.directory / entry["path"]
        path.parent.mkdir()
        path.write_bytes(data)
        return {"patches": [entry]}, path

    def test_patch_digest_is_required_before_application(self):
        pin, path = self.patch_fixture()
        path.write_bytes(path.read_bytes() + b"changed\n")
        with self.assertRaisesRegex(build.BuildError, "Patch digest mismatch"):
            build.read_patch_inputs(pin, self.directory)

    def test_patch_application_uses_verified_bytes_not_mutable_file(self):
        pin, path = self.patch_fixture()
        inputs = build.read_patch_inputs(pin, self.directory)
        path.write_bytes(b"replaced after verification")
        source = self.directory / "source"
        source.mkdir()
        (source / "example.txt").write_bytes(b"before\ncontext\n")
        build.apply_source_patches(source, inputs)
        self.assertEqual((source / "example.txt").read_bytes(), b"after\ncontext\n")

    def test_patch_drift_fails_without_modifying_source(self):
        pin, _ = self.patch_fixture()
        source = self.directory / "source"
        source.mkdir()
        original = b"unexpected\ncontext\n"
        (source / "example.txt").write_bytes(original)
        with self.assertRaises(subprocess.CalledProcessError):
            build.apply_source_patches(source, build.read_patch_inputs(pin, self.directory))
        self.assertEqual((source / "example.txt").read_bytes(), original)

    def test_patch_cannot_escape_the_prepared_tree(self):
        pin, path = self.patch_fixture()
        data = path.read_bytes().replace(b"example.txt", b"../outside.txt")
        source = self.directory / "source"
        source.mkdir()
        outside = self.directory / "outside.txt"
        outside.write_bytes(b"before\ncontext\n")
        with self.assertRaises(subprocess.CalledProcessError):
            build.apply_source_patches(source, [(pin["patches"][0], data)])
        self.assertEqual(outside.read_bytes(), b"before\ncontext\n")

    def test_sync_preserves_unchanged_mtime_and_removes_unpinned_files(self):
        prepared = self.directory / "prepared"
        source = self.directory / "source"
        prepared.mkdir()
        source.mkdir()
        (prepared / "code.rs").write_bytes(b"same")
        current = source / "code.rs"
        current.write_bytes(b"same")
        os.utime(current, (1234567890, 1234567890))
        timestamp = current.stat().st_mtime_ns
        (source / "untracked.rs").write_bytes(b"not pinned")
        build.sync_source_tree(prepared, source)
        self.assertEqual(current.stat().st_mtime_ns, timestamp)
        self.assertFalse((source / "untracked.rs").exists())

    def test_sync_repairs_tampered_source_even_with_newer_timestamp(self):
        prepared = self.directory / "prepared"
        source = self.directory / "source"
        prepared.mkdir()
        source.mkdir()
        (prepared / "code.rs").write_bytes(b"good")
        current = source / "code.rs"
        current.write_bytes(b"evil")
        os.utime(current, (2000000000, 2000000000))
        build.sync_source_tree(prepared, source)
        self.assertEqual(current.read_bytes(), b"good")

    def test_stages_binary_with_exact_licenses_and_receipt(self):
        pin = copy.deepcopy(self.pin)
        pin["runtime"]["license_files"] = ["LICENSE", "NOTICE", "third_party/example/NOTICE"]
        source = self.directory / "source"
        for name in pin["runtime"]["license_files"]:
            path = source / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(f"unchanged {name}\n".encode())
            if name in pin["checksums"]:
                pin["checksums"][name] = build.sha256(path)
        compiled = self.directory / "codex"
        compiled.write_bytes(self.elf_fixture())
        output = self.directory / "output"
        artifact = build.stage_artifacts(source, compiled, pin, output, self.build_info())
        self.assertEqual(artifact.read_bytes(), compiled.read_bytes())
        self.assertTrue(os.access(artifact, os.X_OK))
        assets = output / pin["runtime"]["assets_directory"]
        for name in pin["runtime"]["license_files"]:
            self.assertEqual((assets / name).read_bytes(), (source / name).read_bytes())
        self.assertEqual(json.loads((assets / "source.json").read_text()), pin)
        receipt = json.loads((assets / "build-info.json").read_text())
        self.assertEqual(receipt["binary_sha256"], build.sha256(compiled))
        self.assertEqual(receipt["upstream_revision"], pin["upstream"]["revision"])
        self.assertEqual(receipt["schema_version"], 2)
        self.assertEqual(receipt["artifact_kind"], "claw-agent-tui")
        self.assertFalse((output / "stage").exists())

    def test_non_linux_binary_does_not_replace_existing_artifact(self):
        compiled = self.directory / "codex.exe"
        compiled.write_bytes(b"MZnot-an-ELF")
        output = self.directory / "output"
        artifact = output / self.pin["runtime"]["binary"]
        artifact.parent.mkdir(parents=True)
        artifact.write_bytes(b"old artifact")
        with self.assertRaisesRegex(build.BuildError, "not a Linux ELF"):
            build.stage_artifacts(self.directory, compiled, self.pin, output, self.build_info())
        self.assertEqual(artifact.read_bytes(), b"old artifact")

    def test_verifies_complete_matching_bundle(self):
        root = self.bundle()
        result = build.verify_artifact(root, self.pin, "x86_64-unknown-linux-gnu")
        self.assertEqual(result["binary"], str(root / self.pin["runtime"]["binary"]))
        self.assertEqual(result["target"], "x86_64-unknown-linux-gnu")

    def test_verifies_aarch64_without_executing_it(self):
        root = self.bundle("aarch64-unknown-linux-gnu")
        result = build.verify_artifact(root, self.pin, "aarch64-unknown-linux-gnu")
        self.assertEqual(result["target"], "aarch64-unknown-linux-gnu")

    def test_rejects_wrong_requested_architecture(self):
        root = self.bundle()
        with self.assertRaisesRegex(build.BuildError, "Wrong TUI target"):
            build.verify_artifact(root, self.pin, "aarch64-unknown-linux-gnu")

    def test_checks_elf_architecture_not_only_marker(self):
        root = self.bundle()
        binary = root / self.pin["runtime"]["binary"]
        binary.write_bytes(self.elf_fixture(183))
        self.update_marker(root, lambda value: value.update(binary_sha256=build.sha256(binary)))
        with self.assertRaisesRegex(build.BuildError, "Wrong TUI architecture"):
            build.verify_artifact(root, self.pin, "x86_64-unknown-linux-gnu")

    def test_rejects_missing_binary(self):
        root = self.bundle()
        (root / self.pin["runtime"]["binary"]).unlink()
        with self.assertRaisesRegex(build.BuildError, "Missing artifact bundle file"):
            build.verify_artifact(root, self.pin, "x86_64-unknown-linux-gnu")

    def test_rejects_missing_provenance(self):
        root = self.bundle()
        (root / self.pin["runtime"]["assets_directory"] / "build-info.json").unlink()
        with self.assertRaisesRegex(build.BuildError, "Missing artifact bundle file"):
            build.verify_artifact(root, self.pin, "x86_64-unknown-linux-gnu")

    def test_rejects_legacy_marker(self):
        root = self.bundle()
        self.update_marker(root, lambda value: value.update(schema_version=1))
        with self.assertRaisesRegex(build.BuildError, "obsolete TUI provenance"):
            build.verify_artifact(root, self.pin, "x86_64-unknown-linux-gnu")

    def test_rejects_stale_source_revision(self):
        root = self.bundle()
        self.pin["upstream"]["revision"] = "a" * 40
        with self.assertRaisesRegex(build.BuildError, "Stale TUI source pin"):
            build.verify_artifact(root, self.pin, "x86_64-unknown-linux-gnu")

    def test_rejects_changed_recipe_at_same_revision(self):
        root = self.bundle()
        self.pin["cargo"]["toolchain"] = "1.99.0"
        with self.assertRaisesRegex(build.BuildError, "Stale TUI source pin"):
            build.verify_artifact(root, self.pin, "x86_64-unknown-linux-gnu")

    def test_rejects_corrupted_binary(self):
        root = self.bundle()
        with (root / self.pin["runtime"]["binary"]).open("ab") as stream:
            stream.write(b"changed")
        with self.assertRaisesRegex(build.BuildError, "executable digest"):
            build.verify_artifact(root, self.pin, "x86_64-unknown-linux-gnu")

    def test_rejects_missing_notice(self):
        root = self.bundle()
        (root / self.pin["runtime"]["assets_directory"] / "NOTICE").unlink()
        with self.assertRaisesRegex(build.BuildError, "Missing artifact bundle file"):
            build.verify_artifact(root, self.pin, "x86_64-unknown-linux-gnu")

    def test_rejects_modified_attribution(self):
        root = self.bundle()
        (root / self.pin["runtime"]["assets_directory"] / "NOTICE").write_bytes(b"changed")
        with self.assertRaisesRegex(build.BuildError, "attribution digest mismatch"):
            build.verify_artifact(root, self.pin, "x86_64-unknown-linux-gnu")

    def test_rejects_symlinked_executable_even_when_digest_matches(self):
        root = self.bundle()
        binary = root / self.pin["runtime"]["binary"]
        real_binary = self.directory / "another-codex"
        binary.rename(real_binary)
        binary.symlink_to(real_binary)
        with self.assertRaisesRegex(build.BuildError, "symlinked paths"):
            build.verify_artifact(root, self.pin, "x86_64-unknown-linux-gnu")

    def test_cli_requires_expected_target(self):
        result = subprocess.run(
            [sys.executable, "-B", str(build.MODULE / "build.py"), "--verify-artifact"],
            capture_output=True, text=True, check=False,
        )
        self.assertEqual(result.returncode, 2)
        self.assertEqual(result.stdout, "")
        self.assertIn("requires an explicit --target", result.stderr)

    def test_cli_does_not_package_during_build(self):
        root = self.bundle()
        with (root / "build.lock").open("w") as lock:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
            result = subprocess.run(
                [
                    sys.executable, "-B", str(build.MODULE / "build.py"),
                    "--verify-artifact", "--target", "x86_64-unknown-linux-gnu",
                    "--artifact-root", str(root),
                ],
                capture_output=True, text=True, check=False,
            )
        self.assertEqual(result.returncode, 1)
        self.assertEqual(result.stdout, "")
        self.assertIn("build is in progress", result.stderr)

    def test_rejects_missing_packaged_patch(self):
        root = self.bundle()
        (root / self.pin["runtime"]["assets_directory"] / self.pin["patches"][0]["path"]).unlink()
        with self.assertRaisesRegex(build.BuildError, "Missing artifact bundle file"):
            build.verify_artifact(root, self.pin, "x86_64-unknown-linux-gnu")

    def test_rejects_modified_packaged_patch(self):
        root = self.bundle()
        path = root / self.pin["runtime"]["assets_directory"] / self.pin["patches"][0]["path"]
        path.write_bytes(b"not the applied patch")
        with self.assertRaisesRegex(build.BuildError, "Patch digest mismatch"):
            build.verify_artifact(root, self.pin, "x86_64-unknown-linux-gnu")

    def test_rejects_missing_modification_notice(self):
        root = self.bundle()
        (root / self.pin["runtime"]["assets_directory"] / "MODIFICATIONS").unlink()
        with self.assertRaisesRegex(build.BuildError, "Missing artifact bundle file"):
            build.verify_artifact(root, self.pin, "x86_64-unknown-linux-gnu")

    def test_rejects_modified_modification_notice(self):
        root = self.bundle()
        (root / self.pin["runtime"]["assets_directory"] / "MODIFICATIONS").write_bytes(b"unmodified")
        with self.assertRaisesRegex(build.BuildError, "modification notice"):
            build.verify_artifact(root, self.pin, "x86_64-unknown-linux-gnu")

    def test_unavailable_strip_tool_fails_explicitly(self):
        with patch.object(staging.shutil, "which", return_value=None):
            with self.assertRaisesRegex(build.BuildError, "strip tool is unavailable"):
                staging.select_strip_tool("x86_64-unknown-linux-gnu")

    def test_wrong_target_strip_tool_is_rejected(self):
        replies = [
            subprocess.CompletedProcess([], 0, stdout="GNU strip fixture\n"),
            subprocess.CompletedProcess([], 0, stdout="elf64-littleaarch64\n"),
        ]
        with patch.object(staging.shutil, "which", return_value="/bin/wrong-strip"):
            with patch.object(staging.subprocess, "run", side_effect=replies):
                with self.assertRaisesRegex(build.BuildError, "Wrong-target strip tool"):
                    staging.select_strip_tool("x86_64-unknown-linux-gnu")

    def test_strip_does_not_modify_input_and_receipt_hashes_final_bytes(self):
        root = self.bundle()
        compiled = self.directory / "codex"
        original = self.elf_fixture(debug=True)
        stripped = self.elf_fixture()
        compiled.write_bytes(original)

        def strip_copy(command, **kwargs):
            self.assertEqual(command[1], "--strip-debug")
            destination = Path(command[2])
            self.assertEqual(destination, root / "stage" / self.pin["runtime"]["binary"])
            self.assertNotEqual(destination, compiled)
            destination.write_bytes(stripped)
            return subprocess.CompletedProcess(command, 0)

        with patch.object(staging.subprocess, "run", side_effect=strip_copy):
            binary = build.stage_artifacts(
                self.directory / "source", compiled, self.pin, root, self.build_info(),
            )
        self.assertEqual(compiled.read_bytes(), original)
        self.assertEqual(binary.read_bytes(), stripped)
        verified = build.verify_artifact(root, self.pin, "x86_64-unknown-linux-gnu")
        self.assertEqual(verified["staging_input_sha256"], hashlib.sha256(original).hexdigest())
        self.assertEqual(verified["binary_sha256"], hashlib.sha256(stripped).hexdigest())
        self.assertEqual(verified["staging_recipe"], self.pin["staging"])

    def test_noop_strip_tool_cannot_publish_debug_binary(self):
        root = self.bundle()
        previous = (root / self.pin["runtime"]["binary"]).read_bytes()
        compiled = self.directory / "codex"
        compiled.write_bytes(self.elf_fixture(debug=True))
        with self.assertRaisesRegex(build.BuildError, "left debug sections"):
            build.stage_artifacts(
                self.directory / "source", compiled, self.pin, root, self.build_info(),
            )
        self.assertEqual((root / self.pin["runtime"]["binary"]).read_bytes(), previous)

    def test_strip_output_architecture_is_rechecked(self):
        root = self.bundle()
        previous = (root / self.pin["runtime"]["binary"]).read_bytes()

        def corrupt_target(command, **kwargs):
            Path(command[2]).write_bytes(self.elf_fixture(183))
            return subprocess.CompletedProcess(command, 0)

        with patch.object(staging.subprocess, "run", side_effect=corrupt_target):
            with self.assertRaisesRegex(build.BuildError, "Wrong TUI architecture"):
                build.stage_artifacts(
                    self.directory / "source", self.directory / "codex",
                    self.pin, root, self.build_info(),
                )
        self.assertEqual((root / self.pin["runtime"]["binary"]).read_bytes(), previous)

    def test_verifier_rejects_debug_sections_even_with_updated_hash(self):
        root = self.bundle()
        binary = root / self.pin["runtime"]["binary"]
        binary.write_bytes(self.elf_fixture(debug=True))
        self.update_marker(root, lambda value: value.update(binary_sha256=build.sha256(binary)))
        with self.assertRaisesRegex(build.BuildError, "still contains debug sections"):
            build.verify_artifact(root, self.pin, "x86_64-unknown-linux-gnu")

    def test_preserves_allocated_gdb_helpers_like_gnu_strip(self):
        from artifacts import debug_sections

        binary = self.directory / "gdb-helper"
        data = bytearray(self.elf_fixture(debug=True))
        data[data.index(b".debug_info"):] = b".debug_gdb_scripts\0"
        names_size = len(data) - (64 + 3 * 64)
        struct.pack_into("<Q", data, 64 + 64 + 32, names_size)
        struct.pack_into("<Q", data, 64 + 128 + 8, 2)
        binary.write_bytes(data)
        self.assertEqual(debug_sections(binary), [])

    def test_staging_change_does_not_permit_source_change(self):
        previous = copy.deepcopy(self.pin)
        previous.pop("staging")
        build.require_same_compilation(previous, self.pin)
        previous["upstream"]["revision"] = "b" * 40
        with self.assertRaisesRegex(build.BuildError, "Cannot restage a different source"):
            build.require_same_compilation(previous, self.pin)

    def test_restage_does_not_invoke_cargo_or_source_acquisition(self):
        source_root = self.bundle()
        destination = self.directory / "restaged"
        args = SimpleNamespace(artifact_root=source_root, target="x86_64-unknown-linux-gnu")
        with patch.object(build, "BUILD", destination), patch.object(build, "load_pin", return_value=self.pin):
            with patch.object(build, "run", side_effect=AssertionError("No build command allowed")):
                build.restage_existing(args)
        verified = build.verify_artifact(destination, self.pin, args.target)
        self.assertEqual(verified["staging_recipe"], self.pin["staging"])

    def test_pinned_rust_checks_compile_and_run_before_artifact_build(self):
        output = self.directory / "build"
        output.mkdir()
        source = self.directory / "source"
        source.mkdir()
        pin = copy.deepcopy(self.pin)
        pin["checks"] = {"rust_stdlib_tests": ["codex-rs/fixture.rs"]}
        with patch.object(build, "BUILD", output), patch.object(build, "run") as runner:
            build.run_patch_checks(source, pin, {})
        commands = [call.args[0] for call in runner.call_args_list]
        self.assertIn("rustc", commands[0])
        self.assertIn("--test", commands[0])
        self.assertIn(source / "codex-rs/fixture.rs", commands[0])
        self.assertEqual(commands[1], [output / "patch-checks/stdlib-check-0", "--test-threads=1"])

    def test_failed_pinned_rust_check_is_fatal(self):
        output = self.directory / "build"
        output.mkdir()
        source = self.directory / "source"
        source.mkdir()
        with patch.object(build, "BUILD", output):
            with patch.object(build, "run", side_effect=subprocess.CalledProcessError(1, "rustc")):
                with self.assertRaises(subprocess.CalledProcessError):
                    build.run_patch_checks(source, self.pin, {})


if __name__ == "__main__":
    unittest.main()
