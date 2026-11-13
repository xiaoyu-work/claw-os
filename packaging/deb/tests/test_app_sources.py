"""App main resolves to coherent build snapshots without a runtime fallback."""

import fnmatch
from datetime import datetime, timedelta, timezone
from email.utils import format_datetime
import importlib.util
import json
import os
from pathlib import Path
import shlex
import shutil
import sqlite3
import subprocess
import sys

import pytest

from test_support import authenticated_mcp_params, mcp_process


ROOT = Path(__file__).resolve().parents[3]


@pytest.mark.parametrize("service", [
    "claw-os-app-permissions-v1", "claw-os-capture-v1", "claw-os-media-player-v1", "claw-os-notifications-v1",
])
def test_brokered_desktop_service_is_an_installed_dependency(service):
    agent = (ROOT / "packaging/deb/claw-os-agent/control").read_text()
    desktop = (ROOT / "packaging/deb/claw-os-desktop/control").read_text()
    assert service in next(line for line in agent.splitlines() if line.startswith("Provides:"))
    assert service in next(line for line in desktop.splitlines() if line.startswith("Depends:"))

SPEC = importlib.util.spec_from_file_location("app_sources", ROOT / "scripts" / "app_sources.py")
sources = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(sources)
PACKAGE_SPEC = importlib.util.spec_from_file_location("app_packages", ROOT / "scripts/app_packages.py")
packages = importlib.util.module_from_spec(PACKAGE_SPEC)
PACKAGE_SPEC.loader.exec_module(packages)


@pytest.fixture(scope="module")
def published_source():
    return sources.prepare_sources(sources.read_lock())


@pytest.fixture
def locked_source(tmp_path, monkeypatch):
    monkeypatch.delenv(sources.SOURCE_LOCK_ENV, raising=False)
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
    app = upstream / "products/mail/apps/mail-ai"
    app.mkdir(parents=True)
    (app / "app.json").write_text('{"id":"mail-ai"}')
    (upstream / "products/mail/package.json").write_text('{"apps":["apps/mail-ai"]}')
    subprocess.run([*git, "add", "tools/stage.py", "products/mail"], check=True)
    subprocess.run([*git, "commit", "--quiet", "-m", "fixture"], check=True)
    revision = subprocess.check_output([*git, "rev-parse", "HEAD"], text=True).strip()
    root = tmp_path / "os"
    (root / "packaging").mkdir(parents=True)
    lock = {"version": 1, "repository": str(upstream), "revision": revision,
            "products": ["mail"], "apps": ["mail-ai"]}
    (root / "packaging" / "apps.lock.json").write_text(json.dumps(lock))
    monkeypatch.setattr(sources, "ROOT", root)
    return root, lock


@pytest.fixture
def main_source(locked_source):
    root, locked = locked_source
    subprocess.run(["git", "-C", locked["repository"], "branch", "-M", "main"], check=True)
    selection = {key: value for key, value in locked.items() if key != "revision"}
    selection.update(version=2, branch="main")
    (root / "packaging/apps.lock.json").write_text(json.dumps(selection))
    return root, selection


def advance_source(selection, text):
    upstream = Path(selection["repository"])
    (upstream / "source-version").write_text(text)
    git = ["git", "-C", str(upstream)]
    subprocess.run([*git, "add", "--", "source-version"], check=True)
    subprocess.run([*git, "commit", "--quiet", "-m", text], check=True)
    return subprocess.check_output([*git, "rev-parse", "HEAD"], text=True).strip()


def test_main_refreshes_without_editing_selection_or_previous_snapshot(main_source):
    root, selection = main_source
    original = (root / "packaging/apps.lock.json").read_bytes()
    first = sources.prepare_sources(selection)
    first_manifest = (first / "products/mail/apps/mail-ai/app.json").read_bytes()
    latest = advance_source(selection, "next-main")
    second = sources.prepare_sources(sources.read_lock())
    assert second.name == latest
    assert second != first
    assert (second / "source-version").read_text() == "next-main"
    assert not (first / "source-version").exists()
    assert (first / "products/mail/apps/mail-ai/app.json").read_bytes() == first_manifest
    assert (root / "packaging/apps.lock.json").read_bytes() == original
    resolved = json.loads((root / "build/app-sources/resolved.json").read_text())
    assert resolved["version"] == 1
    assert resolved["branch"] == "main"
    assert resolved["revision"] == latest


def test_main_does_not_follow_another_branch(main_source):
    _, selection = main_source
    first = sources.prepare_sources(selection)
    subprocess.run(["git", "-C", selection["repository"], "checkout", "-qb", "other"], check=True)
    other = advance_source(selection, "other-branch")
    assert other != first.name
    assert sources.prepare_sources(selection) == first


def test_failed_main_resolution_does_not_use_cached_source(main_source):
    root, selection = main_source
    source = sources.prepare_sources(selection)
    recorded = (root / "build/app-sources/resolved.json").read_bytes()
    subprocess.run(["git", "-C", selection["repository"], "branch", "-m", "gone"], check=True)
    with pytest.raises(subprocess.CalledProcessError):
        sources.prepare_sources(selection)
    assert source.is_dir()
    assert (root / "build/app-sources/resolved.json").read_bytes() == recorded


def test_main_rejects_modified_snapshot_instead_of_resetting_it(main_source):
    _, selection = main_source
    source = sources.prepare_sources(selection)
    changed = source / "products/mail/apps/mail-ai/app.json"
    changed.write_text('{"modified": true}')
    with pytest.raises(RuntimeError, match="modified"):
        sources.prepare_sources(selection)
    assert changed.read_text() == '{"modified": true}'


def test_one_build_keeps_its_resolved_main_after_upstream_advances(main_source, tmp_path, monkeypatch):
    root, selection = main_source
    first = sources.prepare_sources(selection)
    snapshot = tmp_path / "build-source.json"
    sources.write_lock(snapshot, sources.resolved_lock(selection, first))
    latest = advance_source(selection, "newer-main")
    monkeypatch.setenv(sources.SOURCE_LOCK_ENV, str(snapshot))
    assert sources.prepare_sources(sources.read_lock()) == first
    assert sources.stage_products(tmp_path / "stage") == ["mail-ai"]
    assert sources.read_lock()["revision"] == first.name
    monkeypatch.delenv(sources.SOURCE_LOCK_ENV)
    assert sources.prepare_sources(sources.read_lock()).name == latest
    assert json.loads((root / "packaging/apps.lock.json").read_text()) == selection


def test_main_cli_records_one_snapshot_for_all_build_steps(main_source, tmp_path):
    root, selection = main_source
    script = root / "scripts/app_sources.py"
    script.parent.mkdir()
    shutil.copy2(ROOT / "scripts/app_sources.py", script)
    snapshot = tmp_path / "package/app-sources.json"
    stage = tmp_path / "stage"
    result = subprocess.run(
        [sys.executable, str(script), "--write-lock", str(snapshot), "--stage", str(stage)],
        capture_output=True, text=True,
    )
    assert result.returncode == 0, result.stderr
    assert result.stdout.strip() == "1"
    resolved = json.loads(snapshot.read_text())
    assert resolved["version"] == 1 and resolved["branch"] == "main"
    assert (stage / "usr/lib/cos/apps/mail-ai").is_dir()
    latest = advance_source(selection, "after-build-start")
    assert latest != resolved["revision"]
    reused = subprocess.run(
        [sys.executable, str(script), "--app-path", "mail-ai"],
        env={**os.environ, sources.SOURCE_LOCK_ENV: str(snapshot)},
        capture_output=True, text=True,
    )
    assert reused.returncode == 0, reused.stderr
    assert Path(reused.stdout.strip()) == (
        root / "build/app-sources" / resolved["revision"] / "products/mail/apps/mail-ai"
    )


@pytest.mark.parametrize("field,value", [
    ("branch", "other"), ("repository", "another-repository"), ("apps", ["other-app"]),
])
def test_prepared_snapshot_must_match_source_selection(main_source, tmp_path, monkeypatch, field, value):
    _, selection = main_source
    source = sources.prepare_sources(selection)
    resolved = sources.resolved_lock(selection, source)
    resolved[field] = value
    snapshot = tmp_path / "wrong-source.json"
    sources.write_lock(snapshot, resolved)
    monkeypatch.setenv(sources.SOURCE_LOCK_ENV, str(snapshot))
    with pytest.raises(ValueError, match="do not match"):
        sources.read_lock()


@pytest.mark.parametrize("change", [
    {"branch": "other"}, {"revision": "a" * 40}, {"version": 3},
])
def test_main_selection_rejects_ambiguous_or_unsupported_inputs(main_source, change):
    root, selection = main_source
    (root / "packaging/apps.lock.json").write_text(json.dumps({**selection, **change}))
    with pytest.raises(ValueError):
        sources.read_lock()


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


def _pin_fixture(root, lock, *paths):
    git = ["git", "-C", lock["repository"]]
    subprocess.run([*git, "add", "--", *paths], check=True)
    subprocess.run([*git, "commit", "--quiet", "-m", "shared staging fixture"], check=True)
    lock["revision"] = subprocess.check_output([*git, "rev-parse", "HEAD"], text=True).strip()
    (root / "packaging/apps.lock.json").write_text(json.dumps(lock))


@pytest.fixture
def shared_source(published_source, locked_source):
    root, lock = locked_source
    upstream = Path(lock["repository"])
    shutil.copy2(published_source / "tools/stage.py", upstream / "tools/stage.py")
    shutil.copy2(published_source / "tools/web_ui.py", upstream / "tools/web_ui.py")
    shutil.copytree(published_source / "shared/python", upstream / "shared/python")
    _pin_fixture(root, lock, "tools/stage.py", "tools/web_ui.py", "shared/python")
    return root, lock


def test_shared_runtime_uses_pinned_public_stage_without_os_apps(shared_source, tmp_path):
    root, lock = shared_source
    assert not (root / "apps").exists()
    target = tmp_path / "stage"
    python = target / "usr/lib/cos/python"
    sdk = python / "claw_os_sdk"
    sdk.mkdir(parents=True)
    (sdk / "owned.py").write_text("OS_SDK = True\n")
    original_sdk = _app_payload(sdk)
    assert sources.stage_shared(target) == python
    common = sources.prepare_sources(lock) / "shared/python"
    for name in ("_shared", "gateway", "canonical_argv.py"):
        assert _app_payload(python / name) == _app_payload(common / name, source=True)
    assert {path.name for path in python.iterdir()} == {
        "_shared", "gateway", "canonical_argv.py", "claw_os_sdk",
    }
    assert not (target / "usr/lib/cos/apps").exists()
    assert _app_payload(sdk) == original_sdk
    before = {path: (path.stat().st_ino, path.stat().st_mtime_ns) for path in python.rglob("*")}
    assert sources.stage_shared(target) == python
    assert before == {path: (path.stat().st_ino, path.stat().st_mtime_ns) for path in python.rglob("*")}
    assert not (root / "apps").exists()


@pytest.mark.parametrize("missing", [
    "_shared/__init__.py", "gateway/__init__.py",
    "gateway/_shared/__init__.py", "canonical_argv.py",
])
def test_missing_pinned_common_library_refuses_before_staging(shared_source, tmp_path, capfd, missing):
    root, lock = shared_source
    (Path(lock["repository"]) / "shared/python" / missing).unlink()
    _pin_fixture(root, lock, "shared/python")
    target = tmp_path / "stage"
    with pytest.raises(subprocess.CalledProcessError):
        sources.stage_shared(target)
    assert "Missing or invalid App shared Python libraries" in capfd.readouterr().err
    assert not target.exists()
    assert not (root / "apps").exists()


@pytest.mark.parametrize("conflict", ["bytes", "mode", "symlink", "extra"])
def test_common_library_conflicts_do_not_merge_or_partially_stage(shared_source, tmp_path, conflict):
    _, lock = shared_source
    target = tmp_path / "stage"
    python = target / "usr/lib/cos/python"
    python.mkdir(parents=True)
    source = sources.prepare_sources(lock) / "shared/python"
    if conflict == "bytes":
        (python / "canonical_argv.py").write_text("FOREIGN_OWNER = True\n")
    elif conflict == "mode":
        shutil.copy2(source / "canonical_argv.py", python / "canonical_argv.py")
        (python / "canonical_argv.py").chmod(0o600)
    elif conflict == "symlink":
        outside = tmp_path / "outside"
        outside.mkdir()
        (outside / "kept").write_text("unrelated content")
        (python / "gateway").symlink_to(outside, target_is_directory=True)
    else:
        shutil.copytree(source / "_shared", python / "_shared")
        (python / "_shared/foreign.py").write_text("FOREIGN_OWNER = True\n")
    before = _app_payload(python)
    with pytest.raises(subprocess.CalledProcessError):
        sources.stage_shared(target)
    assert _app_payload(python) == before


def test_common_library_payload_excludes_tests_and_bytecode(shared_source, tmp_path):
    root, lock = shared_source
    common = Path(lock["repository"]) / "shared/python"
    for relative in [
        "_shared/tests/fixture.json", "gateway/_shared/test/fixture.py",
        "_shared/conftest.py", "_shared/test_probe.py", "_shared/probe_test.py",
        "_shared/__pycache__/probe.pyc", "gateway/probe.pyc", "gateway/probe.pyo",
    ]:
        path = common / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(b"test-only payload")
    _pin_fixture(root, lock, "shared/python")
    python = sources.stage_shared(tmp_path / "stage")
    assert _app_payload(python) == _app_payload(common, source=True)


@pytest.mark.parametrize("output", ['[]', 'null', '{"root":"wrong"}', '"wrong"'])
def test_shared_stage_rejects_an_unexpected_public_result(locked_source, tmp_path, output):
    root, lock = locked_source
    (Path(lock["repository"]) / "tools/stage.py").write_text(f"print({output!r})\n")
    _pin_fixture(root, lock, "tools/stage.py")
    with pytest.raises(RuntimeError, match="declared Python root"):
        sources.stage_shared(tmp_path / "stage")


def test_common_stage_cli_is_separate_from_product_selection(tmp_path):
    result = subprocess.run(
        [sys.executable, str(ROOT / "scripts/app_sources.py"),
         "--stage-shared", str(tmp_path / "stage"), "--package", "agent"],
        capture_output=True, text=True,
    )
    assert result.returncode == 2
    assert "--stage-shared cannot be combined" in result.stderr
    assert not (tmp_path / "stage").exists()


def test_shared_stage_requires_the_returned_directory_to_exist(locked_source, tmp_path):
    root, lock = locked_source
    (Path(lock["repository"]) / "tools/stage.py").write_text(
        'import json, pathlib, sys\n'
        'root = pathlib.Path(sys.argv[sys.argv.index("--root") + 1])\n'
        'print(json.dumps(str(root / "usr/lib/cos/python")))\n'
    )
    _pin_fixture(root, lock, "tools/stage.py")
    with pytest.raises(RuntimeError, match="declared Python root"):
        sources.stage_shared(tmp_path / "stage")


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


@pytest.mark.parametrize("value", [
    "document-engine", [{}], ["document-engine", "document-engine"], ["../document-engine"],
])
def test_invalid_capability_list_is_rejected(locked_source, value):
    root, lock = locked_source
    lock["capabilities"] = value
    (root / "packaging/apps.lock.json").write_text(json.dumps(lock))
    with pytest.raises(ValueError, match="capabilities"):
        sources.read_lock()


def test_optional_capabilities_preserve_version_one_product_locks(locked_source):
    root, lock = locked_source
    assert sources.declared_sources(sources.read_lock()) == [("product", "mail")]
    lock["capabilities"] = []
    (root / "packaging/apps.lock.json").write_text(json.dumps(lock))
    assert sources.declared_sources(sources.read_lock()) == [("product", "mail")]


def test_duplicate_group_name_across_kinds_is_rejected(locked_source):
    root, lock = locked_source
    lock["capabilities"] = ["mail"]
    (root / "packaging/apps.lock.json").write_text(json.dumps(lock))
    with pytest.raises(ValueError, match="Duplicate source group"):
        sources.read_lock()


@pytest.fixture(params=[
    ("document-engine", "doc"), ("storage-sdk", "db"), ("storage-sdk", "kv"), ("http", "net"),
    ("ai-helpers", "summarize"),
])
def capability_source(locked_source, request):
    root, lock = locked_source
    upstream = Path(lock["repository"])
    group_name, app_id = request.param
    group = upstream / "capabilities" / group_name
    app = group / "apps" / app_id
    app.mkdir(parents=True)
    (group / "package.json").write_text(json.dumps({
        "kind": "shared-capability-client", "apps": [f"apps/{app_id}"],
    }))
    (app / "app.json").write_text(json.dumps({"id": app_id}))
    (upstream / "tools/stage.py").write_text(
        'import json, pathlib, shutil, sys\n'
        'kind = sys.argv[sys.argv.index("--kind") + 1] if "--kind" in sys.argv else "product"\n'
        'source = pathlib.Path(__file__).parents[1] / {"product":"products","capability":"capabilities"}[kind] / sys.argv[1]\n'
        'root = pathlib.Path(sys.argv[sys.argv.index("--root") + 1])\n'
        'selected = sys.argv[sys.argv.index("--apps") + 1:] if "--apps" in sys.argv else None\n'
        'installed = []\n'
        'for relative in json.loads((source / "package.json").read_text())["apps"]:\n'
        '    app = source / relative\n'
        '    identity = json.loads((app / "app.json").read_text())["id"]\n'
        '    if selected is None or identity in selected:\n'
        '        shutil.copytree(app, root / "usr/lib/cos/apps" / app.name)\n'
        '        installed.append(identity)\n'
        'print(json.dumps(installed))\n'
    )
    git = ["git", "-C", str(upstream)]
    subprocess.run([*git, "add", f"capabilities/{group_name}", "tools/stage.py"], check=True)
    subprocess.run([*git, "commit", "--quiet", "-m", "capability fixture"], check=True)
    lock["revision"] = subprocess.check_output([*git, "rev-parse", "HEAD"], text=True).strip()
    lock["capabilities"] = [group_name]
    lock["apps"].append(app_id)
    (root / "packaging/apps.lock.json").write_text(json.dumps(lock))
    partition = root / "packaging/deb/claw-os-desktop/apps.list"
    partition.parent.mkdir(parents=True)
    partition.write_text("desktop-fixture\n")
    return root, lock


def test_capability_uses_its_explicit_immutable_root_and_agent_partition(capability_source, tmp_path):
    root, lock = capability_source
    group, app_id = lock["capabilities"][0], lock["apps"][-1]
    assert sources.declared_sources(lock) == [("product", "mail"), ("capability", group)]
    assert sources.stage_products(tmp_path / "all") == ["mail-ai", app_id]
    assert sources.stage_products(tmp_path / "agent", "agent") == ["mail-ai", app_id]
    assert sources.stage_products(tmp_path / "desktop", "desktop") == []
    assert sources.app_path(app_id) == (
        root / "build/app-sources" / lock["revision"] / "capabilities" / group / "apps" / app_id
    )
    assert not (tmp_path / "desktop/usr/lib/cos/apps" / app_id).exists()


def test_missing_capability_never_uses_local_os_or_product_source(capability_source, monkeypatch):
    root, lock = capability_source
    group, app_id = lock["capabilities"][0], lock["apps"][-1]
    source = sources.prepare_sources(lock)
    for alternate in (root / "apps" / app_id, source / "products" / group / "apps" / app_id):
        alternate.mkdir(parents=True)
        (alternate / "app.json").write_text(json.dumps({"id": app_id}))
    (source / "capabilities" / group / "package.json").unlink()
    monkeypatch.setattr(sources, "prepare_sources", lambda _: source)
    with pytest.raises(RuntimeError, match="Locked capability source package is missing"):
        sources.app_path(app_id)


@pytest.mark.parametrize("change", ["kind", "duplicate", "traversal", "missing", "native", "dependency"])
def test_capability_contract_errors_fail_before_staging(capability_source, monkeypatch, tmp_path, change):
    _, lock = capability_source
    group, app_id = lock["capabilities"][0], lock["apps"][-1]
    source = sources.prepare_sources(lock)
    path = source / "capabilities" / group / "package.json"
    package = json.loads(path.read_text())
    if change == "kind":
        package["kind"] = "product"
    elif change == "duplicate":
        package["apps"].append(f"apps/{app_id}")
    elif change == "traversal":
        package["apps"] = [f"apps/../{app_id}"]
    elif change == "missing":
        package["apps"] = ["apps/missing"]
    elif change == "native":
        package["native"] = {}
    else:
        package["python_dependencies"] = [
            {"kind": "product", "name": "unlocked", "library": "claw_files", "apps": [app_id]},
        ]
    path.write_text(json.dumps(package))
    monkeypatch.setattr(sources, "prepare_sources", lambda _: source)
    with pytest.raises((ValueError, RuntimeError), match="kind|Duplicate|layout|missing|native|locked source"):
        sources.stage_products(tmp_path / "stage")
    assert not (tmp_path / "stage").exists()


def test_native_preparation_does_not_invent_capability_components(capability_source):
    _, lock = capability_source
    destination = sources.prepare_native()
    assert (destination / "revision").read_text().strip() == lock["revision"]
    assert json.loads((destination / "native-libraries.json").read_text())["libraries"] == {}
    assert not (destination / lock["capabilities"][0]).exists()
    assert not (destination / "doc").exists()


def test_unexpected_installed_identity_is_rejected(locked_source, tmp_path):
    root, lock = locked_source
    lock["apps"] = ["unexpected"]
    (root / "packaging" / "apps.lock.json").write_text(json.dumps(lock))
    with pytest.raises(RuntimeError, match="identities"):
        sources.stage_products(tmp_path / "stage")


def test_agent_package_keeps_native_authority_in_os():
    script = (ROOT / "packaging/deb/build-debs.sh").read_text()
    assert "scripts/app_sources.py" not in script
    assert '--variant agent --architecture "$DEB_ARCH" --stage "$AGENT_STAGE"' in script
    assert '$AGENT_STAGE/usr/lib/cos/claw-mail-ai-host' in script
    assert '$AGENT_STAGE/usr/lib/cos/bin/claw-app-apt-hook' in script
    assert '$AGENT_STAGE/usr/lib/cos/app-apt-metadata.py' in script
    assert '"$PROJECT_DIR/extensions/claw-mail-ai"' not in script


def _browser_asset_check(tmp_path, *, writable=False):
    extension = tmp_path / "extension"
    extension.mkdir()
    for name in ("manifest.json", "background.js", "content.js", "popup.html", "popup.js"):
        (extension / name).write_text("installed product resource\n")
    host = tmp_path / "browser-native-host.py"
    host.write_bytes((ROOT / "tools/browser_native_host.py").read_bytes())
    launcher = tmp_path / "claw-browser-host"
    launcher.write_text("#!/bin/sh\nexit 0\n")
    launcher.chmod(0o755)
    if writable:
        host.chmod(0o666)
    else:
        host.unlink()
    script = (ROOT / "tools/install-browser-agent.sh").read_text()
    block = script[script.index('if [[ -n "${EXT_ID}"'):script.index('echo "[claw] installing Native Messaging')]
    before = {path: path.read_bytes() for path in tmp_path.rglob("*") if path.is_file()}
    result = subprocess.run(
        ["bash", "-euc", block], capture_output=True, text=True,
        env={**os.environ, "EXT_ID": "", "EXT_DEST": str(extension),
             "HOST_IMPL": str(host), "HOST_LAUNCHER": str(launcher)},
    )
    assert {path: path.read_bytes() for path in tmp_path.rglob("*") if path.is_file()} == before
    assert result.returncode != 0
    return result.stderr


def test_browser_installer_requires_the_installed_os_transport_without_copying_assets(tmp_path):
    assert "missing installed Browser asset" in _browser_asset_check(tmp_path)


def test_browser_installer_refuses_writable_transport_before_registering_it(tmp_path):
    assert "root-owned and not group/world writable" in _browser_asset_check(tmp_path, writable=True)


@pytest.fixture
def browser_transport():
    spec = importlib.util.spec_from_file_location("browser_native_host", ROOT / "tools/browser_native_host.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def test_browser_transport_refuses_socket_overrides(browser_transport, monkeypatch):
    monkeypatch.setenv("XDG_RUNTIME_DIR", "/tmp/not-the-runtime-dir")
    monkeypatch.setenv("CLAW_BROWSER_SOCK", "/tmp/attacker.sock")
    with pytest.raises(RuntimeError, match="must resolve"):
        browser_transport._socket_path()


def test_browser_transport_request_schema_is_closed(browser_transport):
    browser_transport._validate_request({"id": "1", "verb": "tabs.list", "args": {}})
    with pytest.raises(ValueError, match="unexpected fields"):
        browser_transport._validate_request(
            {"id": "1", "verb": "tabs.list", "args": {}, "allow_eval": True}
        )


def test_browser_transport_peer_identity_comes_from_kernel(browser_transport):
    import socket

    left, right = socket.socketpair()
    with left, right:
        assert browser_transport._peer_uid(left) == os.getuid()


def test_os_installer_owns_shared_desktop_policy_without_app_sources(tmp_path):
    source = ROOT / "desktop/initial-setup"
    recipe = (source / "justfile").read_text()
    for name, variable, relative in [
        ("cosmic-settings.rules", "desktop-rules-dst", "rules.d/cosmic-settings.rules"),
        ("com.clawos.Settings.Users.policy", "accounts-policy-dst", "actions/com.clawos.Settings.Users.policy"),
    ]:
        destination = tmp_path / "package root/usr/share/polkit-1" / relative
        command = next(line.strip() for line in recipe.splitlines()
                       if line.strip().startswith("install -Dm0644 res/" + name))
        command = command.replace("'{{" + variable + "}}'", shlex.quote(str(destination)))
        subprocess.run(["bash", "-euc", command], cwd=source, check=True)
        assert destination.read_bytes() == (source / "res" / name).read_bytes()
        assert destination.stat().st_mode & 0o777 == 0o644
    import xml.etree.ElementTree as ET

    action = ET.parse(source / "res/com.clawos.Settings.Users.policy").find("action")
    assert action.get("id") == "com.clawos.Settings.Users.Admin"
    assert {node.tag: node.text for node in action.find("defaults")} == {
        "allow_any": "no", "allow_inactive": "no", "allow_active": "auth_admin_keep",
    }


@pytest.mark.parametrize("relative", [
    "usr/lib/cos/apps/alpha/app.json",
    "usr/lib/cos/python/_shared/atomic.py",
    "usr/lib/cos/python/canonical_argv.py",
    "usr/lib/cos/python/claw_files/document.py",
    "usr/share/applications/com.clawos.Files.desktop",
    "usr/bin/claw-applet-calendar",
])
def test_os_package_refuses_app_support_library_and_native_resource_ownership(tmp_path, relative):
    stage = tmp_path / "stage"
    sdk = stage / "usr/lib/cos/python/claw_os_sdk/__init__.py"
    sdk.parent.mkdir(parents=True)
    sdk.write_text("OS-owned public SDK\n")
    packages.check_stage(stage, {})
    payload = stage / relative
    payload.parent.mkdir(parents=True, exist_ok=True)
    payload.write_text("App-owned bytes\n")
    with pytest.raises(ValueError, match="still owns"):
        packages.check_stage(stage, {relative: "claw-app-fixture"})
    assert payload.read_text() == "App-owned bytes\n"


@pytest.fixture
def apt_selection(tmp_path, monkeypatch):
    root = tmp_path / "os"
    (root / "packaging/deb/claw-os-desktop").mkdir(parents=True)
    (root / "packaging/apps.lock.json").write_text(json.dumps({"apps": ["alpha", "beta", "native"]}))
    (root / "packaging/deb/claw-os-desktop/apps.list").write_text("native\n")
    (root / "packaging/app-platform.json").write_text(json.dumps({
        "schema": "claw.app-platform/v1", "version": "0.2.1", "runtime_abi": 1,
    }))
    monkeypatch.setattr(packages, "ROOT", root)
    configuration = {"sets": {"agent": "claw-apps-agent", "desktop": "claw-apps-desktop"}}
    runtime = "claw-os-app-runtime-v1 (>= 0.2.1)"
    records = [
        {"Package": "claw-apps-agent", "Version": "0.2.1", "Depends": "claw-app-alpha, claw-cap-beta"},
        {"Package": "claw-apps-desktop", "Version": "0.2.1", "Depends": "claw-apps-agent, claw-app-native-desktop"},
        {"Package": "claw-app-support", "Version": "0.2.1", "Depends": runtime, "Provides": "claw-app-support-v1"},
        {"Package": "claw-app-alpha", "Version": "0.2.1", "X-Claw-App-Ids": "alpha",
         "Depends": runtime + ", claw-app-support-v1, claw-app-python-beta-v1"},
        {"Package": "claw-cap-beta", "Version": "0.2.1", "X-Claw-App-Ids": "beta",
         "Depends": runtime + ", claw-app-support-v1", "Provides": "claw-app-python-beta-v1"},
        {"Package": "claw-app-native-desktop", "Version": "0.2.1", "X-Claw-App-Ids": "native",
         "Depends": runtime + ", claw-app-support-v1"},
    ]
    return records, configuration


def test_official_package_selection_resolves_one_library_owner_without_a_desktop(apt_selection):
    records, configuration = apt_selection
    agent = packages.selection(records, "agent", configuration)
    assert {record["Package"] for record in agent} == {
        "claw-apps-agent", "claw-app-alpha", "claw-cap-beta", "claw-app-support",
    }
    desktop = packages.selection(records, "desktop", configuration)
    assert len(desktop) == len(agent) + 2
    with pytest.raises(ValueError, match="Missing"):
        packages.selection([record for record in records if record["Package"] != "claw-cap-beta"],
                           "agent", configuration)
    records[3]["Depends"] = "claw-os-app-runtime-v1, claw-app-support-v1"
    with pytest.raises(ValueError, match="runtime floor"):
        packages.selection(records, "agent", configuration)


def test_official_package_selection_refuses_duplicate_or_incomplete_identity_owners(apt_selection):
    records, configuration = apt_selection
    records[4]["X-Claw-App-Ids"] = "alpha"
    with pytest.raises(ValueError, match="Multiple App"):
        packages.selection(records, "agent", configuration)
    records[4]["X-Claw-App-Ids"] = ""
    with pytest.raises(ValueError, match="retained identity"):
        packages.selection(records, "agent", configuration)


def test_official_native_selection_uses_the_retained_matching_product_version(apt_selection):
    records, configuration = apt_selection
    records.append({**records[3], "Version": "0.2.2"})
    records[5]["Depends"] += ", claw-app-alpha (= 0.2.1)"
    assert next(record for record in packages.selection(records, "agent", configuration)
                if record["Package"] == "claw-app-alpha")["Version"] == "0.2.2"
    assert next(record for record in packages.selection(records, "desktop", configuration)
                if record["Package"] == "claw-app-alpha")["Version"] == "0.2.1"


def test_os_build_refuses_missing_official_artifacts_without_fetching_source(tmp_path):
    with pytest.raises(ValueError, match="Official App artifacts are required"):
        packages.check("agent", "amd64", tmp_path)
    assert list(tmp_path.iterdir()) == []
    result = subprocess.run(
        ["bash", str(ROOT / "packaging/deb/build-debs.sh"), "agent"],
        capture_output=True, text=True, env={
            **os.environ, "COS_PACKAGE_VERSION": "1:0.2.0", "ARCH": "amd64",
            "COS_DEB_OUT_DIR": str(tmp_path / "debs"), "COS_DEB_STAGE_DIR": str(tmp_path / "staging"),
            "CLAW_APP_SOURCE_LOCK": str(tmp_path / "must-not-resolve-source.json"),
        },
    )
    assert result.returncode != 0
    assert "ownership cutover requires Agent/Desktop >= 1:0.2.1" in result.stderr
    assert list(tmp_path.iterdir()) == []


def test_saved_app_apt_evidence_is_reauthenticated_and_fresh_before_use(tmp_path, monkeypatch):
    generation = tmp_path / "key-generation"
    generation.mkdir(mode=0o700)
    gpg = ["gpg", "--no-options", "--batch", "--homedir", str(generation)]
    subprocess.run([*gpg, "--pinentry-mode", "loopback", "--passphrase", "",
                    "--quick-generate-key", "Private App Consumer Fixture", "ed25519", "sign", "1d"],
                   check=True, capture_output=True)
    try:
        listing = subprocess.check_output([*gpg, "--with-colons", "--list-keys"], text=True)
        fingerprint = next(line.split(":")[9] for line in listing.splitlines() if line.startswith("fpr:"))
        key = tmp_path / "archive-key.asc"
        key.write_bytes(subprocess.check_output([*gpg, "--armor", "--export", fingerprint]))
        monkeypatch.setattr(packages, "ROOT", tmp_path)
        configuration = {
            "signing_key": "archive-key.asc", "signing_fingerprint": fingerprint,
            "suite": "trixie", "component": "main",
        }
        directory = tmp_path / "evidence"
        lists = directory / "state/lists"
        lists.mkdir(parents=True)
        index = lists / "fixture_binary-amd64_Packages"
        data = b"Package: claw-app-fixture\nVersion: 0.2.1\nArchitecture: all\n\n"
        index.write_bytes(data)
        source = directory / "Release"
        issued = datetime.now(timezone.utc).replace(microsecond=0)

        def sign(start, end):
            source.write_text(
                f"Suite: trixie\nArchitectures: amd64 arm64\nComponents: main\n"
                f"Date: {format_datetime(start)}\nValid-Until: {format_datetime(end)}\n"
                f"SHA256:\n {packages.sha256(index)} {len(data)} main/binary-amd64/Packages\n"
            )
            subprocess.run([*gpg, "--yes", "--output", str(lists / "fixture_InRelease"),
                            "--clearsign", str(source)], check=True, capture_output=True)

        sign(issued, issued + timedelta(days=1))
        assert packages.authenticated_records(directory, "amd64", configuration) == [
            {"Package": "claw-app-fixture", "Version": "0.2.1", "Architecture": "all"},
        ]
        index.write_bytes(data + b"changed index")
        with pytest.raises(ValueError, match="differs from its signed release"):
            packages.authenticated_records(directory, "amd64", configuration)
        index.write_bytes(data)
        sign(issued - timedelta(days=2), issued - timedelta(days=1))
        with pytest.raises(ValueError, match="stale"):
            packages.authenticated_records(directory, "amd64", configuration)
    finally:
        subprocess.run(["gpgconf", "--homedir", str(generation), "--kill", "gpg-agent"],
                       check=False, capture_output=True)


def test_official_package_bytes_have_one_dpkg_owner_and_never_own_the_os_sdk(tmp_path):
    archives = tmp_path / "archives"
    archives.mkdir()

    def build(name, relative, provides=""):
        stage = tmp_path / name
        (stage / "DEBIAN").mkdir(parents=True)
        (stage / "DEBIAN/control").write_text(
            f"Package: {name}\nVersion: 0.2.1\nArchitecture: all\n"
            "Maintainer: Fixture <fixture@example.invalid>\nDescription: Private ownership fixture\n"
            + (f"Provides: {provides}\n" if provides else "")
        )
        payload = stage / relative
        payload.parent.mkdir(parents=True)
        payload.write_bytes(b"fixture bytes")
        artifact = archives / (name + ".deb")
        subprocess.run(["dpkg-deb", "--root-owner-group", "--build", stage, artifact],
                       check=True, capture_output=True)
        return {
            "Package": name, "Version": "0.2.1", "Architecture": "all",
            "SHA256": packages.sha256(artifact), "Size": str(artifact.stat().st_size),
            **({"Provides": provides} if provides else {}),
        }

    common = "usr/lib/cos/python/_shared/atomic.py"
    first = build("claw-app-support", common)
    assert packages.verify_packages(tmp_path, [first]) == {common: first["Package"]}
    duplicate = build("claw-app-duplicate", common)
    with pytest.raises(ValueError, match="Multiple dpkg owners"):
        packages.verify_packages(tmp_path, [first, duplicate])
    sdk = build("claw-app-bad-sdk", "usr/lib/cos/python/claw_os_sdk/__init__.py")
    with pytest.raises(ValueError, match="OS-owned SDK"):
        packages.verify_packages(tmp_path, [sdk])
    authority = build("claw-app-bad-abi", "usr/share/clawos-fixture/not-an-authority",
                      "claw-os-app-runtime-v1 (= 0.2.1)")
    with pytest.raises(ValueError, match="cannot provide"):
        packages.verify_packages(tmp_path, [authority])
    (archives / (first["Package"] + ".deb")).write_bytes(b"changed")
    with pytest.raises(ValueError, match="changed official App artifact"):
        packages.verify_packages(tmp_path, [first])


def test_runtime_provides_uses_the_platform_implementation_version(tmp_path):
    script = (ROOT / "packaging/deb/build-debs.sh").read_text()
    function = script[script.index("render_control() {"):script.index(
        "###############################################################################\n# Release-security metadata.",
    )]
    rendered = tmp_path / "control"
    subprocess.run(["bash", "-euc", function + '\nrender_control "$CONTROL" "$OUTPUT"'], check=True, env={
        **os.environ, "CONTROL": str(ROOT / "packaging/deb/claw-os-agent/control"),
        "OUTPUT": str(rendered), "VERSION": "1:0.2.1", "DEB_ARCH": "amd64",
        "APP_RUNTIME_VERSION": packages.platform()["version"], "SECURITY_ABI": "1",
        "SECURITY_EPOCH": "1", "MIN_AGENT_VERSION": "1:0.2.0",
        "MIN_BASE_VERSION": "1:0.2.0", "MIN_DESKTOP_VERSION": "1:0.2.0",
    })
    control = packages.fields(rendered.read_text())
    assert "claw-os-app-runtime-v1 (= 0.2.1)" in control["Provides"]
    assert "claw-apps-agent" in control["Recommends"]
    assert "claw-app-support" not in control["Depends"]
    assert "claw-apps-desktop" not in control["Depends"]
    assert control["Replaces"] == "claw-os-desktop (<< 1:0.2.1)"
    desktop = (ROOT / "packaging/deb/claw-os-desktop/control").read_text()
    desktop = desktop.replace("__APP_RUNTIME_VERSION__", packages.platform()["version"])
    assert "claw-os-app-runtime-v1 (>= 0.2.1)" in packages.fields(desktop)["Depends"]

    spec = importlib.util.spec_from_file_location(
        "native_runtime_assets", ROOT / "packaging/deb/native_runtime_assets.py",
    )
    assets = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(assets)
    theme = assets.stage(tmp_path / "agent-stage")
    original = ROOT / "desktop/icons/freedesktop/scalable/actions/edit-paste-symbolic.svg"
    assert (theme / "scalable/actions/edit-paste-symbolic.svg").read_bytes() == original.read_bytes()
    assert (theme / "index.theme").read_bytes() == (ROOT / "desktop/icons/index.theme").read_bytes()
    license_path = tmp_path / "agent-stage/usr/share/doc/claw-os-agent/cosmic-icons"
    assert (license_path / "README.md").read_bytes() == (ROOT / "desktop/icons/README.md").read_bytes()
    assert (license_path / "LICENSE").read_bytes() == (ROOT / "desktop/icons/LICENSE").read_bytes()
    assert (theme.stat().st_mode & 0o777) == 0o755
    with pytest.raises(ValueError, match="already staged"):
        assets.stage(tmp_path / "agent-stage")
    with pytest.raises(ValueError, match="explicit package"):
        assets.stage(Path(tmp_path.anchor))
    outside = tmp_path / "outside-runtime"
    outside.mkdir()
    aliased = tmp_path / "aliased-runtime"
    aliased.mkdir()
    (aliased / "usr").symlink_to(outside, target_is_directory=True)
    with pytest.raises(ValueError, match="cannot be symlinks"):
        assets.stage(aliased)
    assert not list(outside.iterdir())
    installer = (ROOT / "desktop/justfile").read_text()
    assert "prefix={{prefix}} icons/install" not in installer


def _app_payload(directory, *, source=False):
    entries = {}
    for path in [directory, *sorted(directory.rglob("*"))]:
        relative = path.relative_to(directory)
        if source and any(
            part in {"__pycache__", ".pytest_cache", "test", "tests", "conftest.py"}
            or any(fnmatch.fnmatch(part, pattern) for pattern in ("test_*.py", "*_test.py", "*.pyc", "*.pyo"))
            for part in relative.parts
        ):
            continue
        if path.is_symlink():
            value = ("symlink", os.readlink(path))
        elif path.is_file():
            value = ("file", path.read_bytes())
        elif path.is_dir():
            value = ("directory",)
        else:
            raise AssertionError(f"Unsupported App payload entry: {path}")
        entries[relative] = (path.lstat().st_mode, value)
    return entries


def _assert_summarize_public_wire(app, python, root):
    data = root / "owner-data/apps/summarize"
    data.mkdir(parents=True)
    private = root / "private-inputs"
    private.mkdir(mode=0o700)
    protected = [
        root / "owner-data/agent/memory.db", root / "owner-data/ai_budget.db",
        root / "owner-data/apps/db/db/existing.db", root / "owner-data/apps/kv/kv.json",
        root / "other-owner/agent/memory.db",
    ]
    for path in protected:
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(b"unrelated synthetic state")
    summary = "a" * 201 + "\nsecond line\nthird line"
    response = {
        "text": summary, "model": "fixture-model", "provider": "fixture-provider",
        "verb": "ai.chat.untrusted",
        "usage": {"input_tokens": 10, "output_tokens": 20, "units": 30},
        "budget": {"period": "2026-09", "units_used": 40, "units_cap": 100000},
        "review": {"safety": "strict", "prompt_redacted": True},
    }
    response_file = root / "response.json"
    response_file.write_text(json.dumps(response))
    scenario = root / "scenario.json"
    scenario.write_text("{}")
    log = root / "wire.jsonl"
    peer = root / "cos"
    peer.write_text(
        f"#!{sys.executable}\n"
        "import json, os, sys\n"
        "from pathlib import Path\n"
        "assert sys.argv[1] == '--wire=1'\n"
        "assert os.environ['COS_APP_ID'] == 'summarize'\n"
        "assert os.environ['COS_SESSION'] == 'summary-fixture'\n"
        "args = sys.argv[2:]\n"
        "mode = json.loads(Path(os.environ['TEST_SCENARIO']).read_text())\n"
        "record = {}\n"
        "if args[:2] == ['__policy', 'check']:\n"
        "    assert args[2:] == ['ai.chat.untrusted', '--wild'], args\n"
        "    kind = 'policy'\n"
        "    result = {'decision': 'deny' if mode.get('policy_deny') else 'allow'}\n"
        "elif args[:2] == ['ai', 'chat']:\n"
        "    kind = 'ai'\n"
        "    options = dict(zip(args[2::2], args[3::2], strict=True))\n"
        "    assert set(options) == {'--app','--origin','--prompt-file','--system-file','--max-units'}\n"
        "    assert options['--app'] == 'summarize'\n"
        "    assert options['--origin'] == 'external-content' and options['--max-units'] == '4000'\n"
        "    for name in ['prompt', 'system']:\n"
        "        path = Path(options['--' + name + '-file'])\n"
        "        assert path.is_relative_to(os.environ['TMPDIR'])\n"
        "        assert path.stat().st_mode & 0o777 == 0o600\n"
        "        assert path.parent.stat().st_mode & 0o777 == 0o700\n"
        "        record[name] = path.read_text(encoding='utf-8')\n"
        "    result = json.loads(Path(os.environ['TEST_RESPONSE']).read_text())\n"
        "elif args[:2] == ['__memory', 'remember']:\n"
        "    kind = 'memory'\n"
        "    assert len(args) == 4 and args[2] == '--json'\n"
        "    record['entry'] = json.loads(args[3])\n"
        "    assert record['entry']['source'] == 'summarize'\n"
        "    result = {'row_id': 7, 'session_id': 'app:summarize'}\n"
        "else:\n"
        "    raise AssertionError(args)\n"
        "record['kind'] = kind\n"
        "with open(os.environ['TEST_LOG'], 'a') as output:\n"
        "    output.write(json.dumps(record) + '\\n')\n"
        "error = mode.get('ai_error') if kind == 'ai' else ('PERMISSION_DENIED' if kind == 'memory' and mode.get('memory_deny') else None)\n"
        "if error:\n"
        "    print(json.dumps({'ok':False,'wire_version':1,'code':error,'error':'synthetic refusal','detail':{'decision':'deny'}}))\n"
        "    sys.exit(1)\n"
        "print(json.dumps({'ok':True,'wire_version':1,'data':result}))\n"
    )
    peer.chmod(0o755)
    with mcp_process(app, env={
        "PATH": os.defpath, "PYTHONPATH": str(python), "CLAW_COS_BIN": str(peer),
        "COS_DATA_DIR": str(data), "COS_SESSION": "summary-fixture", "TMPDIR": str(private),
        "TEST_SCENARIO": str(scenario), "TEST_RESPONSE": str(response_file), "TEST_LOG": str(log),
    }) as rpc:
        def call(arguments):
            return rpc("tools/call", authenticated_mcp_params({
                "name": "summarize.run", "arguments": arguments,
            }))

        assert [tool["name"] for tool in rpc("tools/list", {})["tools"]] == ["summarize.run"]
        assert not log.exists()
        text = "Explicit synthetic input \u00e9"
        result = call({"text": text})
        assert not result.get("isError"), result
        assert result["structuredContent"] == {
            "summary": summary, "source": "<input>",
            **{key: response[key] for key in ["model", "provider", "usage", "budget", "review"]},
        }
        records = [json.loads(line) for line in log.read_text().splitlines()]
        assert [record["kind"] for record in records] == ["policy", "ai", "memory"]
        assert records[1]["prompt"] == text
        assert records[2]["entry"] == {
            "source": "summarize", "text": f"Summarised <input>: {'a' * 197}...",
            "kind": "note", "tags": ["summarize"], "indexable": True,
        }
        for arguments in [{}, {"text": ""}, {"text": 3}, {"text": "input", "app_id": "other"}]:
            before = log.read_bytes()
            assert call(arguments)["isError"] is True
            assert log.read_bytes() == before
        for mode, error, order in [
            ({"policy_deny": True}, "PermissionDenied", ["policy"]),
            ({"ai_error": "BUDGET_EXCEEDED"}, "AiBudgetExceeded", ["policy", "ai"]),
            ({"ai_error": "SAFETY_VIOLATION"}, "AiSafetyViolation", ["policy", "ai"]),
            ({"ai_error": "PERMISSION_DENIED"}, "AiDenied", ["policy", "ai"]),
            ({"memory_deny": True}, "PermissionDenied", ["policy", "ai", "memory"]),
        ]:
            scenario.write_text(json.dumps(mode))
            log.write_text("")
            result = call({"text": text})
            assert result["isError"] is True
            assert error in result["content"][0]["text"]
            assert [json.loads(line)["kind"] for line in log.read_text().splitlines()] == order
    assert not list(private.iterdir())
    assert not list(data.iterdir())
    assert not (data.parent / "ai-helpers").exists()
    assert all(path.read_bytes() == b"unrelated synthetic state" for path in protected)


def test_published_capability_pin_stages_all_partitions_and_real_agent_runtime(tmp_path):
    lock = sources.read_lock()
    assert lock["capabilities"] == ["document-engine", "storage-sdk", "http", "ai-helpers"]
    assert len(lock["products"]) == 24
    assert len(lock["apps"]) == 63
    source = sources.prepare_sources(lock)
    lock = sources.resolved_lock(lock, source)
    snapshot = tmp_path / "selected-apps.json"
    sources.write_lock(snapshot, lock)
    expected = set(lock["apps"])
    desktop = expected & set(sources.desktop_apps())
    assert len(desktop) == 12
    assert len(expected - desktop) == 51
    assert set(sources.stage_products(tmp_path / "all", lock=lock)) == expected
    assert set(sources.stage_products(tmp_path / "agent", "agent", lock=lock)) == expected - desktop
    assert set(sources.stage_products(tmp_path / "desktop", "desktop", lock=lock)) == desktop
    assert not (tmp_path / "desktop/usr/lib/cos/python/claw_files").exists()
    assert sources.app_path("doc", lock=lock) == source / "capabilities/document-engine/apps/doc"
    assert sources.app_path("db", lock=lock) == source / "capabilities/storage-sdk/apps/db"
    assert sources.app_path("kv", lock=lock) == source / "capabilities/storage-sdk/apps/kv"
    assert sources.app_path("net", lock=lock) == source / "capabilities/http/apps/net"
    assert sources.app_path("summarize", lock=lock) == source / "capabilities/ai-helpers/apps/summarize"
    assert not (ROOT / "apps").exists()
    for partition_name in ("all", "agent", "desktop"):
        assert not (tmp_path / partition_name / "usr/lib/cos/apps/_shared").exists()
        assert not (tmp_path / partition_name / "usr/lib/cos/apps/gateway/_shared").exists()
        for library in ("_shared", "gateway", "canonical_argv.py"):
            assert not (tmp_path / partition_name / "usr/lib/cos/python" / library).exists()
    for app_id in ("db", "kv", "net", "summarize"):
        assert app_id not in desktop
        assert not (tmp_path / "desktop/usr/lib/cos/apps" / app_id).exists()

    staged = tmp_path / "agent-package"
    python = staged / "usr/lib/cos/python"
    python.mkdir(parents=True)
    (staged / "usr/lib/cos/apps").mkdir()
    for library in ("claw-os-sdk/python/src/claw_os_sdk", "cos-runtime/python/src/cos_runtime"):
        shutil.copytree(ROOT / library, python / Path(library).name,
                        ignore=shutil.ignore_patterns("__pycache__", "test_*.py"))
    sources.stage_shared(staged, lock=lock)
    sources.stage_products(staged, "agent", lock=lock)
    assert json.loads(snapshot.read_text()) == lock
    for name in ("_shared", "gateway", "canonical_argv.py"):
        assert _app_payload(python / name) == _app_payload(source / "shared/python" / name, source=True)
    assert not (staged / "usr/lib/cos/apps/_shared").exists()
    assert not (staged / "usr/lib/cos/apps/gateway/_shared").exists()
    imports = subprocess.run(
        [sys.executable, "-I", "-B", "-c",
         "import importlib, json, pathlib, sys; sys.path.insert(0, sys.argv[1]); "
         "names = ['_shared.atomic', '_shared.credentials', '_shared.safe_http', "
         "'gateway._shared.gateway_args', 'gateway._shared.safe_egress', 'canonical_argv']; "
         "print(json.dumps({name: importlib.import_module(name).__file__ for name in names}))",
         str(python)],
        cwd=tmp_path, capture_output=True, text=True, check=True,
        env={"PATH": os.defpath, "PYTHONDONTWRITEBYTECODE": "1"},
    )
    assert all(Path(path).is_relative_to(python) for path in json.loads(imports.stdout).values())
    for app_id, group in (
        ("doc", "document-engine"), ("db", "storage-sdk"), ("kv", "storage-sdk"), ("net", "http"),
        ("summarize", "ai-helpers"),
    ):
        original = source / "capabilities" / group / "apps" / app_id
        expected_ui = _app_payload(source / "capabilities" / group / "web")
        expected_ui.update({
            Path("shared") / path: entry
            for path, entry in _app_payload(source / "shared/web").items()
        })
        for partition_name in ("all", "agent", "agent-package"):
            installed = tmp_path / partition_name / "usr/lib/cos/apps" / app_id
            code = {
                path: entry for path, entry in _app_payload(installed).items()
                if path != Path("web-ui.json") and path.parts[:1] != ("web",)
            }
            assert code == _app_payload(original, source=True)
            assert _app_payload(installed / "web") == expected_ui
            descriptor = json.loads((installed / "web-ui.json").read_text())
            assert descriptor["schema"] == "claw.web-app/v1"
            assert descriptor["app_id"] == app_id
            assert descriptor["source_kind"] == "capability"
            assert descriptor["source_name"] == group
            assert descriptor["entry"] == "web/index.html"
    parser = python / "claw_files/document.py"
    assert parser.read_bytes() == (source / "products/files/python/claw_files/document.py").read_bytes()
    manifests = list((staged / "usr/lib/cos/apps").rglob("app.json"))
    assert len(manifests) == len(expected - desktop)
    assert {json.loads(path.read_text())["id"] for path in manifests} == (
        expected - desktop
    )
    _assert_summarize_public_wire(
        staged / "usr/lib/cos/apps/summarize", python, tmp_path / "summarize-runtime",
    )
    document = tmp_path / "synthetic.txt"
    document.write_text("immutable capability in the real Agent composition")
    policy = tmp_path / "cos"
    policy.write_text(
        '#!/bin/sh\n'
        'test "$1:$2:$3" = "--wire=1:__policy:check" || exit 99\n'
        'printf \'%s\\n\' \'{"ok":true,"wire_version":1,"data":{"decision":"allow"}}\'\n'
    )
    policy.chmod(0o755)
    result = subprocess.run(
        [sys.executable, "-c",
         "import json; import main; import claw_files.document as library; "
         "print(json.dumps({'library':library.__file__, 'result':main.run('read', "
         f"[{str(document)!r}])}}))"],
        cwd=staged / "usr/lib/cos/apps/doc", capture_output=True, text=True, check=True, timeout=20,
        env={"PATH": os.defpath, "PYTHONPATH": str(python),
             "PYTHONDONTWRITEBYTECODE": "1", "CLAW_COS_BIN": str(policy)},
    )
    payload = json.loads(result.stdout)
    assert payload["library"] == str(parser)
    assert payload["result"]["content"] == document.read_text()

    net = staged / "usr/lib/cos/apps/net"
    net_manifest = json.loads((net / "app.json").read_text())
    (net / net_manifest["mcp"]["entry"]).rename(net / "http_mcp.py")
    net_manifest["mcp"]["entry"] = "http_mcp.py"
    (net / "app.json").write_text(json.dumps(net_manifest))
    denied_policy = tmp_path / "deny-cos"
    denied_policy.write_text(
        '#!/bin/sh\n'
        'test "$1:$2:$3" = "--wire=1:__policy:check" || exit 99\n'
        'printf \'%s\\n\' \'{"ok":true,"wire_version":1,"data":{"decision":"deny"}}\'\n'
    )
    denied_policy.chmod(0o755)
    output = tmp_path / "not-downloaded.bin"
    with mcp_process(net, env={
        "PATH": os.defpath,
        "PYTHONPATH": str(python),
        "COS_DATA_DIR": str(tmp_path / "net-data"), "CLAW_COS_BIN": str(denied_policy),
    }) as request:
        catalog = request("tools/list", {})
        assert {tool["name"] for tool in catalog["tools"]} == {"net.fetch", "net.download"}
        for name, arguments in [
            ("fetch", {"url": "http://example.test:8080/resource"}),
            ("download", {"url": "http://example.test:8080/resource", "output": str(output)}),
        ]:
            result = request("tools/call", authenticated_mcp_params({
                "name": f"net.{name}", "arguments": arguments,
            }))
            assert result["isError"] is True
            assert "PermissionDenied" in result["content"][0]["text"]
    assert not output.exists()

    data = tmp_path / "owner-data/apps/db"
    database = data / "db/inventory.db"
    database.parent.mkdir(parents=True)
    with sqlite3.connect(database) as connection:
        connection.execute("CREATE TABLE items (value TEXT)")
        connection.execute("INSERT INTO items VALUES ('existing state')")
    identity = (database.stat().st_dev, database.stat().st_ino)
    neighbours = [
        tmp_path / "owner-data/apps/kv/kv.json",
        tmp_path / "owner-data/agent/memory.db",
        tmp_path / "other-owner/apps/db/db/inventory.db",
    ]
    for neighbour in neighbours:
        neighbour.parent.mkdir(parents=True, exist_ok=True)
        neighbour.write_bytes(b"unrelated private state")
    db_policy = tmp_path / "db-policy"
    db_policy.write_text(
        '#!/bin/sh\n'
        'test "$1:$2:$3" = "--wire=1:__policy:check" || exit 99\n'
        'test "$COS_APP_ID:$COS_SESSION" = "db:db-fixture" || exit 98\n'
        'case "$4:$5:$6" in\n'
        '  data.db.read:--name:inventory|data.db.write:--name:inventory) decision=allow;;\n'
        '  *) decision=deny;;\n'
        'esac\n'
        'printf \'{"ok":true,"wire_version":1,"data":{"decision":"%s"}}\\n\' "$decision"\n'
    )
    db_policy.chmod(0o755)
    calls = [
        ("query", {"database": "inventory", "sql": "SELECT value FROM items"}),
        ("exec", {"database": "inventory", "sql": "UPDATE items SET value = 'updated state'"}),
        ("query", {"database": "inventory", "sql": "SELECT value FROM items"}),
        ("query", {
            "database": "inventory",
            "sql": "WITH RECURSIVE seq(n) AS (VALUES(1) UNION ALL SELECT n+1 FROM seq WHERE n<1001) SELECT n FROM seq",
        }),
        ("schema", {"database": "inventory", "table": "items"}),
        ("tables", {"database": "inventory"}),
        ("exec", {"database": "other", "sql": "CREATE TABLE denied (value)"}),
        ("databases", {}),
        ("query", {"database": "inventory", "sql": "DELETE FROM items"}),
    ]
    db_app = staged / "usr/lib/cos/apps/db"
    with mcp_process(db_app, env={
        "PATH": os.defpath, "PYTHONPATH": str(python), "COS_DATA_DIR": str(data),
        "COS_SESSION": "db-fixture", "CLAW_COS_BIN": str(db_policy),
    }) as request:
        results = [
            request("tools/call", authenticated_mcp_params({
                "name": f"db.{command}", "arguments": arguments,
            }))
            for command, arguments in calls
        ]
    assert results[0]["structuredContent"] == {
        "database": "inventory", "columns": ["value"], "rows": [["existing state"]], "count": 1,
    }
    assert results[1]["structuredContent"]["rows_affected"] == 1
    assert results[2]["structuredContent"]["rows"] == [["updated state"]]
    assert results[3]["structuredContent"] == {
        "database": "inventory", "columns": ["n"], "rows": [[i] for i in range(1, 1001)],
        "count": 1000, "truncated": True, "total_rows": 1001,
    }
    assert results[4]["structuredContent"] == {
        "database": "inventory", "table": "items", "schema": "CREATE TABLE items (value TEXT)",
    }
    assert results[5]["structuredContent"] == {"database": "inventory", "tables": ["items"]}
    for denied in results[6:8]:
        assert denied["isError"] is True
        assert "PermissionDenied" in denied["content"][0]["text"]
    assert results[8]["isError"] is True
    assert "database query failed" in results[8]["content"][0]["text"]
    with sqlite3.connect(database) as connection:
        assert connection.execute("SELECT value FROM items").fetchall() == [("updated state",)]
    assert (database.stat().st_dev, database.stat().st_ino) == identity
    assert [path.name for path in database.parent.iterdir()] == ["inventory.db"]
    assert all(neighbour.read_bytes() == b"unrelated private state" for neighbour in neighbours)
    assert not (data.parent / "storage-sdk").exists()

    kv_data = tmp_path / "kv-owner-data/apps/kv"
    kv_data.mkdir(parents=True)
    store = kv_data / "kv.json"
    original = b'{\n  "kept": "existing state", "empty": ""\n}\n'
    store.write_bytes(original)
    kv_neighbours = [
        tmp_path / "kv-owner-data/apps/db/db/existing.db",
        tmp_path / "kv-owner-data/apps/storage-manager/state",
        tmp_path / "kv-owner-data/agent/memory.db",
        tmp_path / "kv-other-owner/apps/kv/kv.json",
    ]
    for neighbour in kv_neighbours:
        neighbour.parent.mkdir(parents=True, exist_ok=True)
        neighbour.write_bytes(b"unrelated private state")
    kv_app = staged / "usr/lib/cos/apps/kv"
    environment = {
        "PATH": os.defpath, "PYTHONPATH": str(python),
        "COS_DATA_DIR": str(kv_data), "COS_SESSION": "kv-fixture",
    }

    def kv_call(request, command, arguments=None):
        return request("tools/call", authenticated_mcp_params({
            "name": f"kv.{command}", "arguments": arguments or {},
        }))

    with (
        store.open("rb") as old,
        mcp_process(kv_app, env=environment) as first,
        mcp_process(kv_app, env=environment) as second,
    ):
        identity = os.fstat(old.fileno())
        catalog = first("tools/list", {})["tools"]
        assert [tool["name"] for tool in catalog] == [
            "kv.get", "kv.set", "kv.del", "kv.list", "kv.dump",
        ]
        assert catalog[3]["inputSchema"]["properties"]["pattern"]["default"] == "*"
        assert kv_call(first, "get", {"key": "kept"})["content"][0]["text"] == "existing state"
        assert kv_call(second, "dump")["structuredContent"] == {
            "count": 2, "data": {"kept": "existing state", "empty": ""},
        }
        assert store.read_bytes() == original
        for peer, key, value in ((first, "left", "one"), (second, "right", "two")):
            assert kv_call(peer, "set", {"key": key, "value": value})["structuredContent"] == {
                "key": key, "value": value,
            }
        assert kv_call(first, "list")["structuredContent"] == {
            "pattern": "*", "keys": ["empty", "kept", "left", "right"],
        }
        assert kv_call(second, "list", {"pattern": "l*"})["structuredContent"] == {
            "pattern": "l*", "keys": ["left"],
        }
        assert kv_call(first, "del", {"key": "left"})["structuredContent"] == {
            "key": "left", "deleted": True,
        }
        assert kv_call(second, "get", {"key": "left"})["content"][0]["text"] == ""
        assert old.read() == original
        assert (store.stat().st_dev, store.stat().st_ino) != (identity.st_dev, identity.st_ino)
        for command, arguments in [
            ("get", {"key": 42}), ("set", {"key": "bad", "value": False}),
            ("dump", {"session_id": "forged"}),
        ]:
            assert kv_call(first, command, arguments)["isError"] is True
    expected = {"kept": "existing state", "empty": "", "right": "two"}
    assert json.loads(store.read_text()) == expected
    assert store.stat().st_mode & 0o777 == 0o600
    assert (kv_data / "kv.json.lock").stat().st_mode & 0o777 == 0o600
    assert {path.name for path in kv_data.iterdir()} == {"kv.json", "kv.json.lock"}
    with mcp_process(kv_app, env=environment) as restarted:
        assert kv_call(restarted, "dump")["structuredContent"] == {"count": 3, "data": expected}
        store.write_bytes(b'{"corrupt": false}')
        for command, arguments in [("dump", {}), ("set", {"key": "new", "value": "value"})]:
            result = kv_call(restarted, command, arguments)
            assert result["isError"] is True
            assert "kv store must contain only string keys and values" in result["content"][0]["text"]
            assert store.read_bytes() == b'{"corrupt": false}'
    assert all(neighbour.read_bytes() == b"unrelated private state" for neighbour in kv_neighbours)
    assert not (kv_data.parent / "storage-sdk").exists()
    assert not (tmp_path / "kv-owner-data/kv.json").exists()


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
    assert json.loads((destination / "source-lock.json").read_text()) == lock
    cached = sources.prepare_sources(lock)
    (cached / "products/calendar/native/Cargo.toml").write_text("tampered")
    with pytest.raises(RuntimeError, match="modified"):
        sources.prepare_native()


def test_native_manual_image_and_asset_build_paths_agree():
    applets = ROOT / "desktop/applets"
    cargo = (applets / "cosmic-applets/Cargo.toml").read_text()
    assert "build/native-apps" not in cargo
    build = (applets / "cosmic-applets/build.rs").read_text()
    dispatcher = (applets / "cosmic-applets/src/main.rs").read_text()
    just = (applets / "justfile").read_text()
    for product in ("calendar", "clipboard", "widget-rail"):
        assert f"claw-applet-{product}" not in cargo + build + just
        assert f"claw_applet_{product.replace('-', '_')}" not in dispatcher
    assert "scripts/app_sources.py" not in just
    assert "install-applet-provider" in just
    layout = (ROOT / "desktop/panel/cosmic-panel-config/src/container_config.rs").read_text()
    for identity in ("PanelCalendarButton", "AppletClipboard", "AppletWidgetRail"):
        assert f"com.clawos.{identity}" in layout
    script = (ROOT / "rootfs/features/desktop/install.sh").read_text()
    assert "scripts/app_sources.py" not in script
    assert "CHROOT_NATIVE_APPS" not in script
    assert '"$PROJECT_DIR/scripts/app_packages.py" native-boundary' in script
    assert '"$APP_PACKAGE_INPUTS"/archives/*.deb' in script
    packages.native_boundary()


def test_native_source_guard_still_rejects_reintroduced_product_dependencies(tmp_path, monkeypatch):
    source = tmp_path / "desktop/applets/consumer"
    source.mkdir(parents=True)
    (source / "Cargo.toml").write_text(
        '[dependencies]\nproduct = { path = "../../../build/native-apps/product" }\n'
    )
    monkeypatch.setattr(packages, "ROOT", tmp_path)
    with pytest.raises(ValueError, match="still links App-owned"):
        packages.native_boundary()


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
    ("notifications", "cosmic-notifications"),
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
    script = (ROOT / "packaging/deb/build-desktop-deb.sh").read_text()
    assert "scripts/app_sources.py" not in script
    assert '--variant desktop --architecture "$DEB_ARCH" --stage "$STAGE_ROOT"' in script


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
    ("notifications", "notifications"),
])
def test_standalone_app_uses_external_source_and_matching_chroot_layout(product, component):
    just = (ROOT / "desktop/justfile").read_text()
    assert f"{product} := '../build/native-apps/cosmic-{component}/justfile'" in just
    assert f"{product}-build: prepare-native-apps" in just
    assert f"test -f {{{{ {product} }}}}" in just
    assert "default: build" in just
    assert "build-source-fixtures: prepare-native-apps\n    #!/usr/bin/env bash" in just
    production = just.split("\nbuild:", 1)[1].split("# Explicit development fixtures", 1)[0]
    assert "scripts/app_packages.py native-boundary" in production
    assert "--justfile" not in production
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
    snapshot = tmp_path / "source-fixture.json"
    snapshot.write_text('{"version":1,"revision":"explicit-test-fixture"}\n')
    env["CLAW_APP_SOURCE_LOCK"] = str(snapshot)
    subprocess.run(
        ["just", "--justfile", str(desktop / "justfile"),
         f"just={runner}", f"make={runner}", "build-source-fixtures"],
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
    assert calls.count("notifications-build") == 1
    assert calls.count("calendar-build") == 1
    assert calls.count("clipboard-build") == 1
    assert calls.count("widgets-build") == 1
    assert calls.count("build-shell") == 1


def test_media_player_is_package_only_for_hot_swap(tmp_path):
    result = subprocess.run(
        ["bash", str(ROOT / "scripts/hot-swap-to-vmware.sh"), "--no-build", "cosmic-player"],
        env={**os.environ, "QCOW": str(tmp_path / "must-not-be-opened.qcow2")},
        capture_output=True, text=True,
    )
    assert result.returncode != 0
    assert "Media Player requires its signed manifest and claw-os-media-player-v1" in result.stderr
    assert "qcow2 missing" not in result.stderr


def test_notifications_is_package_only_for_hot_swap(tmp_path):
    result = subprocess.run(
        ["bash", str(ROOT / "scripts/hot-swap-to-vmware.sh"), "--no-build", "cosmic-notifications"],
        env={**os.environ, "QCOW": str(tmp_path / "must-not-be-opened.qcow2")},
        capture_output=True, text=True,
    )
    assert result.returncode != 0
    assert "Notifications requires its signed manifest and claw-os-notifications-v1" in result.stderr
    assert "qcow2 missing" not in result.stderr


def test_native_notifications_keep_private_inputs_and_legacy_state_separate(published_source, tmp_path):
    lock = sources.read_lock()
    assert lock["apps"].count("cosmic-notifications") == 1
    assert "notifications" in lock["products"]
    assert not (ROOT / "desktop/notifications").exists()
    assert not (ROOT / "apps/cosmic-notifications").exists()
    assert not (ROOT / "apps/notify").exists()
    assert lock["apps"].count("notify") == 1
    applet = (ROOT / "desktop/applets/cosmic-applet-notifications/Cargo.toml").read_text()
    panel = (ROOT / "desktop/panel/cosmic-panel-bin/Cargo.toml").read_text()
    for library in ("cosmic-notifications-config", "cosmic-notifications-util"):
        assert library not in applet + panel
        assert f'cosmic-notifications/{library}/Cargo.toml' not in (
            ROOT / "rootfs/features/desktop/install.sh"
        ).read_text()
    product = published_source / "products/notifications"
    package = json.loads((product / "package.json").read_text())
    assert sources.native_libraries(package, product) == {}
    staged = tmp_path / "native"
    output = subprocess.check_output(
        [sys.executable, str(published_source / "tools/stage_native.py"),
         "notifications", "--root", str(staged)],
        text=True, cwd=published_source,
    )
    assert json.loads(output) == ["cosmic-notifications"]
    component = product / package["native"]["cosmic-notifications"]
    for library in ("cosmic-notifications-config", "cosmic-notifications-util"):
        assert _app_payload(staged / "cosmic-notifications" / library) == _app_payload(component / library)
    assert "claw-notification-presentation" in applet + panel
    assert 'git = "https://github.com/pop-os/cosmic-notifications"' not in applet + panel
    panel_just = (ROOT / "desktop/panel/justfile").read_text()
    assert "prepare-apps" not in panel_just
    assert "scripts/app_sources.py" not in panel_just
    status = (ROOT / "docs/app-product-redesign.md").read_text()
    assert "clawos-app/products/notifications" in status
    assert "Source migration covered all original 75 identities" in status
    assert "**24 business product groups**" in status
    assert "**4 shared-capability source groups**: 51 Agent-package identities and" in status
    assert "products/notifications" in (ROOT / "desktop/PROVENANCE.md").read_text()
    assert "just notifications-build" in (ROOT / "desktop/README.md").read_text()


def test_notify_is_agent_owned_and_preserves_history_without_an_installer_transition():
    lock = sources.read_lock()
    desktop = set(sources.desktop_apps())
    assert len(lock["apps"]) == 63
    assert len(lock["products"]) == 24
    assert lock["capabilities"] == ["document-engine", "storage-sdk", "http", "ai-helpers"]
    assert len(set(lock["apps"]) & desktop) == 12
    assert len(set(lock["apps"]) - desktop) == 51
    assert "gateway-email" not in lock["apps"]
    assert "notify" not in desktop
    assert "cosmic-notifications" in desktop
    assert not (ROOT / "apps/notify").exists()
    migration = (ROOT / "core/src/worker/migrate.rs").read_text()
    assert '("notify", &[Legacy::File("notifications.json")])' not in migration
    contract = (ROOT / "docs/updating.md").read_text()
    assert "Historical JSON is preserved, not imported" in contract
    assert "not part of the new service list" in contract
    assert "data.inbox.read" in contract
    assert "warning severity" in contract
    assert "51 Agent identities plus 12 desktop" in (ROOT / "packaging/MODULE.md").read_text()


@pytest.mark.parametrize("export", [
    {"component": "other", "path": "public"},
    {"component": "daemon", "path": "../outside"},
    {"component": "daemon", "path": "/private"},
    {"component": [], "path": "public"},
    {"component": "daemon", "path": "public", "authority": True},
])
def test_invalid_public_native_libraries_are_refused_before_replacement(tmp_path, monkeypatch, export):
    product = tmp_path / "source/products/notifications"
    product.mkdir(parents=True)
    (product / "package.json").write_text(json.dumps({
        "native": {"daemon": "native"}, "native_libraries": {"public-library": export},
    }))
    monkeypatch.setattr(sources, "ROOT", tmp_path)
    monkeypatch.setattr(sources, "read_lock", lambda: {"products": ["notifications"]})
    monkeypatch.setattr(sources, "prepare_sources", lambda _: tmp_path / "source")
    with pytest.raises(ValueError, match="native|Native"):
        sources.prepare_native()
    assert not (tmp_path / "build/native-apps").exists()


def test_native_library_paths_and_cargo_identity_are_bound_to_the_product(tmp_path):
    library = tmp_path / "native/public"
    library.mkdir(parents=True)
    (library / "Cargo.toml").write_text('[package]\nname = "public-library"\n')
    package = {
        "native": {"daemon": "native"},
        "native_libraries": {"public-library": {"component": "daemon", "path": "public"}},
    }
    assert sources.native_libraries(package, tmp_path) == {"public-library": "daemon/public"}
    (library / "Cargo.toml").write_text('[package]\nname = "another-library"\n')
    with pytest.raises(ValueError, match="identity"):
        sources.native_libraries(package, tmp_path)


def test_native_player_source_status_and_package_identity_are_consistent():
    lock = sources.read_lock()
    assert "media-player" in lock["products"]
    assert lock["apps"].count("cosmic-player") == 1
    assert "cosmic-player" in sources.desktop_apps()
    assert not (ROOT / "desktop/player").exists()
    assert not (ROOT / "apps/cosmic-player").exists()
    status = (ROOT / "docs/app-product-redesign.md").read_text()
    assert f"retains **{len(lock['apps'])} identities**" in status
    desktop = len(set(lock["apps"]) & set(sources.desktop_apps()))
    assert f"{len(lock['apps']) - desktop} Agent-package identities and {desktop} desktop identities" in " ".join(status.split())
    assert f"**{len(lock['products'])} business product groups**" in status
    assert "clawos-app/products/media-player" in status
    assert "products/media-player" in (ROOT / "desktop/PROVENANCE.md").read_text()
    assert "just player-build" in (ROOT / "desktop/README.md").read_text()
