"""Nonprivileged staging checks, not native Editor UI acceptance."""

import importlib.util
import json
from pathlib import Path
import platform
import shutil

import pytest


ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location(
    "native_editor_payload", ROOT / "scripts/ci/native_editor_payload.py",
)
editor = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(editor)


@pytest.fixture
def payload(tmp_path):
    if platform.machine() not in ("x86_64", "AMD64") or not Path("/bin/true").is_file():
        pytest.skip("This payload parser test uses an existing real amd64 Linux ELF")
    source, prepared = tmp_path / "source", tmp_path / "prepared"
    manifest = {
        "id": "cosmic-edit", "runtime": "binary", "entry": editor.ENTRY,
        "mcp": {"entry": editor.ENTRY},
        "desktop": {
            "exec": "--gui",
            "needs": [{"verb": "fs.meta", "scope": {"kind": "owner-path", "path": "**"}}],
            "optional_needs": [{"verb": "fs.read"}, {"verb": "fs.write"}],
        },
    }
    for root, relative, content in (
        (source, "products/editor/apps/cosmic-edit/app.json", json.dumps(manifest)),
        (source, "products/editor/native/LICENSE", "synthetic product license\n"),
        (source, "LICENSE", "synthetic support license\n"),
        (prepared, "app.json", json.dumps(manifest)),
        (prepared, "LICENSE", "synthetic product license\n"),
        (prepared, "licenses/claw-app-gui-argv/LICENSE", "synthetic support license\n"),
    ):
        path = root / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(content)
    (source / "platform.lock.json").write_text(json.dumps({
        "version": "0.2.1", "runtime_abi": 1, "sha256": "a" * 64,
        "url": "https://github.com/xiaoyu-work/claw-os/releases/download/"
               "app-platform-v0.2.1/claw-os-app-platform-0.2.1.tar.gz",
    }))
    (prepared / "bin").mkdir()
    shutil.copy2("/bin/true", prepared / editor.ENTRY)
    for relative in editor.RESOURCES:
        path = prepared / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text("synthetic resource; never an Editor acceptance input\n")
    return source, prepared


def test_inventory_binds_every_payload_byte_and_entry_mode(payload):
    source, prepared = payload
    record = editor.payload_record(source, prepared, {"revision": "b" * 40})
    assert record["entrypoints"] == [editor.ENTRY]
    assert record["resources"] == sorted(editor.RESOURCES)
    before = record["payload_inventory"]
    resource = prepared / editor.RESOURCES[0]
    resource.write_text(resource.read_text() + "changed")
    assert editor.inventory(prepared) != before
    executable = next(item for item in before if item["path"] == editor.ENTRY)
    assert executable["mode"] & 0o111
    assert executable["digest"] == editor.digest(prepared / editor.ENTRY)


def test_product_contract_and_original_licenses_cannot_be_rewritten(payload):
    source, prepared = payload
    path = prepared / "app.json"
    original = path.read_bytes()
    manifest = json.loads(original)
    manifest["desktop"]["optional_needs"] = []
    path.write_text(json.dumps(manifest))
    with pytest.raises(ValueError, match="exact product manifest"):
        editor.payload_record(source, prepared, {})
    path.write_bytes(original)
    (prepared / "LICENSE").write_text("different license")
    with pytest.raises(ValueError, match="original product/support license"):
        editor.payload_record(source, prepared, {})


def test_header_only_elf_and_missing_installer_resources_cannot_enter_lane(payload):
    source, prepared = payload
    executable = prepared / editor.ENTRY
    original = executable.read_bytes()
    executable.write_bytes(original[:64])
    with pytest.raises(ValueError, match="loadable amd64 ELF"):
        editor.payload_record(source, prepared, {})
    executable.write_bytes(original)
    (prepared / editor.RESOURCES[1]).unlink()
    with pytest.raises(ValueError, match="original Editor installer resource"):
        editor.payload_record(source, prepared, {})
