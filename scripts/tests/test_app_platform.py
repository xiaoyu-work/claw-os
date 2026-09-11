"""The App development release contains only immutable, declared OS libraries."""

import importlib.util
import json
from pathlib import Path
import subprocess
import tarfile

import pytest


ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location("app_platform", ROOT / "scripts/app_platform.py")
platform = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(platform)


@pytest.fixture
def source(tmp_path):
    root = tmp_path / "source"
    root.mkdir()
    contract = platform.read_contract(ROOT)
    contract["version"] = "1.0.0"
    (root / "packaging").mkdir()
    (root / "packaging/app-platform.json").write_text(json.dumps(contract))
    for path in contract["exports"].values():
        directory = root / path
        directory.mkdir(parents=True, exist_ok=True)
        (directory / "library.txt").write_text(f"public SDK: {path}\n")
    for relative in ("apps/_shared/private.py", "core/src/lib.rs", "credentials.json"):
        path = root / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text("must not enter an App SDK release\n")
    for relative in (
        "claw-os-sdk/rust/src/applet/mod.rs",
        "claw-os-sdk/rust/src/applet/client.rs",
        "claw-os-sdk/rust/src/applet/protocol.rs",
        "claw-os-sdk/wire/v1/applet-services.md",
        "claw-os-sdk/wire/v1/applet-services.cases.json",
        "desktop/applets/claw-applet-services/src/service.rs",
        "desktop/applets/claw-applet-services/src/bin/claw-os-applet-provider.rs",
    ):
        path = root / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes((ROOT / relative).read_bytes())
    subprocess.run(["git", "init", "--quiet", str(root)], check=True)
    subprocess.run(["git", "-C", str(root), "add", "."], check=True)
    subprocess.run(
        ["git", "-C", str(root), "-c", "user.name=Fixture",
         "-c", "user.email=fixture@example.invalid", "commit", "--quiet", "-m", "SDK fixture"],
        check=True,
    )
    return root


def test_platform_exports_are_complete_and_source_reproducible(source, tmp_path):
    first = platform.build(tmp_path / "first", "1.0.0", source)
    second = platform.build(tmp_path / "second", "1.0.0", source)
    assert first.read_bytes() == second.read_bytes()
    with tarfile.open(first) as archive:
        manifest = json.load(archive.extractfile("platform.json"))
        assert manifest["schema"] == "claw.app-platform/v1"
        assert manifest["version"] == "1.0.0"
        assert manifest["runtime_abi"] == 1
        assert set(manifest["exports"]) == platform.EXPORT_NAMES
        assert len(manifest["source_revision"]) == 40
        names = archive.getnames()
        assert not any(name.startswith(("apps/", "core/")) for name in names)
        assert "credentials.json" not in names
        for item in manifest["files"]:
            assert item["path"] in names
            if item["kind"] == "file":
                data = archive.extractfile(item["path"]).read()
                assert platform.hashlib.sha256(data).hexdigest() == item["sha256"]
                assert len(data) == item["size"]


def test_release_version_cannot_override_source_contract(source, tmp_path):
    with pytest.raises(ValueError, match="must match"):
        platform.build(tmp_path / "release", "2.0.0", source)


def test_applet_export_contains_public_client_and_contract_but_no_os_provider(source, tmp_path):
    archive_path = platform.build(tmp_path / "applet-client", "1.0.0", source)
    with tarfile.open(archive_path) as archive:
        for relative in (
            "claw-os-sdk/rust/src/applet/mod.rs",
            "claw-os-sdk/rust/src/applet/client.rs",
            "claw-os-sdk/rust/src/applet/protocol.rs",
            "claw-os-sdk/wire/v1/applet-services.md",
            "claw-os-sdk/wire/v1/applet-services.cases.json",
        ):
            assert archive.extractfile(relative).read() == (ROOT / relative).read_bytes()
        assert not any(name.startswith(("desktop/applets/", "usr/libexec/")) for name in archive.getnames())


def test_modified_export_is_not_published(source, tmp_path):
    (source / "cos-runtime/rust/library.txt").write_text("uncommitted change\n")
    with pytest.raises(ValueError, match="Commit the SDK"):
        platform.build(tmp_path / "release", "1.0.0", source)


def test_release_output_cannot_be_rebound(source, tmp_path):
    output = tmp_path / "release"
    platform.build(output, "1.0.0", source)
    with pytest.raises(FileExistsError, match="immutable"):
        platform.build(output, "1.0.0", source)


def test_contract_cannot_export_os_implementation(source):
    path = source / "packaging/app-platform.json"
    contract = json.loads(path.read_text())
    contract["exports"]["python-runtime"] = "core/src"
    path.write_text(json.dumps(contract))
    with pytest.raises(ValueError, match="not an SDK library"):
        platform.read_contract(source)


@pytest.mark.parametrize("path", ["../core", "/core", "desktop/../core", "apps\\shared", "./sdk"])
def test_noncanonical_export_path_is_rejected(path):
    with pytest.raises(ValueError):
        platform.validate_path(path)


def test_symlink_cannot_smuggle_private_os_sources(source, tmp_path):
    link = source / "cos-runtime/rust/private"
    link.symlink_to("../../core")
    subprocess.run(["git", "-C", str(source), "add", str(link)], check=True)
    subprocess.run(
        ["git", "-C", str(source), "-c", "user.name=Fixture",
         "-c", "user.email=fixture@example.invalid", "commit", "--quiet", "-m", "Unsafe link"],
        check=True,
    )
    with pytest.raises(ValueError, match="escapes"):
        platform.build(tmp_path / "release", "1.0.0", source)
