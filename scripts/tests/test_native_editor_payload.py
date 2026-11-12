"""Nonprivileged staging checks, not native product UI acceptance."""

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


@pytest.fixture(params=["editor", "files", "terminal", "calendar"])
def payload(tmp_path, request):
    if platform.machine() not in ("x86_64", "AMD64") or not Path("/bin/true").is_file():
        pytest.skip("This payload parser test uses an existing real amd64 Linux ELF")
    source, prepared = tmp_path / "source", tmp_path / "prepared"
    product = request.param
    app, entries, resources = editor.product_layout(product)
    manifest = {
        "id": app, "runtime": "binary", "entry": entries[0],
        "mcp": {"entry": entries[0]},
        "desktop": {
            "exec": "--gui",
            "needs": [{"verb": "fs.meta", "scope": {"kind": "owner-path", "path": "**"}}],
            "optional_needs": [{"verb": "fs.read"}, {"verb": "fs.write"}],
            "entries": {
                f"auxiliary-{index}": {"entry": entry, "exec": "--gui", "needs": []}
                for index, entry in enumerate(entries[1:])
            },
        },
    }
    if product == "terminal":
        manifest["desktop"] = {"exec": "--gui"}
    elif product == "calendar":
        manifest["desktop"] = {
            "exec": "open", "panel_applet": True,
            "needs": [
                {"verb": "data.db.read", "scope": {
                    "kind": "fixed", "scope": {"kind": "name", "value": "calendar"},
                }},
                {"verb": "ui.panel", "scope": {"kind": "fixed", "scope": {"kind": "wild"}}},
            ],
        }
    for root, relative, content in (
        (source, f"products/{product}/apps/{app}/app.json", json.dumps(manifest)),
        (source, f"products/{product}/native/LICENSE", "synthetic product license\n"),
        (source, "LICENSE", "synthetic support license\n"),
        (prepared, "app.json", json.dumps(manifest)),
        (prepared, "LICENSE", "synthetic product license\n"),
    ):
        path = root / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(content)
    if product != "calendar":
        support = prepared / "licenses/claw-app-gui-argv/LICENSE"
        support.parent.mkdir(parents=True)
        support.write_text("synthetic support license\n")
    (source / "platform.lock.json").write_text(json.dumps({
        "version": "0.2.1", "runtime_abi": 1,
        "sha256": (
            "0412f750add06a32c81312c9405338687471249a01dcbf0c9688e66885dde8d8"
            if product in ("terminal", "calendar") else "a" * 64
        ),
        "url": "https://github.com/xiaoyu-work/claw-os/releases/download/"
               "app-platform-v0.2.1/claw-os-app-platform-0.2.1.tar.gz",
    }))
    (prepared / "bin").mkdir()
    for entry in entries:
        shutil.copy2("/bin/true", prepared / entry)
    for relative in resources:
        path = prepared / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text("synthetic resource; never a product acceptance input\n")
    return source, prepared, product


def test_inventory_binds_every_payload_byte_and_entry_mode(payload):
    source, prepared, product = payload
    _, entries, resources = editor.product_layout(product)
    record = editor.payload_record(source, prepared, {"revision": "b" * 40}, product)
    assert record["entrypoints"] == entries
    assert record["resources"] == sorted(resources)
    before = record["payload_inventory"]
    resource = prepared / resources[0]
    resource.write_text(resource.read_text() + "changed")
    assert editor.inventory(prepared) != before
    for entry in entries:
        executable = next(item for item in before if item["path"] == entry)
        assert executable["mode"] & 0o111
        assert executable["digest"] == editor.digest(prepared / entry)


def test_product_contract_and_original_licenses_cannot_be_rewritten(payload):
    source, prepared, product = payload
    path = prepared / "app.json"
    original = path.read_bytes()
    manifest = json.loads(original)
    manifest["desktop"]["optional_needs"] = []
    path.write_text(json.dumps(manifest))
    with pytest.raises(ValueError, match="exact product manifest"):
        editor.payload_record(source, prepared, {}, product)
    path.write_bytes(original)
    (prepared / "LICENSE").write_text("different license")
    with pytest.raises(ValueError, match="original product/support license"):
        editor.payload_record(source, prepared, {}, product)


def test_header_only_elf_and_missing_installer_resources_cannot_enter_lane(payload):
    source, prepared, product = payload
    _, entries, resources = editor.product_layout(product)
    for entry in entries:
        executable = prepared / entry
        original = executable.read_bytes()
        executable.write_bytes(original[:64])
        with pytest.raises(ValueError, match="loadable amd64 ELF"):
            editor.payload_record(source, prepared, {}, product)
        executable.write_bytes(original)
    (prepared / resources[1]).unlink()
    with pytest.raises(ValueError, match="original native installer resource"):
        editor.payload_record(source, prepared, {}, product)


@pytest.mark.parametrize("payload", ["terminal", "calendar"], indirect=True)
def test_native_product_refuses_an_unapproved_platform_digest(payload):
    source, prepared, product = payload
    path = source / "platform.lock.json"
    lock = json.loads(path.read_text())
    lock["sha256"] = "a" * 64
    path.write_text(json.dumps(lock))
    with pytest.raises(ValueError, match="approved published platform bytes"):
        editor.payload_record(source, prepared, {}, product)
