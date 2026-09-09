"""Pinned external App inputs must not create an unsigned runtime fallback."""

import importlib.util
import json
import os
from pathlib import Path
import shutil
import subprocess

import pytest


ROOT = Path(__file__).resolve().parents[3]


@pytest.mark.parametrize("service", [
    "claw-os-app-permissions-v1", "claw-os-capture-v1", "claw-os-media-player-v1",
])
def test_brokered_desktop_service_is_an_installed_dependency(service):
    agent = (ROOT / "packaging/deb/claw-os-agent/control").read_text()
    desktop = (ROOT / "packaging/deb/claw-os-desktop/control").read_text()
    assert service in next(line for line in agent.splitlines() if line.startswith("Provides:"))
    assert service in next(line for line in desktop.splitlines() if line.startswith("Depends:"))

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


def test_browser_installer_resolves_both_sources_from_the_os_pin(locked_source):
    root, lock = locked_source
    upstream = Path(lock["repository"])
    for relative in (
        "products/browser/extension/manifest.json",
        "products/browser/apps/browser-attached/native_host.py",
    ):
        path = upstream / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text("fixture\n")
    git = ["git", "-C", str(upstream)]
    subprocess.run([*git, "add", "products/browser"], check=True)
    subprocess.run([*git, "commit", "--quiet", "-m", "browser fixture"], check=True)
    lock["revision"] = subprocess.check_output([*git, "rev-parse", "HEAD"], text=True).strip()
    lock["products"] = ["browser"]
    lock["apps"] = ["browser-attached"]
    (root / "packaging/apps.lock.json").write_text(json.dumps(lock))
    (root / "scripts").mkdir()
    shutil.copyfile(ROOT / "scripts/app_sources.py", root / "scripts/app_sources.py")
    script = (ROOT / "tools/install-browser-agent.sh").read_text()
    block = script.split('APP_SOURCES=', 1)[1].split('\nEXT_DEST=', 1)[0]
    result = subprocess.run(
        ["bash", "-euc", 'APP_SOURCES=' + block + '\nprintf "%s\\n" "$EXT_SRC" "$APP_SRC"'],
        check=True, capture_output=True, text=True,
        env={**os.environ, "REPO_ROOT": str(root)},
    )
    cached = root / "build/app-sources" / lock["revision"]
    assert result.stdout.splitlines() == [
        str(cached / "products/browser/extension"),
        str(cached / "products/browser/apps/browser-attached"),
    ]
    assert (cached / "products/browser/extension/manifest.json").is_file()
    assert (cached / "products/browser/apps/browser-attached/native_host.py").is_file()


def test_browser_installer_copies_extension_assets_without_development_tests(tmp_path):
    source = tmp_path / "extension"
    source.mkdir()
    assets = {"manifest.json", "background.js", "content.js", "popup.html", "popup.js", "README.md"}
    for name in assets | {"test_contract.py"}:
        (source / name).write_text(name)
    target = tmp_path / "installed"
    script = (ROOT / "tools/install-browser-agent.sh").read_text()
    start = script.index('install -d -m 0755 "${EXT_DEST}"')
    end = script.index('echo "[claw] installing native host')
    subprocess.run(["bash", "-euc", script[start:end]], check=True, env={
        **os.environ, "EXT_SRC": str(source), "EXT_DEST": str(target),
    })
    assert {path.name for path in target.iterdir()} == assets
    for path in target.iterdir():
        assert path.read_bytes() == (source / path.name).read_bytes()
        assert path.stat().st_mode & 0o777 == 0o644


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
        'if "--stage" in sys.argv:\n'
        '    app = Path(sys.argv[sys.argv.index("--stage") + 1]) / "usr/lib/cos/apps/gateway/email"\n'
        '    app.mkdir(parents=True)\n'
        '    (app / "app.json").write_text(\'{"id":"gateway-email"}\')\n'
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


def test_native_inputs_follow_lock_updates_without_stale_files(locked_source):
    root, lock = locked_source
    upstream = Path(lock["repository"])
    (upstream / "tools/stage_native.py").write_text(
        'import json, pathlib, shutil, sys\n'
        'root = pathlib.Path(sys.argv[sys.argv.index("--root") + 1])\n'
        'product = sys.argv[1]\n'
        'shutil.copytree(pathlib.Path(__file__).parents[1] / "products" / product / "native", root / ("claw-applet-" + product))\n'
        'print(json.dumps(["claw-applet-" + product]))\n'
    )
    lock["products"] = ["calendar", "clipboard"]
    for product in lock["products"]:
        product_root = upstream / "products" / product
        (product_root / "native").mkdir(parents=True)
        (product_root / "native/Cargo.toml").write_text(product)
        (product_root / "package.json").write_text(json.dumps({
            "native": {f"claw-applet-{product}": "native"},
        }))
    native = upstream / "products/calendar/native"
    (native / "Cargo.toml").write_text("first")
    (native / "removed.rs").write_text("old")
    git = ["git", "-C", str(upstream)]

    def publish():
        subprocess.run([*git, "add", "tools/stage_native.py", "products"], check=True)
        subprocess.run([*git, "commit", "--quiet", "-m", "native fixture"], check=True)
        lock["revision"] = subprocess.check_output([*git, "rev-parse", "HEAD"], text=True).strip()
        (root / "packaging/apps.lock.json").write_text(json.dumps(lock))

    publish()
    destination = sources.prepare_native()
    assert (destination / "claw-applet-clipboard/Cargo.toml").read_text() == "clipboard"
    (destination / "unrelated").mkdir()
    (destination / "unrelated/keep").write_text("preserve")
    assert (destination / "claw-applet-calendar/removed.rs").is_file()
    (native / "removed.rs").unlink()
    (native / "Cargo.toml").write_text("second")
    publish()
    assert sources.prepare_native() == destination
    assert (destination / "claw-applet-calendar/Cargo.toml").read_text() == "second"
    assert not (destination / "claw-applet-calendar/removed.rs").exists()
    assert (destination / "unrelated/keep").read_text() == "preserve"
    assert (destination / "revision").read_text().strip() == lock["revision"]
    cached = sources.prepare_sources(lock)
    (cached / "products/calendar/native/Cargo.toml").write_text("tampered")
    with pytest.raises(RuntimeError, match="modified"):
        sources.prepare_native()


def test_native_manual_image_and_asset_build_paths_agree():
    applets = ROOT / "desktop/applets"
    cargo = (applets / "cosmic-applets/Cargo.toml").read_text()
    assert '../../../build/native-apps/claw-applet-calendar' in cargo
    assert '../../../build/native-apps/claw-applet-clipboard' in cargo
    assert '../../../build/native-apps/claw-applet-widget-rail' in cargo
    just = (applets / "justfile").read_text()
    assert 'build-debug *args: prepare-apps' in just
    assert 'python3 ../../scripts/app_sources.py --native' in just
    assert "(_install_icons calendar-src)" in just
    assert "_install_calendar" in just.split("install:", 1)[1]
    assert "_install_clipboard" in just.split("install:", 1)[1]
    assert "_install_widget_rail" in just.split("install:", 1)[1]
    assert "test -f {{ widget-rail-src }}/Cargo.toml" in just
    assert "test -f {{ clipboard-src }}/Cargo.toml" in just
    script = (ROOT / "rootfs/features/desktop/install.sh").read_text()
    assert 'CHROOT_NATIVE_APPS="$ROOTFS/build/build/native-apps"' in script
    assert 'mount --bind "$NATIVE_APP_SOURCES" "$CHROOT_NATIVE_APPS"' in script
    assert 'CLAW_NATIVE_APPS_PREPARED=1' in script
    assert 'umount "$CHROOT_NATIVE_APPS"' in script
    assert '../../../build/native-apps/{name}/data/{id}.desktop' in (
        applets / "cosmic-applets/build.rs"
    ).read_text()


@pytest.mark.parametrize(("product", "app_id"), [
    ("calendar", "panel-calendar"), ("clipboard", "panel-clipboard"),
    ("desktop-widgets", "widget-rail"),
    ("launcher", "cosmic-launcher"),
    ("editor", "cosmic-edit"),
    ("files", "cosmic-files"),
    ("terminal", "cosmic-term"),
    ("store", "cosmic-store"),
    ("settings", "cosmic-settings"),
    ("capture", "cosmic-screenshot"),
    ("media-player", "cosmic-player"),
])
def test_external_desktop_app_stays_out_of_agent_package(locked_source, tmp_path, product, app_id):
    root, lock = locked_source
    upstream = Path(lock["repository"])
    package = upstream / "products" / product
    app = package / "apps" / app_id
    app.mkdir(parents=True)
    (app / "app.json").write_text(json.dumps({"id": app_id}))
    (package / "package.json").write_text(json.dumps({"apps": [f"apps/{app_id}"]}))
    (upstream / "tools/stage.py").write_text(
        'import json, pathlib, shutil, sys\n'
        'root = pathlib.Path(sys.argv[sys.argv.index("--root") + 1])\n'
        'ids = sys.argv[sys.argv.index("--apps") + 1:]\n'
        f'if "{app_id}" in ids:\n'
        f'    source = pathlib.Path(__file__).parents[1] / "products/{product}/apps/{app_id}"\n'
        f'    shutil.copytree(source, root / "usr/lib/cos/apps/{app_id}")\n'
        'print(json.dumps(ids))\n'
    )
    git = ["git", "-C", str(upstream)]
    subprocess.run([*git, "add", f"products/{product}", "tools/stage.py"], check=True)
    subprocess.run([*git, "commit", "--quiet", "-m", "desktop fixture"], check=True)
    lock["revision"] = subprocess.check_output([*git, "rev-parse", "HEAD"], text=True).strip()
    lock["products"] = [product]
    lock["apps"] = [app_id]
    (root / "packaging/apps.lock.json").write_text(json.dumps(lock))
    partition = root / "packaging/deb/claw-os-desktop/apps.list"
    partition.parent.mkdir(parents=True)
    partition.write_text(app_id + "\n")
    assert sources.stage_products(tmp_path / "agent", "agent") == []
    assert not (tmp_path / "agent/usr/lib/cos/apps" / app_id).exists()
    assert sources.stage_products(tmp_path / "desktop", "desktop") == [app_id]
    assert (tmp_path / "desktop/usr/lib/cos/apps" / app_id / "app.json").is_file()
    assert sources.app_path(app_id).name == app_id
    with pytest.raises(ValueError, match="not in the source lock"):
        sources.app_path("unknown")
    (root / "scripts").mkdir()
    shutil.copyfile(ROOT / "scripts/app_sources.py", root / "scripts/app_sources.py")
    script = (ROOT / "packaging/deb/build-desktop-deb.sh").read_text()
    block = script[script.index("DESKTOP_APPS_FILE="):script.index("THERMALD_DEP=")]
    stage = tmp_path / "desktop-deb"
    subprocess.run(["bash", "-euc", block], check=True, env={
        **os.environ, "PROJECT_DIR": str(root), "SCRIPT_DIR": str(root / "packaging/deb"),
        "STAGE_ROOT": str(stage),
    })
    assert (stage / "usr/lib/cos/apps" / app_id / "app.json").is_file()


@pytest.mark.parametrize("components", [
    {"../escape": "native"},
    {"valid": "../escape"},
    {"valid": 123},
    ["not-a-map"],
])
def test_invalid_native_exports_are_rejected_before_replacement(tmp_path, monkeypatch, components):
    source = tmp_path / "source"
    product = source / "products/clipboard"
    product.mkdir(parents=True)
    (product / "package.json").write_text(json.dumps({"native": components}))
    monkeypatch.setattr(sources, "ROOT", tmp_path)
    monkeypatch.setattr(sources, "read_lock", lambda: {"products": ["clipboard"]})
    monkeypatch.setattr(sources, "prepare_sources", lambda _: source)
    with pytest.raises(ValueError, match="native|Native"):
        sources.prepare_native()
    assert not (tmp_path / "build/native-apps").exists()


def test_duplicate_native_exports_are_rejected(tmp_path, monkeypatch):
    for product in ("calendar", "clipboard"):
        root = tmp_path / "products" / product
        root.mkdir(parents=True)
        (root / "package.json").write_text('{"native":{"duplicate":"native"}}')
    monkeypatch.setattr(sources, "read_lock", lambda: {"products": ["calendar", "clipboard"]})
    monkeypatch.setattr(sources, "prepare_sources", lambda _: tmp_path)
    with pytest.raises(ValueError, match="duplicate"):
        sources.prepare_native()


@pytest.mark.parametrize(("product", "component"), [
    ("launcher", "launcher"), ("editor", "edit"), ("files", "files"), ("terminal", "term"),
    ("store", "store"), ("settings", "settings"), ("capture", "screenshot"), ("player", "player"),
])
def test_standalone_app_uses_external_source_and_matching_chroot_layout(product, component):
    just = (ROOT / "desktop/justfile").read_text()
    assert f"{product} := '../build/native-apps/cosmic-{component}/justfile'" in just
    assert f"{product}-build: prepare-native-apps" in just
    assert f"test -f {{{{ {product} }}}}" in just
    assert "[default]\nbuild:" in just
    assert "build: prepare-native-apps\n    #!/usr/bin/env bash" in just
    assert "export CLAW_NATIVE_APPS_PREPARED=1" in just
    assert "{{ just }} --justfile {{ " + product + " }} build-release --locked" in just
    assert "{{ just }} --justfile {{ " + product + " }} rootdir={{rootdir}} prefix={{prefix}} install" in just
    assert "{{ just }} " + component + "/build-release" not in just
    script = (ROOT / "rootfs/features/desktop/install.sh").read_text()
    assert 'CHROOT_SRC="$ROOTFS/build/desktop"' in script
    assert "cd /build/desktop" in script
    assert "/build/desktop-src" not in script
    assert not (ROOT / "desktop" / component).exists()
    assert not (ROOT / "apps" / f"cosmic-{component}").exists()
    assert f"cosmic-{component}" in sources.read_lock()["apps"]
    assert f"cosmic-{component}" in sources.desktop_apps()


@pytest.mark.skipif(shutil.which("just") is None, reason="desktop just runner unavailable")
def test_desktop_build_prepares_once_and_propagates_prepared_inputs(tmp_path):
    desktop = tmp_path / "desktop"
    desktop.mkdir()
    shutil.copyfile(ROOT / "desktop/justfile", desktop / "justfile")
    scripts = tmp_path / "scripts"
    scripts.mkdir()
    (scripts / "app_sources.py").write_text(
        "from pathlib import Path\n"
        "root = Path(__file__).resolve().parents[1]\n"
        "with (root / 'prepared').open('a') as log: log.write('prepared\\n')\n"
    )
    runner = tmp_path / "runner"
    runner.write_text(
        "#!/bin/sh\n"
        'test "$CLAW_NATIVE_APPS_PREPARED" = 1 || exit 97\n'
        'printf "%s\\n" "$*" >> "$0.calls"\n'
    )
    runner.chmod(0o755)
    env = os.environ.copy()
    env.pop("CLAW_NATIVE_APPS_PREPARED", None)
    subprocess.run(
        ["just", "--justfile", str(desktop / "justfile"),
         f"just={runner}", f"make={runner}", "build"],
        check=True, env=env, capture_output=True, text=True,
    )
    assert (tmp_path / "prepared").read_text() == "prepared\n"
    calls = (tmp_path / "runner.calls").read_text().splitlines()
    assert calls.count("editor-build") == 1
    assert calls.count("files-build") == 1
    assert calls.count("launcher-build") == 1
    assert calls.count("terminal-build") == 1
    assert calls.count("store-build") == 1
    assert calls.count("settings-build") == 1
    assert calls.count("capture-build") == 1
    assert calls.count("player-build") == 1
    assert calls.count("applets/build-release") == 1


def test_media_player_is_package_only_for_hot_swap(tmp_path):
    result = subprocess.run(
        ["bash", str(ROOT / "scripts/hot-swap-to-vmware.sh"), "--no-build", "cosmic-player"],
        env={**os.environ, "QCOW": str(tmp_path / "must-not-be-opened.qcow2")},
        capture_output=True, text=True,
    )
    assert result.returncode != 0
    assert "Media Player requires its signed manifest and claw-os-media-player-v1" in result.stderr
    assert "qcow2 missing" not in result.stderr


def test_native_player_source_status_and_package_identity_are_consistent():
    lock = sources.read_lock()
    assert "media-player" in lock["products"]
    assert lock["apps"].count("cosmic-player") == 1
    assert "cosmic-player" in sources.desktop_apps()
    assert not (ROOT / "desktop/player").exists()
    assert not (ROOT / "apps/cosmic-player").exists()
    status = (ROOT / "docs/app-product-redesign.md").read_text()
    assert f"**{len(lock['apps'])} of the original 75 identities**" in status
    desktop = len(set(lock["apps"]) & set(sources.desktop_apps()))
    assert f"{len(lock['apps']) - desktop} Agent-package identities and {desktop} desktop identities" in status
    assert f"**{len(lock['products'])} product groups**" in status
    assert "clawos-app/products/media-player" in status
    assert "products/media-player" in (ROOT / "desktop/PROVENANCE.md").read_text()
    assert "just player-build" in (ROOT / "desktop/README.md").read_text()
