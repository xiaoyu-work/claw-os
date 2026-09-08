"""The Mail UI/native protocol pair must be built and upgraded together."""

import importlib.util
import json
import os
from pathlib import Path
import zipfile

import pytest


BUILDER = Path(__file__).resolve().parents[1] / "build-mail-extension.py"
SPEC = importlib.util.spec_from_file_location("build_mail_extension", BUILDER)
builder = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(builder)


def _source(tmp_path, extension_id=builder.EXTENSION_ID):
    source = tmp_path / "extension"
    source.mkdir()
    (source / "manifest.json").write_text(json.dumps({
        "browser_specific_settings": {"gecko": {"id": extension_id}},
    }), encoding="utf-8")
    (source / "background.js").write_text("// Mail UI\n", encoding="utf-8")
    (source / "ui").mkdir()
    (source / "ui" / "popup.html").write_text("<p>Mail</p>", encoding="utf-8")
    (source / "test_fixture.py").write_text("not an asset", encoding="utf-8")
    (source / "__pycache__").mkdir()
    (source / "__pycache__" / "cache.pyc").write_bytes(b"not an asset")
    return source


def test_archive_is_deterministic_and_uses_root_manifest(tmp_path):
    source = _source(tmp_path)
    first = tmp_path / "first.xpi"
    second = tmp_path / "second.xpi"
    builder.build_extension(source, first)
    os.utime(source / "background.js", (1_700_000_000, 1_700_000_000))
    (source / "background.js").chmod(0o755)
    builder.build_extension(source, second)
    assert first.read_bytes() == second.read_bytes()
    with zipfile.ZipFile(first) as archive:
        assert archive.namelist() == ["background.js", "manifest.json", "ui/popup.html"]
        assert archive.read("ui/popup.html") == b"<p>Mail</p>"
        for item in archive.infolist():
            assert item.date_time == (1980, 1, 1, 0, 0, 0)
            assert item.external_attr >> 16 == 0o100644


def test_wrong_extension_identity_is_not_packaged(tmp_path):
    source = _source(tmp_path, "other@example.invalid")
    destination = tmp_path / "bad.xpi"
    with pytest.raises(ValueError, match="identity"):
        builder.build_extension(source, destination)
    assert not destination.exists()


def test_bundled_ui_archive_matches_the_shared_app_version(tmp_path):
    repository = BUILDER.parents[2]
    source = repository / "extensions" / "claw-mail-ai"
    destination = tmp_path / f"{builder.EXTENSION_ID}.xpi"
    builder.build_extension(source, destination)
    app = json.loads((repository / "apps/mail-ai/app.json").read_text(encoding="utf-8"))
    with zipfile.ZipFile(destination) as archive:
        manifest = json.loads(archive.read("manifest.json"))
        assert manifest["version"] == app["version"]
        assert archive.read("background.js") == (source / "background.js").read_bytes()
        assert archive.read("ui/spaces/assistant.js") == (source / "ui/spaces/assistant.js").read_bytes()
        assert "test_contract.py" not in archive.namelist()


def test_symlinked_assets_are_not_packaged(tmp_path):
    source = _source(tmp_path)
    (source / "linked.js").symlink_to(source / "background.js")
    destination = tmp_path / "bad.xpi"
    with pytest.raises(ValueError, match="symlink"):
        builder.build_extension(source, destination)
    assert not destination.exists()


def test_agent_package_owns_the_ui_native_protocol_pair():
    script = (BUILDER.parent / "build-debs.sh").read_text(encoding="utf-8")
    agent_stage = script.split("# 1. claw-os-agent", 1)[1].split("# 2. claw-os-base", 1)[0]
    assert '"$SCRIPT_DIR/build-mail-extension.py"' in agent_stage
    assert '"$PROJECT_DIR/extensions/claw-mail-ai"' in agent_stage
    assert '$AGENT_STAGE/usr/lib/thunderbird/distribution/extensions/claw-mail-ai@claw.os.xpi' in agent_stage
    assert '$AGENT_STAGE/usr/lib/cos/claw-mail-ai-host' in agent_stage
