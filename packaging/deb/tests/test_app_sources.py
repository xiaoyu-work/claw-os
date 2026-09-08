"""Pinned external App inputs must not create an unsigned runtime fallback."""

import importlib.util
import json
import os
from pathlib import Path
import subprocess

import pytest


ROOT = Path(__file__).resolve().parents[3]
SPEC = importlib.util.spec_from_file_location("app_sources", ROOT / "scripts" / "app_sources.py")
sources = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(sources)


@pytest.fixture
def locked_source(tmp_path, monkeypatch):
    upstream = tmp_path / "upstream"
    upstream.mkdir()
    git = ["git", "-C", str(upstream)]
    subprocess.run([*git, "init", "--quiet"], check=True)
    subprocess.run([*git, "config", "user.name", "Fixture"], check=True)
    subprocess.run([*git, "config", "user.email", "fixture@example.invalid"], check=True)
    (upstream / "tools").mkdir()
    (upstream / "tools" / "stage.py").write_text(
        'import pathlib, sys\n'
        'root = pathlib.Path(sys.argv[sys.argv.index("--root") + 1])\n'
        '(root / "usr/lib/cos/apps/mail-ai").mkdir(parents=True)\n'
        'print(\'["mail-ai"]\')\n'
    )
    subprocess.run([*git, "add", "tools/stage.py"], check=True)
    subprocess.run([*git, "commit", "--quiet", "-m", "fixture"], check=True)
    revision = subprocess.check_output([*git, "rev-parse", "HEAD"], text=True).strip()
    root = tmp_path / "os"
    (root / "packaging").mkdir(parents=True)
    lock = {"version": 1, "repository": str(upstream), "revision": revision,
            "products": ["mail"], "apps": ["mail-ai"]}
    (root / "packaging" / "apps.lock.json").write_text(json.dumps(lock))
    monkeypatch.setattr(sources, "ROOT", root)
    return root, lock


def test_staging_uses_exact_locked_source(locked_source, tmp_path):
    _, lock = locked_source
    target = tmp_path / "stage"
    assert sources.stage_products(target) == ["mail-ai"]
    assert (target / "usr/lib/cos/apps/mail-ai").is_dir()
    cached = sources.prepare_sources(lock)
    assert subprocess.check_output(
        ["git", "-C", str(cached), "rev-parse", "HEAD"], text=True
    ).strip() == lock["revision"]


def test_modified_cached_source_is_rejected(locked_source):
    _, lock = locked_source
    cached = sources.prepare_sources(lock)
    (cached / "tools" / "stage.py").write_text("raise RuntimeError('modified')\n")
    with pytest.raises(RuntimeError, match="modified"):
        sources.prepare_sources(lock)


def test_moving_branch_is_not_a_valid_pin(locked_source):
    root, lock = locked_source
    lock["revision"] = "main"
    (root / "packaging" / "apps.lock.json").write_text(json.dumps(lock))
    with pytest.raises(ValueError, match="full Git revision"):
        sources.read_lock()


@pytest.mark.parametrize("value", ["mail", [], [{}], ["mail", "mail"], ["../mail"]])
def test_invalid_product_list_is_rejected(locked_source, value):
    root, lock = locked_source
    lock["products"] = value
    (root / "packaging" / "apps.lock.json").write_text(json.dumps(lock))
    with pytest.raises(ValueError, match="products"):
        sources.read_lock()


def test_unexpected_installed_identity_is_rejected(locked_source, tmp_path):
    root, lock = locked_source
    lock["apps"] = ["unexpected"]
    (root / "packaging" / "apps.lock.json").write_text(json.dumps(lock))
    with pytest.raises(RuntimeError, match="identities"):
        sources.stage_products(tmp_path / "stage")


def test_agent_package_keeps_native_authority_in_os():
    script = (ROOT / "packaging/deb/build-debs.sh").read_text()
    assert '"$PROJECT_DIR/scripts/app_sources.py" --stage "$AGENT_STAGE"' in script
    assert '$AGENT_STAGE/usr/lib/cos/claw-mail-ai-host' in script
    assert '"$PROJECT_DIR/extensions/claw-mail-ai"' not in script


def test_real_app_staging_counts_nested_apps_and_installs_shared_parser(tmp_path):
    project = tmp_path / "project"
    for relative in ("alpha", "desktop-app", "gateway/slack"):
        app = project / "apps" / relative
        app.mkdir(parents=True)
        (app / "app.json").write_text(json.dumps({"id": relative.replace("/", "-")}))
    (project / "apps/canonical_argv.py").write_text("SHARED_PARSER = True\n")
    packaging = project / "packaging/deb"
    (packaging / "claw-os-desktop").mkdir(parents=True)
    (packaging / "claw-os-desktop/apps.list").write_text("desktop-app\n")
    (project / "scripts").mkdir()
    (project / "scripts/app_sources.py").write_text(
        'from pathlib import Path\n'
        'import sys\n'
        'app = Path(sys.argv[-1]) / "usr/lib/cos/apps/gateway/email"\n'
        'app.mkdir(parents=True)\n'
        '(app / "app.json").write_text(\'{"id":"gateway-email"}\')\n'
        'print(1)\n'
    )
    stage = tmp_path / "stage"
    (stage / "usr/lib/cos/apps").mkdir(parents=True)
    (stage / "usr/lib/cos/python").mkdir()
    script = (ROOT / "packaging/deb/build-debs.sh").read_text()
    block = script.split("# All non-graphical apps", 1)[1]
    block = block[block.index("DESKTOP_APPS_FILE="):]
    block = block.split('if [ -d "$PROJECT_DIR/skills" ]; then', 1)[0]
    subprocess.run(["bash", "-euc", block], check=True, env={
        **os.environ, "PROJECT_DIR": str(project), "SCRIPT_DIR": str(packaging),
        "AGENT_STAGE": str(stage),
    })
    assert (stage / "usr/lib/cos/apps/gateway/email/app.json").is_file()
    assert (stage / "usr/lib/cos/apps/gateway/slack/app.json").is_file()
    assert not (stage / "usr/lib/cos/apps/desktop-app").exists()
    assert (stage / "usr/lib/cos/python/canonical_argv.py").read_text() == "SHARED_PARSER = True\n"
